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
- **Input wait**: from an input's datagram arriving (stamped by the receive thread) to the tick that applies it.
- **The ladder**: level, tick rate, dilation and pace per window, the share of clients degraded by their own bandwidth ladder, and ticks spent at each level.

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
- **`baseline.sh` two-machine mode** works on any pair: `BOTS_SSH=user@host SERVER_IP=<address> TOKEN_KEY=<hex>`. The server then takes every core and the bots half of theirs. The bot machine's preflight goes into `env.txt` too.
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
