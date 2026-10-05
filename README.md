# lattice-net

The custom UDP transport for a PlanetSide-style MMOFPS, where each continent runs as one server process targeting up to 10k players. Written in Rust; its one dependency is `ring`, for ChaCha20-Poly1305 and OS randomness. It's sans-IO: the protocol code never touches a socket, so you feed it datagrams and a timestamp and drain the datagrams it wants sent.

```
cargo test --release                                   # workspace: 42 transport tests incl. 25%-loss/jitter/dup sim, + sim/
cargo run --release --example server                   # real UDP, 30 Hz tick, echo
cargo run --release --example client 127.0.0.1:40000 5
```

The game rules both sides run (movement, the world, message formats) are in [`game/`](game/) (`lattice-game`). The client every player runs, bots and humans alike (input clock, prediction, the render timeline and entity interpolation), is [`client-core/`](client-core/) (`lattice-client-core`). The playable Bevy client is [`client/`](client/README.md) (`lattice-client`), its own Cargo workspace; `scripts/client-windows.sh` builds it for Windows from WSL. The M1 headless scale test (the server, a bot swarm and per-phase tick metrics) lives in [`sim/`](sim/README.md): `scripts/m1.sh blob 3000 60` runs one scenario, and `scripts/baseline.sh full <name>` runs the whole matrix into a comparable baseline in `baselines/`.

## Wire format

Every datagram starts with a type byte. There's no checksum: packets are authenticated instead.

- **Payload, disconnect and accept packets are sealed** with ChaCha20-Poly1305, using one key per direction per connection. The 16-byte tag rejects garbage, corruption, forgeries and other protocol versions before any state is touched.
- **The protocol id is part of every tag and token, but never sent.**
- **The client's handshake packets carry a connect token** that only the server can open.

### Payload packet (the hot path, sent every tick)

```
type:1 | seq:2 | sealed( ack:2 | ack_bits:32 | ack_delay:2 | messages... ) | tag:16
                                                                            ^ 27 B fixed overhead
message := kind:1 [id:2 if reliable] len:1-2 bytes
padding := kind:1 (=2) zeros...     // to the end of the packet
nonce   := 0:4 | the sender's 64-bit packet counter      AD := protocol_id ++ type ++ seq
```

- `seq` is the low 16 bits of the sender's packet counter, compared with half-range arithmetic.
- **The nonce is the full 64-bit counter.** The receiver rebuilds it from `seq` and the newest counter it has authenticated, as QUIC decodes packet numbers. The counter never repeats under a key, and an old packet replayed from a previous wrap fails the tag. A replay inside the window authenticates but is dropped as a duplicate.
- `ack` is the newest packet received from the peer. `ack_bits` covers the 32 packets before it.
- So **every packet acks the last 33**. With 30–60 packets/s per direction, an ack survives unless ~33 consecutive packets are lost. You never send dedicated ack packets.
- `ack_delay` is how long packet `ack` waited here before this packet carried its ack, in 10 µs units (saturating at ~655 ms). The peer subtracts it from its RTT sample (as QUIC does), so RTT measures the network, not the peer's tick rate.
- **Disconnect** is `type | seq | tag`, sealed like a payload. It uses up a sequence, so it can't be forged or replayed.
- **Padding.** With `Config::pad_packets`, `flush` pads every packet but a connection's last of the tick to `max_packet_size`, so the tick's packets can go out as one GSO send (`UDP_SEGMENT` needs equal-size segments). Padding is a kind-2 marker and zeros to the end; a receiver always accepts it and needs no setting. Padding bytes are counted in `Stats::padding_bytes`. `Config::packet_body_size` and `Config::unreliable_wire_size` let an application predict how its messages pack, and so fill packets instead of padding them (the sim does this for its mid and far tiers).

### Connect tokens (`token.rs`, netcode.io-style)

A login service (not in this crate) authenticates the player, then mints a token for one game server (`ConnectToken::mint`) and hands it to the client over TLS:

```
server_id:8 | expires:8 | c2s_key:32 | s2c_key:32 | private:132
private := nonce:12 | ChaCha20-Poly1305(token_key, ad = protocol_id ++ server_id ++ expires,
                                        user_id:8 | c2s_key:32 | s2c_key:32 | user_data:32) | tag:16
```

- The client can't read or alter `private`.
- The server opens it with the `token_key` it shares with the login service (`ServerIdentity`). It learns the user and the connection's two fresh keys without ever talking to the login service.
- Tokens should expire within tens of seconds. Nonces are random, which is safe for ~2^32 tokens per key.

### Handshake (stateless until accept, can't be used for amplification)

```
C→S  Request    { server_id, expires, private, salt }                 padded to 256 B
S→C  Challenge  { salt, cookie }                                      17 B   ← server stores NOTHING
C→S  Response   { server_id, expires, private, salt, cookie, tag }    padded to 256 B
S→C  Accepted   { salt, sealed(client_id), tag }                      29 B   ← slot allocated here
```

- **A request only gets a Challenge if its token opens.** It must be for this server, unexpired, sealed with our key, and on our protocol version. Anything else is dropped without a reply.
- **The cookie proves the address.** `cookie = SipHash(server_secret, client_addr, salt, 10s_time_bucket)`, so only a client that receives packets at its claimed address can echo it back. Spoofed floods can't fill connection slots.
- **The Response proves the keys.** It's tagged with the client→server key (nonce domain 1, counter = cookie). A token copied off the wire is useless without the keys the client got over TLS.
- **Accepted is sealed with the server→client key,** so a client only believes its own server.
- **A token connects once.** Its keys become the connection's, so a second connection would reuse their nonces. The server remembers used tokens until they expire, in a small registry shared by the shards. It's behind one lock, taken only on accept and removal.
- **A user connecting again replaces the old connection** (`DisconnectReason::Replaced`), even across shards, and so does a new client instance at a connected address. The new connection needs a fresh token, and it proved its keys.
- **The app learns who connected:** `ServerEvent::Connected` carries the token's `user_id` and 32 B of `user_data`.
- **Wall clock:** `Server::update(now, unix_secs)` takes wall-clock time for token expiry.
- **Amplification:** requests and responses are padded bigger than any reply. The server rejects handshake packets of the wrong size.
- **Admission control.** `Config::max_accepts_per_tick` (default 256, server-wide, split across shards) caps new connections per tick. A client over the budget just isn't answered: it resends its Response every 100 ms, and its cookie stays valid for 10–20 s. Because the handshake is stateless, deferring costs nothing, and a mass join (a server restart, a continent unlocking) is spread over several ticks.
- **Cheap accepts.** Removed connections go back to a per-shard pool, and `Server::preallocate(n)` fills the pools up front (with 25% headroom for uneven hashing). An accept then resets a pooled connection: about 4 µs, against 24 µs for a fresh one and 138 µs before the windows shrank. In the sim, 10k simultaneous joins without a budget finish in p99 215 ms, with one 58 ms tick. With the default budget, the worst tick is ~31 ms and p99 join time is 1.75 s. Keep the budget as a safety valve.

### Crypto cost

`ring`'s ChaCha20-Poly1305 (BoringSSL's assembly, dispatched at run time) takes 0.17 µs to open a 60 B packet and 0.59 µs for 1,200 B on the Ryzen 5700X3D. The CRC32 it replaced took 0.11 and 2.4 µs.

RustCrypto's `chacha20poly1305` was tried first: 0.43 and 1.26 µs even with AVX2 enabled at compile time. The small-packet cost added ~0.7 ms of ingress at 10k and cost the server a ladder level on the WSL box. With `ring`, the level and tick p99 match the unencrypted build (see `sim/README.md`).

## Channels

| | Unreliable | Reliable (ordered) |
|---|---|---|
| Use for | snapshots, inputs, anything latest-wins | kills, captures, spawns, chat, inventory |
| Overhead | 2 B/msg | 4 B/msg |
| On loss | gone; the next tick's state supersedes it | resent after ~1.25×RTT until acked |
| Ordering | none. Tag state with a tick number | strict, exactly-once |
| Blocking | never | a lost message holds back later **reliable** messages only |

**How reliable works on top of packet acks:**
- Each sent packet records which reliable message ids it carried.
- When that packet gets acked (via any of the 33 redundant acks), those messages are done.
- Unacked messages are rewritten into new packets after the resend interval.
- The receiver buffers out-of-order ids in a 256-slot window and delivers contiguous runs. The window is part of the protocol: both ends must use the same size. The protocol id changes with any such rule: `LATTICE1` was these windows, and `LATTICE2` added padding.
- Limits:
  - Up to 256 messages can be in flight; excess waits in a backlog.
  - Up to 32 reliable messages per packet. A sent packet stays tracked for 256 packets, during which at most 32 × 256 = 8,192 new ids are issued, far below 65,536. So a late ack can never point at a reused message id.

**Unreliable** messages are packed after reliable ones. Anything that doesn't fit in this flush's packets (max 4 per connection per flush by default) is dropped and counted in `Stats::unreliable_dropped`. That's deliberate: next tick's snapshot is fresher.

**Delivery tags.** `send_tagged(client, data, tag)` sends an unreliable message with a `u32` tag of the application's choosing. Each sent packet keeps up to 8 tags inline. When a packet is acked, its tags come back through `take_acked(client, &mut out)`. Loss is never reported; a tag just doesn't come back. That's what delta compression needs (the sim's near tier uses its tick as the tag), and the transport stays agnostic about what the tags mean.

## Sharding

`Server::with_shards(cfg, identity, max_clients, n, now)` partitions connections into `n` independent `Shard`s. The crate still spawns no threads: the caller drives the shards from its own pool.

**Socket groups.** `Server::with_socket_groups(.., shards, groups, now)` is for several receiving sockets on one port (`SO_REUSEPORT`).
- The kernel picks each datagram's socket by hashing the 4-tuple, and keeps that choice while the socket set is fixed.
- So the shards are split into one equal run per socket. The receiving socket decides the run, and the keyed hash picks the shard within it: `Router::shard_in(socket, from)`, or `Server::receive_in`.
- No receive thread hands datagrams to another socket's shards. Each shard sends from its own group's socket, which spreads sends over the NIC's transmit queues instead of one.

- **Routing.** `Router::shard(addr)` is a keyed hash of the peer address. A given address always lands in the same shard, so the "already connected?" check never leaves the shard. The key is random per server, so remote peers can't aim many addresses at one shard. Hand a cloned `Router` to the receive thread so it buckets datagrams per shard.
- **Ids encode their shard.** `id % n == shard`, so `send`, `stats` and `disconnect` for an id need no lookup table.
- **Shared state.** Shards share only read-only config and keys plus an atomic client count, which enforces `max_clients` with a CAS. The whole handshake, including accepting a client, runs inside the shard.
- **Misrouting.** A shard drops handshake packets from addresses that don't route to it. Handshakes are the only packets that create state.
- `Shard` is `Send`. For each shard, in parallel: `receive` its bucket, `update`, `poll_event`, `send`, `flush`, `drain_outgoing`.
- `Server`'s own methods do the same work serially and route internally. `Server::new` is simply one shard.
- `Server::preallocate(n)` spreads a pool of ready connections over the shards (see Admission control).

In the M1 sim, 64 shards on 8 threads cut the 10k-client ingress from 9.6 ms to 2.4 ms and transport from 9.1 ms to 2.2 ms (see `sim/README.md`).

## Stats per connection

The per-connection stats are packets and bytes sent and received, acked packets, lost packets, duplicates, dropped unreliable messages, padding bytes, smoothed RTT (EWMA 0.1), and smoothed loss (EWMA 0.05).
- A packet counts as lost if it's still unacked 128 packets later.
- RTT excludes the peer's hold time, via `ack_delay`. It still includes the gap between a datagram arriving and the `now` you pass to `receive`, so pass arrival timestamps: a receive thread's clock, or the kernel's `SO_TIMESTAMPNS`. With both, the sim's bots read 1.7–1.9 ms on loopback, down from 33–62 ms.
- Only the newest ack yields an RTT sample (as in QUIC), since a packet first acked through `ack_bits` was held for an unknown extra time. Under heavy reordering this favors fast packets, so RTT reads low: 115 ms on the lossy-link test, whose mean path is ~140–155 ms.
- Memory per connection is ~25 KB: a 256-packet sent ring with the reliable ids stored inline, and a 128-packet received ring. The reliable windows (~11 KB each) are allocated on first use. That's ~36 KB with one reliable message sent, 355 MB at 10k, down from ~130 KB (1.3 GB).

## Code map

| file | what |
|---|---|
| `wire.rs` | LE reader/writer, 1–2 byte varlen |
| `seq.rs` | wrapping u16 compare + `SequenceBuffer<T>` ring (clears skipped slots on jumps) |
| `packet.rs` | packet types, handshake encode/decode, sealed payload framing |
| `crypto.rs` | ChaCha20-Poly1305 sealing with nonce domains, 64-bit seq rebuilt from 16 bits |
| `token.rs` | connect tokens: mint, bytes, open (`TokenOpener`), `DEV_TOKEN_KEY` |
| `channel.rs` | `ReliableSender` / `ReliableReceiver` |
| `connection.rs` | seq/ack/RTT/loss, `flush()` packs reliable then unreliable |
| `server.rs` | `Server` → `Shard`s + `Router`: token handshake, client tables, used-token/user registry, events, timeouts |
| `client.rs` | handshake state machine, resends every 100 ms |
| `bitpack.rs` | bit writer/reader + quantization, e.g. a far-tier player in 8 bytes |

## Test coverage (`tests/lossy_link.rs`)

The simulated link does loss, duplication, and base delay + jitter (which causes reordering), all in simulated time and deterministic.

- **Reliable under a harsh link.** 5,000 reliable messages each way (client → server → echo) arrive in order, exactly once, with 25% loss, 5% duplication and 30–110 ms jitter. The RTT and loss estimators land in the expected ranges.
- **Wraparound.** 70,000 reliable messages wrap the message ids, and 70,000 packets wrap the packet sequence. Nothing breaks.
- **Many clients.** 300 clients connect through the lossy link, each with a unique id.
- **Unreliable doesn't block.** 3 × 500 B unreliable messages per tick keep streaming, with no head-of-line blocking.
- **Timeouts.** Both sides time out when the cable is cut.
- **Crypto** (unit tests in `crypto.rs`, `token.rs`, `packet.rs`, `connection.rs`). Seal/open with every byte flipped, and a wrong key, protocol, counter or domain. Sequence rebuilding across five wraps. Tokens refused for the wrong server, expiry, key, protocol or bytes. Handshake packets round-trip and aren't amplifiers. Replays deliver nothing twice.
- **Token handshake** (`lossy_link.rs`). A token copied off the wire can't connect without its keys (the owner still can). A token connects once. The same user with a new token replaces the old connection across shards, and the old client is told by a sealed disconnect. Expired, foreign, forged and other-protocol tokens get no reply. Forged payloads and disconnects from a client's address are ignored.
- **Padding** (unit tests in `connection.rs`). A flush of five 500 B messages makes three packets, the first two exactly 1,200 B, and the receiver gets all five messages. A lone packet is never padded, and padding that isn't all zeros is rejected.
- **Server full.** A client over the limit gets Denied.
- **Sharding.** Across 8 shards, 300 clients connect through the lossy link and echo reliably, and each id's shard matches its address route. `max_clients` holds across 16 shards (exactly 25 of 40 accepted). Shards run on real threads via `std::thread::scope`. A misrouted handshake is dropped. With an accept budget of 8 per tick, 200 simultaneous joins all get in, and no tick accepts more than 8.
- **Recycled connections start clean.** A client that leaves unacked reliable messages behind hands its pooled connection to the next client. The next client gets none of the old messages, and its first reliable id isn't mistaken for a duplicate. (Skipping either reset fails this test.)
- **RTT excludes the peer's hold.** On a 20 ms round trip where the peer held the ack for 30 ms, the sample reads 20 ms, not 50 ms.
- **Delivery tags.** Of two packets carrying 12 tagged messages (8 tags per packet at most), only the delivered one's tags come back, each once.
- **Bad input.** Garbage packets, junk tokens, and payloads with a spoofed address but no valid tag are all dropped.

## What's deliberately missing (next steps, roughly in order)

1. ~~**Encryption + auth tokens.**~~ Done: see Wire format.
2. **Bandwidth budget per connection.** A token bucket (e.g. 1.5 Mbps down) that `flush` respects, with prioritized content filling the budget. This is where the interest-management layer plugs in: it decides *what* goes in the unreliable stream, and the budget decides *how much*.
3. **Fragmentation** for messages > ~1.2 KB (initial world state, loadouts). Split them into a sliced reliable "block" channel, one block in flight at a time.
4. **Serialize-once fan-out.** Right now `flush` copies the body into the packet. For 10k clients, write headers in place and assemble per-client packets from shared, pre-encoded entity blobs.
5. **Syscall batching.** `sendmmsg` egress is done in the sim server (10k: 10.8 → 8.3 ms p50 on WSL). Still to do:
    - **`recvmmsg`.**
    - **`SO_REUSEPORT` socket groups: built** (see Sharding; `lattice-server --sockets N`). `SO_ATTACH_REUSEPORT_CBPF` is the fallback if exact control over the kernel's choice is ever needed.
    - **`UDP_SEGMENT` GSO is done** in the sim server (`--egress gso`): each client's padded packets go out as one `sendmmsg` entry. In the 3,000-player blob (2 packets per client-tick), egress falls ~25% on WSL for ~0.3% more bytes. Clients with one packet per tick gain nothing.

    Hand datagrams to the sim via SPSC rings. Move to AF_XDP only if pps becomes the bottleneck. Do the deep egress tuning on bare metal, since WSL's syscall and vswitch overhead distorts it. Because the protocol is sans-IO, none of this touches protocol code.
6. **Connection memory, further.** The inline `[u16; 32]` of reliable ids makes the sent ring 256 × 88 B = 22 KB, most of a connection. Most packets carry no reliable ids, so a shared id ring would bring a connection to ~6 KB.

Done:
- **Sharding connections across threads** (see Sharding above).
- **Smaller, lazier windows plus a connection pool** (see Admission control and Stats).
