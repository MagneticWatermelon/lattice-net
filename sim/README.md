# lattice-sim: M1 headless scale test

A movement-only authoritative server and a bot swarm on top of `lattice-net`, measuring per-phase tick time, bytes per client, pps and prediction corrections. Both halves are sans-IO (`SimServer`, `BotBrain`); the binaries own the sockets.

```
cargo test --release -p lattice-sim            # incl. in-process swarm: bit-exact prediction, loss recovery
scripts/m1.sh blob 3000 60                     # scenario, bots, seconds -> results/m1-blob-3000-<time>/
scripts/m1.sh <uniform|hotspots|blob|joins> <bots> [seconds] [extra lattice-server args]
scripts/preflight.sh                           # is this machine set up for scale runs?
scripts/baseline.sh full <name>                # the whole matrix, ~16 min -> baselines/<date>-<name>/

cargo run --release --bin lattice-server -- --help
cargo run --release --bin lattice-bots -- --help
```

## What runs

**Server (`server.rs`)**: the CLAUDE.md phase pipeline, every phase timed:

| phase | work | parallel |
|---|---|---|
| ingress | each transport shard opens its datagrams: AEAD tag, acks, token handshakes, timeouts | per shard |
| events | spawn/despawn, and push inputs into per-entity queues (touches the world) | no |
| movement | consume exactly one input seq per entity: the real input, or a stand-in if it hasn't arrived | rayon |
| grid | counting-sort rebuild of the one shared 32 m grid | no |
| history | positions into a 200 ms lag-comp ring (unused until M3) | no |
| serialize | each entity once per tier: a 15-byte near blob for all, an 11-byte mid/far blob for those due | rayon |
| assembly | per client: pick near/mid/far entities (`interest.rs`), fit the byte budget, memcpy blobs into messages | per shard |
| transport | each shard's `send` + `flush`: framing, acks, sealing | per shard |
| egress | `send_to` per packet (in the binary) | per shard |

The transport is 64 `lattice_net::Shard`s by default (`--shards`). The binary's receive thread buckets each datagram by `Router::shard`, so routing costs no tick time. It receives with `recvmmsg` (`--ingress`, default on Linux): up to 64 datagrams a call, each stamped by the kernel on arrival, then a wait of up to 1 ms after a short batch (`--rx-gather-us`) so the next call finds a batch. The main loop publishes when the next tick starts, and a wait ends 200 µs before it, so gathering never makes a datagram miss its tick.

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

**Debug map (M2c, `debugmap.rs`).** `lattice-server --debug-http 0.0.0.0:8080` serves a top-down map of what one client receives. From Windows, open `http://<WSL IP>:8080/`, taking the IP from `hostname -I`.
- **Capture.** The sim records the watched client's decisions every 6th tick (5 Hz): its near set with each entity's age and whether it went as a delta or in full, the mid and far entities sent, far skips and starvation, its radii (with its bandwidth level), and its bytes. The rest of the server is unaffected.
- **Page.** It draws every entity, the three tier rings, and colors each entity by what the client got. Near entities that were held back fade with their age, so starvation and tier flips show up at a glance. Scroll to zoom; click an entity to watch its client.
- **Implementation.** Std only: one HTTP thread, hand-written JSON (`/frame`), and `/watch?entity=N`.

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
- **Input wait**: from an input's datagram arriving to the tick that applies it. With `recvmmsg` the kernel stamps the arrival (`SO_TIMESTAMPNS`), so time queued in the socket counts; with `--ingress recvfrom` the receive thread stamps it when read.
- **Ingress**: datagrams per receive call, and each receive thread's CPU time as a share of one core (`ingress_thread_busy_max_pct`; near 100% means that socket can't keep up: add `--sockets`).
- **The ladder**: level, tick rate, dilation and pace per window, the share of clients degraded by their own bandwidth ladder, and ticks spent at each level.

**Threads and sockets:**
- `--threads` defaults to one per physical core; SMT siblings only add scheduler overhead.
- `--sockets N` opens N receiving sockets on the port (`SO_REUSEPORT`). Each has its own receive thread and an equal run of the shards, and each shard sends from its group's socket.
- **`recvmmsg` ingress** (2026-10-07, WSL, same load, A/B against `--ingress recvfrom`):

  | run | receive calls | datagrams per call | receive thread busy | tick p50 / p99 |
  |---|---|---|---|---|
  | uniform 5k, `recvfrom` | 150k/s | 1 | 39.7% | 19.9 / 22.6 ms |
  | uniform 5k, `recvmmsg`, no gather | 48k/s | 3.1 | 40.4% | 18.6 / 20.7 ms |
  | uniform 5k, gather 250 µs | 3.5k/s | 42 | 15.8% | 16.7 / 20.1 ms |
  | uniform 5k, gather 1 ms | 2.7k/s | 55 | 13.0% | 16.2 / 20.4 ms |
  | uniform 5k, gather 250 µs, `--sockets 4` | 11.6k/s | 13 | 7.9% each | 16.6 / 19.1 ms |
  | blob 3k, `recvfrom` | 90k/s | 1 | 32.5% | 20.2 / 22.8 ms |
  | blob 3k, gather 250 µs | 3.1k/s | 29 | 11.3% | 16.3 / 18.8 ms |

  **Then the gather was capped at the tick** (a wait ends 200 µs before the next tick starts), which made a longer one free. Uniform 5k, same day:

  | gather | sockets | datagrams per call | receive thread busy | input wait p50 / p99 | tick p50 / p99 |
  |---|---|---|---|---|---|
  | 250 µs | 1 | 39 | 16.0% | 50.7 / 66.4 ms | 16.5 / 19.7 ms |
  | **1 ms (default)** | 1 | 55 | 12.4% | 47.7 / 64.6 ms | 16.1 / 18.7 ms |
  | 250 µs | 16 | 3.4 | 6.2% each (99% of a core in all) | 50.0 / 66.1 ms | 17.7 / 22.2 ms |
  | **1 ms (default)** | 16 | 9.9 | 2.6% each (42% in all) | 49.9 / 66.1 ms | 16.7 / 19.6 ms |

  Input waits didn't grow, so nothing missed a tick.

  **On AWS** (2× c7a.2xlarge, 8 cores each, server and bots on separate boxes; `baselines/2026-10-07-aws-small-*`):

  | run | receive calls | datagrams per call | receive thread busy | tick p50 / p99 |
  |---|---|---|---|---|
  | uniform 1k, `recvfrom` | 30k/s | 1 | 9.2% | 2.37 / 2.56 ms |
  | uniform 1k, `recvmmsg` (1 ms) | 640/s | 47 | 2.6% | 2.07 / 2.21 ms |
  | blob 1k, `recvfrom` | 30k/s | 1 | 10.1% | 4.28 / 4.60 ms |
  | blob 1k, `recvmmsg` (1 ms) | 750/s | 40 | 2.7% | 3.75 / 3.93 ms |

  - **The tick got 12–13% faster:** the server runs one rayon thread per core, so the receive thread's CPU comes out of the tick.
  - **No errors:** 0 corrections, 0 late inputs, and no drops, errors or allowance limits on either card.
  - **Input wait p50 wasn't comparable between runs on the rig.** It came out 38–41 ms in some runs and 58–62 ms in others, in either mode, and the old `recv_from` rehearsal did the same (40 and 58.6 ms). p99 stayed ~66–68 ms (uniform).
  - **Why:** each bot thread sent all its bots' inputs together, once per period. With 4 bot threads on another machine, arrivals came at only 4 phases of the server's tick, so the median moved with where those fell. Real players' phases are spread out.
  - **Fixed (2026-10-07):** each bot thread now ticks its bots in 8 slots interleaved over the period (`lattice-bots --slots`; `--slots 1` is the old behaviour). That's threads × 8 phases.
    - WSL, uniform 1k, 2 bot threads, 3 runs each:
      - `--slots 1`: p10 / p50 / p90 of 56 / 59 / 66 ms in one run, 36–41 / 59–61 / 62–64 ms in the others;
      - `--slots 8`: 36–38 / 49–50 / 63–66 ms in every run.
    - The server summary now has `input_wait_p10_ms` and `input_wait_p90_ms`, so one run shows whether arrivals are spread.

  - **The wake-ups were the cost.** Batching alone barely moved the thread (3 datagrams a call, still 40% busy).
  - **The tick got faster** because WSL's oversubscribed cores got the time back. Expect less on bare metal.
  - **Waits stay exact.** Input wait is unchanged: p50 ~50 ms, p99 ~66 ms.
  - **No errors:** 0 corrections, 0 kernel drops, 0 late inputs.
  - **Still per datagram:** a `Vec` per datagram and the keyed hash route. Pooled buffers are the next step if the 10k bare-metal run shows a busy thread.
  - M3e's `m3e-uniform-10k-recvfrom` repeats the A/B at 10k.

**Phase breakdown:**
- For the phases split by shard (ingress, assembly, transport, egress), the server records the longest shard task and the total work each tick.
- `summary.md`'s "Phase breakdown" table compares each phase's wall time with what perfect scheduling would take, max(longest task, work ÷ threads). The difference is dispatch, waiting and imbalance.

With `--summary PATH`, the server and the bots also write their end-of-run results as `key=value` lines. The server's cover the steady state: phase times, ladder levels, pps, bytes per client-tick, stand-ins and kernel drops. `scripts/m1.sh` passes it, and `scripts/baseline.sh` reads it.

The server preallocates `max-clients` connections at startup (`--no-prealloc` to skip). Egress uses `sendmmsg` on Linux (`--egress sendto` for comparison). `--egress gso` sends each client's packets for the tick as one `UDP_SEGMENT` send, and the summary counts datagrams, sends and syscalls.

**Bot output**
- Snapshots, entities per snapshot per tier, **update interval per tier** (every 20th bot tracks the entities it hears about, `--track-every`), corrections, kbps per bot, RTT, join latency (connect → Welcome), and **swarm overruns**. An overrun means the bots were late, so treat any corrections in that window as swarm artifacts.
- Input latency, split three ways:
  - **Input → applied on the server ≈ RTT/2 + server wait.** This is the number to tune the spare against: it's how late other players see you and when your shots resolve. The wait comes back in each snapshot, and needs no clock sync.
  - **Server wait** alone. The spare input is one tick of it.
  - **Round trip:** input → acked in a snapshot the bot has read. This bounds reconciliation replay. It's quantized to whole bot ticks, so its median jumps between 67 and 100 ms from run to run.
- **Swarm busy %**: how much of the bot threads' time went to work. The harness shares the box with the server, so this shows how much it competes.
- **Sink bots** (`--full-every K`; `BOT_ARGS="--full-every 30"` in `scripts/m1.sh`). All but every Kth bot keep playing (inputs, prediction, pace) but only count the entity messages they get. Every bot receives with `recvmmsg`, one syscall per bot per tick.
- The bots' RTT is a network RTT. The server reports its hold as `ack_delay`, and the bots stamp arrivals with `SO_TIMESTAMPNS` instead of their tick time. It reads 1.7–1.9 ms on loopback.

## Baselines (`scripts/baseline.sh`)

One command runs the whole scenario matrix the same way on any Linux machine, and records the machine next to the numbers. Two baselines from different machines are then directly comparable.

```
scripts/preflight.sh                            # checks, with the fix for each problem
PROFILE=1 scripts/baseline.sh full <name>       # ~16 min; leave the machine alone meanwhile
```

**The matrix** (`full`): 60 s runs, each scenario twice, interleaved so drift doesn't land on one scenario.

| run | what it answers |
|---|---|
| `uniform-1k`, `uniform-5k`, `uniform-10k` | how the tick scales with players (10k settles on a ladder level) |
| `uniform-10k-noladder` | 10k's raw cost at full rate and radii (`--ladder off`) |
| `hotspots-5k` | 3 hotspots of ~800 |
| `blob-3k-sendmmsg`, `blob-3k-gso` | the M1 pass bar (p99 under ~25 ms), and GSO against `sendmmsg` |
| `joins-5k` | 500 joins at 50/s on top of 5,000 players |

`quick` runs two small scenarios for 20 s: it checks the harness, not the machine.

**Output** in `baselines/<date>-<name>/`:
- `env.txt`: the machine (CPU, kernel, governor, socket limits, conntrack, loopback offloads, toolchain) and the preflight checks.
- `summary.md`: server, phase and client tables, one row per run.
- One directory per run with its logs, per-window CSV and `key=value` summaries.
- With `PROFILE=1` and perf installed, a flat profile of the server in the first 10k and GSO blob runs (`perf.txt`).

Baselines are small text files. Commit them.

**Preflight** checks, and prints the fix for anything wrong:
- **Failures:** a missing toolchain (`ring` needs a C compiler), the port taken, or a hard open-file limit too low for one socket per bot. The bots raise their soft limit themselves, since stock Linux allows only 1,024.
- **Warnings:** WSL or a VM, socket buffers capped below the server's 16 MiB, a CPU governor other than `performance`, too few ephemeral ports, conntrack tracking flows, other load on the machine, low memory, and perf that can't profile.

### The WSL reference (`baselines/2026-09-29-wsl2`)

The dev box under WSL2, with the server on 8 threads and the bots on the other 8. Two runs each; tick times in ms.

| run | level | tick p50 / p99 | notes |
|---|---|---|---|
| uniform 1k | L0 | 3.5–3.7 / 4.4–4.8 | |
| uniform 5k | L0 | 17.2–17.5 / 21.2–23.4 | |
| uniform 10k | L6–L8, mostly dilation 0.9–0.8 | 34.6–34.9 / 46.4–48.5 | input → applied p50 80 ms |
| uniform 10k, ladder off | L0, every tick over its 33 ms | 47.2–47.8 / 62.5–63.6 | 1.75 packets, 1,313 B per client-tick |
| hotspots 5k | L0–L2 | 22.2 / 26.9–29.4 | |
| blob 3k, `sendmmsg` | mostly L1 | 22.8–23.2 / 27.0–28.1 | egress p50 6.7–6.8 |
| blob 3k, GSO | mostly L0 | 21.4–21.6 / 27.6–28.5 | egress p50 5.0–5.1 |
| joins 5k + 500 | L0 | 18.4–18.7 / 21.5–24.0 | joiners' p99 join time 165 ms |

- **The blob is over the M1 pass bar here: p99 27–28.5 ms against ~25 ms.** At M1 it was 13 ms. M2's per-client interest work made assembly the biggest phase (10.6–10.8 ms p50).
- **Sustained load is worse than short runs.** Within each minute-long run the tick p50 creeps up by 1–3 ms, and the p99 spikes (30–43 ms) come in the last 20 s. The earlier 40 s runs, each after a pause, showed blob p99 23–24.5 ms and 10k at L6.
  - Likely causes: the CPU clocking down as it heats over 16 minutes of full load, or activity on the Windows host.
  - WSL can't show clock speeds, so bare metal has to tell these apart.
- **Kernel receive drops only at 10k:** 0–1,666 per run. WSL caps socket buffers at 4 MiB.

### Bare metal: Scaleway, 2026-10-04 (`baselines/2026-10-04-scaleway-*`)

- **Server:** EM-I620E (EPYC 8534P, 64 cores / 128 threads, Zen 4c).
- **Bots:** EM-I320E (EPYC 8224P, 24 cores), on the other end of a Private Network that `iperf3` measured at 23.5 Gbps each way.
- **Bot box load:** at most 14% busy, so the bots never limited a run.

Tick times in ms; two runs each, and they agree within ~0.5 ms.

| run | 8 server threads (dev box's count) | 128 threads (whole box) | WSL reference |
|---|---|---|---|
| uniform 1k | L0, 2.8–2.9 / 3.0–3.1 | L0, 2.9 / 3.3 | L0, 3.5–3.7 / 4.4–4.8 |
| uniform 5k | L0, 15.5–15.6 / 16.4 | L0, 7.0–7.1 / 8.0–8.1 | L0, 17.2–17.5 / 21.2–23.4 |
| uniform 10k | **L6**, 31.7–31.9 / 33.3–33.7 | **L0**, 14.1 / 15.4–15.5 | L6–L8, 34.6–34.9 / 46.4–48.5 |
| uniform 10k, ladder off | every tick over, 42.4–43.0 / 44.5–45.4 | 13.9 / 15.4–15.6 | 47.2–47.8 / 62.5–63.6 |
| hotspots 5k | L0, 19.5–19.6 / 20.6 | L0, 7.8 / 8.9–9.1 | L0–L2, 22.2 / 26.9–29.4 |
| blob 3k, `sendmmsg` | L0, 18.6–18.7 / **19.4** | L0, 7.7 / 8.5–8.6 | mostly L1, 22.8–23.2 / 27.0–28.1 |
| blob 3k, GSO | L0, 18.6 / **19.5** | L0, 8.2–8.3 / 9.0–9.4 | mostly L0, 21.4–21.6 / 27.6–28.5 |
| joins 5k + 500 | L0, 17.1–17.2 / 18.2 | L0, 7.3–7.5 / 8.3 | L0, 18.4–18.7 / 21.5–24.0 |

**10k with the ladder off** (one 45 s run per thread count, `2026-10-04-scaleway-scaling`), tick p50 / p99 in ms:

| server threads | 8 | 16 | 32 | 64 | 128 |
|---|---|---|---|---|---|
| tick | 42.4 / 44.5 | 24.9 / 25.9 | 16.3 / 17.4 | **12.2 / 13.0** | 13.9 / 15.6 |

**Findings:**

1. **The blob passes M1's bar on bare metal:** p99 19.4–19.5 ms with the dev box's 8 threads (27–28.5 ms on WSL), and 8.5–9.4 ms on the whole box.
   - WSL mostly cost tails: the medians are only 15% lower here, but p99 sits ~1 ms above p50 instead of 6–7 ms.
   - Zero stand-ins, corrections or kernel drops in any run.
2. **10k fits at full rate (level 0, 30 Hz, full radii) on the 64-core box,** with p99 15.4 ms, under half the 33 ms budget. With 8 threads it settles at level 6 (20 Hz), as on WSL but without the dilation.
3. **The tick scales to the physical cores, not the SMT threads.**
   - From 8 to 64 threads it's 3.5× faster, and 128 threads is slower than 64.
   - Profiles at 128 threads: 35–45% of the CPU is rayon's scheduler (idle workers spinning and stealing, `crossbeam_epoch`).
   - Run with threads = physical cores. The serial events phase (~2.4 ms at 10k) is the floor.
4. **One socket is now a lock.** In the blob profile at 128 threads, 8.5% of the CPU is a kernel spinlock on the send path (`native_queued_spin_lock_slowpath` next to `fq_codel_enqueue`): every shard sends through the same UDP socket. This is the case for `SO_REUSEPORT` socket groups (transport step 5).
5. **GSO doesn't help on this NIC.** Broadcom `bnxt_en` has no UDP segmentation offload (`tx-udp-segmentation off [fixed]`), so the kernel segments in software. GSO ties `sendmmsg` at 8 threads and is slightly slower at 128 (egress 2.6 vs 2.1 ms p50). The hardware verdict needs a NIC with offload (ConnectX or E810).
6. **Encryption is cheap:** `ring`'s ChaCha20-Poly1305 seal is under 2% of the CPU at 10k.

**Session notes:**
- The OS install took 10 min.
- SSH came up ~8 min after Scaleway reported the servers ready: big EPYC boxes take a while to boot.
- Setup to teardown took about 1 h 5 min, roughly €3 at €2.82/h.
- `cloud-up.sh --resume` finishes a setup that stopped partway.

### Limits: crowds and counts (`baselines/2026-10-04-scaleway-limits`)

`scripts/baseline.sh limits`, on the Scaleway pair with 64 server threads:
- Players join at 50/s (100/s for 20k), then hold for 40 s.
- `summary.md` shows the cost at each step of players.

| ramp | where it breaks |
|---|---|
| everyone in a 25 m disk, ladder off | tick p99 over 33.3 ms at **7,217 players** (at 10k: p50 56 ms, p99 60 ms) |
| everyone in 25 m, ladder on | leaves level 0 at 6,470, reaches level 8 (dilation 0.8), and is still over its 50 ms tick at **9,729** |
| everyone in a 200 m disk, ladder off | **within budget to 10,000** (p99 19.3 ms) |
| uniform, ladder off | over at **17,954 players** |

1. **Density is the limit, not player count.** Uniform holds ~1.8× the 10k target. One capture point holds ~7k.
2. **The nearest-player search is quadratic in a crowd.** In the 25 m pile, assembly goes 2.5 → 7.9 → 26 → 44 ms at 1.7k → 4k → 7.7k → 10k players.
   - The profile at 10k: `select_nth_unstable` (partial sorts of the candidates) is 46% of the CPU, the grid walk 14%.
   - With 32 m grid cells, every client starts from everyone in the pile and partially sorts them, twice per tick.
3. **The ladder can't rescue a pile.** Its levers shrink radii and lower rates, and everyone is inside every radius.
4. **Fixed since by a new search** (see the next section): the pile's cost is now linear in its size.

**Shards:** 256 shards instead of 64 doesn't help at 10k (tick p50 12.6 vs 12.1 ms, the parallel phases identical; the serial events phase grows 2.4 → 3.1 ms). Uneven shards aren't the hidden serial cost (`2026-10-04-scaleway-shards`).

**netem at 10k is invalid** (`2026-10-04-scaleway-netem-10k`, see its `NOTE.md`):
- With netem on the server's own interface, its one locked queue throttled the server: egress 2.75 → 25 ms p50, tick 12 → 40 ms.
- The fix: shape only on the bot box (its egress, plus an `ifb` device on its ingress).

### M3a: a world, 2.5D movement and soft separation (2026-10-04)

The game rules both sides run now live in the `lattice-game` crate (`game/`): movement, the world, the message formats and the near codec. The server, the bots and the coming client all depend on it.

**The world** (`game/src/world.rs`) is built from a seed, identically on every machine.
- **Terrain:** an 8 km heightmap at 4 m. It's integer value noise in fixed point, about 160 m of relief, heights in cm, 8 MB.
- **Cover:** boxes in about a quarter of the 32 m cells. Thin walls are 2.5–4 m tall; crates are under 1 m.
- **`World::shared`** keeps one copy per seed per process. Generating it takes a fraction of a second.
- **The seed travels in the Welcome** (`--world-seed`).

**Movement** (`game/src/movement.rs`) is 2.5D and still bit-exact:
- players walk on the terrain, jump and fall, and can't climb slopes over 45°;
- they step onto ledges up to 0.45 m, and slide along cover;
- when falling, they catch a ledge within a step, which is how a jump gets onto a crate.

**On the wire:**
- inputs carry pitch (7 B each);
- the snapshot's own state adds height, vertical speed, grounded, and a push counter;
- the near codec sends height as a 1 cm delta (0 bits on the flat), pitch on its own changed-flag, and an airborne flag;
- mid and far are unchanged: clients put those players on the terrain.

**Soft separation** is server-only and a pipeline phase of its own (`separate`, after the grid).
- **The rule:** players closer than 0.8 m are pushed apart, half each, by half the overlap per tick, at most 0.1 m a tick (3 m/s). Coincident players get a direction from their ids.
- **Computed, then applied:** all pushes are computed from the same positions before any is applied. Neighbors come through the grid's sub-cells (`Grid::for_each_within`).
- **Snapshots count pushes,** so clients can tell a push correction (unpredictable by design) from a real misprediction. After a push the client rebases on any difference at all: a sub-millimeter push left alone would grow into a "misprediction" later.
- **It's soft:** it can't stop players who keep walking into an over-full crowd at 6 m/s, but it halves the overlap (swarm test: 150 players in 3 m). `--no-separation` turns it off for comparisons.

**WSL cost, before → after M3a** (8 threads, sharing the box with the bots):

| run | tick p50 / p99 | movement | `separate` | real corrections | push corrections (largest) |
|---|---|---|---|---|---|
| pile 4k, ladder off | 24.3 / 27.6 → 24.9 / 28.5 ms | 0.47 → 0.52 ms | 0.98 ms | 0 | 4.6M (0.39 m) |
| blob 3k | 16.7 / 19.4 → 17.5 / 22.5 ms | 0.46 → 0.50 ms | 0.85 ms | 0 | 162k (0.30 m) |
| uniform 5k | 15.1 / 17.7 → 15.9 / 19.9 ms | 0.35 → 0.64 ms | 0.50 ms | 0 | 1.2k |

Prediction stays bit-exact (the swarm test `clean_link_predicts_bit_exactly` runs on terrain, with cover and jumps). Over UDP, the only corrections outside pushes come from stand-ins when the box is overloaded.

### M3b: the client core and one render timeline (2026-10-05)

The client every player runs, bots and humans alike, is now `lattice-client-core` (`client-core/`): the input clock, prediction and reconciliation, and the entity store. A bot (`sim/src/bot.rs`) is that core plus a wander AI as its input source.

**The timeline is game steps, not ticks.** Snapshots carry `step` (1/30 s of game time). Every other message's tick maps to a step through the snapshots (`TickSteps`). At 30 Hz a tick is a step; at 20 Hz it's 1 or 2, so rendering on ticks would play movement at alternating 0.67× and 1.33× speed.

**Render clock** (`client-core/src/clock.rs`):
- other entities are drawn at the newest step minus 100 ms (`--interp-ms` on the bots);
- the newest step tracks the earliest snapshot arrivals: it moves up at once, and down by 2% per late snapshot, so jitter doesn't move it;
- the render step slews at most ±10% to follow and never runs backwards; it snaps only past 0.5 s of error;
- under dilation it runs at pace × 30 steps/s.

**Entities** (`client-core/src/entities.rs`) keep one timeline of samples each, whatever tier the samples came from.
- **Drawing:** interpolated when bracketed; otherwise extrapolated (near: its velocity; mid/far: derived from the last two samples) for one update interval, then held.
- **Smoothing:** a sample that changes what's on screen becomes a visual offset that decays over 100 ms. Its size before smoothing is the *pop*, which tracked bots report per tier.
- **Forgetting:** an entity that stops coming (it left interest; leaving isn't sent) is dropped at twice its update interval overdue, at least 0.5 s and at most 2 s.
- **Blobs:** mid/far blobs now fill their altitude (16 cm), pitch and airborne bits.

**Render times in inputs.** Each input carries the render step it was made at (u16 in 1/64 steps, +2 B per input). The server records **rewind** = applied step − render step: what lag compensation would rewind for that input (server summary `rewind_*`).

**The input clock has two entry points:**
- `step_inputs`: exactly one step per call, for callers at 30 Hz (the bots);
- `tick_inputs`: by elapsed time, for a frame loop (the human client).

A 30 Hz caller running by elapsed time doubled the extra/skipped inputs and raised p99 server wait from 67–74 to 80–102 ms (WSL blob, 3 runs each). `own_render` draws the client's own player between its last two predicted steps.

**Swarm tests** (`render_timeline_*`, 60 bots roaming a 500 m disk, renders checked against the server's states at the render step):

| link | near interpolated, error p99 | mid interpolated, error p99 | far error p50 / p99, pops p50 / p99 | rewind p50 / p99 |
|---|---|---|---|---|
| clean | 100%, 7 mm | 99.9%, 10 cm | 0.31 / 3.5 m, 0.64 / 5.0 m | 167 / 167 ms |
| 5% loss, 0–33 ms jitter | 100%, 1.2 cm | 96.4%, 25 cm | 0.35 / 4.5 m, 0.77 / 5.9 m | 167 / 200 ms |
| 20 Hz at dilation 0.8 (ladder bottom) | 99.9%, 6 mm | 40% (mid every 5 ticks), 1.4 m | 2.1 / 13 m | |

- **Near and mid are exact to their quantization.** Mid's error is mostly its 16 cm height steps.
- **Far (2 Hz) is extrapolated ~75% of the time by design.** A metre-scale error at 500–1,500 m is a pixel or a few. These bots turn at random every ~1.5 s, a harsh case. This is the number that decides whether mid/far need velocity bytes; not yet.
- **Rewind on a zero-latency link is 167 ms:** the 100 ms render delay, the spare input (33 ms), and the wait for the next tick (33 ms). **So rewind = RTT + ~167 ms**, which M3d's 200 ms cap must reckon with: only players under ~33 ms RTT would be fully compensated.
- **The delay sweep** (`render_delay_sweep`, ignored by default) shows the trade:
  - 67 ms: rewind 134 ms; mid pops p99 0.2 m clean, 0.4 m lossy;
  - 100 ms: near and mid without pops;
  - 133 ms: rewind 199 ms.

**Over UDP on WSL**, blob 1k: 0 corrections, render delay 100.1 ms, 0 clock snaps; near 99.5% interpolated, mid 94.4% (2.9% held: entities churning out of the capped mid set); rewind p50 167 / p99 200 ms at 7.6 ms RTT.

**Netem on WSL** (`baselines/2026-10-05-wsl2-m3b-netem`, 1k bots, every 20th tracked). Shares of frames interpolated; pops p99 in mm; uniform / blob.

| link (one way) | near | mid | mid pops p99 | far interpolated / extrapolated, pops p99 | rewind p50 / p99 |
|---|---|---|---|---|---|
| clean | 99.8 / 99.7% | 98.7 / 95.1% | 118 / 117 | 20 / 79%, 4.9 m | 167 / 200 ms |
| LAN: 15 ± 2 | 99.8 / 99.7% | 99.6 / 95.0% | 46 / 145 | 20 / 79%, 4.9 m | 200 / 210 ms |
| typical: 40 ± 5, 0.5% loss | 99.8 / 99.6% | 96.9 / 94.5% | 133 / 168 | 21 / 78%, 5.0 m | 233–249 / 267 ms |
| far: 75 ± 10, 1% loss | 99.8 / 99.4% | 96.7 / 91.6% | 193 / 201 | 20 / 78%, 5.3 m | 302–304 / 330 ms |
| lossy: 40 ± 5, 5% loss | 99.8 / 98.0% | 91.7 / 90.1% | 324 / 320 | 20 / 76%, 5.6 m | 234 / 270–299 ms |
| jittery: 40 ± 20 (σ) | 99.8 / 98.2% | 81.1 / 83.0% | 332 / 333 | 17 / 83%, 5.3 m | 217–233 / 261–267 ms |

- **Near holds on every link** (≥98%, pops ≤ 8 cm at p99), and the render delay stays 100–102 ms with no snaps.
- **Mid misses its ≥99% clean bar over UDP:** 98.7% uniform, 95% blob. In the blob, ~2% are held: entities that left the capped mid set and linger until forgotten. The rest is extrapolation of 1–3 steps where a 10 Hz update lands right at the render step. Under ±20 ms jitter (σ, so tails of 60 ms), mid falls to ~82% interpolated, with pops of 0.33 m at p99, smoothed over 100 ms.
- **Rewind = RTT + ~150–170 ms on every link.** On the far link (150 ms RTT) it's ~300 ms.
- **Unchanged from 2026-10-04:** input → applied, stand-ins, and corrections (≤0.18 per bot-minute).

### Per-tier render delays (2026-10-05)

One 100 ms timeline was too long for near players (it added 33 ms to every rewind for nothing) and too short for mid ones (at 10 Hz, one late or lost update made them extrapolate). Now each tier has its own delay:
- **Near: 67 ms,** two 30 Hz updates. The render clock runs at this delay.
- **Mid and far: 200 ms,** two 10 Hz updates, so a lost one is bridged. 200 ms in the past is invisible at 150 m and beyond. Far (2 Hz) is still mostly extrapolated, but less.
- **A tier change glides:** an entity's lag behind the render clock slews at 25% (133 ms over ~0.5 s). It plays a little fast or slow, never skips.
- **Lag compensation rewinds each target by its own tier's delay.** Inputs carry the near render step, and each batch carries the mid lag (1 B). The server reports `rewind_near_*` and `rewind_mid_*`.
- **16 samples per entity** (was 8): a mid entity is drawn ~6 steps behind its newest sample.

**The delay sweep** (`render_delay_sweep`: 60 bots in a 500 m disk, renders checked against the server's states at each entity's own step; lossy is 5% loss with 0–33 ms of jitter):

| near / mid delay | near interpolated, clean / lossy | mid interpolated, clean / lossy | mid pops p99, lossy | far interpolated, error p50 | rewind near / mid, clean |
|---|---|---|---|---|---|
| 100 / 100 ms (before) | 100 / 100% | 99.9 / 96.4% | 320 mm | 27%, 0.31 m | 167 / 167 ms |
| 33 / 133 ms | 99.9 / **95.0%** | 99.9 / 96.4% | 215 mm | 27%, 0.26 m | 99 / 199 ms |
| 67 / 133 ms | 100 / 100% | 99.8 / 96.4% | 215 mm | 27%, 0.25 m | 134 / 201 ms |
| **67 / 200 ms** | **100 / 99.99%** | **99.9 / 99.7%** | **0** | **47%, 0.17 m** | **134 / 267 ms** |
| 100 / 267 ms | 100 / 99.99% | 99.97 / 99.9% | 0 | 60%, 0.12 m | 167 / 333 ms |

- **67 / 200 is the knee.** Near at 33 ms loses interpolation under loss. Mid needs 200 ms to bridge a lost update; at 133 ms it gains nothing over 100. Going to 267 ms buys 0.2% for 67 ms more rewind.
- **At the ladder's bottom** (20 Hz, dilation 0.8), mid is 79% interpolated (was 40%), and near keeps 99.9%.
- **On the Windows client** (1k blob, WSL server): near 99.7% interpolated, mid's extrapolated frames 1.26% → 0.92%, 0 corrections, render delay 68 ms. Mid's remaining misses are entities churning in and out of the capped mid set.

**Near adapts to crowds.** WSL netem with near fixed at 67 ms (`baselines/2026-10-05-wsl2-tier-delays-netem`) fixed mid: uniform runs are ≥99.2% interpolated on every link, against 81–99% before. But near regressed in the 1k blob: jittery 98.2 → 83.0%, lossy 98.0 → 93.4%.
- **The cause:** in a crowd, ~100 near candidates share the near tier's 64 sends per tick, so a near entity updates every 1–3 ticks, not every tick.
- **The fix (`NearNeed`):** for each near update, the client records how far behind the newest step its predecessor was when it arrived. That's the delay that would have kept the entity interpolated.
- **The delay follows the 99th percentile of that,** with a 3 s memory, between `--near-ms` 67 and `--near-max-ms` 133. It goes up at once and down with hysteresis.
- **Mid stays at 200 ms** in all, and inputs already carry the absolute near render step, so the server's rewind follows.
- **Swarm crowd test** (near capped at 20 per tick): fixed at 67 ms, near is 62.7% interpolated (pops p99 0.36 m); adaptive, 98.1% at 133 ms (pops p99 0.11 m). At the ladder's bottom it settles at 92 ms, and the near error p99 is 6 mm.

**WSL netem with the adaptive near delay** (`baselines/2026-10-05-wsl2-adaptive-near-netem`). Interpolated %, uniform / blob:

| link (one way) | near: one 100 ms timeline | near: fixed 67 ms | near: adaptive | its delay (tracked bots) | mid: 100 ms | mid: 200 ms |
|---|---|---|---|---|---|---|
| clean | 99.8 / 99.7 | 99.9 / 98.0 | **99.9 / 99.8** | 73 / 82 ms | 98.7 / 95.1 | **99.5 / 95.3** |
| LAN: 15 ± 2 | 99.8 / 99.7 | 99.9 / 99.7 | **99.9 / 99.8** | 67 / 87 ms | 99.6 / 95.0 | **99.5 / 96.1** |
| typical: 40 ± 5, 0.5% loss | 99.8 / 99.6 | 99.9 / 98.4 | **99.9 / 99.7** | 76 / 100 ms | 96.9 / 94.5 | **99.5 / 96.2** |
| far: 75 ± 10, 1% loss | 99.8 / 99.4 | 99.7 / 93.5 | **99.9 / 99.7** | 95 / 127 ms | 96.7 / 91.6 | **99.4 / 96.2** |
| lossy: 40 ± 5, 5% loss | 99.8 / 98.0 | 99.4 / 93.4 | **99.8 / 99.5** | 92 / 133 ms | 91.7 / 90.1 | **99.2 / 95.6** |
| jittery: 40 ± 20 (σ) | 99.8 / 98.2 | 98.3 / 83.0 | **99.9 / 99.8** | 111 / 134 ms | 81.1 / 83.0 | **99.5 / 96.4** |

- **Near and mid pops** are 0–46 mm at p99 on every link (mid was 0.3 m under loss and jitter before).
- **Mid in the blob** sits at ~96%: ~3% are entities churning in and out of the capped mid set (held, or new), and ~1.3% is extrapolation.
- **The server's rewind numbers** come from every bot, and only every 20th tracks entities and adapts. So `rewind_near_*` there mostly reflects 67 ms: clean p50 133 ms, RTT + 133 on the other links. A client at the 133 ms maximum adds 67 ms to that.
- **Input → applied and corrections** are unchanged.

**Separation across heights** has a unit test now (`separation_pushes_bodies_that_overlap_in_height_too`). Pairs a body height or more apart vertically aren't pushed; a player on a crate beside another overlaps it and is.

### M3d: combat

**M3d.1: factions, health, death and respawn.**
- **Factions:** a player's faction is its entity id mod 3, from per-faction id lists, so it costs nothing on the wire.
- **Health:** own health and a `life` counter are in the snapshot. Near states carry exact health and mid/far blobs a 1/15th bucket, with a dead flag in both.
- **The dead:** their inputs move nothing (`movement::dead_input`), on the server and in prediction alike, and their bodies keep the aim they died with. They respawn after 5 s on their faction's side of the scenario's area.
- **Clients:** a death or respawn rebases and replays (a life event, never a correction) and is a cut: never smoothed, never interpolated across.
- **`--deaths-per-sec`** kills random players, standing in for weapons.

**M3d.2: shots and lag compensation.**
- **The rifle** (`game/src/weapon.rs`): 600 m/s with gravity, 10 rounds/s, 2 s range, 20 body / 40 head damage. Its kinematics are shared with the client's tracers.
- **Hit geometry** (`game/src/hit.rs`): segment against capsule, sphere, box and terrain. Hitboxes are a body capsule (r 0.4, 0.4–1.1 m) and a head sphere (r 0.2 at 1.6 m).
- **Shots ride their input** (`BUTTON_FIRE` + 7 B: when in the step it fired, the aim, the render step then), redundant ×3.
- **The server fires a shot when its input is applied,** from the shooter's eye between its states for that step (late ones too, within 8 steps), never from stand-ins, at most every 3 steps.
- **The shots phase** (`sim/src/shots.rs`) runs after history. A projectile lives in its shooter's timeline: each half-step segment is tested against terrain, cover and players placed at τ − D for their tier (capped at 300 / 367 ms), from a 3D, 16-tick history that never interpolates across a life change. Hits apply in projectile order.

**The swarm tests** (`what_you_see_is_what_you_hit` and the rest). A gunner aims where its client draws the target, leading for flight time and drop at the drawn velocity:

| case | hits |
|---|---|
| target strafing 6 m/s, 50 m out, 0 or 33 ms one-way | 200 / 200 |
| headshots at a standing target | 100 / 100 head |
| 133 ms one-way (past the 300 ms near cap), aiming at the drawn target | 3% |
| ...leading the clipped 2–3 steps | 92% |
| target behind a wall | 0 (all stop on the wall) |

- **Kills land exactly at 100 HP.** Hits on a target that died since the shooter saw it deal nothing (`hits_too_late`).
- **Hits are keyed to the life the shooter saw.** History keeps each entity's life counter. A hit deals damage only while the target is still on that life, so a shot at a corpse can't hurt its respawned body, even when the rewound position is right.
- **Render-step claims are trimmed (the backtrack cheat).** A client could claim it drew a target earlier than it did, to shoot where the target used to be. The server bounds each shot's rewind by what an honest client could need: RTT + the input's wait on the server + the protocol's longest render delay (4 steps near, 6 mid/far) + 2 steps of slack. Anything beyond is trimmed and counted (`rewinds_trimmed`). There's no trim until the connection has an RTT.
  - `backtrack_claims_are_trimmed`: a cheat claiming 10 steps further back has every shot trimmed and hits 10 of 100.
  - Honest gunners are never trimmed (`what_you_see_is_what_you_hit` checks this).
- **The input queue's rules are unit-tested:** shots fire once, at the rifle's rate, never from stand-ins; late ones still fire within 8 steps; shots from the dead are refused.

**Firing load on WSL** (`lattice-bots --fire-share F`: bots hold the trigger, level along their heading; 8 server threads sharing the box):

| run | shots | shots phase p50 / p99 | tick p50 | hits, kills | corrections |
|---|---|---|---|---|---|
| uniform 5k, 20% firing | ~9k/s | 2.0 / 2.8 ms | 21.6 ms | 942, 42 | 0 |
| 1k blob, all firing | ~7k/s | 1.6 / 2.2 ms | 8.6 ms | 10,147, 1,844 | 0 |

- **About 180 ns per segment** across 8 threads. Most is the terrain march: these bots fire level into hills, so 70–80% of shots end in the ground. A coarse max-height skip is the obvious saving, if bare metal asks.
- **The blob's refused shots** (1,184) are bots that died on the server before they heard.
- **The Windows client** fires on the left mouse button, with a cosmetic tracer. It was self-checked with `--autoplay --autofire`: 122 shots, 0 corrections.

**M3d.3: hit feedback and others' shots** (`game/src/events.rs`).
- **Reliable `Events`,** one message per client per tick:
  - `Hit` to the shooter (target, damage, head, killed);
  - `Hurt` to the target (shooter, amount, and the direction to the shooter, computed on the server so it works when the shooter isn't drawn);
  - `Kill` to both and their squads.
- **Unreliable `Shots`:** each tick, the shots fired by a client's near-tier players (7 B each: shooter, aim, and how long before the tick's step). The client starts each tracer when it draws the shooter at that moment. Its bytes count in the client's budget before mid and far.
- **Contacts:** a hit makes shooter and target near-tier for each other for 5 s (up to 4 contacts each, priced like squadmates).
- **Swarm tests:**
  - every damaging hit gives the shooter one marker and the target one Hurt from the right direction (within 2/256 of a turn);
  - kills reach both and not a bystander, who does see the shooter's tracers;
  - a shooter 200 m away (mid tier) joins its target's near tier on hitting, and drops back 5 s after.

**Distant fights** (`activity.rs`; format in `game/src/activity.rs`). Exact tracers only come from near-tier shooters, so without this, a fight beyond ~150 m (or beyond the 100 near candidates in a crowd) can't be seen.
- **Server:** each shot is counted into its 128 m cell per faction over a 15-tick window, as one 5 B entry (shot count, mean aim, mean origin to 8 m), encoded once for every recipient. A client gets the finished window's cells within its far radius once per window, on its own tick, after the far tier and inside its byte budget (nearest kept if cut: `activity_cut`).
- **Client:** it subtracts the shots it drew exactly (near tracers and its own) and spreads the rest as ambient shots over the next window, capped at 600/s. Each comes from a drawn player of that faction in that cell, or from near the cell's mean origin.
- **Swarm test:** watchers at 400 m and 1 km draw all 360 shots, from the shooters, aimed right. The near watcher has exact tracers and nothing doubled, and 2 km is past the radius.

| WSL run (30 s) | activity bytes per client-tick | entries sent per client | tracked bots: exact tracers vs ambient shots |
|---|---|---|---|
| 1k blob, 30% fighting | 5.6 | ~1,000 | 4.2 M vs 8.8 M (most at the 600/s cap) |
| uniform 5k, 20% firing | 42 | ~7,400 | 0.2 M vs 13 M |

Ticks were unchanged in the blob (p50 8.6 ms). Nothing was cut by the budget in either run.

Worst case (every cell active): a 3.1 µs gather per client per window, and a 0.25 ms serial finish per window (`gather_cost`, ignored).

**Cone of fire and aiming down sights** (PlanetSide 2's model; numbers and the client's side in client/README, The gun):
- **Server-side spread:** every shot leaves within its cone of fire, picked by the server from the shooter's id and the shot's seq (`weapon::spread`).
- **Bloom:** the server keeps each player's bloom (`InputQueue::bloom`). The cone comes from the shooter's state before the shot's step and the input's `BUTTON_ADS`.
- **The client agrees:** it computes the same direction, so its tracers are the real shots. Others' tracers carry the spread aim.
- **`SimConfig::cone_of_fire` / `lattice-server --no-cone`:** turns the cone off. The lag-compensation tests (`what_you_see_is_what_you_hit`, the fairness test, walls, headshots) run without it, since they measure geometry.
- **Fighting bots aim down sights while they have a target:** they move at half speed. `FightConfig::ads: false` turns it off, as the fairness test does: slower targets hide what lag costs.
- **Old baselines:** fight baselines before this (`baselines/2026-10-05-wsl2-fight`) had no cone and no sights.

**M3d.4: bots that fight, in latency classes** (`baselines/2026-10-05-wsl2-fight`).
- **Fighting bots** (`BotBrain::set_fight`; `lattice-bots --fight-every N --aim-error MRAD`) take the nearest enemy they draw within 150 m and in sight (`hit::line_clear`, the same test as the server's after-cover count). They aim with lead (`bot::aim_at`) plus Gaussian error, firing in bursts.
- **Latency classes:** `lattice-bots --classes K` binds class k's sockets from port 16384 × (k + 1), so one `tc` u32 match (mask 0xc000) catches a class. `scripts/baseline.sh fight` gives each class its own netem band: 10 / 50 / 75 ms one-way, i.e. 20 / 100 / 150 ms RTT. The server listens below the ranges (port 14500). With `BOTS_SSH`, the bot box is shaped: egress by source port, ingress through ifb by destination port.
- **`lattice-server --immortal`:** hits land and are confirmed but deal no damage, so hit rates measure aim and lag compensation rather than who died first.

| run | 20 ms RTT | 100 ms | 150 ms |
|---|---|---|---|
| 1k blob, immortal | 92.8% | 93.3% | 84.5% |
| 1k blob, lethal | 23.8% | 21.1% | 18.7% |
| uniform 5k, lethal (WSL overloaded: tick p50 39 ms) | 26.3% | 25.6% | 24.1% |

- **Within the rewind cap, players hit alike; past it, less.** In a crowd the near delay sits at 133 ms, which puts 150 ms RTT past the 300 ms near cap. The swarm test (`latency_classes_hit_alike_within_the_cap`, deterministic) agrees: 33 / 100 / 300 ms RTT hit 90.9 / 92.4 / 54.8%.
- **Lethal fights cost latency more,** because shots go into the already dead (13k "too late" hits in the blob). That's information arriving later, not lag compensation.
- **The lethal WSL runs had corrections** (144 in the blob). They all fall in two report windows with server tick spikes (41 and 50 ms) on the shared box; the immortal blob and a lethal run without netem had none.

### Nearest-player search: `Grid::knn` (2026-10-04)

Following `reports/Nearest player search algorithms.md`, the near and mid tiers' k-nearest search is now `Grid::knn`, in `grid.rs`.

- **Exact:** it gives the same k nearest as brute force, with ties broken by id. Property tests check it against a brute-force oracle on a pile on a cell corner, a 200 m disk, blob + uniform, uniform, world corners, coincident stacks, filters, and k of 0, 1, 100 and above the population.
- **Packed keys and a running threshold.** Candidates are `u64` keys (`d² bits << 32 | id`). Only keys below the current k-th are kept, and the buffer is cut back to k once it doubles, so most candidates cost one compare. Squadmates are appended after the selection.
- **Positions inline in the grid.** `xs` and `ys` sit next to `items`, so the distance loop never reads the entity arrays.
- **Sub-cells only where it's dense.** A cell holding more than 256 items is counting-sorted within its own slice into sub-cells of ~32 items (0.5 m minimum side). Every other reader of the grid is unaffected, so it stays the one shared index.
- **A pruned walk.** Rings as before, but any cell or sub-cell whose box is farther than the current k-th is skipped, and a dense cell's sub-cells are walked nearest first. The walk stops when the k-th is closer than the exact edge of the searched box. An empty stretch of a ring's row costs one compare.

**Candidates scanned per query and time, single-threaded on WSL** (`cargo run --release -p lattice-sim --example knn_bench`, 10k players, k = 100, 150 m):

| crowd | before | after |
|---|---|---|
| 25 m pile | 10,000 candidates, 68–86 µs | **492 candidates, 2.1 µs (30–40×)** |
| 200 m disk | 645, 4.3–5.5 µs | 510, 2.6 µs (1.7–2.1×) |
| uniform | 26, 0.62 µs | 14, 0.67 µs (7% slower: per-cell overhead on sparse cells) |

**The whole server on WSL** (8 threads, sharing the box with the bots):
- **Pile ramp to 6,000, ladder off.** Assembly used to grow quadratically (4.5 / 25.8 / 58.6 / 75.6 ms at 1.5k / 3.5k / 5.5k / 6k). Now it grows linearly, ~2.8 ms per 1,000 players: 2.9 / 7.9 / 14.1 / 17.0 ms. The tick crosses 33 ms at ~5,500 players instead of ~2,800.
- **Blob 3k:** assembly 9.0 → 5.9 ms, tick p99 22.7 → 19.2 ms.
- **Uniform 5k:** assembly 4.00 → 4.29 ms, tick p99 unchanged.

The server summary now reports `near_scanned_per_query` and `mid_scanned_per_query`. Not yet measured on bare metal. Still open from the report, if a profile asks for them: a temporal seed for the threshold, per-leaf shared candidate sets, and an approximate cap for coincident stacks.

### Network conditions: netem on WSL (`baselines/2026-10-04-wsl2-netem`)

`scripts/baseline.sh netem <name>` runs uniform 1k and a 1k blob under six links, 60 s each.
- **Locally it needs no root.** It reruns itself in a private network namespace (`unshare -rn`) and puts netem on that namespace's loopback, which delays each direction once. Nothing outside the namespace is shaped.
- **netem's queue limit is raised.** The default 1,000 packets would drop on its own at these rates.
- **With `BOTS_SSH`,** only the bot machine is shaped (`sudo tc`): netem on its egress delays bots → server, and its ingress is redirected through an `ifb` device with the same netem for server → bots. Never shape the server: at 10k a netem queue on its own interface throttled its sends (see Limits above).

This measures behavior, not capacity, so WSL is fine for it. The blob rows match the uniform ones. Times in ms.

| link (one way) | input → applied p50 / p99 | round trip p50 / p99 | stand-ins (share of input ticks) | corrections per bot-minute, largest |
|---|---|---|---|---|
| clean | 52 / 69 | 67 / 100 | 0 | 0 |
| LAN: 15 ± 2 | 68 / 85 | 100 / 133 | 0 | 0 |
| typical: 40 ± 5, 0.5% loss | 93 / 117 | 167 / 167 | 0.025% | 0.014, 0.11 m |
| far: 75 ± 10, 1% loss | 129 / 166 | 233 / 267 | 0.10% | 0.04, 0.13 m |
| lossy: 40 ± 5, 5% loss | 96 / 138 | 167 / 200 | 0.34% | 0.20, 0.53 m |
| jittery: 40 ± 20 (σ) | 92 / 144 | 167 / 200 | 0.32% | 0.18, 0.24 m |

1. **Nothing breaks.** There were no near-delta decode errors, resyncs or discarded inputs on any link, including 5% loss and ±20 ms jitter (which reorders packets). A player sees a prediction correction at most every ~5 minutes, of at most half a meter.
2. **Input → applied is one-way delay plus ~50 ms.** That ~50 ms is the spare: the server-side wait, about 1.5 ticks, constant across links. It's the controllable part of how late others see you. The tuning options CLAUDE.md lists (process inputs faster than the tick, a fractional spare) act on exactly it.
3. **Loss and jitter turn inputs late, not lost.** Inputs go out three times, so almost every stand-in is an input that arrived after its tick. With 5% loss, a lost packet's inputs come one packet (33 ms) later. With ±20 ms jitter, the tail outruns the spare. Either way it's ~0.33% of input ticks.
4. **Under jitter the input clock hunts.** At ±20 ms it sends ~13 extra inputs and skips ~13 ticks per bot-minute (about 2 of each on a clean link), with 300 backlog skips. A spare sized to each client's measured jitter (a jitter buffer) would settle it, and cut stand-ins on bad links while keeping the spare small on good ones.

### M3e on AWS EC2, 2026-10-07 (`baselines/2026-10-07-aws-*`)

Scaleway was blocked, so M3e ran on AWS:
- **Server:** c7a.16xlarge (EPYC 9R14, 64 cores without SMT, 25 Gbps). It's a VM, so tick times include virtualization.
- **Bots:** m6in.8xlarge (16 cores / 32 threads, 50 Gbps), with 28 bot threads (`BOT_THREADS=28`).
- **Link:** one cluster placement group; `iperf3` measured 24.8 Gbps.
- **Session:** about 2 h, ~$12.

**M3's pass bars:**

| bar | result |
|---|---|
| 10k at level 0 with ~20% firing, tick p99 under 25 ms | **met:** p99 24.1 ms (p50 21.1); a rerun gave 23.2 ms |
| a 3k blob all fighting, p99 under 25 ms | **met:** 20.5 ms (p50 17.6) |
| 20 and 100 ms RTT hit alike, 150 ms measurably less (1k blob, immortal) | **met:** 99.3 / 99.2 / 96.9% |
| corrections only from pushes and respawns | **met** in the steady state; the 10k runs' 30–90 corrections all come in the join burst (first 5 s) |
| honest shooters never trimmed | **mostly met, after a client fix** (below): 107 / 334 / 45 trims per fight run fell to 1 / 0 / 0; 62 of 724k shots remain at 10k with firing, each at most 0.4 steps over |

**Receiving at 10k** (uniform, no firing):

| | 1 socket | 4 | 8 | 16 | 1 socket, `recvfrom` |
|---|---|---|---|---|---|
| tick p50 / p99 (ms) | 16.98 / 18.42 | **15.87 / 17.39** | 16.20 / 17.66 | 16.43 / 18.01 | 18.45 / 20.04 |
| receive thread busy | 31% | 8.2% | 4.8% | 3.0% | 59% |

**The `full` matrix against Scaleway** (tick p50 / p99 in ms; two runs each, agreeing within ~0.5 ms). The code changed in between (all of M3, `Grid::knn`, `recvmmsg`), so this mixes VM cost with code changes:

| run | AWS, 64 cores (VM) | Scaleway 2026-10-04, 128 threads |
|---|---|---|
| uniform 1k | 2.75–2.77 / 3.0–3.2 | 2.9 / 3.3 |
| uniform 5k | 7.9–8.0 / 8.8–9.1 | 7.0–7.1 / 8.0–8.1 |
| uniform 10k | 17.2–17.6 / 19.1–20.2 | 14.1 / 15.4–15.5 |
| uniform 10k, ladder off | 17.0–17.3 / 18.8–18.9 | 13.9 / 15.4–15.6 |
| hotspots 5k | 9.4–9.5 / 10.3–10.4 | 7.8 / 8.9–9.1 |
| blob 3k, `sendmmsg` | 7.1 / 7.8–7.9 | 7.7 / 8.5–8.6 |
| blob 3k, GSO | 7.3–7.6 / 8.3–8.9 | 8.2–8.3 / 9.0–9.4 |
| joins 5k + 500 | 8.4–8.5 / 9.4–9.5 | 7.3–7.5 / 8.3 |

**Limits** (`limits`):

| ramp | AWS | Scaleway 2026-10-04 |
|---|---|---|
| everyone in a 25 m disk, ladder off | **within budget to 10k** (p99 25.2 ms) | over at 7,217 (before `Grid::knn`) |
| everyone in a 200 m disk, ladder off | within budget to 10k (p99 23.8 ms) | within budget (p99 19.3 ms) |
| uniform, ladder off | over at 15,961 | over at 17,954 |

**netem at 10k** (`netem`, `NETEM_BOTS=10000`; the first valid run at this load). The tick holds at 16.7–17.4 / 18.5–19.3 ms on every profile:

| profile | late inputs (60 s) | corrections per bot-minute (uniform / blob) |
|---|---|---|
| clean | 0 | 0.009 / 0.004 (join burst) |
| lan | 1,145 / 130 | 0.13 / 0.30 |
| typical | 9,682 / 11,751 | 0.32 / 0.59 |
| far | 29,659 / 34,262 | 0.66 / 1.08 |
| lossy | 70,739 / 64,607 | 0.79 / 0.62 |
| jittery | 71,082 / 63,794 | 0.49 / 0.43 |

Late inputs match WSL's 1k netem runs (0.39% under lossy and jittery against 0.33%). Corrections run higher than WSL's ≤0.2 per bot-minute.

**Findings:**

1. **The bot box must be network-optimized.**
   - **First choice, failed:** the first bot box, a c7a.8xlarge (12.5 Gbps), queued or dropped 34% of the server's packets at its inbound bandwidth allowance and hit its packets-per-second allowance 18M times (ENA's `bw_in_allowance_exceeded`, `pps_allowance_exceeded`). RTT read ~20 ms inside the placement group, inputs ran late, and bots saw corrections.
   - **What worked:** an m6in.8xlarge (50 Gbps) showed no allowance hits. c6in.8xlarge had no capacity in eu-central-1a; `aws-up.sh --resume` now launches a missing bot box of another type next to the server.
   - **Why it matters:** the server sends each tick in a ~4 ms burst (~27 Gbps, ~4 Mpps for that moment), and that burst is what counts against an allowance.
2. **The server's own allowance queues its bursts too:** 11–14% of its packets at 10k (`bw_out_allowance_exceeded` on its 25 Gbps). The delay is well under a millisecond here, but a real AWS deployment at more load would want paced egress or a bigger instance.
3. **How many bots a thread carries depends on the tail of a bot's tick, not its mean.**
   - **16 threads:** with 625 bots each (78 per slot), slot work was p50 0.7 ms but p99 7.0 ms, longer than the 4.2 ms slot. Lateness reached p99 31.5 ms (a tick), so inputs missed their steps: 340k late inputs in 40 s and 3 corrections per bot-minute, with the box 79% idle.
   - **28 threads:** slot work p99 3.7 ms, 0 late inputs.
   - **Rule of thumb:** keep under ~360 bots per thread on these VMs. The 20k `limits` ramp is over that, so only its server numbers count.
   - `lattice-bots` now reports `slot_work` and `slot_late`. The 10× tail itself isn't explained yet.
4. **Honest trims were a client bug.**
   - **The diagnostic:** every trim in the first fight runs was a mid/far claim, 0.3 steps past the bound at the median and 1.0 at most (`trim_excess_*`, `rewinds_trimmed_mid` in the server summary).
   - **The bug:** when the adaptive near delay changed, the near render clock slewed to it but the mid lag jumped at once. So mid and far were drawn, and claimed, up to the whole change past their 200 ms.
   - **The fix:** the mid lag now slews with the near clock.
   - **Still open:** the 62 remaining trims at 10k (each ≤ 0.4 steps over) aren't explained.
5. **RTT reads 15–20 ms too high at 10k.** The server stamps its sends (and the hold it reports in `ack_delay`) with the tick's start, but sends 15–20 ms later. That's cosmetic for the input clock, which steers on queue depth. It does make the server's backtrack bound that much more lenient. Fix candidate: stamp with the transport phase's time.
6. **GSO is slower on ENA:** there's no UDP segmentation offload (`USO=sw`), so egress is 1.80 vs 1.44 ms p50 for `sendmmsg` in the blob. Keep `sendmmsg` on AWS.
7. **The serial events phase is the floor at 10k:** 4.1 ms p50 (2.4 ms on 2026-10-04's code), next to egress 4.1 and assembly 3.3. Rayon's idle spinning is ~22% of the CPU in profiles, as on Scaleway.

### Running it on bare metal (the desktop, dual-booted)

The repo has no remote, so carry it over as a git bundle.

1. **In WSL**, bundle the repo onto the Windows drive: `git bundle create /mnt/c/lattice-net.bundle master`.
2. **Boot Linux** (Ubuntu 24.04 is what WSL runs, so the same kernel family and toolchain). Install the tools: `sudo apt install build-essential git linux-tools-common linux-tools-$(uname -r)`, then rustup (`curl https://sh.rustup.rs -sSf | sh`).
3. **Clone from the Windows drive:** open it once in the Files app so it mounts, then `git clone /media/$USER/<drive>/lattice-net.bundle ~/lattice-net`.
4. **Run `scripts/preflight.sh`** and apply its fixes. They're temporary and gone after a reboot. On a stock install that's typically:
   ```
   sudo sysctl -w net.core.rmem_max=16777216 net.core.wmem_max=16777216
   sudo sysctl -w kernel.perf_event_paranoid=1 kernel.kptr_restrict=0
   echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor
   ```
5. **Run `PROFILE=1 scripts/baseline.sh full desktop-baremetal`**, from a terminal with nothing else running. Close the browser.
6. **Bring the results back:** commit them, then `git bundle create /media/$USER/<drive>/baseline.bundle master`. Back in WSL, run `git pull /mnt/c/baseline.bundle master`.
   - If the Windows drive mounts read-only, Windows is hibernated ("Fast Startup"). Use a USB stick, or turn Fast Startup off in Windows' power settings.

The WSL reference to compare against is `baselines/2026-09-29-wsl2`: the same commit, script and matrix.

### Two machines (Scaleway Elastic Metal)

On one box, the bots compete with the server for the same cores, and loopback isn't a NIC. So a single-box run can't settle whether 10k needs level 6, or what a real NIC does with GSO. The two-machine rig:

| role | Scaleway model | CPU | price |
|---|---|---|---|
| server | EM-I620E-NVMe | EPYC 8534P, 64 cores / 128 threads, Zen 4c | €1.863/h |
| bots | EM-I320E-NVMe | EPYC 8224P, 24 cores / 48 threads | €0.959/h |

Both are hourly bare metal in one zone, joined by a Private Network (25 Gbps per the API).

```
scripts/cloud-up.sh                                  # rents both, sets them up, measures the link
scripts/cloud-run.sh full scaleway-8t SERVER_THREADS=8    # the matrix with the dev box's thread count
scripts/cloud-run.sh full scaleway-64t PROFILE=1          # the server on every core
scripts/cloud-down.sh                                # copies baselines back, deletes both, shows the list
```

- **Billing runs from creation until `cloud-down.sh` deletes them.** Stopping a server doesn't stop the bill.
- **`cloud-up.sh`:**
  - checks that both offers are hourly and in stock, and asks before creating;
  - saves the server ids as soon as they exist, so `cloud-down.sh` can always clean up;
  - after the OS install (10–20 min), configures the Private Network's VLAN inside Linux and installs the toolchain;
  - copies this working tree over, builds, and lets the server box ssh to the bot box;
  - measures the link with `iperf3` (warns below 5 Gbps), generates a session token key, and preflights both machines.
- **`cloud-run.sh`** syncs the working tree, rebuilds, and runs `baseline.sh` on the server box:
  - the server listens on its Private Network address only;
  - the bots run on the other box over ssh;
  - both use the session key, never the public dev key.
- **`baseline.sh` two-machine mode** works on any pair: `BOTS_SSH=user@host SERVER_IP=<address> TOKEN_KEY=<hex>`. The server then takes every core and the bots all but 4 of their threads (`BOT_THREADS` overrides it). The bot machine's preflight goes into `env.txt` too.
- **What to read in the results:**
  - the 8-thread run against the WSL reference: was the blob's p99 WSL, or the code?
  - 8 against 64 threads: does the tick scale with cores, and does 10k fit at level 0?
  - the blob with GSO on a real NIC;
  - `swarm busy` above ~70% means the bot box is the limit.
- **Per-core speed:** the Zen 4c parts run at 3.0–3.1 GHz against 4.1 GHz on the dev box, so expect slower per-core numbers.

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

## M2b: near-tier deltas on the WSL2 dev box

**How it works** (`delta.rs`):
- **History.** Every entity's quantized near state (~8 mm position, velocity, yaw) is kept for 32 ticks. The history is tick-major and shared by all clients, since the state is serialize-once.
- **Tags.** Each client-tick's near message is sent with `send_tagged(.., tag = tick)`. The client-slot remembers, for 16 ticks, which entities each message carried. When the transport reports a tag acked, those entities' baselines advance.
- **Encoding.** Each near entity goes as a delta against its newest acked baseline (≤31 ticks back, and from this entity's lifetime, not a previous occupant's of the slot), or in full if there is none.
    - Deltas are variable-length bit codes: id gaps, zigzag position and velocity deltas, and yaw and the rest as "changed" bits.
    - A moving entity costs ~5 B, against 15 B for a full near blob.
- **Decoding.** The client keeps the same 32-tick history per near entity. Baselines are only ever ones it acked, so a delta never refers to a state it lacks.

**Results:**

| scenario | bytes per client-tick | near bytes | deltas | packets/s out | assembly p50 |
|---|---|---|---|---|---|
| blob 3,000 | 1,342 B (was 1,948) | 357 B (was ~967, −63%) | 99% | 180k (unchanged: 2 per client-tick) | 10.1 ms (was ~7.5) |
| hotspots 5,000 | 759 B (was 1,087) | 194 B | 100% | 190k (was 241k, −21%) | 8.5 ms (was 5.5) |
| uniform 10,000 (level 6, 20 Hz) | 331 B (was 477) | 96 B | 100% | 200k (unchanged: 1 per client-tick) | 10.0 ms (was 6.2) |

- **The pass bar (near bytes at least halved) is met.** The blob stays at two packets, as decided: its mid tier alone is ~950 B.
- **Correctness.** Decoded near states match the server's exactly in in-process tests, including under 30% loss both ways, and tracked bots over UDP saw zero decode errors.
- **The cost is ~3.5 µs of assembly per client per tick.**
    - About 1 µs is ack bookkeeping: `take_acked`, the sent ring, baseline updates.
    - The rest is encoding (~30 ns per entity for the codec itself), plus building the entries and a message per client.
    - At 10k that's ~4 ms. The level-6 tick still fits (p50 33 ms of its 50 ms period).
- **Candidates if it matters:** writing the bits directly into the message buffer, and a flat per-shard index for acks.

## Encryption on the WSL2 dev box

Every packet after the handshake is sealed with ChaCha20-Poly1305 (`ring`), and clients connect with tokens. The bots mint their own tokens with `--token-key` (default: the public dev key, and the server warns about it), standing in for a login service. `lattice-server` takes the same `--token-key` and `--server-id`.

**Before and after** (GSO egress in the blob; "before" is `4ad39ca`, with the CRC):

| scenario | build | tick p50 / p99 | ingress p50 | transport p50 | ladder |
|---|---|---|---|---|---|
| blob 3,000 | before (2 runs) | 20.1–20.5 / 23.1–24.4 ms | 1.0–1.1 ms | 3.4–3.5 ms | L0 (one run fell to L1 after a 46 ms hitch) |
| blob 3,000 | `ring` | 20.7 / 24.5 ms | 1.28 ms | 3.0 ms | L0 |
| uniform 10,000 | before (2 runs) | 33.2–33.3 / 37.2–39.5 ms | 3.9–4.0 ms | 6.1 ms | L6 |
| uniform 10,000 | RustCrypto (2 runs) | 34.0–34.7 / 39.9–43.4 ms | 4.6–4.7 ms | 6.2–6.3 ms | mostly **L7** |
| uniform 10,000 | `ring` (3 runs) | 33.9–34.0 / 39.2–39.7 ms | 4.4–4.6 ms | 6.0–6.2 ms | L6 |

- **Sealing big packets is cheaper than the CRC was,** so transport time falls. Opening the small input packets costs more, so ingress rises ~0.5 ms at 10k.
- **RustCrypto's AEAD cost 10k a ladder level.** With it, 10k spent most of the run at dilation 0.9. `ring` keeps level 6, so it's the one used.
- **Bytes:** +8 B per packet (27 B overhead, was 19). That's +1.1% down in the blob, and ~+12% up, since inputs are small packets.
- **Candidate if ingress matters:** the receive path zeroes a 1,200 B stack buffer per packet before opening into it.

## GSO on the WSL2 dev box

`UDP_SEGMENT` hands the kernel one buffer per destination, which it cuts into datagrams (on a real NIC, in hardware). Every segment but the last must be the same size, so:

- **The sim fills packets.** Mid and far messages are lists of 11 B blobs, so they can be split at any entity. `PacketFill` (`msg.rs`) predicts how the transport packs a client's messages, and sizes each tier's first message to fill what's left of the current packet. The byte budget uses the same model. Every packet but a client's last leaves nearly full.
- **The transport pads the rest** (`Config::pad_packets`, protocol `LATTICE2`), only with `--egress gso`.
- **Egress groups each client's datagrams into one `sendmmsg` entry** with a `UDP_SEGMENT` cmsg. A run is cut wherever the equal-size rule breaks, and at the kernel's 64-segment limit, so egress stays correct even without padding.

Without the filling, padding is 39.5% of bytes in the swarm test below. In the blob, a client's first packet carries the snapshot and the near message (~400 B), and the ~950 B mid message starts the second. With it, padding is 0.6% of bytes in that test (150 bots, several packets each), and 0.3% over UDP in the blob.

**Results** (blob 3,000, two runs of each, in alternating order):

| egress | datagrams per client-tick | sends per client-tick | egress p50 / p99 (ms) | tick p99 (ms) | bytes per snapshot |
|---|---|---|---|---|---|
| `sendmmsg` | 2.00 | 2.00 | 6.2–6.7 / 7.5–7.8 | 26.1–26.4 | 1,400 B |
| `gso` | 2.00 | 1.00 | 4.5–4.9 / 5.7–6.0 | 24.5–24.6 | 1,405 B |

- **Egress falls ~25%** for 0.3% more bytes, with zero send and decode errors. The syscall count doesn't change: one `sendmmsg` per shard per tick either way. What GSO saves is the per-datagram trip through the stack.
- **The blob still needs 2 packets** (1,400 B of messages against 1,181 B per packet). Splitting costs ~0.7% of bytes (one more message header per client-tick) and saves no packet here. It can save one where whole-message packing would spill into a third.
- **Clients with one packet per tick gain nothing.** That's most of hotspots and all of 10k at level 6.
- **The real verdict needs bare metal.** On a NIC with segmentation offload, the kernel does the cutting once per client instead of once per datagram.

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
