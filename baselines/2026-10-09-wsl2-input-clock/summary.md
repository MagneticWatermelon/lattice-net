# Baseline: wsl2-input-clock, 2026-10-09

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** b28543d (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 8 threads (kept awake through ticks: on; sending during assembly: on; worker CPUs -, receive CPUs -), bots 8 threads, on the same machine over loopback. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1709) | 6.33 / 7.19 / 14.38 | 0 | 61 | 2.02 | 1639 | 393.6 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 6.40 / 7.21 / 30.01 | 0 | 60 | 2.00 | 1540 | 369.7 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L6 (20 Hz; L2:18,L3:178,L4:31,L5:573,L6:605) | 25.79 / 37.28 / 79.25 | 18 | 123 | 1.00 | 294 | 58.0 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.70 / 0.99 | 0.00 / 0.00 | 0.25 / 0.32 | 0.16 / 0.28 | 0.04 / 0.06 | 0.07 / 0.10 | 4.45 / 5.11 | 0.00 / 0.00 | 0.03 / 0.07 |
| fight-blob-1k #1 | 0.71 / 0.99 | 0.00 / 0.00 | 0.25 / 0.32 | 0.23 / 0.28 | 0.04 / 0.06 | 0.07 / 0.09 | 4.50 / 5.13 | 0.00 / 0.00 | 0.03 / 0.06 |
| fight-uniform-5k #1 | 2.78 / 4.11 | 0.00 / 0.00 | 0.58 / 0.85 | 0.37 / 0.44 | 0.04 / 0.08 | 0.05 / 0.12 | 20.38 / 30.79 | 0.00 / 0.00 | 0.04 / 0.10 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread. Sending during assembly (`send_during_assembly=on`), each shard's assembly task also frames and sends: assembly's columns cover all three, and transport and egress show only their (near-zero) wall time.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 8 | 6.33 | 0.70 / 0.12 / 0.57 | 4.45 / 0.83 / 4.04 | 0.00 | 0.03 | 0.54 | 0.16 |
| fight-blob-1k #1 | 8 | 6.40 | 0.71 / 0.12 / 0.56 | 4.50 / 0.86 / 4.11 | 0.00 | 0.03 | 0.54 | 0.23 |
| fight-uniform-5k #1 | 8 | 25.79 | 2.78 / 0.43 / 2.48 | 20.38 / 3.24 / 19.50 | 0.00 | 0.04 | 1.18 | 0.38 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | recvmmsg (1000 us) | 1 | 30 | 29.82 | 4.4% / 4.4% |
| fight-blob-1k #1 | recvmmsg (1000 us) | 1 | 30 | 29.72 | 4.4% / 4.4% |
| fight-uniform-5k #1 | recvmmsg (1000 us) | 1 | 149 | 53.57 | 16.7% / 16.7% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 653 | 100.0 / 141.0 | 50.2 | 0 / 0 | 0 / 0 | 0 | 27150 | 0 | 37% |
| fight-blob-1k #1 | 1000 / 1000 | 650 | 100.0 / 142.0 | 50.4 | 704 / 246 | 950 / 0 | 221 | 37079 | 0 | 42% |
| fight-uniform-5k #1 | 5000 / 5000 | 1001 | 101.0 / 155.0 | 51.3 | 3458 / 0 | 3135 / 0 | 115 | 1089 | 0 | 57% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.79 / 0.03 / 0.00 | 96.93 / 0.93 / 1.26 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 86.4 | 0 | 242 / 342 | 367 / 433 |
| fight-blob-1k #1 | 96.83 / 1.22 / 1.21 | 90.87 / 3.21 / 3.49 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 91.9 | 0 | 242 / 340 | 367 / 433 |
| fight-uniform-5k #1 | 99.53 / 0.24 / 0.09 | 89.62 / 9.30 / 0.33 | 19.29 / 71.31 / 7.58 | 0 / 580 / 14263 | 95.7 | 0 | 234 / 333 | 367 / 436 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 21.3 | 65589 | 65149 | 99.3 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 101.3 | 64646 | 64173 | 99.3 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 151.3 | 64999 | 63237 | 97.3 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 21.5 | 31425 | 7006 | 22.3 | 1331 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 101.6 | 33669 | 6689 | 19.9 | 1383 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 151.5 | 32047 | 5909 | 18.4 | 1213 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 22.7 | 37405 | 6616 | 17.7 | 1295 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 102.7 | 38367 | 7009 | 18.3 | 1346 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 152.8 | 37483 | 6304 | 16.8 | 1221 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 275465 | 1555 / 199107 | 173 | 0 | 182986 | 0 | 0.30 / 0.53 | 6.33 / 7.19 | 0 |
| fight-blob-1k #1 | 147883 | 1991 / 28422 | 929 | 10244 | 98436 | 4029 | 0.30 / 0.50 | 6.40 / 7.21 | 221 |
| fight-uniform-5k #1 | 585135 | 1284 / 21720 | 462 | 2328 | 380613 | 3912 | 1.11 / 2.03 | 25.79 / 37.28 | 115 |
