# Baseline: wsl2-fight, 2026-10-05

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** 466e3fc (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 8 threads, bots 8 threads, on the same machine over loopback. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1710) | 12.69 / 14.00 / 19.30 | 0 | 60 | 2.00 | 1640 | 393.7 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 12.80 / 15.23 / 49.59 | 2 | 60 | 2.00 | 1536 | 368.6 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L7 (20 Hz; L2:5,L3:31,L4:31,L5:31,L6:53,L7:910) | 39.10 / 45.39 / 53.15 | 98 | 93 | 1.00 | 274 | 40.8 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.63 / 1.12 | 0.30 / 0.44 | 0.29 / 0.37 | 0.17 / 0.28 | 0.30 / 0.38 | 0.05 / 0.09 | 2.92 / 3.42 | 1.17 / 1.53 | 5.80 / 6.65 |
| fight-blob-1k #1 | 0.64 / 1.13 | 0.30 / 0.45 | 0.30 / 0.40 | 0.20 / 0.28 | 0.30 / 0.38 | 0.05 / 0.10 | 2.98 / 3.49 | 1.16 / 1.57 | 5.73 / 7.22 |
| fight-uniform-5k #1 | 2.38 / 3.07 | 1.89 / 3.05 | 0.97 / 1.30 | 0.27 / 0.43 | 0.54 / 1.48 | 0.54 / 0.74 | 4.20 / 5.02 | 2.12 / 2.90 | 23.61 / 29.38 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 8 | 12.69 | 0.63 / 0.08 / 0.33 | 2.92 / 0.52 / 2.22 | 1.17 / 0.19 / 0.62 | 5.80 / 1.25 / 5.14 | 2.20 | 0.47 |
| fight-blob-1k #1 | 8 | 12.80 | 0.64 / 0.08 / 0.33 | 2.98 / 0.51 / 2.27 | 1.16 / 0.18 / 0.63 | 5.73 / 1.25 / 5.06 | 2.22 | 0.50 |
| fight-uniform-5k #1 | 8 | 39.10 | 2.38 / 0.32 / 1.70 | 4.20 / 0.60 / 3.44 | 2.12 / 0.35 / 1.50 | 23.61 / 3.99 / 22.33 | 3.34 | 2.17 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 625 | 107.0 / 149.0 | 49.6 | 0 / 0 | 0 / 0 | 0 | 27135 | 0 | 42% |
| fight-blob-1k #1 | 1000 / 1000 | 695 | 104.0 / 149.0 | 48.9 | 442 / 226 | 668 / 0 | 144 | 31717 | 0 | 46% |
| fight-uniform-5k #1 | 5000 / 5000 | 1126 | 129.0 / 219.0 | 62.1 | 17336 / 51 | 17387 / 0 | 616 | 1086 | 0 | 56% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.77 / 0.04 / 0.00 | 95.92 / 1.31 / 1.62 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 91.5 | 0 | 242 / 342 | 367 / 433 |
| fight-blob-1k #1 | 96.99 / 1.14 / 1.12 | 90.24 / 3.47 / 3.72 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 102.0 | 0 | 245 / 363 | 367 / 433 |
| fight-uniform-5k #1 | 98.81 / 0.89 / 0.14 | 79.89 / 18.84 / 0.51 | 13.38 / 76.05 / 9.06 | 7 / 745 / 16709 | 122.2 | 0 | 235 / 347 | 366 / 441 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 37.1 | 66884 | 62037 | 92.8 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 118.5 | 65472 | 61055 | 93.3 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 167.0 | 65182 | 55095 | 84.5 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 39.0 | 31405 | 7477 | 23.8 | 1405 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 118.9 | 29361 | 6183 | 21.1 | 1324 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 169.6 | 31735 | 5940 | 18.7 | 1184 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 55.7 | 26629 | 6996 | 26.3 | 1379 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 134.6 | 30233 | 7744 | 25.6 | 1513 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 184.6 | 30200 | 7285 | 24.1 | 1402 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 277289 | 2200 / 186603 | 241 | 0 | 184061 | 0 | 0.97 / 1.29 | 12.69 / 14.00 | 0 |
| fight-blob-1k #1 | 143506 | 1908 / 31347 | 1464 | 12973 | 92460 | 4028 | 1.06 / 1.47 | 12.80 / 15.23 | 144 |
| fight-uniform-5k #1 | 455557 | 1122 / 24786 | 561 | 3099 | 286810 | 4354 | 2.33 / 3.23 | 39.10 / 45.39 | 616 |
