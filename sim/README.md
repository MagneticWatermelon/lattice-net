# lattice-sim: M1 headless scale test

A movement-only authoritative server and a bot swarm on top of `lattice-net`, measuring per-phase tick time, bytes per client, pps and prediction corrections. Both halves are sans-IO (`SimServer`, `BotBrain`); the binaries own the sockets.

```
cargo test --release -p lattice-sim            # incl. in-process swarm: bit-exact prediction, loss recovery
scripts/m1.sh blob 3000 60                     # scenario, bots, seconds -> results/m1-blob-3000-<time>/
scripts/m1.sh <uniform|hotspots|blob|joins> <bots> [seconds] [extra lattice-server args]

cargo run --release --bin lattice-server -- --help
cargo run --release --bin lattice-bots -- --help
```

## What runs

**Server (`server.rs`)**: the CLAUDE.md phase pipeline, every phase timed:

| phase | work | parallel |
|---|---|---|
| ingress | each transport shard decodes its datagrams: CRC, acks, handshakes, timeouts | per shard |
| events | spawn/despawn, and push inputs into per-entity queues (touches the world) | no |
| movement | consume exactly one input seq per entity: the real input, or a stand-in if it hasn't arrived | rayon |
| grid | counting-sort rebuild of the one shared 32 m grid | no |
| history | positions into a 200 ms lag-comp ring (unused until M3) | no |
| serialize | each entity once per tier: a 15-byte near blob for all, an 11-byte mid/far blob for those due | rayon |
| assembly | per client: pick near/mid/far entities (`interest.rs`), fit the byte budget, memcpy blobs into messages | per shard |
| transport | each shard's `send` + `flush`: framing, acks, CRC | per shard |
| egress | `send_to` per packet (in the binary) | per shard |

The transport is 64 `lattice_net::Shard`s by default (`--shards`). The binary's receive thread buckets each datagram by `Router::shard`, so routing costs no tick time.

Shots and event application (phases 3–4) arrive with M3.

**Interest management (M2a, `interest.rs`).** Every client gets its own full-precision state, plus other entities in three tiers:

| tier | range | rate | per tick | blob |
|---|---|---|---|---|
| near | 150 m, or a squadmate at any distance | 30 Hz, priority accumulator | 64 of up to 100 candidates | 15 B, with velocity |
| mid | 500 m | 10 Hz, due when `id % 3 == tick % 3` | nearest 86 | 11 B |
| far | 1500 m | 2 Hz, due when `id % 15 == tick % 15` | nearest 67 | 11 B |

- **Stagger by entity id.** Mid and far are staggered by entity id alone, so every client gets entity X on the same tick. Only 1/3 and 1/15 of entities need a mid/far blob each tick, and there's no per-client state.
- **Due-set grids.** Their queries run over grids of just the due entities: a 64 m grid for mid, and a 512 m grid for far.
- **The near tier is an accumulator.** Candidates are ranked by `base(distance, squad) × ticks since last sent`, and the top 64 are sent. Per client that's one array of at most ~100 `(entity, last sent)` pairs.
- **Promotion.** An entity new to the near set has its age seeded from its old tier's stagger slot (or "never sent"), so one arriving stale outranks the rest and is sent at once.
- **Budget order.** Near first, then due mid, then due far, within `--budget-kbps` (1,500 by default). Due far entities that don't fit are carried to the next tick and sent first. A carried entity that doesn't fit again counts as `far_starved`, the signal for the degradation ladder (M2d), which never cuts into the near tier.
- **One packet per message.** Each tier goes out as Entities messages of at most one packet, since there's no fragmentation yet. A client typically gets 2–3 packets per tick.
- **k-nearest ring walk.** Near and mid candidates come from a ring walk over the grid (`Grid::walk_rings`). It stops once the k-th candidate is closer than any unvisited cell, so a 3,000-player crowd costs a few cells per client, not every entity within the radius.
- **Squads.** Squads (`--squad-size`, 4 by default) spawn and roam together, and squadmates are always near-tier.

`--spawn line:<meters>` places players on a line at fixed spacing, to check tiers by distance.

**Degradation ladder (M2d, `ladder.rs`).** Under load the server gives up quality in the order CLAUDE.md fixes. Each level keeps the changes before it:

| level | change |
|---|---|
| 1–3 | shrink mid/far radii (down to 70% / 45%); near candidates 80 at level 3 |
| 4–5 | slower staggers (mid every 5 ticks, far every 30); near 48, then 40 per tick from 64 candidates |
| 6 | 20 Hz tick |
| 7–8 | time dilation 0.9, then 0.8 |

- **The controller.** Its input is each tick's work time (simulation + egress, reported via `observe_tick`) as a fraction of the tick's period. It steps down when the p90 over the last 30 ticks exceeds 0.85, and holds 30 ticks after each step. It steps up after 90 consecutive ticks below 0.6. Flags: `--ladder off`, `--ladder-high`, `--ladder-low`.
- **A lower tick rate doesn't change what an input means.** Each input is still one 1/30 s movement step, and a 20 Hz tick consumes 1.5 of them on average. Prediction stays bit-exact, and the reported queue depth is normalized by steps per tick.
- **Pace.** Every snapshot carries `pace`: game-seconds per wall-second. That's the dilation, divided by how far the server is behind its own schedule (actual tick intervals over intended ones, across 30 ticks; up to 2% counts as sleep jitter). Planned changes show at once, and a server that falls behind slows its clients too. Bots send inputs at `pace × 30/s`, the depth nudge fine-tunes it, and a backlog of 3+ extra inputs is dropped at once.
- **Per-client bandwidth ladder.** A client whose carried far entities starve gets its own mid/far radii shrunk, levels 0–3. It probes back up after 90 calm ticks, and each probe that fails at once doubles the wait (up to 30 s), so a client that can't afford more doesn't starve every few seconds. The near tier is never cut.

**Bots (`bot.rs`, `lattice-bots`):** each bot runs a real `lattice_net::Client` with its own UDP socket, sharded across ≤8 threads that each tick their bots at 30 Hz. A bot wanders around the anchor the server's Welcome gives it, sends each input 3× redundantly, predicts with the same `movement::step`, and reconciles against the acked input.

Movement is deterministic f32 code shared by both sides, so prediction matches the server **bit-for-bit**. A correction only happens when the server had to stand in for an input it hadn't received.

**Input policy.** Every tick consumes exactly one input seq, so each server step matches exactly one client step:
- If the next input is missing, a stand-in takes its seq: the last input for 2 ticks (`GRACE_TICKS`), then a frozen input (no movement, facing kept).
- The real input is dropped if it shows up later (`late_inputs`). Holding packets back (a lag switch) therefore buys no movement.
- There's no 2-per-tick catch-up. Instead, each snapshot carries the server's queue depth for that client (`buffered`), and the bot nudges its input clock by up to ±5% to keep the input due now plus one spare queued. It's left alone while the smoothed depth is within [1.75, 2.5]. Two spares (depth 3, which a stall can leave behind) sit above that band, so they drain back.
- The spare is there from the start: the first tick after Welcome sends 2 inputs. Starting at depth 1 left every input arriving just in time, and jitter kept bots one tick late.
- When a snapshot reports a stand-in (depth 0), the bot sends one extra input at once, at most once per 10 ticks. The nudge handles slow drift.
- A client that stalls past the server's seq jumps ahead to it (`resyncs`) and rebuilds its spare, instead of staying late forever.
- At most 3 inputs go out per tick, the most one redundant batch carries.

Stand-ins can exceed bot corrections: a stand-in whose input matches what the bot sent (for example, a repeat of an unchanged input) costs no correction.

**Scenarios:**
- `uniform`: all bots spread over the 8×8 km continent.
- `hotspots`: the first 2,400 bots go to 3 hotspots of ~800, 150 m radius.
- `blob`: the first 3,000 bots go in a 200 m disk.
- `joins`: N uniform bots, plus 500 more joining at 50/s from t=15 s.

## Output

**Server output**
- A line every `--report` seconds (with `--csv`, one row per window).
- At exit, p50/p99/max per phase over the **steady state**: after warmup, and before clients drain below 90% of peak. A mass disconnect loses some Disconnect packets, and those entities sit frozen until they time out, so the drain is left out.
- The system-wide kernel `RcvbufErrors`/`SndbufErrors` deltas from `/proc/net/snmp`.
- **Joins deferred** by the accept budget (`--accepts-per-tick`, default 256 per tick server-wide).
- **Input wait**: from an input's datagram arriving (stamped by the receive thread) to the tick that applies it.
- **The ladder**: level, tick rate, dilation and pace per window, the share of clients degraded by their own bandwidth ladder, and ticks spent at each level.

The server preallocates `max-clients` connections at startup (`--no-prealloc` to skip). Egress uses `sendmmsg` on Linux (`--egress sendto` for comparison).

**Bot output**
- Snapshots, entities per snapshot per tier, **update interval per tier** (every 20th bot tracks the entities it hears about, `--track-every`), corrections, kbps per bot, RTT, join latency (connect → Welcome), and **swarm overruns**. An overrun means the bots were late, so treat any corrections in that window as swarm artifacts.
- Input latency, split three ways:
  - **Input → applied on the server ≈ RTT/2 + server wait.** This is the number to tune the spare against: it's how late other players see you and when your shots resolve. The wait comes back in each snapshot, and needs no clock sync.
  - **Server wait** alone. The spare input is one tick of it.
  - **Round trip:** input → acked in a snapshot the bot has read. This bounds reconciliation replay. It's quantized to whole bot ticks, so its median jumps between 67 and 100 ms from run to run.
- **Swarm busy %**: how much of the bot threads' time went to work. The harness shares the box with the server, so this shows how much it competes.
- **Sink bots** (`--full-every K`; `BOT_ARGS="--full-every 30"` in `scripts/m1.sh`). All but every Kth bot keep playing (inputs, prediction, pace) but only count the entity messages they get. Every bot receives with `recvmmsg`, one syscall per bot per tick.
- The bots' RTT is a network RTT. The server reports its hold as `ack_delay`, and the bots stamp arrivals with `SO_TIMESTAMPNS` instead of their tick time. It reads 1.7–1.9 ms on loopback.

## M2a: tiered interest on the WSL2 dev box

Same box and setup as the M1 baseline below. Phase columns are p50 in ms.

| scenario | tick p50 / p99 (ms) | assembly | transport | egress | near / mid / far per client-tick | bytes per client-tick | update interval near / mid / far (p50) |
|---|---|---|---|---|---|---|---|
| blob 3,000 | 20.7 / 24.8 | 7.8 | 3.5 | 6.9 | 64 / 86 / 0 | 1,948 B (~470 kbps) | 67 / 100 / – ms |
| hotspots 5,000 | 21.3 / 25.1 | 5.5 | 3.9 | 8.4 | 35 / 25 / 22 | 1,087 B (~270 kbps) | 67 / 100 / 500 ms |
| uniform 10,000 | 47.9 / 59.7, every tick over | 10.6 | 10.3 | 17.7 | 16 / 42 / 59 | 1,392 B (~330 kbps) | 33 / 100 / 500 ms |

Findings:

1. **The tiers behave as designed.** In-process tests on a line of players check that each tier matches its distance band and that update intervals are exactly 1, 3 and 15 ticks. They also check that a squadmate 300 m away is near-tier, and that a tight budget skips far entities (counted) but never touches near. Over UDP, update intervals match their periods at p50 and p99. In the blob, near is every 2 ticks (100 candidates share 64 slots per tick).
2. **The blob sits just under the bar.** Its p99 of 24.8 ms against 25 ms is now a real interest-management workload, not M1's "64 nearest".
    - **Assembly is 7.8 ms.** It was 14.4 ms in the first version, which computed a priority for all ~3,000 candidates in range. What fixed it: squared distances, priorities only for the ≤100 survivors, a lookup array instead of sorts in the near select, and a k-nearest ring walk for near and mid.
    - **What's left is per-client nearest-neighbor work in a dense crowd.** The ring walk still visits ~600 entities to prove the 100th is nearest. This is what M2d's ladder (shrinking radii under load) is for.
3. **At 10k, output now costs more than the game.** Transport (10.3 ms) and egress (17.7 ms) scale with bytes and packets: 1.4 KB and 2–3 packets per client-tick, 2.3 Gbps over loopback, CRC and two copies of ~14 MB per tick. The bots decode the same traffic on the same 16 cores. What should help:
    - GSO (clients now get several packets per tick);
    - serialize-once fan-out;
    - `SO_REUSEPORT` socket groups;
    - bare metal.
4. **A server that couldn't hold 30 Hz broke the input clock (fixed by M2d, below).** At ~21 Hz, the bots still sent 30 inputs per second. The queues overflowed (2.9M inputs discarded, each a correction) and input → applied reached ~550 ms, because the ±5% nudge can't follow a server at 70% speed.

## M2d: the degradation ladder on the WSL2 dev box

| scenario | levels used | tick p50 / p99 (ms) | discarded inputs | corrections | input → applied (mean) | bytes per client-tick |
|---|---|---|---|---|---|---|
| blob 3,000 | 0 only | 19.6 / 23.6 | 0 | 0 | 61 ms | ~1,950 B |
| hotspots 5,000 | 0 only | 21.2 / 23.9 | 0 | 0 | 60 ms | ~1,090 B |
| uniform 10,000 | 0 → 6 within 10 s, then 6 (20 Hz) | 29.3 / 35.3 against a 50 ms period | 0 (was 2.9M) | 17 in 60 s (was 2.9M) | 70 ms (was ~550) | 477 B at 20 Hz (~76 kbps) |

- **The ladder leaves healthy scenarios alone.** The blob and hotspots run at a load of ~0.65, below the 0.85 step-down threshold.
- **At 10k it settles where the tick fits.** Level 6 (20 Hz, shrunk radii, slower staggers) runs at a load of ~0.6, between the thresholds, so it doesn't flap.
- **Stand-ins at 10k are 0.003% of input steps**: arrival jitter at 1.5 steps per tick with one spare input.
- **Sink bots don't separate the swarm's cost from the server's.** With 29 of 30 bots as sinks, the swarm is still 51% busy (53% with all bots full), and the server still settles at level 6 with the same tick. A bot's cost is what every client pays to stay connected: receive syscalls, and the transport's CRC, acks and parsing, for 2.3 Gbps of loopback. On one box that can't be split off, so whether 10k truly needs level 6 takes bots on a second machine.
- **The in-process tests cover every rung, both ways.** Sustained forced load walks the server down all nine levels (20 Hz, dilation 0.8) and back up, with zero discarded inputs, stand-ins or corrections throughout. A server that ticks at 20 Hz while meaning to tick at 30 advertises a pace of 2/3, and the bots follow it with zero discards. With the bots' pacing disabled, that test reproduces the 10k failure (720 discards).

## M1 baseline: WSL2 dev box (behavior, not capacity)

Test box: 16 cores, WSL2 on Windows 10, with the server (8 rayon threads, 64 shards) and the bots (8 threads) on the same machine over loopback. Only bare-metal numbers count; these runs show where the time goes. Phase columns are p50 in ms; "before" is the single-shard server. The blob and 10k rows use `sendmmsg` and the accept budget; the other rows predate both and use `send_to`.

| scenario | clients | tick p50 / p99 (ms) | before | ingress | events | assembly | transport | egress | stand-ins after warmup | down kbps/client |
|---|---|---|---|---|---|---|---|---|---|---|
| uniform | 1,000 | 2.8 / 3.4 | 3.0 / 3.8 | 0.35 | 0.09 | 0.30 | 0.31 | 1.4 | 0 | ~15 |
| blob | 3,000 | 11.5 / **13.1** | 16.7 / 19.7 | 0.57 | 0.25 | 4.4 | 1.7 | 3.5 | 0 | ~180 |
| joins | 3,000 + 500 | 7.0 / 8.6 | 8.7 / 13.9 | 0.63 | 0.26 | 0.35 | 0.66 | 4.0 | 0 | ~21 |
| hotspots | 5,000 | 11.4 / 13.5 | 17.8 / 23.7 | 1.1 | 0.38 | 1.7 | 1.6 | 5.5 | 0 | ~98 |
| uniform | 5,000 | 10.0 / 12.1 | 14.3 / 17.6 | 1.2 | 0.39 | 0.44 | 1.1 | 5.8 | 0 | ~28 |
| uniform | 10,000 | 16.5 / 22.2, 3 overruns | 31.1 / 39.6, 180 overruns | 2.4 | 1.2 | 1.2 | 2.3 | 8.3 | 0 | ~43 |

Findings:

1. **The blob passes with room to spare** (p99 13.1 ms against the 25 ms bar), and **10k now fits the 33 ms tick**: 3 overruns in 40 s, all during machine-wide stalls that also overran the swarm, versus 180 before.
2. **Sharding removed the serial transport cost.** At 10k, ingress fell from 9.6 to 2.4 ms and transport from 9.1 to 2.2 ms. What's left serial is the events phase (1.2 ms at 10k), where inputs are pushed into the world's queues.
3. **Egress is still the biggest phase.** At 10k in the same session, `sendmmsg` takes 8.3 ms p50 against 10.8 ms for `send_to`, 23% less, and the tick p50 falls from 18.9 to 16.5 ms. The rest waits for `SO_REUSEPORT`, GSO after M2, and bare metal.
4. **Assembly scales with density, as expected:** 4.5 ms for the blob (every client scans about 3,000 candidates) versus 1.2 ms for 10k uniform. This is where M2's tiers and budgets land.
5. **Steady state has no stand-ins and no corrections in any scenario.**
    - **Joins used to spike the tick.** 10k bots connecting within about 0.5 s pushed one tick to about 150 ms, because each accept cost ~138 µs page-faulting ~130 KB of connection windows.
    - **Accepts are now ~4 µs**, with smaller windows and pooled connections. Without a budget, all 10k join in p99 215 ms, with one 58 ms tick from the handshake burst and first sends.
    - **With the default budget (256 per tick)**, the worst join tick is ~31 ms and joins take p99 1.75 s.
    - **No jitter or loss is simulated over the sockets yet;** run under `netem` for that.
6. **Input → applied averages 51 ms with 200 bots and 57–67 ms at 3k–10k.** The spare input is 33 ms of it. RTT/2 is under 1 ms on loopback; the rest is server wait:

    | | wait |
    |---|---|
    | the spare input (in-process lockstep) | exactly 1 tick |
    | plus tick phase alignment | +0.5 tick |
    | mean at 200 bots | 50 ms |
    | mean at 3k–10k | 54–57 ms |

    - The round trip players never feel is 80–100 ms.
    - **Found by this metric, and fixed: after a hiccup, bots could stay at 2 spares.** A stall made bots rebuild their lead and settle at depth 3, and the old [2, 3] dead band never pulled them back. In one 10k run the server wait rose from ~50 to ~80 ms at t = 20 s and stayed there. The band is now [1.75, 2.5]; a test delays every input a tick for 3 s, then checks the wait returns to one tick.
7. **Zone entry isn't modeled yet.** A spawn here is just a Welcome. The real zone-entry cost is sending the initial world state, which needs fragmentation first.
