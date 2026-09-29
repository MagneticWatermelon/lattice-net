# lattice-net

The custom UDP transport for a PlanetSide-style MMOFPS, where each continent runs as one server process targeting up to 10k players. Written in Rust with zero dependencies. It's sans-IO: the protocol code never touches a socket, so you feed it datagrams and a timestamp and drain the datagrams it wants sent.

```
cargo test --release                                   # workspace: 22 transport tests incl. 25%-loss/jitter/dup sim, + sim/
cargo run --release --example server                   # real UDP, 30 Hz tick, echo
cargo run --release --example client 127.0.0.1:40000 5
```

The M1 headless scale test (a movement-only server, a bot swarm and per-phase tick metrics) lives in [`sim/`](sim/README.md): `scripts/m1.sh blob 3000 60`.

## Wire format

Every datagram starts with a CRC32 and a type byte:

```
crc32:4 | type:1 | ...
crc32 = CRC32(protocol_id ++ bytes[4..])    // protocol id is never sent
```

The CRC is a filter, not security. It cheaply rejects garbage, corrupted packets, and traffic from other games or other protocol versions, before any state is touched.

### Payload packet (the hot path, sent every tick)

```
crc32:4 | type:1 | session:4 | seq:2 | ack:2 | ack_bits:32 | messages...
                                                              ^ 17 B fixed overhead
message := kind:1 [id:2 if reliable] len:1-2 bytes
```

- `seq` is this packet's number, a u16 that wraps (compared with half-range arithmetic).
- `ack` is the newest packet received from the peer. `ack_bits` covers the 32 packets before it.
- So **every packet acks the last 33**. With 30–60 packets/s per direction, an ack survives unless ~33 consecutive packets are lost. You never send dedicated ack packets.
- `session` is a 32-bit tag derived from the handshake cookie. An off-path attacker who spoofs the client's IP still has to guess it.

### Handshake (stateless, can't be used for amplification)

```
C→S  ConnectionRequest  { salt }             padded to 256 B
S→C  Challenge          { salt, cookie }     21 B      ← server stores NOTHING
C→S  ChallengeResponse  { salt, cookie }     padded to 256 B
S→C  Accepted           { salt, client_id }            ← slot allocated here
```

- `cookie = SipHash(server_secret, client_addr, salt, 10s_time_bucket)`.
- Only a client that actually receives packets at its claimed address can echo the cookie back, so spoofed floods can't fill connection slots.
- Requests are padded bigger than the replies, so the server can't be used for reflection or amplification attacks.
- The server rejects unpadded requests at decode time.

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
- The receiver buffers out-of-order ids in a 1024-slot window and delivers contiguous runs.
- Limits:
  - Up to 1024 messages can be in flight; excess waits in a backlog.
  - Up to 32 reliable messages per packet. That keeps 32 × 1024 tracked packets < 65536 ids, so a late ack can never point at a reused message id.

**Unreliable** messages are packed after reliable ones. Anything that doesn't fit in this flush's packets (max 4 per connection per flush by default) is dropped and counted in `Stats::unreliable_dropped`. That's deliberate: next tick's snapshot is fresher.

## Sharding

`Server::with_shards(cfg, max_clients, n, now)` partitions connections into `n` independent `Shard`s. The crate still spawns no threads: the caller drives the shards from its own pool.

- **Routing.** `Router::shard(addr)` is a keyed hash of the peer address. A given address always lands in the same shard, so the "already connected?" check never leaves the shard. The key is random per server, so remote peers can't aim many addresses at one shard. Hand a cloned `Router` to the receive thread so it buckets datagrams per shard.
- **Ids encode their shard.** `id % n == shard`, so `send`, `stats` and `disconnect` for an id need no lookup table.
- **Shared state.** Shards share only read-only config and keys plus an atomic client count, which enforces `max_clients` with a CAS. The whole handshake, including accepting a client, runs inside the shard.
- **Misrouting.** A shard drops handshake packets from addresses that don't route to it. Handshakes are the only packets that create state.
- `Shard` is `Send`. For each shard, in parallel: `receive` its bucket, `update`, `poll_event`, `send`, `flush`, `drain_outgoing`.
- `Server`'s own methods do the same work serially and route internally. `Server::new` is simply one shard.

In the M1 sim, 64 shards on 8 threads cut the 10k-client ingress from 9.6 ms to 2.4 ms and transport from 9.1 ms to 2.2 ms (see `sim/README.md`).

## Stats per connection

The per-connection stats are packets and bytes sent and received, acked packets, lost packets, duplicates, dropped unreliable messages, smoothed RTT (EWMA 0.1), and smoothed loss (EWMA 0.05).
- A packet counts as lost if it's still unacked 128 packets later.
- RTT includes tick quantization, e.g. ~26 ms on localhost with a 30 Hz server. To measure pure network RTT, echo send timestamps and subtract the peer's hold time.

## Code map

| file | what |
|---|---|
| `wire.rs` | LE reader/writer, 1–2 byte varlen, table CRC32 |
| `seq.rs` | wrapping u16 compare + `SequenceBuffer<T>` ring (clears skipped slots on jumps) |
| `packet.rs` | packet types, encode/decode, padding rules |
| `channel.rs` | `ReliableSender` / `ReliableReceiver` |
| `connection.rs` | seq/ack/RTT/loss, `flush()` packs reliable then unreliable |
| `server.rs` | `Server` → `Shard`s + `Router`: handshake, client tables, events, timeouts |
| `client.rs` | handshake state machine, resends every 100 ms |
| `bitpack.rs` | bit writer/reader + quantization, e.g. a far-tier player in 8 bytes |

## Test coverage (`tests/lossy_link.rs`)

The simulated link does loss, duplication, and base delay + jitter (which causes reordering), all in simulated time and deterministic.

- **Reliable under a harsh link.** 5,000 reliable messages each way (client → server → echo) arrive in order, exactly once, with 25% loss, 5% duplication and 30–110 ms jitter. The RTT and loss estimators land in the expected ranges.
- **Wraparound.** 70,000 reliable messages wrap the message ids, and 70,000 packets wrap the packet sequence. Nothing breaks.
- **Many clients.** 300 clients connect through the lossy link, each with a unique id.
- **Unreliable doesn't block.** 3 × 500 B unreliable messages per tick keep streaming, with no head-of-line blocking.
- **Timeouts.** Both sides time out when the cable is cut.
- **Server full.** A client over the limit gets Denied.
- **Sharding.** Across 8 shards, 300 clients connect through the lossy link and echo reliably, and each id's shard matches its address route. `max_clients` holds across 16 shards (exactly 25 of 40 accepted). Shards run on real threads via `std::thread::scope`. A misrouted handshake is dropped.
- **Bad input.** Garbage packets, forged cookies, and payloads with a spoofed address but wrong session are all dropped.

## What's deliberately missing (next steps, roughly in order)

1. **Encryption + auth tokens.** Swap the SipHash cookie and session tag for netcode.io-style connect tokens: a login service issues a token encrypted with XChaCha20-Poly1305, and packets are then AEAD-encrypted per connection. Encryption also kills the CRC (the AEAD tag replaces it) and makes the session tag real authentication. Use the `chacha20poly1305` crate; don't roll your own.
2. **Bandwidth budget per connection.** A token bucket (e.g. 1.5 Mbps down) that `flush` respects, with prioritized content filling the budget. This is where the interest-management layer plugs in: it decides *what* goes in the unreliable stream, and the budget decides *how much*.
3. **Fragmentation** for messages > ~1.2 KB (initial world state, loadouts). Split them into a sliced reliable "block" channel, one block in flight at a time.
4. **Serialize-once fan-out.** Right now `flush` copies the body into the packet. For 10k clients, write headers in place and assemble per-client packets from shared, pre-encoded entity blobs.
5. **Syscall batching.** Use `recvmmsg`/`sendmmsg` (or `UDP_SEGMENT` GSO), `SO_REUSEPORT` with N network threads (a natural fit: one socket per group of shards), and hand datagrams to the sim via SPSC rings. After sharding, one `send_to` per packet is the biggest phase at 10k (~11 ms). Move to AF_XDP only if pps becomes the bottleneck. Because the protocol is sans-IO, none of this touches protocol code.

Done: **sharding connections across threads** (see Sharding above).
