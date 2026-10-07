# Baseline: aws-fight, 2026-10-07

- **Machine:** AMD EPYC 9R14, 64 (1 socket(s) x 64 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** af340e4 (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 28 threads, bots on a second machine (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), server listening on 172.31.17.93. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1708) | 7.21 / 7.96 / 8.71 | 0 | 60 | 2.01 | 1642 | 394.2 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 5.17 / 6.60 / 7.08 | 0 | 60 | 2.00 | 1540 | 369.7 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L0 (30 Hz; L0:1709) | 9.93 / 10.85 / 11.20 | 0 | 158 | 1.05 | 811 | 194.8 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.52 / 0.66 | 0.45 / 0.57 | 0.34 / 0.56 | 0.18 / 0.34 | 0.16 / 0.41 | 0.06 / 0.10 | 3.14 / 3.67 | 0.67 / 0.91 | 0.56 / 0.72 |
| fight-blob-1k #1 | 0.53 / 0.64 | 0.45 / 0.58 | 0.33 / 0.57 | 0.18 / 0.27 | 0.16 / 0.42 | 0.06 / 0.10 | 1.38 / 2.53 | 0.35 / 0.51 | 0.56 / 0.69 |
| fight-uniform-5k #1 | 0.80 / 1.07 | 2.09 / 2.64 | 0.63 / 0.89 | 0.35 / 0.47 | 0.35 / 0.64 | 0.21 / 0.28 | 1.37 / 1.91 | 0.92 / 1.24 | 1.34 / 1.65 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 64 | 7.21 | 0.52 / 0.07 / 0.05 | 3.14 / 2.82 / 1.86 | 0.67 / 0.56 / 0.35 | 0.56 / 0.48 / 0.21 | 0.96 | 0.64 |
| fight-blob-1k #1 | 64 | 5.17 | 0.53 / 0.08 / 0.04 | 1.38 / 1.05 / 0.54 | 0.35 / 0.18 / 0.11 | 0.56 / 0.49 / 0.21 | 1.02 | 0.64 |
| fight-uniform-5k #1 | 64 | 9.93 | 0.80 / 0.34 / 0.26 | 1.37 / 0.81 / 0.65 | 0.92 / 0.60 / 0.44 | 1.34 / 1.23 / 0.59 | 1.45 | 2.45 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | recvmmsg (1000 us) | 1 | 30 | 28.28 | 8.8% / 8.8% |
| fight-blob-1k #1 | recvmmsg (1000 us) | 1 | 30 | 27.96 | 5.3% / 5.3% |
| fight-uniform-5k #1 | recvmmsg (1000 us) | 1 | 150 | 49.94 | 15.0% / 15.0% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 566 | 103.0 / 145.0 | 49.4 | 0 / 0 | 0 / 0 | 0 | 26906 | 0 | 11% |
| fight-blob-1k #1 | 1000 / 1000 | 649 | 103.0 / 143.0 | 50.1 | 0 / 0 | 0 / 0 | 0 | 36235 | 0 | 12% |
| fight-uniform-5k #1 | 5000 / 5000 | 1178 | 105.0 / 147.0 | 50.4 | 0 / 0 | 0 / 0 | 0 | 1183 | 0 | 15% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.74 / 0.08 / 0.00 | 96.95 / 0.93 / 1.24 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 89.0 | 0 | 242 / 342 | 367 / 433 |
| fight-blob-1k #1 | 96.85 / 1.22 / 1.18 | 90.98 / 3.19 / 3.44 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 90.8 | 0 | 242 / 342 | 367 / 433 |
| fight-uniform-5k #1 | 97.47 / 2.32 / 0.05 | 88.11 / 10.03 / 1.28 | 28.36 / 54.91 / 14.50 | 6 / 948 / 8772 | 122.5 | 0 | 233 / 367 | 367 / 433 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 27.2 | 66362 | 65901 | 99.3 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 107.1 | 65474 | 64958 | 99.2 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 157.2 | 65656 | 63621 | 96.9 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 25.0 | 31846 | 7063 | 22.2 | 1319 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 105.1 | 31909 | 6302 | 19.7 | 1300 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 155.1 | 31919 | 6049 | 19.0 | 1296 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 30.6 | 37728 | 7053 | 18.7 | 1357 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 110.5 | 39675 | 6903 | 17.4 | 1336 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 161.1 | 39231 | 6480 | 16.5 | 1253 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 280387 | 1627 / 201463 | 148 | 0 | 186264 | 0 | 1.06 / 1.36 | 7.21 / 7.96 | 0 |
| fight-blob-1k #1 | 148468 | 2070 / 28009 | 967 | 10086 | 98206 | 3995 | 1.10 / 1.45 | 5.17 / 6.60 | 0 |
| fight-uniform-5k #1 | 569594 | 1273 / 22194 | 448 | 2313 | 380333 | 3987 | 1.69 / 2.15 | 9.93 / 10.85 | 0 |
