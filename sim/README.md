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
| ingress | datagrams into `lattice_net::Server`, timeouts, spawn/despawn, queue inputs | no |
| movement | consume exactly one input seq per entity: the real input, or a stand-in if it hasn't arrived | rayon |
| grid | counting-sort rebuild of the one shared 32 m grid | no |
| history | positions into a 200 ms lag-comp ring (unused until M3) | no |
| serialize | each entity once into an 11-byte far-tier blob | rayon |
| assembly | per client: K nearest via the grid, memcpy blobs into a snapshot | rayon |
| transport | `Server::send` + `flush`: framing, acks, CRC | no |
| egress | `send_to` per packet (in the binary) | rayon |

Shots and event application (phases 3–4) arrive with M3.

**Interest stand-in:** M2 owns tiers and the priority accumulator. Until then every client gets its own full-precision state plus the 64 nearest entities within 150 m, every tick (`--near-max`, `--near-radius`).

**Bots (`bot.rs`, `lattice-bots`):** each bot runs a real `lattice_net::Client` with its own UDP socket, sharded across ≤8 threads that each tick their bots at 30 Hz. A bot wanders around the anchor the server's Welcome gives it, sends each input 3× redundantly, predicts with the same `movement::step`, and reconciles against the acked input.

Movement is deterministic f32 code shared by both sides, so prediction matches the server **bit-for-bit**. A correction only happens when the server had to stand in for an input it hadn't received.

**Input policy.** Every tick consumes exactly one input seq, so each server step matches exactly one client step:
- If the next input is missing, a stand-in takes its seq: the last input for 2 ticks (`GRACE_TICKS`), then a frozen input (no movement, facing kept).
- The real input is dropped if it shows up later (`late_inputs`). Holding packets back (a lag switch) therefore buys no movement.
- There's no 2-per-tick catch-up. Instead, each snapshot carries the server's queue depth for that client (`buffered`), and the bot nudges its input clock by up to ±5% to keep the input due now plus 1–2 spare queued. Inside that band the clock is left alone.
- A client that stalls past the server's seq jumps ahead to it (`resyncs`) instead of staying late forever.

With this policy, stand-ins exceed bot corrections in some runs (110 against 15 at uniform 5k). The likely cause, not yet confirmed, is stand-ins during swarm shutdown that no bot ever sees.

**Scenarios:**
- `uniform`: all bots spread over the 8×8 km continent.
- `hotspots`: the first 2,400 bots go to 3 hotspots of ~800, 150 m radius.
- `blob`: the first 3,000 bots go in a 200 m disk.
- `joins`: N uniform bots, plus 500 more joining at 50/s from t=15 s.

## Output

The server prints a line every `--report` seconds (with `--csv`, one row per window). At exit it prints p50/p99/max per phase over every tick after warmup. It also reports the system-wide kernel `RcvbufErrors`/`SndbufErrors` deltas from `/proc/net/snmp`.

The bots report snapshots, entities per snapshot, corrections, kbps per bot, RTT, join latency (connect → Welcome), and **swarm overruns**. An overrun means the bots were late, so treat any corrections in that window as swarm artifacts.

## Baseline: WSL2 dev box (behavior, not capacity)

The table below predates the input policy above: it used repeat-forever plus a 2-per-tick catch-up. Rerun under the new policy:

| scenario | stand-ins before → after | corrections before → after |
|---|---|---|
| blob 3,000 | 55 → 0 | 55 → 0 |
| uniform 5,000 | 418 → 110 | 273 → 15 |

Test box: 16 cores, WSL2 on Windows 10, with the server (8 rayon threads) and the bots (8 threads) on the same machine over loopback. Only bare-metal numbers count; these runs show where the time goes.

| scenario | clients | tick p50 / p99 (ms) | ingress | assembly | transport | egress | corrections | down kbps/client |
|---|---|---|---|---|---|---|---|---|
| uniform | 1,000 | 3.0 / 3.8 | 0.45 | 0.56 | 0.45 | 1.3 | 0 | ~14 |
| blob | 3,000 | 16.7 / **19.7** | 1.7 | 4.3 | 5.7 | 3.9 | 55 (= starved) | ~180 |
| joins | 3,000 + 500 | 8.7 / 13.9 | 1.9 | 0.6 | 2.2 | 2.6 | 0 | ~22 |
| hotspots | 5,000 | 17.8 / 23.7 | 3.5 | 1.8 | 6.1 | 5.4 | 58 (96 starved) | ~100 |
| uniform | 10,000 | 31.1 / 39.6, 180 overruns | 9.6 | 1.1 | 9.1 | 10.1 | 19,950 (= starved) | ~40 |

Phase columns are p50 in ms. Findings:

1. **The blob passes the bar on WSL** (p99 19.7 ms against 25 ms), though with less headroom than you'd want.
2. **The per-packet serial path dominates, not the game phases.** At 10k, movement, grid, serialize and assembly total about 2 ms. Ingress, transport and egress together take about 29 ms, which is roughly 1 µs per packet per phase on one thread (plus a syscall each for egress). That points at transport steps 4–6 in the root README: serialize-once fan-out, `sendmmsg`/GSO, and sharding `Connection`s across threads.
3. **Assembly scales with density, as expected:** 4.3 ms for the blob (every client scans about 3,000 candidates) versus 1.1 ms for 10k uniform. This is where M2's tiers and budgets land.
4. **Corrections come only from starvation, and starvation appeared only in runs where the server or the swarm overran.** Neither jitter nor loss is simulated yet; run under `netem` for that.
5. **Joins don't spike the tick yet,** because a spawn here is just a Welcome. Zone-entry cost (the initial world state, which needs fragmentation) isn't modeled.
