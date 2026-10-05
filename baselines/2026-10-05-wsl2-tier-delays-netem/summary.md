# Baseline: wsl2-tier-delays-netem, 2026-10-05

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** 40db658
- **Runs:** 12 scenarios x 1, 60 s each, interleaved. Server 8 threads, bots 8 threads, on the same machine over loopback. Took 12 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 3.65 / 4.37 / 4.98 | 0 | 30 | 1.00 | 243 | 58.4 | 0 / 0 |
| clean-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 6.64 / 7.73 / 31.11 | 0 | 60 | 2.00 | 1476 | 354.4 | 0 / 0 |
| lan-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 3.88 / 4.78 / 15.13 | 0 | 30 | 1.00 | 244 | 58.6 | 0 / 0 |
| lan-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 7.76 / 9.38 / 34.31 | 1 | 60 | 2.00 | 1482 | 355.9 | 0 / 0 |
| typical-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 4.24 / 5.05 / 16.72 | 0 | 30 | 1.00 | 245 | 58.9 | 0 / 0 |
| typical-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 8.43 / 9.83 / 22.33 | 0 | 60 | 2.00 | 1507 | 361.7 | 0 / 0 |
| far-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 4.61 / 5.83 / 6.42 | 0 | 30 | 1.00 | 248 | 59.6 | 0 / 0 |
| far-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 8.90 / 10.60 / 33.99 | 1 | 60 | 2.00 | 1546 | 371.2 | 0 / 0 |
| lossy-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1707) | 4.22 / 5.13 / 12.02 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| lossy-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 9.28 / 11.04 / 46.67 | 1 | 60 | 2.00 | 1510 | 362.5 | 0 / 0 |
| jittery-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1707) | 4.59 / 5.31 / 5.70 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| jittery-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 9.07 / 10.96 / 29.21 | 0 | 60 | 2.00 | 1509 | 362.2 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 0.38 / 0.56 | 0.10 / 0.15 | 0.33 / 0.43 | 0.16 / 0.29 | 0.17 / 0.30 | 0.04 / 0.07 | 0.84 / 1.19 | 0.40 / 0.82 | 1.18 / 1.71 |
| clean-blob-1k #1 | 0.39 / 0.58 | 0.12 / 0.18 | 0.28 / 0.37 | 0.16 / 0.26 | 0.30 / 0.39 | 0.04 / 0.07 | 2.30 / 2.78 | 0.98 / 1.34 | 2.04 / 2.53 |
| lan-uniform-1k #1 | 0.38 / 0.55 | 0.10 / 0.16 | 0.29 / 0.38 | 0.16 / 0.21 | 0.17 / 0.30 | 0.04 / 0.07 | 0.85 / 1.22 | 0.40 / 0.85 | 1.45 / 1.83 |
| lan-blob-1k #1 | 0.40 / 0.68 | 0.11 / 0.18 | 0.25 / 0.35 | 0.16 / 0.23 | 0.29 / 0.39 | 0.04 / 0.08 | 2.23 / 2.65 | 0.95 / 1.33 | 3.31 / 4.32 |
| typical-uniform-1k #1 | 0.38 / 0.82 | 0.10 / 0.17 | 0.29 / 0.38 | 0.15 / 0.24 | 0.17 / 0.29 | 0.04 / 0.08 | 0.80 / 1.14 | 0.40 / 0.81 | 1.83 / 2.31 |
| typical-blob-1k #1 | 0.40 / 0.85 | 0.11 / 0.34 | 0.26 / 0.47 | 0.15 / 0.40 | 0.31 / 0.51 | 0.04 / 0.12 | 2.33 / 2.89 | 0.98 / 1.33 | 3.78 / 4.57 |
| far-uniform-1k #1 | 0.38 / 0.89 | 0.10 / 0.28 | 0.30 / 0.55 | 0.16 / 0.36 | 0.17 / 0.32 | 0.04 / 0.11 | 0.89 / 1.31 | 0.42 / 0.96 | 2.06 / 2.55 |
| far-blob-1k #1 | 0.41 / 1.00 | 0.11 / 0.36 | 0.26 / 0.60 | 0.15 / 0.41 | 0.31 / 0.62 | 0.04 / 0.14 | 2.40 / 3.17 | 1.00 / 1.45 | 4.15 / 4.97 |
| lossy-uniform-1k #1 | 0.36 / 0.70 | 0.10 / 0.20 | 0.29 / 0.45 | 0.17 / 0.31 | 0.17 / 0.29 | 0.04 / 0.10 | 0.75 / 1.19 | 0.41 / 0.88 | 1.83 / 2.25 |
| lossy-blob-1k #1 | 0.40 / 0.79 | 0.11 / 0.21 | 0.26 / 0.38 | 0.17 / 0.28 | 0.30 / 0.44 | 0.04 / 0.09 | 2.31 / 2.77 | 0.97 / 1.35 | 4.71 / 6.00 |
| jittery-uniform-1k #1 | 0.34 / 0.52 | 0.10 / 0.16 | 0.28 / 0.38 | 0.15 / 0.23 | 0.17 / 0.28 | 0.04 / 0.07 | 0.78 / 1.16 | 0.41 / 0.85 | 2.23 / 2.74 |
| jittery-blob-1k #1 | 0.41 / 1.03 | 0.12 / 0.55 | 0.27 / 0.65 | 0.15 / 0.48 | 0.30 / 0.60 | 0.04 / 0.10 | 2.35 / 2.96 | 0.94 / 1.33 | 4.40 / 5.28 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 8 | 3.65 | 0.38 / 0.02 / 0.10 | 0.84 / 0.09 / 0.36 | 0.40 / 0.05 / 0.17 | 1.18 / 0.18 / 0.70 | 1.47 | 0.26 |
| clean-blob-1k #1 | 8 | 6.64 | 0.39 / 0.03 / 0.12 | 2.30 / 0.38 / 1.65 | 0.98 / 0.17 / 0.49 | 2.04 / 0.34 / 1.45 | 2.01 | 0.28 |
| lan-uniform-1k #1 | 8 | 3.88 | 0.38 / 0.02 / 0.10 | 0.85 / 0.09 / 0.37 | 0.40 / 0.05 / 0.17 | 1.45 / 0.22 / 0.91 | 1.53 | 0.26 |
| lan-blob-1k #1 | 8 | 7.76 | 0.40 / 0.03 / 0.12 | 2.23 / 0.37 / 1.55 | 0.95 / 0.15 / 0.45 | 3.31 / 0.60 / 2.66 | 2.11 | 0.27 |
| typical-uniform-1k #1 | 8 | 4.24 | 0.38 / 0.04 / 0.10 | 0.80 / 0.08 / 0.35 | 0.40 / 0.05 / 0.17 | 1.83 / 0.30 / 1.26 | 1.53 | 0.25 |
| typical-blob-1k #1 | 8 | 8.43 | 0.40 / 0.03 / 0.12 | 2.33 / 0.36 / 1.65 | 0.98 / 0.16 / 0.48 | 3.78 / 0.65 / 3.07 | 2.17 | 0.26 |
| far-uniform-1k #1 | 8 | 4.61 | 0.38 / 0.02 / 0.10 | 0.89 / 0.09 / 0.36 | 0.42 / 0.06 / 0.18 | 2.06 / 0.33 / 1.44 | 1.67 | 0.26 |
| far-blob-1k #1 | 8 | 8.90 | 0.41 / 0.03 / 0.13 | 2.40 / 0.39 / 1.71 | 1.00 / 0.17 / 0.49 | 4.15 / 0.74 / 3.47 | 2.16 | 0.26 |
| lossy-uniform-1k #1 | 8 | 4.22 | 0.36 / 0.03 / 0.10 | 0.75 / 0.09 / 0.34 | 0.41 / 0.06 / 0.18 | 1.83 / 0.32 / 1.30 | 1.44 | 0.27 |
| lossy-blob-1k #1 | 8 | 9.28 | 0.40 / 0.03 / 0.12 | 2.31 / 0.40 / 1.62 | 0.97 / 0.15 / 0.47 | 4.71 / 0.92 / 4.05 | 2.13 | 0.28 |
| jittery-uniform-1k #1 | 8 | 4.59 | 0.34 / 0.02 / 0.09 | 0.78 / 0.09 / 0.35 | 0.41 / 0.05 / 0.18 | 2.23 / 0.39 / 1.62 | 1.52 | 0.25 |
| jittery-blob-1k #1 | 8 | 9.07 | 0.41 / 0.05 / 0.13 | 2.35 / 0.40 / 1.66 | 0.94 / 0.14 / 0.45 | 4.40 / 0.85 / 3.73 | 2.13 | 0.27 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 / 1000 | 401 | 42.0 / 69.0 | 39.7 | 0 / 0 | 0 / 0 | 0 | 106 | 0 | 8% |
| clean-blob-1k #1 | 1000 / 1000 | 301 | 56.0 / 69.0 | 51.6 | 0 / 0 | 0 / 0 | 0 | 28195 | 0 | 12% |
| lan-uniform-1k #1 | 1000 / 1000 | 591 | 67.0 / 79.0 | 49.7 | 0 / 0 | 0 / 0 | 0 | 106 | 0 | 5% |
| lan-blob-1k #1 | 1000 / 1000 | 558 | 64.0 / 82.0 | 45.3 | 0 / 0 | 0 / 0 | 0 | 26793 | 0 | 10% |
| typical-uniform-1k #1 | 1000 / 1000 | 600 | 85.0 / 117.0 | 43.7 | 448 / 0 | 447 / 0 | 24 | 71 | 0 | 6% |
| typical-blob-1k #1 | 1000 / 1000 | 636 | 84.0 / 121.0 | 41.8 | 1078 / 2 | 1075 / 0 | 41 | 27332 | 0 | 9% |
| far-uniform-1k #1 | 1000 / 1000 | 762 | 125.0 / 166.0 | 48.5 | 1843 / 0 | 1838 / 0 | 55 | 118 | 0 | 6% |
| far-blob-1k #1 | 1000 / 1000 | 791 | 128.0 / 168.0 | 52.2 | 2518 / 0 | 2505 / 0 | 98 | 26806 | 0 | 11% |
| lossy-uniform-1k #1 | 1000 / 1000 | 727 | 91.0 / 134.0 | 48.6 | 4299 / 12 | 4068 / 0 | 166 | 68 | 0 | 5% |
| lossy-blob-1k #1 | 1000 / 1000 | 650 | 95.0 / 137.0 | 51.9 | 5623 / 13 | 5391 / 0 | 235 | 25345 | 0 | 12% |
| jittery-uniform-1k #1 | 1000 / 1000 | 541 | 94.0 / 143.0 | 57.1 | 5869 / 23 | 5891 / 0 | 244 | 50 | 0 | 7% |
| jittery-blob-1k #1 | 1000 / 1000 | 560 | 87.0 / 142.0 | 52.8 | 4281 / 1 | 4279 / 0 | 184 | 23799 | 0 | 10% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 99.91 / 0.01 / 0.00 | 99.56 / 0.12 / 0.01 | 39.91 / 59.42 / 0.32 | 1 / 0 / 3653 | 66.7 | 0 | 133 / 167 | 267 / 300 |
| clean-blob-1k #1 | 98.03 / 1.84 / 0.00 | 95.52 / 1.34 / 2.05 | 0.00 / 0.00 / 0.00 | 71 / 0 / 0 | 66.9 | 0 | 133 / 167 | 267 / 300 |
| lan-uniform-1k #1 | 99.87 / 0.01 / 0.00 | 99.52 / 0.14 / 0.01 | 39.98 / 59.38 / 0.24 | 0 / 0 / 3639 | 66.7 | 0 | 167 / 167 | 300 / 300 |
| lan-blob-1k #1 | 99.65 / 0.20 / 0.00 | 95.93 / 1.33 / 1.59 | 0.00 / 0.00 / 0.00 | 14 / 0 / 0 | 66.8 | 0 | 167 / 167 | 300 / 300 |
| typical-uniform-1k #1 | 99.85 / 0.03 / 0.00 | 99.53 / 0.13 / 0.01 | 39.76 / 59.21 / 0.63 | 0 / 0 / 3833 | 67.3 | 0 | 200 / 233 | 333 / 367 |
| typical-blob-1k #1 | 98.40 / 1.47 / 0.00 | 96.06 / 1.30 / 1.50 | 0.00 / 0.00 / 0.00 | 23 / 0 / 0 | 66.8 | 0 | 200 / 233 | 333 / 367 |
| far-uniform-1k #1 | 99.70 / 0.17 / 0.00 | 99.41 / 0.21 / 0.02 | 40.40 / 58.33 / 0.86 | 4 / 0 / 3949 | 67.1 | 0 | 267 / 301 | 400 / 434 |
| far-blob-1k #1 | 93.49 / 6.37 / 0.00 | 95.80 / 1.41 / 1.59 | 0.00 / 0.00 / 0.00 | 99 / 4 / 0 | 67.5 | 0 | 268 / 302 | 402 / 435 |
| lossy-uniform-1k #1 | 99.37 / 0.50 / 0.00 | 99.15 / 0.49 / 0.02 | 38.59 / 57.92 / 2.99 | 9 / 49 / 4528 | 67.2 | 0 | 200 / 241 | 333 / 374 |
| lossy-blob-1k #1 | 93.40 / 6.46 / 0.00 | 95.51 / 1.68 / 1.64 | 0.00 / 0.00 / 0.00 | 108 / 51 / 0 | 67.0 | 0 | 204 / 260 | 337 / 393 |
| jittery-uniform-1k #1 | 98.29 / 1.59 / 0.00 | 99.52 / 0.13 / 0.01 | 39.40 / 59.87 / 0.33 | 54 / 0 / 4019 | 67.1 | 0 | 200 / 233 | 333 / 367 |
| jittery-blob-1k #1 | 83.02 / 16.84 / 0.00 | 96.25 / 1.30 / 1.37 | 0.00 / 0.00 / 0.00 | 176 / 0 / 0 | 67.9 | 0 | 186 / 233 | 319 / 367 |

## Network

netem delays each direction once, so the round trip is about twice the delay. Times in ms.

| run | link (one way) | input -> applied p50 / p99 | round trip p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections (per bot-minute) | near decode errors | resyncs | input clock extra / skipped |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | clean | 42.0 / 69.0 | 67.0 / 100.0 | 39.7 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1312 / 720 |
| clean-blob-1k #1 | clean | 56.0 / 69.0 | 67.0 / 100.0 | 51.6 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1000 / 119 |
| lan-uniform-1k #1 | delay 15ms 2ms distribution normal | 67.0 / 79.0 | 100.0 / 100.0 | 49.7 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1008 / 62 |
| lan-blob-1k #1 | delay 15ms 2ms distribution normal | 64.0 / 82.0 | 100.0 / 133.0 | 45.3 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1797 / 849 |
| typical-uniform-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 85.0 / 117.0 | 133.0 / 167.0 | 43.7 | 448 / 0 | 447 / 0 | 24 (0.024) | 0 | 0 | 3038 / 2244 |
| typical-blob-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 84.0 / 121.0 | 133.0 / 167.0 | 41.8 | 1078 / 2 | 1075 / 0 | 41 (0.041) | 0 | 0 | 13798 / 12982 |
| far-uniform-1k #1 | delay 75ms 10ms distribution normal loss 1% | 125.0 / 166.0 | 233.0 / 267.0 | 48.5 | 1843 / 0 | 1838 / 0 | 55 (0.055) | 0 | 0 | 7441 / 6503 |
| far-blob-1k #1 | delay 75ms 10ms distribution normal loss 1% | 128.0 / 168.0 | 233.0 / 267.0 | 52.2 | 2518 / 0 | 2505 / 0 | 98 (0.098) | 0 | 0 | 17220 / 16047 |
| lossy-uniform-1k #1 | delay 40ms 5ms distribution normal loss 5% | 91.0 / 134.0 | 133.0 / 200.0 | 48.6 | 4299 / 12 | 4068 / 0 | 166 (0.166) | 0 | 0 | 5056 / 4148 |
| lossy-blob-1k #1 | delay 40ms 5ms distribution normal loss 5% | 95.0 / 137.0 | 167.0 / 200.0 | 51.9 | 5623 / 13 | 5391 / 0 | 235 (0.235) | 0 | 0 | 7542 / 6881 |
| jittery-uniform-1k #1 | delay 40ms 20ms distribution normal | 94.0 / 143.0 | 167.0 / 200.0 | 57.1 | 5869 / 23 | 5891 / 0 | 244 (0.244) | 0 | 0 | 12749 / 11880 |
| jittery-blob-1k #1 | delay 40ms 20ms distribution normal | 87.0 / 142.0 | 167.0 / 200.0 | 52.8 | 4281 / 1 | 4279 / 0 | 184 (0.184) | 0 | 0 | 11216 / 10094 |
