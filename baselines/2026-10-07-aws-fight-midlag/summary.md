# Baseline: aws-fight-midlag, 2026-10-07

- **Machine:** AMD EPYC 9R14, 64 (1 socket(s) x 64 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** af340e4 (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 28 threads, bots on a second machine (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), server listening on 172.31.17.93. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1708) | 7.32 / 8.03 / 8.31 | 0 | 60 | 2.01 | 1641 | 394.1 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 5.03 / 6.92 / 7.38 | 0 | 60 | 2.00 | 1539 | 369.6 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L0 (30 Hz; L0:1708) | 9.83 / 10.81 / 11.51 | 0 | 158 | 1.05 | 812 | 194.9 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.52 / 0.63 | 0.46 / 0.57 | 0.34 / 0.53 | 0.18 / 0.22 | 0.16 / 0.38 | 0.06 / 0.11 | 3.19 / 3.71 | 0.73 / 0.98 | 0.57 / 0.72 |
| fight-blob-1k #1 | 0.53 / 0.63 | 0.43 / 0.57 | 0.34 / 0.37 | 0.18 / 0.22 | 0.16 / 0.19 | 0.06 / 0.10 | 1.33 / 2.84 | 0.33 / 0.53 | 0.55 / 0.74 |
| fight-uniform-5k #1 | 0.80 / 1.07 | 2.06 / 2.65 | 0.62 / 0.78 | 0.35 / 0.41 | 0.34 / 0.64 | 0.21 / 0.26 | 1.34 / 1.84 | 0.92 / 1.22 | 1.30 / 1.60 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 64 | 7.32 | 0.52 / 0.08 / 0.05 | 3.19 / 2.82 / 1.81 | 0.73 / 0.56 / 0.33 | 0.57 / 0.49 / 0.21 | 1.06 | 0.65 |
| fight-blob-1k #1 | 64 | 5.03 | 0.53 / 0.07 / 0.04 | 1.33 / 0.99 / 0.52 | 0.33 / 0.18 / 0.11 | 0.55 / 0.48 / 0.21 | 1.02 | 0.62 |
| fight-uniform-5k #1 | 64 | 9.83 | 0.80 / 0.35 / 0.26 | 1.34 / 0.83 / 0.64 | 0.92 / 0.61 / 0.44 | 1.30 / 1.19 / 0.58 | 1.38 | 2.42 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | recvmmsg (1000 us) | 1 | 30 | 28.51 | 9.7% / 9.7% |
| fight-blob-1k #1 | recvmmsg (1000 us) | 1 | 30 | 28.54 | 5.1% / 5.1% |
| fight-uniform-5k #1 | recvmmsg (1000 us) | 1 | 150 | 51.45 | 15.0% / 15.0% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 653 | 105.0 / 144.0 | 50.9 | 0 / 0 | 0 / 0 | 0 | 25349 | 0 | 11% |
| fight-blob-1k #1 | 1000 / 1000 | 569 | 103.0 / 144.0 | 49.3 | 0 / 0 | 0 / 0 | 0 | 34271 | 0 | 12% |
| fight-uniform-5k #1 | 5000 / 5000 | 1285 | 106.0 / 146.0 | 50.6 | 0 / 0 | 0 / 0 | 1 | 1226 | 0 | 15% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.66 / 0.16 / 0.00 | 96.77 / 0.98 / 1.34 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 95.3 | 0 | 242 / 367 | 367 / 433 |
| fight-blob-1k #1 | 96.66 / 1.32 / 1.23 | 90.93 / 3.24 / 3.41 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 94.7 | 0 | 242 / 366 | 367 / 433 |
| fight-uniform-5k #1 | 97.63 / 2.17 / 0.04 | 88.53 / 9.73 / 1.17 | 28.65 / 54.99 / 14.17 | 6 / 923 / 8633 | 122.6 | 0 | 233 / 367 | 367 / 433 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 27.6 | 66840 | 66395 | 99.3 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 107.6 | 65707 | 65127 | 99.1 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 157.6 | 66060 | 62717 | 94.9 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 24.8 | 32484 | 7391 | 22.8 | 1379 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 104.8 | 31500 | 6293 | 20.0 | 1299 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 154.8 | 32283 | 6033 | 18.7 | 1267 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 30.2 | 37982 | 6892 | 18.1 | 1347 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 110.2 | 39557 | 6993 | 17.7 | 1358 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 160.7 | 39972 | 6516 | 16.3 | 1261 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 281805 | 1544 / 200831 | 114 | 0 | 187361 | 0 | 1.06 / 1.32 | 7.32 / 8.03 | 0 |
| fight-blob-1k #1 | 149726 | 2093 / 28234 | 896 | 10020 | 99164 | 4037 | 1.09 / 1.36 | 5.03 / 6.92 | 0 |
| fight-uniform-5k #1 | 577004 | 1376 / 22151 | 440 | 2342 | 384267 | 4012 | 1.71 / 2.10 | 9.83 / 10.81 | 1 |
