# lattice-net: project context for Claude Code

Netcode for an experimental PlanetSide-style spiritual successor (MMOFPS). This crate is the UDP transport. Start with `README.md`, which covers the wire format, channels, test coverage and the missing pieces.

## Core architecture decisions (already made)

- **One authoritative server process per continent**, with every player on the continent in that one process. Initial hard ceiling: **10k players**. Don't split one battle across servers: cross-server hit resolution means two clocks and two rewind histories.
- **Server is authoritative for movement and hits.**
  - Lag-compensated rewind is capped at **300 ms for near targets and 367 ms for mid/far** (decided 2026-10-05, was ~150–200 ms). It fully covers RTT ≤ 100 ms; beyond that, shooters lead. The rewind's fixed part (render delay + spare + tick wait) is already ~134–200 ms.
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
  5. syscall batching: `recvmmsg` (`SO_REUSEPORT` socket groups are built)
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
  - **Limits on bare metal** (2026-10-04, 64 threads, `baselines/2026-10-04-scaleway-limits`):
    - one 25 m pile goes over the 33 ms tick at ~7.2k players;
    - a 200 m disk holds 10k (p99 19.3 ms);
    - uniform goes over at ~18k.
    - The pile cost was the quadratic nearest-player search (`select_nth_unstable` 46% of CPU with 32 m cells), and the ladder can't cut it. **Fixed by `Grid::knn`** (below).
  - 256 shards don't help.
  - netem at 10k was invalid: netem on the server's interface throttled egress. Shape on the bot box only (egress + `ifb` ingress).
  - **Built since** (2026-10-04, not yet measured on bare metal):
    - netem shapes the bot box only (egress + `ifb` ingress);
    - `--threads` defaults to physical cores;
    - a per-phase breakdown (wall vs longest shard task vs work ÷ threads);
    - `SO_REUSEPORT` socket groups (`Server::with_socket_groups`, `lattice-server --sockets N`).
  - **Next:**
    - a short bare-metal session to measure these (`--sockets` 1/4/8/16 at 10k, and netem at 10k);
    - ~~the nearest-player search~~ **done: `Grid::knn`** (2026-10-04, per `reports/Nearest player search algorithms.md`). Exact (oracle-tested), with packed keys and a running threshold, positions inline, sub-cells only in cells over 256 items, and a box-pruned walk. A 10k pile query scans ~490 candidates instead of 10,000 (30–40× faster single-threaded). The pile's assembly is now linear in its size: on WSL it crosses 33 ms at ~5,500 instead of ~2,800. Blob 3k assembly 9.0 → 5.9 ms; uniform 7% slower per query. Not yet on bare metal.
    - then M3 with a CPU budget.
- **M3: combat and the first playable client** (scoped 2026-10-04). The client is part of M3; there's no walk-only release first.
  - **Decided:**
    - soft player separation (the server pushes overlapping players apart over a few ticks);
    - a procedural heightmap plus simple static cover (axis-aligned boxes that block movement and shots);
    - 3 factions;
    - projectiles simulated on the server from a rewound origin;
    - sub-tick shot timing;
    - **a render delay per tier** (decided 2026-10-05, replacing one render time for every entity):
      - near is drawn 67 ms behind the newest server step (two 30 Hz updates), growing up to 133 ms to cover the 99th percentile of how late near updates actually come. In a crowd, the near tier's per-tick cap makes them come every 1–3 ticks; fixed at 67 ms, near was 83% interpolated in the jittery blob;
      - mid and far are drawn 200 ms behind (two 10 Hz updates, so a lost one is bridged; far is mostly extrapolated on it);
      - an entity changing tier glides between delays at 25% (133 ms over ~0.5 s);
      - lag compensation rewinds each target by its own tier's delay, so inputs carry the near render step plus the batch's mid lag;
    - **the rewind cap is "the highest RTT we fully compensate" + the fixed part:** a near target is rewound by RTT + 67–133 ms render + ~33 ms spare + ~33 ms tick wait. **Decided in M3d: RTT ≤ 100 ms**, so caps of 300 ms (near) and 367 ms (mid/far); players beyond lead their shots. A deliberate fairness tradeoff against shots landing behind cover;
    - mid/far velocity derived on the client first, with velocity bytes in the blobs only if the bots' smoothness numbers ask.
  - **Order:**
    - **M3a world and movement: done** (2026-10-04).
      - `lattice-game` (`game/`) holds movement, `world.rs` (8 km heightmap at 4 m from integer noise, plus cover boxes), the message formats and the near codec.
      - Movement is 2.5D: pitch, jump, gravity, 45° max slope, 0.45 m steps, sliding along cover, ledge catch when falling.
      - Soft separation is a server phase (`separate`). Snapshots carry a push counter, so bots count push corrections apart from real ones.
      - Prediction stays bit-exact; on WSL there are 0 real corrections in the pile and blob, and the cost is +0.3–1 ms of tick. See sim/README.
    - **M3b `lattice-client-core`: built** (`client-core/`, 2026-10-05; see sim/README). Bots and humans run the same client code.
      - **Results** (WSL netem, `baselines/2026-10-05-wsl2-m3b-netem`):
        - near is ≥98% interpolated on every link (exact to 1 cm);
        - far is ~20% interpolated (p50 error 0.3 m, p99 3–5 m);
        - there are 0 clock snaps, and render time never goes backwards.
      - **Mid missed its ≥99% clean bar over UDP** with one 100 ms render time: 98.7% uniform, 95% blob (2% were entities leaving the capped mid set), ~82% under ±20 ms σ jitter. **Fixed by per-tier delays, with the near delay adaptive** (2026-10-05, `baselines/2026-10-05-wsl2-adaptive-near-netem`):
        - near is ≥99.5% interpolated on every link, blob included;
        - mid in uniform is ≥99.2% on every link (in the blob ~96%, of which ~3% is churn in and out of the capped mid set);
        - pops are under 5 cm at p99.
        - Re-judge with human eyes in M3c before tuning.
      - **Finding for M3d: rewind = RTT + render delay + ~67 ms** (the spare input + the wait for the next tick). With one 100 ms render time that was RTT + 167 ms, and a 200 ms cap fully compensates only players under ~33 ms RTT. That led to per-tier delays: near targets RTT + 134 ms, mid/far RTT + 267 ms.
      - **The input clock has two entry points.** `step_inputs` is for 30 Hz callers (the bots); `tick_inputs` runs by elapsed time, for frame loops. Run by time, a 30 Hz caller's jitter raised p99 server wait from ~70 to 80–100 ms.
      - **Extraction.** Prediction, the input clock and the entity store move out of `BotBrain`. The bot AI (`think`) stays in sim, as the input source passed to `tick_inputs`.
      - **The timeline is game steps (1/30 s), not server ticks.** Snapshots carry `step`. Ticks map to steps 1:1 at 30 Hz but 1-or-2 at 20 Hz, so interpolating in ticks would play 20 Hz movement at alternating 0.67×/1.33× speed. Under dilation the clock runs at pace × 30 steps/s.
      - **Render clock.** It tracks the envelope of snapshot arrivals: up at once when a step arrives earlier than expected, slow drift down when it arrives later. Render = newest − delay (100 ms default), slewed at most ±10% and never backwards; it snaps past 0.5 s of error.
      - **Entities.** One sample timeline per entity across tiers.
        - Bracketed: interpolate.
        - Otherwise: extrapolate, with the near tier's velocity or one derived from the last two samples (mid/far), for at most 250 ms, then hold.
        - A sample that changes what's on screen becomes a visual offset that decays (~100 ms), never a pop.
        - Entities unheard for 2 s are dropped (explicit despawns come with M3d's events).
      - **Mid/far blobs** fill their unused altitude (12 bits, 16 cm), pitch and airborne bits.
      - **Inputs carry the render step** (near: u16 in 1/64 steps, wrapping; +2 B per input; plus the batch's mid lag, 1 B). The server records rewind = applied step − render step, per tier.
      - **The client's own player** is drawn between its last two predicted steps by the input clock's phase. A reconciliation becomes a decaying offset. The input clock runs on elapsed time, so a 144 Hz client works.
      - **Measurement.** Tracked bots record, per tier:
        - the share of frames interpolated, extrapolated or held;
        - the size of corrections when samples arrive (pops, before smoothing);
        - the actual render delay.

        The swarm tests compare rendered positions with the server's history.
      - **Pass bars:**
        - prediction stays bit-exact;
        - on a clean link, near and mid are interpolated in ≥99% of frames;
        - under 5% loss and ±20 ms jitter, near is interpolated in ≥95%;
        - render time never goes backwards.

        Far pops are measured, not barred: they decide whether mid/far need velocity bytes.
    - **M3c Bevy client: first pass built** (`client/`, `lattice-client`, 2026-10-05; see client/README): terrain, capsules, first-person, chase and spectator cameras, net graph, server ghost, tier colors.
      - **On the Windows desktop** (AMD GPU) with a 1k-bot blob in view:
        - frame time p50 3.5 / p99 4.7 ms (~285 fps);
        - 0 corrections and 0 resyncs;
        - near 99.9% and mid 97.5% interpolated, render delay 100.2 ms.
      - **Built with `scripts/client-windows.sh`:** WSL → `x86_64-pc-windows-gnullvm` with a user-space llvm-mingw, into `C:\lattice`. WSL interop can launch it for self-checks (`--screenshot`, `--exit-after`).
      - **Found and fixed:** a frame slower than 100 ms made only one batch of inputs and starved the server (42 resyncs at 9 fps). `tick_inputs` now makes up to 300 ms of inputs per call, as several batches.
      - **Judged by eye (the user, 2026-10-05):** "all movement seems very smooth". M3c's last pass bar is met.
      - **Open:** below ~10 fps the server still starves between frames (a net thread would decouple input from rendering).
      - **Its own Cargo workspace** (the root `exclude`s it), so `cargo test` and `clippy` at the root never build Bevy. It uses Bevy 0.19.1 without audio or gamepads, which need ALSA and libudev headers on Linux.
      - **Networking in the frame loop.** `client/src/net.rs` (`Session`) is plain Rust, no Bevy: a non-blocking UDP socket, a `lattice_net::Client` and a `ClientCore`.
        - Each frame: receive and deliver, then `tick_inputs` with the frame's keyboard and mouse, then flush.
        - It mints its own dev connect token (`--token-key`, `--user`), like the bots, until a login service exists.
        - It's tested against an in-process `SimServer` over loopback UDP.
      - **Coordinates:** game (x, y, z up) maps to Bevy (x, z, −y).
      - **Terrain:**
        - 128 m chunks at the full 4 m resolution, streamed within ~1.5 km of the camera;
        - one coarse 32 m mesh of the whole map, for the horizon;
        - vertex colors by height and slope.
      - **Cover:** every box, as a scaled unit cube.
      - **Players:** capsules of the game's own size, colored by tier (near, mid, far, held, new), with a nose that shows yaw and pitch.
      - **Cameras.** First person at eye height on `own_render`, with mouse look and WASD, sprint and jump. A spectator free-fly camera on F.
      - **Server ghost** (G): our own last authoritative state, and each entity's newest sample, unsmoothed, drawn next to what's rendered.
      - **Net graph** (N):
        - text: fps and frame time, RTT, loss, bandwidth, server step and level, render delay, entities per tier, interpolated %, corrections and pops;
        - sparklines of frame time and snapshot arrival gaps.
      - **Windows build:** cross-compiled from WSL (`x86_64-pc-windows-gnu` with a user-space llvm-mingw, no sudo; `scripts/client-windows.sh`), or a native MSVC `cargo build`. It reaches the WSL server at the WSL IP.
      - **Self-check without Windows:** it runs in WSL under WSLg. `--autoplay` drives it with the bots' AI, and `--screenshot PATH --after S` saves a frame, so renders can be looked at.
      - **Pass bars:**
        - 0 corrections when walking alone;
        - net-graph smoothness matches the bots on the same link;
        - frame time p99 under the display's refresh with a 1k blob in view on the Windows box;
        - other players move without visible pops, judged by eye.
    - **M3d combat** (scoped 2026-10-05).
      - **Decided (the user):**
        - **Rewind fully covers RTT ≤ 100 ms.** The caps are 300 ms for near targets and 367 ms for mid/far (100 ms RTT + the largest render delay + ~67 ms of spare and tick wait). Beyond a cap, the target is taken at the cap and the shooter leads.
        - **One automatic rifle:** projectile ~600 m/s with gravity, 10 rounds/s, ~1 km range.
        - **100 HP, 20 body / 40 head** (~0.5 s time-to-kill).
        - **Respawn back into the scenario's fight after 5 s,** on the faction's side, so load tests keep fighting at a steady size.
      - **Factions (3):** the faction is `entity id % 3`, allocated from per-faction free lists: zero bytes on the wire. Assigned by squad, round-robin. No friendly fire.
      - **Health and death:**
        - near states carry health (7 bits) and mid/far blobs a 4-bit bucket; one spare flag bit in each is `dead`;
        - the dead don't move, aren't pushed, don't block, and respawn after 5 s;
        - snapshots carry own health and a `life` counter, so a respawn teleport isn't a misprediction (like `pushes`).
      - **Shots ride the input batch.**
        - `BUTTON_FIRE`, plus each shot as `{seq, frac:u8, yaw:2, pitch:2, render:2}` (8 B), sent redundantly with its inputs;
        - `frac` is when in its 1/30 s step the trigger fired (sub-tick timing). The origin is the shooter's eye between its server states for seq−1 and seq. The aim is the view at that frame; the target time is that frame's render step (near), plus the batch's mid lag;
        - the server enforces the fire rate; there's no ammo or reload in M3d.
      - **Projectiles live in the shooter's timeline.**
        - each keeps its per-tier rewind for its whole flight and is tested against targets at (now − rewind);
        - it moves ~20 m a tick with gravity; each tick's segment is tested against terrain (heightmap march ≤ 2 m), cover boxes (slab test) and players. Player candidates come from the shared grid, padded by max speed × rewind: a capsule (r 0.4) plus a head sphere (r 0.15 at 1.65 m), positioned from history. The first hit wins;
        - a target's tier is the server's own record of what it sent the shooter (its current near set).
      - **History** is 3D positions per tick with their step: 16 ticks (≥ 0.53 s), tick-major (was 6 ticks, 2D).
      - **Shots phase:**
        - placed after history; parallel over projectiles (rayon);
        - hits are collected, then applied serially (damage, deaths, events).
      - **Events:**
        - reliable: hit confirms to the shooter (target, damage, head, killed); damage taken to the victim (from, amount); kills to both and their squads;
        - unreliable: a per-tick shots message for tracers (shooters in the client's near set, ~7 B each);
        - attackers and victims become near-tier candidates for a few seconds (the interest "interaction" clause).
      - **Client:**
        - left mouse fires, with the frame's sub-tick fraction;
        - cosmetic tracers and muzzle flash at once; hit markers from confirms;
        - a health bar, damage direction, a death view with a respawn timer, and a kill feed;
        - faction colors, with tier colors on T.
      - **Bots fight:**
        - tracked bots aim at the nearest enemy they render, with Gaussian aim error, in bursts; sink bots fire along their heading, for load;
        - latency classes: per-bot delay in the swarm harness (a deterministic fairness test), and port-range `tc` classes on the bot side for UDP runs.
      - **Measured:**
        - the shots phase (wall / longest / work), projectiles alive, segment and candidate tests, hits, event bytes;
        - hit rate by latency class;
        - post-cover hits (target occluded from the shooter's present eye at hit time) with their rewind.
      - **Order:**
        1. **M3d.1: factions, health, death, respawn. Done** (2026-10-05, checked by eye on Windows):
           - a death or respawn is a life event (rebase, replay, never a correction) and a cut (never smoothed, never interpolated across);
           - corpses keep the aim they died with;
           - `--deaths-per-sec` stands in for weapons;
           - UDP, 100 tracked bots, 10 deaths/s: 0 corrections, streak frames 67,994 → 742 after the fixes.
        2. **M3d.2: shots, 3D history, projectiles, damage. Done** (2026-10-05; see sim/README). The user played it on Windows against 300 bots: "looks great".
           - **Swarm:** a target strafing at 6 m/s 50 m out takes 200/200 hits at 0 and 33 ms one-way; headshots 100/100; past the cap 3% until leading the clipped 2–3 steps (92%); walls stop everything; kills exactly at 100 HP.
           - **WSL, `--fire-share`:**
             - uniform 5k at 20% (~9k shots/s): shots phase p50 2.0 ms;
             - 1k blob all firing: 1.6 ms;
             - 0 corrections in both.
           - **Each shot also carries its exact render step** (7 B, not 5). Deriving it from the input's assumed the input is made at its step's end, which 30 Hz callers don't do.
           - **Hits on a target that died since the shooter saw it** deal no damage and are counted (`hits_too_late`).
           - The scope as written:
           - **Shared rules, in `lattice-game`:**
             - `weapon.rs` holds the rifle: 600 m/s, gravity 9.81, 100 ms between shots, 2 s range, 20 body / 40 head damage. It also holds the projectile kinematics (semi-implicit, per half step), so the server and the client's cosmetic tracers fly the same arc;
             - `hit.rs` holds the geometry: segment against capsule, sphere, box (slab) and terrain (≤2 m march, then bisection);
             - hitboxes: a body capsule (r 0.4, z+0.4 to z+1.1) and a head sphere (r 0.2 at z+1.6). The movement collider is unchanged;
             - `EYE_HEIGHT` 1.6 moves into the game crate.
           - **Shots ride their input.**
             - `BUTTON_FIRE` marks an input carrying a shot: `frac:1 yaw:2 pitch:2` (+5 B), redundant ×3 like the input;
             - it's at most one shot per step: the rifle fires every 3 steps, so the input's seq is the shot's id;
             - `frac` is when in that step it fired, from the input clock's phase, so it's sub-tick;
             - the shot's render step is the input's minus (1 − frac) steps (no extra bytes, ≤ 3 ms off), and the batch's mid lag gives the mid/far one;
             - the client rate-limits itself the same way the server does.
           - **Server, receiving:**
             - a shot fires when its input is applied, its origin the shooter's eye between its own states for seq−1 and seq at `frac` (bit-exact with the client's prediction);
             - **stand-ins never fire** (a repeated input drops its shot);
             - a shot whose seq a stand-in already consumed still fires late, within 8 steps, from a ring of the shooter's last 16 states, and is counted;
             - duplicates are dropped (seq ≤ the last fired), and so are shots faster than 3 steps apart, and shots from the dead.
           - **Projectiles live in the shooter's timeline.**
             - a shot fired at step τ0 (seq − 1 + frac) when the client drew tier T at render step R_T keeps D_T = τ0 − R_T, capped at 9 steps for near targets (300 ms) and 11 for mid/far (367 ms);
             - at projectile time τ, it's tested against targets at τ − D_T: what the shooter saw, advanced by the flight time;
             - the first tick catches up from τ0 to now; after that it runs with the server's steps.
           - **History:** each tick's step plus, per entity, its 3D position, a life counter and alive-and-not-dead (16 B). The last 16 ticks are kept (≥ 0.53 s, ~2.6 MB at 10k). Positions are interpolated between ticks, never across a life change.
           - **Shots phase** (new, after history, before serialize; parallel over projectiles). Each half-step segment is tested against:
             1. the terrain;
             2. cover in the cover cells it crosses;
             3. players from the shared grid within segment/2 + 0.4 + 9 m/s × (D + 1 step).
           - **Player candidates:**
             - skipped: the shooter, its faction, and the dead (at the rewound time);
             - each one's tier comes from the shooter's current near set (a per-entity index into the shard client lists);
             - the nearest hit along the segment wins; on a player, the head counts when it's first along the ray.
             - Results are sorted by projectile id and applied serially: damage, then deaths, with kill credit to the shot that took health to 0.
           - **Measured:**
             - shots fired, rejected, late; projectiles alive; segments; terrain, cover and candidate tests;
             - hits by body or head, kills;
             - rewinds clipped by a cap;
             - post-cover hits (target occluded from the shooter's eye at the present): count and rewind;
             - the shots phase's wall / longest / work, plus a `take_hits()` log for tests.
           - **Load tests before fighting bots:**
             - `lattice-bots --fire-share F`: that share of bots holds the trigger, aiming along its heading;
             - `--fire-share` runs on WSL: uniform 5k at 20%, and a 1k blob all firing.
           - **Client:**
             - left mouse fires (held: every 100 ms) with the frame's phase and view;
             - a cosmetic local tracer flies the shared arc;
             - hit feedback (markers, damage direction, kill feed, others' tracers) is M3d.3, but deaths and health already show.
           - **Swarm tests:**
             - per-bot one-way delay in the harness, in steps, both directions;
             - **what you see is what you hit:** a gunner aims at the rendered body, leading by the flight time at the rendered velocity, at a wandering target 30–80 m away. It hits ≥95% at 33 and 100 ms RTT; at ~250 ms RTT (past the near cap) it misses until it leads by the clipped time;
             - headshot aim hits the head ≥90%;
             - a wall or hill between shooter and target (placed with a test `teleport` API) blocks 100%, and post-cover hits only land within the cap;
             - damage is exact: 5 body or 40+40+20 head/body; no friendly fire, self-damage, or damage to the dead;
             - redundant copies never fire twice, stand-ins never fire, the fire rate holds;
             - 20 Hz with dilation stays correct (all in steps);
             - prediction stays bit-exact while shooting.
           - **Deferred:**
             - a "target hint" per shot (+3 B: the entity under the crosshair and its exact drawn step), if transition-time misses show up in the measurements;
             - projectile-vs-projectile;
             - ammo and reload.
        3. **M3d.3: events, tracers, client combat. Done** (2026-10-05). The user played it: the network side is right; visuals (tracers etc.) are deliberately basic, not the focus:
           - **events:** reliable, one `Events` message per client per tick (`game/src/events.rs`); Hurt carries a server-computed direction;
           - **tracers:** unreliable `Shots` with each shot's step; the client starts a tracer when the shooter is drawn firing;
           - **contacts:** a hit makes shooter and target near-tier for each other for 5 s;
           - **swarm:** one marker and one Hurt per damaging hit, kills to both and not a bystander, tracers to the bystander, and a 200 m shooter joins its target's near tier;
           - **Windows self-check, 200 bots:** 26 hits confirmed, 5 kills, 4,618 tracers, 0 corrections.
        4. M3d.4: fighting bots, latency classes, measurements.
    - **M3e** one bare-metal validation session.
  - **Pass bars:**
    - prediction stays bit-exact (0 corrections on a clean link, outside separation pushes);
    - 10k at level 0 with heavy fire (~20% of players at 10 Hz), p99 < 25 ms on the 64-core box;
    - a 3k blob all fighting, p99 < 25 ms;
    - 20 ms and 100 ms RTT bots hit at the same rate (±10%) for the same aim error; 150 ms bots measurably less (they lead ~50 ms);
    - no hit lands with a rewind beyond its cap (300 / 367 ms); post-cover hits are measured;
    - corrections only from separation pushes and respawns, both flagged.
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

- `cargo test --release` (runs the whole workspace) and `cargo clippy --workspace --all-targets` must stay clean. The client is its own workspace: `cargo test` and `cargo clippy --all-targets` inside `client/` too.
- Zero dependencies in the core crate unless there's a strong reason. Crypto uses audited, widely used crates, never hand-rolled. The one dependency is `ring` (BoringSSL-derived ChaCha20-Poly1305 and OS randomness); it builds C/asm, so a C toolchain is needed (MSVC on Windows).
