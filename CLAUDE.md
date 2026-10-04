# lattice-net: project context for Claude Code

Netcode for an experimental PlanetSide-style spiritual successor (MMOFPS). This crate is the UDP transport. Start with `README.md`, which covers the wire format, channels, test coverage and the missing pieces.

## Core architecture decisions (already made)

- **One authoritative server process per continent**, with every player on the continent in that one process. Initial hard ceiling: **10k players**. Don't split one battle across servers: cross-server hit resolution means two clocks and two rewind histories.
- **Server is authoritative for movement and hits.**
  - Lag-compensated rewind is capped at ~150–200 ms.
  - Projectiles are simulated on the server from a rewound origin.
  - Hit detection is not client-side (PS2's model).
- **Per-client downstream is O(k), not O(N).** Interest management uses tiers:
  - Near (~64 entities @ 30 Hz)
  - Mid (~256 @ 10 Hz)
  - Far (~1000 @ 2 Hz, 8 B each, see `bitpack.rs`)
  - Budget: ~0.7–1.5 Mbps per client, ~7–15 Gbps egress at 10k, so the server needs a 25G NIC.
- **Server is a many-core box (64–96 cores).** Rust, ECS/SoA data layout. The tick is a phase pipeline, each phase run in parallel:
  1. ingress
  2. movement
  3. shots (rewind)
  4. apply events
  5. spatial grid rebuild (one index shared by ALL systems, including networking, NPC proximity and AoE)
  6. lag-comp history
  7. **serialize each entity once per tier**
  8. per-client assembly (priority accumulator + memcpy)
  9. egress
- **Delta compression scheme:** far and mid tiers send absolute state quantized relative to the grid cell, shared by all clients. Only the near tier gets per-client deltas against that client's last acked state.
- **Degrade under load in this order:** shrink tier radii, then lower update rates, then lower the tick rate (30 → 20 Hz), then a mild time dilation (≥0.8).
- **Lessons from Daybreak's 2023 PS2 profiling:** zone entry/spawn cost and NPC proximity sweeps killed performance in big fights, not bandwidth. Budget those systems.

## Transport decisions

- **Custom UDP, not QUIC, for tick traffic.**
  - QUIC doesn't expose per-datagram acks, and delta-vs-last-acked needs them.
  - QUIC's congestion control applies to datagrams and fights a fixed 30–60 Hz send rate.
  - QUIC (quinn-proto) is fine for a separate non-tick connection: login, loadouts, asset and map deltas, chat.
- **The protocol code never does socket I/O.** Keep it that way: the Linux fast paths (`recvmmsg`/`sendmmsg`, GSO, `SO_REUSEPORT`, AF_XDP) only touch the socket loop, behind `cfg(target_os = "linux")`.
- **Next transport steps, in order:**
  1. ~~connect tokens + per-connection ChaCha20-Poly1305~~ (done, see below)
  2. per-connection bandwidth budget (token bucket)
  3. fragmentation for >1.2 KB messages
  4. serialize-once fan-out without copying bodies
  5. syscall batching: `recvmmsg`, then `SO_REUSEPORT` (still the biggest phase at 10k)
- **Done:**
  - **Connect tokens and sealed packets (protocol `LATTICE3`).**
    - **Tokens:** netcode.io-style (`token.rs`). ChaCha20-Poly1305 under a key the login service shares with the servers, holding the user id, both connection keys and 32 B of user data. Server id and expiry are clear associated data.
    - **Packets:** payload and disconnect are `type:1 seq:2 sealed(...) tag:16` (27 B overhead). The nonce is the 64-bit packet counter, rebuilt from the u16 seq. There is no CRC and no session tag.
    - **Handshake:** stateless until accept. The Response proves the keys, Accepted is sealed, and a token connects once. The same user with a new token replaces the old connection.
    - **Crypto is `ring`, not RustCrypto** (decided 2026-09-29). RustCrypto's `chacha20poly1305` took 0.43 µs to open a 60 B packet and cost 10k a ladder level on WSL. `ring` takes 0.17 µs, needs no build flags, and matches the unencrypted build at 10k and in the blob.
  - Connection sharding: `Server` is N `Shard`s routed by a keyed address hash, and the sim drives them from rayon.
  - Accept budget per tick (`Config::max_accepts_per_tick`).
  - `sendmmsg` egress.
  - Connection memory ~130 → ~36 KB: windows of 256 sent / 128 received / 256 reliable, reliable ids inline, reliable windows lazy.
  - A per-shard connection pool (`Server::preallocate`); an accept costs about 4 µs.
  - `ack_delay` in the header, so RTT excludes the peer's hold.
  - GSO: `Config::pad_packets` pads all but a connection's last packet of the tick (a kind-2 padding message, protocol id `LATTICE2`), and the sim server's `--egress gso` sends each client's packets as one `UDP_SEGMENT` send. The sim fills packets first (`PacketFill`), so padding is ~0.3% of bytes. Blob egress −25% on WSL.
- **Decided (M1 review, 2026-09-29):**
  - **With `SO_REUSEPORT`, the receiving socket picks the shard group.** The kernel hashes the 4-tuple, so the shard comes from the socket that received the handshake: each socket owns a fixed group of shards, and the keyed hash picks one within it. Don't re-bucket in receive threads.
  - **GSO (`UDP_SEGMENT`) only after M2.** It batches datagrams to a single destination, which only helps once clients get several packets per tick.
  - **Do deep egress tuning on bare metal, not WSL2.**
  - **Tune the spare against input → applied on the server, not against the round trip.** Measure it without clock sync as RTT/2 + the server-reported queue wait (bots report both). With prediction, a player's own movement is instant; the spare delays how others see you, when your shots resolve, and how far reconciliation replays. Options for later: process inputs faster than the tick, or target a fractional spare.

## Milestones

- **M0: protocol.** Done: this crate, 18 tests.
- **M1: headless scale test.** The pass bar was met on the WSL2 dev box at M1 (blob p99 13 ms). On WSL with M2 (2026-09-29) the blob was at p99 27–28.5 ms, over the ~25 ms bar. **On bare metal (Scaleway, 2026-10-04) it passes:** p99 19.4 ms with 8 server threads, 8.5–9.4 ms on 64 cores; WSL mostly cost tail latency. **10k runs at level 0 (30 Hz, full radii) with p99 15.4 ms on the 64-core box.** The tick scales to physical cores (8 → 64 threads: 42.4 → 12.2 ms at 10k), not SMT threads; the serial events phase (~2.4 ms) is the floor. See sim/README. Lives in `sim/` (`lattice-sim`, which may have deps: rayon, socket2). See `sim/README.md` for the WSL baseline.
  - Server does movement only. A Rust bot swarm (`lattice-bots`, real `lattice_net::Client`s, one socket per bot, ≤8 threads) drives it at 1k, 5k and 10k. It's Rust, not Go, so there's one protocol implementation to change when crypto lands.
  - Measure p50/p99 per-phase tick time, bytes per client, pps, and prediction corrections.
  - Scenarios: uniform spread; 3 hotspots of ~800; a 3,000-player blob within 200 m; 500 joins within 10 s.
  - Pass bar: p99 tick under ~25 ms on the blob.
- **M2 (built on WSL):** interest management (tiers, priority accumulator, grid), plus a web top-down debug map showing what client X receives.
  - **M2a (tiers) is built:** `sim/src/interest.rs`. Its design was decided in review (2026-09-29):
    - mid and far tiers are staggered by entity id alone, with no per-pair state;
    - the near tier is a per-client accumulator over ≤100 candidates;
    - near membership is distance OR interaction (squad now, hits/targeting/scope with combat);
    - a new near candidate's age is seeded from its old tier's stagger slot;
    - the budget fills near → mid → far, with far skips carried one tick.
  - **M2d (degradation ladder) is built:** `sim/src/ladder.rs`.
    - Levels 1–3 shrink radii, 4–5 lower rates, 6 is 20 Hz, 7–8 are dilation 0.9 and 0.8. The controller steps down on p90 work/period > 0.85 over 30 ticks and up after 90 ticks < 0.6.
    - A lower tick rate keeps inputs as 1/30 s movement steps (1.5 per 20 Hz tick), so prediction stays bit-exact.
    - Snapshots carry `pace` (dilation ÷ how far the server is behind schedule), and clients send inputs at pace × 30/s. Without this, a server below 30 Hz overflows input queues.
    - A per-client bandwidth ladder shrinks a starving client's mid/far radii, and its probes back up back off exponentially.
  - **M2b (near-tier deltas) is built:** `sim/src/delta.rs`.
    - The transport tags unreliable messages (`send_tagged`, `take_acked`).
    - The near message of each tick is tagged with that tick; acked ticks become per-entity baselines. Deltas are taken against a 32-tick, tick-major state history on both sides.
    - Near bytes in the blob fell 63% (967 → 357 B per client-tick). The blob stays at 2 packets, which was decided: mid alone is ~950 B.
    - The cost: ~3.5 µs of assembly per client per tick (encoding + ack bookkeeping).
  - **Sink bots don't isolate server cost on one box:** the swarm is ~52% busy either way. Judging 10k's ladder level needs bots on a second machine.
  - **M2c (debug map) is built:** `lattice-server --debug-http 0.0.0.0:8080` serves a top-down map of what one client receives (`sim/src/debugmap.rs`, std only).
  - **GSO is built** (see Transport decisions, Done). Mid and far messages are split to fill packets rather than fragmented; real fragmentation stays transport step 3.
  - **Transport step 1 (tokens + encryption) is done** (see Transport decisions, Done).
  - **Bare-metal baseline done** (2026-10-04, `baselines/2026-10-04-scaleway-*`). What it pointed at:
    - one UDP socket is a kernel lock at high thread counts (8.5% spinlock in the blob profile), so `SO_REUSEPORT` socket groups come next;
    - run rayon with threads = physical cores (128 threads was slower than 64; idle-worker spinning is 35–45% of the CPU);
    - this Broadcom NIC has no UDP segmentation offload, so GSO is untested in hardware.
  - **netem on WSL done** (2026-10-04, `baselines/2026-10-04-wsl2-netem`, `scripts/baseline.sh netem`). Under every profile up to 5% loss and ±20 ms jitter, there were no decode errors, resyncs or discards, and corrections were ≤0.2 per bot-minute, ≤0.53 m. Input → applied is one-way delay + ~50 ms of spare. Loss and jitter make ~0.33% of inputs late, and the input clock hunts under jitter: candidate, a spare sized to each client's measured jitter.
  - **Next:** one bare-metal session (the pile limits test, `--shards 256`, netem at 10k), then `SO_REUSEPORT` + physical-core default, then M3 with a CPU budget.
- **M3:** combat with rewind. Run a fairness test: 20 ms vs 150 ms bots through netem.
- **M4:** vehicles.
- **M5:** minimal playable client.

## Dev environment

- Windows 10 22H2 desktop with WSL2 in NAT mode (mirrored networking isn't available on Win10).
  - Keep the repo in WSL's `~/`, not `/mnt/c`.
  - A Windows client reaches the WSL server through the WSL IP from `hostname -I`. Bind the server to `0.0.0.0`.
- Server and bots are developed in WSL2. The game client is native Windows.
- Performance numbers only count from bare-metal Linux: a dual-boot on the desktop, then a rented 10/25 GbE server for 5k–10k runs.
  - `scripts/baseline.sh full <name>` runs the scenario matrix into `baselines/<date>-<name>/`, with the machine recorded in `env.txt`. Run `scripts/preflight.sh` first. Baselines are committed.
  - The WSL reference is `baselines/2026-09-29-wsl2`. sim/README has the dual-boot procedure (the repo travels as a git bundle; there's no remote).
  - **Two-machine runs on Scaleway Elastic Metal** (hourly, fr-par-2): EM-I620E server (EPYC 8534P, 64C/128T) + EM-I320E bots (EPYC 8224P, 24C/48T), joined by the `lattice-test` Private Network (25 Gbps per the API), about €2.82/h for the pair.
    - `scripts/cloud-up.sh` rents both and sets them up (VLAN, toolchain, build, iperf3 check, session token key). Billing starts at creation.
    - `scripts/cloud-run.sh full <name> [SERVER_THREADS=8 ...]` runs `baseline.sh` with the server on one box and the bots on the other, and copies `baselines/` back.
    - `scripts/cloud-down.sh` copies back, deletes every server tagged `lattice-net`, and shows the server list.
    - `.claude/settings.json` asks before `cloud-up.sh` or any `scw` call that creates, changes or deletes; listing, `cloud-run.sh` and `cloud-down.sh` (which only deletes our tagged servers) are allowed.
    - **Never end a session with Scaleway servers running: always finish with `scripts/cloud-down.sh` and show the empty server list.** A €20/month budget alert exists, but alerts don't stop spending.
    - Quirks, already handled:
      - offer names are case-sensitive (`EM-I620E-NVME`);
      - SSH comes up ~8 min after "ready";
      - the VLAN must link to cloud-init's netplan id (`eth0`) and be named `vlan<id>` (15-character limit);
      - `cloud-up.sh --resume` finishes a setup that stopped partway.

## Conventions

- `cargo test --release` (runs the whole workspace) and `cargo clippy --workspace --all-targets` must stay clean.
- Zero dependencies in the core crate unless there's a strong reason. Crypto uses audited, widely used crates, never hand-rolled. The one dependency is `ring` (BoringSSL-derived ChaCha20-Poly1305 and OS randomness); it builds C/asm, so a C toolchain is needed (MSVC on Windows).
