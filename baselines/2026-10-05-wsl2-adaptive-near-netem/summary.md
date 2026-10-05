# Baseline: wsl2-adaptive-near-netem, 2026-10-05

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** 4121954
- **Runs:** 12 scenarios x 1, 60 s each, interleaved. Server 8 threads, bots 8 threads, on the same machine over loopback. Took 12 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 3.74 / 4.52 / 10.84 | 0 | 30 | 1.00 | 243 | 58.4 | 0 / 0 |
| clean-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 6.88 / 7.92 / 10.51 | 0 | 60 | 2.00 | 1477 | 354.5 | 0 / 0 |
| lan-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 4.43 / 5.24 / 11.70 | 0 | 30 | 1.00 | 244 | 58.7 | 0 / 0 |
| lan-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 7.59 / 8.76 / 54.48 | 1 | 60 | 2.00 | 1482 | 355.8 | 0 / 0 |
| typical-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 4.30 / 5.22 / 15.39 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| typical-blob-1k #1 | 1000 | L0 (30 Hz; L0:1707) | 8.53 / 9.87 / 22.36 | 0 | 60 | 2.00 | 1507 | 361.9 | 0 / 0 |
| far-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1705) | 4.64 / 5.51 / 8.71 | 0 | 30 | 1.00 | 248 | 59.5 | 0 / 0 |
| far-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 9.38 / 10.96 / 42.77 | 1 | 60 | 2.00 | 1546 | 371.1 | 0 / 0 |
| lossy-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 4.36 / 5.12 / 15.37 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| lossy-blob-1k #1 | 1000 | L0 (30 Hz; L0:1707) | 8.34 / 9.37 / 12.38 | 0 | 60 | 2.00 | 1511 | 362.7 | 0 / 0 |
| jittery-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1710) | 4.68 / 5.53 / 15.38 | 0 | 30 | 1.00 | 245 | 58.9 | 0 / 0 |
| jittery-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 9.77 / 11.12 / 22.64 | 0 | 60 | 2.00 | 1509 | 362.3 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 0.38 / 0.57 | 0.10 / 0.17 | 0.31 / 0.42 | 0.17 / 0.30 | 0.20 / 0.32 | 0.05 / 0.10 | 0.84 / 1.18 | 0.41 / 0.81 | 1.25 / 1.78 |
| clean-blob-1k #1 | 0.36 / 0.56 | 0.10 / 0.17 | 0.25 / 0.33 | 0.17 / 0.25 | 0.30 / 0.39 | 0.04 / 0.07 | 2.33 / 2.76 | 1.01 / 1.34 | 2.30 / 2.77 |
| lan-uniform-1k #1 | 0.39 / 0.92 | 0.10 / 0.34 | 0.29 / 0.50 | 0.16 / 0.26 | 0.17 / 0.31 | 0.04 / 0.07 | 0.89 / 1.24 | 0.40 / 0.84 | 1.92 / 2.36 |
| lan-blob-1k #1 | 0.40 / 0.56 | 0.11 / 0.20 | 0.26 / 0.34 | 0.15 / 0.20 | 0.30 / 0.38 | 0.04 / 0.08 | 2.31 / 2.76 | 0.98 / 1.30 | 3.01 / 3.80 |
| typical-uniform-1k #1 | 0.38 / 0.83 | 0.10 / 0.22 | 0.30 / 0.47 | 0.16 / 0.24 | 0.17 / 0.31 | 0.04 / 0.07 | 0.83 / 1.21 | 0.40 / 0.81 | 1.84 / 2.35 |
| typical-blob-1k #1 | 0.41 / 0.55 | 0.12 / 0.19 | 0.26 / 0.34 | 0.15 / 0.21 | 0.30 / 0.39 | 0.04 / 0.08 | 2.29 / 2.74 | 0.95 / 1.28 | 3.98 / 4.84 |
| far-uniform-1k #1 | 0.38 / 0.60 | 0.10 / 0.19 | 0.29 / 0.42 | 0.16 / 0.26 | 0.18 / 0.30 | 0.04 / 0.08 | 0.87 / 1.23 | 0.42 / 0.87 | 2.15 / 2.66 |
| far-blob-1k #1 | 0.41 / 0.56 | 0.12 / 0.18 | 0.27 / 0.35 | 0.15 / 0.26 | 0.30 / 0.40 | 0.05 / 0.10 | 2.51 / 2.94 | 1.00 / 1.36 | 4.52 / 5.79 |
| lossy-uniform-1k #1 | 0.37 / 0.75 | 0.11 / 0.23 | 0.29 / 0.43 | 0.16 / 0.29 | 0.17 / 0.30 | 0.04 / 0.10 | 0.84 / 1.22 | 0.41 / 0.81 | 1.92 / 2.31 |
| lossy-blob-1k #1 | 0.40 / 0.60 | 0.12 / 0.19 | 0.27 / 0.34 | 0.15 / 0.26 | 0.30 / 0.40 | 0.04 / 0.09 | 2.32 / 2.77 | 0.96 / 1.30 | 3.76 / 4.47 |
| jittery-uniform-1k #1 | 0.40 / 0.78 | 0.11 / 0.17 | 0.29 / 0.38 | 0.16 / 0.25 | 0.17 / 0.31 | 0.04 / 0.08 | 0.85 / 1.18 | 0.41 / 0.83 | 2.19 / 2.65 |
| jittery-blob-1k #1 | 0.41 / 1.01 | 0.12 / 0.56 | 0.26 / 0.62 | 0.15 / 0.51 | 0.30 / 0.58 | 0.04 / 0.21 | 2.42 / 2.93 | 1.05 / 1.39 | 4.89 / 5.76 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 8 | 3.74 | 0.38 / 0.02 / 0.10 | 0.84 / 0.09 / 0.36 | 0.41 / 0.05 / 0.18 | 1.25 / 0.19 / 0.77 | 1.48 | 0.27 |
| clean-blob-1k #1 | 8 | 6.88 | 0.36 / 0.02 / 0.11 | 2.33 / 0.37 / 1.66 | 1.01 / 0.17 / 0.50 | 2.30 / 0.38 / 1.69 | 2.04 | 0.27 |
| lan-uniform-1k #1 | 8 | 4.43 | 0.39 / 0.03 / 0.10 | 0.89 / 0.09 / 0.35 | 0.40 / 0.05 / 0.17 | 1.92 / 0.31 / 1.32 | 1.65 | 0.26 |
| lan-blob-1k #1 | 8 | 7.59 | 0.40 / 0.03 / 0.12 | 2.31 / 0.41 / 1.65 | 0.98 / 0.17 / 0.48 | 3.01 / 0.57 / 2.42 | 2.03 | 0.26 |
| typical-uniform-1k #1 | 8 | 4.30 | 0.38 / 0.03 / 0.10 | 0.83 / 0.09 / 0.35 | 0.40 / 0.05 / 0.17 | 1.84 / 0.31 / 1.25 | 1.58 | 0.26 |
| typical-blob-1k #1 | 8 | 8.53 | 0.41 / 0.03 / 0.12 | 2.29 / 0.38 / 1.63 | 0.95 / 0.15 / 0.47 | 3.98 / 0.74 / 3.33 | 2.08 | 0.27 |
| far-uniform-1k #1 | 8 | 4.64 | 0.38 / 0.02 / 0.10 | 0.87 / 0.09 / 0.36 | 0.42 / 0.06 / 0.18 | 2.15 / 0.36 / 1.52 | 1.65 | 0.26 |
| far-blob-1k #1 | 8 | 9.38 | 0.41 / 0.03 / 0.13 | 2.51 / 0.41 / 1.83 | 1.00 / 0.16 / 0.51 | 4.52 / 0.84 / 3.75 | 2.22 | 0.27 |
| lossy-uniform-1k #1 | 8 | 4.36 | 0.37 / 0.03 / 0.10 | 0.84 / 0.08 / 0.35 | 0.41 / 0.05 / 0.17 | 1.92 / 0.33 / 1.35 | 1.56 | 0.27 |
| lossy-blob-1k #1 | 8 | 8.34 | 0.40 / 0.03 / 0.12 | 2.32 / 0.40 / 1.66 | 0.96 / 0.16 / 0.48 | 3.76 / 0.67 / 3.03 | 2.15 | 0.27 |
| jittery-uniform-1k #1 | 8 | 4.68 | 0.40 / 0.04 / 0.10 | 0.85 / 0.09 / 0.36 | 0.41 / 0.05 / 0.18 | 2.19 / 0.37 / 1.59 | 1.61 | 0.27 |
| jittery-blob-1k #1 | 8 | 9.77 | 0.41 / 0.04 / 0.12 | 2.42 / 0.41 / 1.71 | 1.05 / 0.17 / 0.53 | 4.89 / 0.97 / 4.18 | 2.21 | 0.27 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 / 1000 | 358 | 61.0 / 68.0 | 58.5 | 0 / 0 | 0 / 0 | 0 | 54 | 0 | 8% |
| clean-blob-1k #1 | 1000 / 1000 | 388 | 53.0 / 71.0 | 47.6 | 0 / 0 | 0 / 0 | 0 | 27648 | 0 | 11% |
| lan-uniform-1k #1 | 1000 / 1000 | 620 | 69.0 / 87.0 | 51.3 | 0 / 0 | 0 / 0 | 0 | 90 | 0 | 7% |
| lan-blob-1k #1 | 1000 / 1000 | 620 | 67.0 / 85.0 | 49.5 | 0 / 0 | 0 / 0 | 0 | 27196 | 0 | 9% |
| typical-uniform-1k #1 | 1000 / 1000 | 654 | 85.0 / 115.0 | 43.7 | 405 / 0 | 404 / 0 | 11 | 118 | 0 | 5% |
| typical-blob-1k #1 | 1000 / 1000 | 630 | 90.0 / 118.0 | 47.7 | 317 / 0 | 317 / 0 | 16 | 25551 | 0 | 12% |
| far-uniform-1k #1 | 1000 / 1000 | 732 | 127.0 / 169.0 | 51.4 | 2375 / 0 | 2363 / 0 | 81 | 74 | 0 | 6% |
| far-blob-1k #1 | 1000 / 1000 | 760 | 134.0 / 165.0 | 57.7 | 1265 / 0 | 1259 / 0 | 40 | 26423 | 0 | 11% |
| lossy-uniform-1k #1 | 1000 / 1000 | 697 | 88.0 / 134.0 | 45.9 | 5492 / 10 | 5246 / 0 | 218 | 110 | 0 | 5% |
| lossy-blob-1k #1 | 1000 / 1000 | 687 | 95.0 / 137.0 | 52.4 | 4877 / 9 | 4639 / 0 | 212 | 24972 | 0 | 11% |
| jittery-uniform-1k #1 | 1000 / 1000 | 558 | 92.0 / 145.0 | 54.9 | 4883 / 5 | 4890 / 0 | 177 | 94 | 0 | 7% |
| jittery-blob-1k #1 | 1000 / 1000 | 562 | 90.0 / 136.0 | 55.7 | 2622 / 0 | 2619 / 0 | 155 | 25132 | 0 | 11% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 99.89 / 0.01 / 0.00 | 99.53 / 0.12 / 0.01 | 39.99 / 59.31 / 0.32 | 0 / 0 / 3649 | 73.0 | 0 | 133 / 167 | 267 / 300 |
| clean-blob-1k #1 | 99.77 / 0.08 / 0.00 | 95.27 / 1.44 / 2.15 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 82.2 | 0 | 133 / 167 | 267 / 300 |
| lan-uniform-1k #1 | 99.87 / 0.02 / 0.00 | 99.50 / 0.15 / 0.01 | 40.26 / 59.06 / 0.27 | 0 / 0 / 3679 | 67.2 | 0 | 167 / 170 | 300 / 303 |
| lan-blob-1k #1 | 99.77 / 0.04 / 0.00 | 96.05 / 1.28 / 1.56 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 86.7 | 0 | 167 / 192 | 300 / 301 |
| typical-uniform-1k #1 | 99.86 / 0.02 / 0.00 | 99.53 / 0.13 / 0.01 | 39.86 / 59.13 / 0.61 | 0 / 0 / 3834 | 75.9 | 0 | 200 / 233 | 333 / 367 |
| typical-blob-1k #1 | 99.68 / 0.18 / 0.00 | 96.16 / 1.23 / 1.51 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 100.4 | 0 | 200 / 259 | 333 / 367 |
| far-uniform-1k #1 | 99.87 / 0.01 / 0.00 | 99.44 / 0.20 / 0.01 | 39.58 / 59.11 / 0.90 | 0 / 0 / 3913 | 95.3 | 0 | 267 / 333 | 400 / 435 |
| far-blob-1k #1 | 99.70 / 0.15 / 0.00 | 96.21 / 1.19 / 1.50 | 0.00 / 0.00 / 0.00 | 0 / 2 / 0 | 127.1 | 0 | 274 / 342 | 407 / 434 |
| lossy-uniform-1k #1 | 99.84 / 0.03 / 0.00 | 99.19 / 0.45 / 0.03 | 38.21 / 58.14 / 3.16 | 0 / 46 / 4584 | 92.1 | 0 | 200 / 255 | 333 / 375 |
| lossy-blob-1k #1 | 99.49 / 0.35 / 0.00 | 95.58 / 1.64 / 1.62 | 0.00 / 0.00 / 0.00 | 1 / 19 / 0 | 132.6 | 0 | 203 / 300 | 335 / 399 |
| jittery-uniform-1k #1 | 99.86 / 0.02 / 0.00 | 99.51 / 0.12 / 0.01 | 38.18 / 61.07 / 0.35 | 1 / 0 / 4069 | 110.9 | 0 | 189 / 247 | 320 / 366 |
| jittery-blob-1k #1 | 99.80 / 0.06 / 0.00 | 96.39 / 1.19 / 1.34 | 0.00 / 0.00 / 0.00 | 2 / 0 / 0 | 133.7 | 0 | 200 / 268 | 333 / 367 |

## Network

netem delays each direction once, so the round trip is about twice the delay. Times in ms.

| run | link (one way) | input -> applied p50 / p99 | round trip p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections (per bot-minute) | near decode errors | resyncs | input clock extra / skipped |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | clean | 61.0 / 68.0 | 67.0 / 100.0 | 58.5 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1000 / 256 |
| clean-blob-1k #1 | clean | 53.0 / 71.0 | 67.0 / 100.0 | 47.6 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1375 / 685 |
| lan-uniform-1k #1 | delay 15ms 2ms distribution normal | 69.0 / 87.0 | 100.0 / 133.0 | 51.3 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 8460 / 7478 |
| lan-blob-1k #1 | delay 15ms 2ms distribution normal | 67.0 / 85.0 | 100.0 / 133.0 | 49.5 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1694 / 1187 |
| typical-uniform-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 85.0 / 115.0 | 133.0 / 167.0 | 43.7 | 405 / 0 | 404 / 0 | 11 (0.011) | 0 | 0 | 1842 / 1029 |
| typical-blob-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 90.0 / 118.0 | 167.0 / 167.0 | 47.7 | 317 / 0 | 317 / 0 | 16 (0.016) | 0 | 0 | 3824 / 2937 |
| far-uniform-1k #1 | delay 75ms 10ms distribution normal loss 1% | 127.0 / 169.0 | 233.0 / 267.0 | 51.4 | 2375 / 0 | 2363 / 0 | 81 (0.081) | 0 | 0 | 16430 / 15478 |
| far-blob-1k #1 | delay 75ms 10ms distribution normal loss 1% | 134.0 / 165.0 | 233.0 / 267.0 | 57.7 | 1265 / 0 | 1259 / 0 | 40 (0.040) | 0 | 0 | 8583 / 7778 |
| lossy-uniform-1k #1 | delay 40ms 5ms distribution normal loss 5% | 88.0 / 134.0 | 133.0 / 200.0 | 45.9 | 5492 / 10 | 5246 / 0 | 218 (0.218) | 0 | 0 | 6254 / 5303 |
| lossy-blob-1k #1 | delay 40ms 5ms distribution normal loss 5% | 95.0 / 137.0 | 167.0 / 200.0 | 52.4 | 4877 / 9 | 4639 / 0 | 212 (0.212) | 0 | 0 | 7293 / 6513 |
| jittery-uniform-1k #1 | delay 40ms 20ms distribution normal | 92.0 / 145.0 | 167.0 / 200.0 | 54.9 | 4883 / 5 | 4890 / 0 | 177 (0.177) | 0 | 0 | 12872 / 11916 |
| jittery-blob-1k #1 | delay 40ms 20ms distribution normal | 90.0 / 136.0 | 167.0 / 200.0 | 55.7 | 2622 / 0 | 2619 / 0 | 155 (0.155) | 0 | 0 | 6922 / 5848 |
