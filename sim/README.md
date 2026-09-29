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
| serialize | each entity once into an 11-byte far-tier blob | rayon |
| assembly | per client: K nearest via the grid, memcpy blobs into a snapshot | per shard |
| transport | each shard's `send` + `flush`: framing, acks, CRC | per shard |
| egress | `send_to` per packet (in the binary) | per shard |

The transport is 64 `lattice_net::Shard`s by default (`--shards`). The binary's receive thread buckets each datagram by `Router::shard`, so routing costs no tick time.

Shots and event application (phases 3–4) arrive with M3.

**Interest stand-in:** M2 owns tiers and the priority accumulator. Until then every client gets its own full-precision state plus the 64 nearest entities within 150 m, every tick (`--near-max`, `--near-radius`).

**Bots (`bot.rs`, `lattice-bots`):** each bot runs a real `lattice_net::Client` with its own UDP socket, sharded across ≤8 threads that each tick their bots at 30 Hz. A bot wanders around the anchor the server's Welcome gives it, sends each input 3× redundantly, predicts with the same `movement::step`, and reconciles against the acked input.

Movement is deterministic f32 code shared by both sides, so prediction matches the server **bit-for-bit**. A correction only happens when the server had to stand in for an input it hadn't received.

**Input policy.** Every tick consumes exactly one input seq, so each server step matches exactly one client step:
- If the next input is missing, a stand-in takes its seq: the last input for 2 ticks (`GRACE_TICKS`), then a frozen input (no movement, facing kept).
- The real input is dropped if it shows up later (`late_inputs`). Holding packets back (a lag switch) therefore buys no movement.
- There's no 2-per-tick catch-up. Instead, each snapshot carries the server's queue depth for that client (`buffered`), and the bot nudges its input clock by up to ±5% to keep the input due now plus 1–2 spare queued. Inside that band the clock is left alone.
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

The server prints a line every `--report` seconds (with `--csv`, one row per window). At exit it prints p50/p99/max per phase over every tick after warmup. It also reports the system-wide kernel `RcvbufErrors`/`SndbufErrors` deltas from `/proc/net/snmp`.

The bots report snapshots, entities per snapshot, corrections, kbps per bot, RTT, join latency (connect → Welcome), and **swarm overruns**. An overrun means the bots were late, so treat any corrections in that window as swarm artifacts.

## Baseline: WSL2 dev box (behavior, not capacity)

Test box: 16 cores, WSL2 on Windows 10, with the server (8 rayon threads, 64 shards) and the bots (8 threads) on the same machine over loopback. Only bare-metal numbers count; these runs show where the time goes. Phase columns are p50 in ms; "before" is the single-shard server.

| scenario | clients | tick p50 / p99 (ms) | before | ingress | events | assembly | transport | egress | stand-ins after warmup | down kbps/client |
|---|---|---|---|---|---|---|---|---|---|---|
| uniform | 1,000 | 2.8 / 3.4 | 3.0 / 3.8 | 0.35 | 0.09 | 0.30 | 0.31 | 1.4 | 0 | ~15 |
| blob | 3,000 | 11.9 / **14.1** | 16.7 / 19.7 | 0.56 | 0.25 | 4.5 | 1.6 | 4.1 | 0 | ~180 |
| joins | 3,000 + 500 | 7.0 / 8.6 | 8.7 / 13.9 | 0.63 | 0.26 | 0.35 | 0.66 | 4.0 | 0 | ~21 |
| hotspots | 5,000 | 11.4 / 13.5 | 17.8 / 23.7 | 1.1 | 0.38 | 1.7 | 1.6 | 5.5 | 0 | ~98 |
| uniform | 5,000 | 10.0 / 12.1 | 14.3 / 17.6 | 1.2 | 0.39 | 0.44 | 1.1 | 5.8 | 0 | ~28 |
| uniform | 10,000 | 19.0 / 22.4, 1 overrun | 31.1 / 39.6, 180 overruns | 2.4 | 1.2 | 1.2 | 2.2 | 11.0 | 0 | ~45 |

Findings:

1. **The blob passes with room to spare** (p99 14.1 ms against the 25 ms bar), and **10k now fits the 33 ms tick**: 1 overrun, down from 180.
2. **Sharding removed the serial transport cost.** At 10k, ingress fell from 9.6 to 2.4 ms and transport from 9.1 to 2.2 ms. What's left serial is the events phase (1.2 ms at 10k), where inputs are pushed into the world's queues.
3. **Egress is now the biggest phase:** about 11 ms at 10k, one `send_to` per packet. That's transport step 5: `sendmmsg`/GSO, and `SO_REUSEPORT` sockets per group of shards.
4. **Assembly scales with density, as expected:** 4.5 ms for the blob (every client scans about 3,000 candidates) versus 1.2 ms for 10k uniform. This is where M2's tiers and budgets land.
5. **Steady state has no stand-ins and no corrections in any scenario.** All stand-ins happen during a thundering-herd join: 10,000 bots connecting within about 0.5 s pushed the tick to ~150 ms once. The summary's "after warmup" line separates that burst out. No jitter or loss is simulated over the sockets yet; run under `netem` for that.
6. **Joins don't spike the tick yet,** because a spawn here is just a Welcome. Zone-entry cost (the initial world state, which needs fragmentation) isn't modeled.
