# Baseline: wsl2-m3b-netem, 2026-10-05

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** 23fd8c1
- **Runs:** 12 scenarios x 1, 60 s each, interleaved. Server 8 threads, bots 8 threads, on the same machine over loopback. Took 12 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1710) | 3.82 / 4.51 / 11.16 | 0 | 30 | 1.00 | 243 | 58.4 | 0 / 0 |
| clean-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 6.35 / 7.33 / 9.11 | 0 | 60 | 2.00 | 1476 | 354.3 | 0 / 0 |
| lan-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 4.10 / 4.95 / 8.03 | 0 | 30 | 1.00 | 245 | 58.7 | 0 / 0 |
| lan-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 8.21 / 9.11 / 11.19 | 0 | 60 | 2.00 | 1483 | 356.1 | 0 / 0 |
| typical-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1707) | 4.10 / 4.90 / 5.45 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| typical-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 9.62 / 11.54 / 42.52 | 1 | 60 | 2.00 | 1509 | 362.3 | 0 / 0 |
| far-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1705) | 4.99 / 5.89 / 21.30 | 0 | 30 | 1.00 | 248 | 59.6 | 0 / 0 |
| far-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 9.29 / 10.65 / 23.70 | 0 | 60 | 2.00 | 1546 | 371.3 | 0 / 0 |
| lossy-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 4.08 / 5.00 / 26.32 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| lossy-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 8.53 / 10.26 / 23.13 | 0 | 60 | 2.00 | 1511 | 362.9 | 0 / 0 |
| jittery-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 4.74 / 5.76 / 13.78 | 0 | 30 | 1.00 | 246 | 59.0 | 0 / 0 |
| jittery-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 9.84 / 10.87 / 13.04 | 0 | 60 | 2.00 | 1509 | 362.3 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 0.36 / 0.55 | 0.10 / 0.17 | 0.30 / 0.41 | 0.18 / 0.23 | 0.17 / 0.29 | 0.04 / 0.07 | 0.84 / 1.21 | 0.41 / 0.88 | 1.40 / 1.76 |
| clean-blob-1k #1 | 0.35 / 0.55 | 0.09 / 0.15 | 0.24 / 0.33 | 0.17 / 0.21 | 0.29 / 0.38 | 0.04 / 0.07 | 2.20 / 2.62 | 0.98 / 1.32 | 1.95 / 2.37 |
| lan-uniform-1k #1 | 0.36 / 0.74 | 0.09 / 0.16 | 0.27 / 0.40 | 0.18 / 0.26 | 0.17 / 0.32 | 0.04 / 0.10 | 0.83 / 1.18 | 0.41 / 0.81 | 1.71 / 2.15 |
| lan-blob-1k #1 | 0.39 / 0.55 | 0.11 / 0.17 | 0.27 / 0.36 | 0.17 / 0.27 | 0.30 / 0.39 | 0.04 / 0.07 | 2.23 / 2.64 | 0.93 / 1.28 | 3.73 / 4.35 |
| typical-uniform-1k #1 | 0.38 / 0.74 | 0.10 / 0.21 | 0.28 / 0.41 | 0.17 / 0.30 | 0.18 / 0.31 | 0.04 / 0.10 | 0.88 / 1.23 | 0.41 / 0.90 | 1.61 / 2.02 |
| typical-blob-1k #1 | 0.40 / 0.71 | 0.11 / 0.20 | 0.25 / 0.35 | 0.16 / 0.28 | 0.32 / 0.41 | 0.04 / 0.08 | 2.46 / 2.90 | 0.96 / 1.27 | 4.88 / 6.14 |
| far-uniform-1k #1 | 0.38 / 0.69 | 0.10 / 0.17 | 0.31 / 0.41 | 0.17 / 0.31 | 0.18 / 0.29 | 0.04 / 0.08 | 0.96 / 1.29 | 0.42 / 0.88 | 2.37 / 2.89 |
| far-blob-1k #1 | 0.41 / 0.87 | 0.11 / 0.21 | 0.26 / 0.34 | 0.16 / 0.29 | 0.31 / 0.40 | 0.04 / 0.08 | 2.50 / 2.92 | 0.97 / 1.31 | 4.50 / 5.24 |
| lossy-uniform-1k #1 | 0.37 / 0.69 | 0.10 / 0.18 | 0.29 / 0.40 | 0.17 / 0.26 | 0.18 / 0.31 | 0.04 / 0.09 | 0.81 / 1.15 | 0.40 / 0.87 | 1.66 / 2.10 |
| lossy-blob-1k #1 | 0.39 / 0.53 | 0.12 / 0.17 | 0.26 / 0.32 | 0.16 / 0.25 | 0.30 / 0.39 | 0.04 / 0.07 | 2.29 / 2.65 | 0.96 / 1.30 | 4.04 / 5.18 |
| jittery-uniform-1k #1 | 0.37 / 0.94 | 0.10 / 0.35 | 0.28 / 0.60 | 0.18 / 0.46 | 0.18 / 0.36 | 0.04 / 0.13 | 0.82 / 1.25 | 0.41 / 0.85 | 2.23 / 2.73 |
| jittery-blob-1k #1 | 0.40 / 1.01 | 0.11 / 0.55 | 0.25 / 0.62 | 0.17 / 0.53 | 0.31 / 0.63 | 0.04 / 0.19 | 2.35 / 2.95 | 1.01 / 1.38 | 5.07 / 5.97 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 8 | 3.82 | 0.36 / 0.02 / 0.09 | 0.84 / 0.09 / 0.36 | 0.41 / 0.06 / 0.18 | 1.40 / 0.20 / 0.83 | 1.54 | 0.28 |
| clean-blob-1k #1 | 8 | 6.35 | 0.35 / 0.02 / 0.10 | 2.20 / 0.35 / 1.52 | 0.98 / 0.17 / 0.47 | 1.95 / 0.31 / 1.35 | 2.04 | 0.26 |
| lan-uniform-1k #1 | 8 | 4.10 | 0.36 / 0.03 / 0.10 | 0.83 / 0.09 / 0.35 | 0.41 / 0.05 / 0.18 | 1.71 / 0.29 / 1.15 | 1.53 | 0.27 |
| lan-blob-1k #1 | 8 | 8.21 | 0.39 / 0.02 / 0.11 | 2.23 / 0.36 / 1.60 | 0.93 / 0.16 / 0.47 | 3.73 / 0.67 / 3.11 | 1.98 | 0.28 |
| typical-uniform-1k #1 | 8 | 4.10 | 0.38 / 0.03 / 0.10 | 0.88 / 0.09 / 0.34 | 0.41 / 0.05 / 0.17 | 1.61 / 0.24 / 1.05 | 1.62 | 0.27 |
| typical-blob-1k #1 | 8 | 9.62 | 0.40 / 0.04 / 0.12 | 2.46 / 0.42 / 1.82 | 0.96 / 0.16 / 0.48 | 4.88 / 0.90 / 4.03 | 2.25 | 0.27 |
| far-uniform-1k #1 | 8 | 4.99 | 0.38 / 0.03 / 0.10 | 0.96 / 0.11 / 0.41 | 0.42 / 0.06 / 0.19 | 2.37 / 0.46 / 1.72 | 1.71 | 0.27 |
| far-blob-1k #1 | 8 | 9.29 | 0.41 / 0.04 / 0.12 | 2.50 / 0.43 / 1.83 | 0.97 / 0.16 / 0.49 | 4.50 / 0.84 / 3.81 | 2.13 | 0.27 |
| lossy-uniform-1k #1 | 8 | 4.08 | 0.37 / 0.03 / 0.10 | 0.81 / 0.09 / 0.34 | 0.40 / 0.06 / 0.17 | 1.66 / 0.25 / 1.11 | 1.52 | 0.27 |
| lossy-blob-1k #1 | 8 | 8.53 | 0.39 / 0.03 / 0.11 | 2.29 / 0.38 / 1.61 | 0.96 / 0.16 / 0.47 | 4.04 / 0.75 / 3.34 | 2.15 | 0.28 |
| jittery-uniform-1k #1 | 8 | 4.74 | 0.37 / 0.03 / 0.10 | 0.82 / 0.09 / 0.35 | 0.41 / 0.05 / 0.18 | 2.23 / 0.38 / 1.66 | 1.54 | 0.28 |
| jittery-blob-1k #1 | 8 | 9.84 | 0.40 / 0.03 / 0.11 | 2.35 / 0.40 / 1.66 | 1.01 / 0.17 / 0.51 | 5.07 / 1.11 / 4.44 | 2.11 | 0.28 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 / 1000 | 429 | 55.0 / 67.0 | 52.9 | 0 / 0 | 0 / 0 | 0 | 44 | 0 | 8% |
| clean-blob-1k #1 | 1000 / 1000 | 431 | 53.0 / 67.0 | 49.8 | 0 / 0 | 0 / 0 | 0 | 26423 | 0 | 11% |
| lan-uniform-1k #1 | 1000 / 1000 | 662 | 65.0 / 82.0 | 47.7 | 0 / 0 | 0 / 0 | 0 | 174 | 0 | 6% |
| lan-blob-1k #1 | 1000 / 1000 | 529 | 69.0 / 87.0 | 50.3 | 0 / 0 | 0 / 0 | 0 | 26008 | 0 | 11% |
| typical-uniform-1k #1 | 1000 / 1000 | 599 | 86.0 / 113.0 | 44.4 | 260 / 0 | 259 / 0 | 13 | 58 | 0 | 5% |
| typical-blob-1k #1 | 1000 / 1000 | 629 | 99.0 / 118.0 | 56.1 | 374 / 0 | 373 / 0 | 12 | 25773 | 0 | 9% |
| far-uniform-1k #1 | 1000 / 1000 | 729 | 133.0 / 159.0 | 56.4 | 355 / 0 | 352 / 0 | 20 | 77 | 0 | 5% |
| far-blob-1k #1 | 1000 / 1000 | 700 | 132.0 / 164.0 | 55.8 | 1060 / 0 | 1050 / 0 | 42 | 26706 | 0 | 12% |
| lossy-uniform-1k #1 | 1000 / 1000 | 662 | 95.0 / 134.0 | 53.3 | 4047 / 5 | 3810 / 0 | 155 | 71 | 0 | 5% |
| lossy-blob-1k #1 | 1000 / 1000 | 687 | 96.0 / 137.0 | 53.3 | 4162 / 16 | 3931 / 0 | 183 | 25549 | 0 | 10% |
| jittery-uniform-1k #1 | 1000 / 1000 | 545 | 91.0 / 141.0 | 54.5 | 3370 / 7 | 3376 / 0 | 148 | 113 | 0 | 6% |
| jittery-blob-1k #1 | 1000 / 1000 | 557 | 90.0 / 137.0 | 55.1 | 2660 / 3 | 2663 / 0 | 130 | 24338 | 0 | 12% |

## Smoothness

Tracked bots draw a frame every tick, 100 ms behind the newest server step (lattice-bots --interp-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step: what lag compensation would rewind.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | render delay | clock snaps | rewind p50 / p99 (ms) |
|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 99.84 / 0.00 / 0.00 | 98.70 / 1.11 / 0.04 | 20.31 / 79.00 / 0.51 | 0 / 118 / 4897 | 100.2 | 0 | 167 / 200 |
| clean-blob-1k #1 | 99.74 / 0.07 / 0.00 | 95.08 / 2.32 / 2.04 | 0.00 / 0.00 / 0.00 | 0 / 117 / 0 | 100.1 | 0 | 167 / 200 |
| lan-uniform-1k #1 | 99.83 / 0.00 / 0.00 | 99.58 / 0.22 / 0.04 | 20.08 / 79.25 / 0.47 | 0 / 46 / 4886 | 100.1 | 0 | 200 / 200 |
| lan-blob-1k #1 | 99.73 / 0.05 / 0.00 | 94.95 / 2.66 / 1.78 | 0.00 / 0.00 / 0.00 | 0 / 145 / 0 | 100.3 | 0 | 200 / 210 |
| typical-uniform-1k #1 | 99.83 / 0.00 / 0.00 | 96.90 / 2.89 / 0.04 | 20.75 / 78.15 / 0.89 | 0 / 133 / 4970 | 100.1 | 0 | 233 / 267 |
| typical-blob-1k #1 | 99.59 / 0.21 / 0.00 | 94.53 / 3.11 / 1.75 | 0.00 / 0.00 / 0.00 | 0 / 168 / 0 | 100.4 | 0 | 249 / 267 |
| far-uniform-1k #1 | 99.83 / 0.00 / 0.00 | 96.74 / 3.03 / 0.06 | 20.13 / 78.31 / 1.35 | 0 / 193 / 5268 | 100.8 | 0 | 302 / 330 |
| far-blob-1k #1 | 99.36 / 0.44 / 0.00 | 91.58 / 6.26 / 1.57 | 0.00 / 0.00 / 0.00 | 13 / 201 / 0 | 100.8 | 0 | 304 / 334 |
| lossy-uniform-1k #1 | 99.79 / 0.04 / 0.00 | 91.69 / 8.06 / 0.08 | 19.73 / 75.82 / 4.19 | 0 / 324 / 5625 | 100.5 | 0 | 234 / 270 |
| lossy-blob-1k #1 | 97.96 / 1.84 / 0.00 | 90.12 / 7.64 / 1.64 | 0.00 / 0.00 / 0.00 | 30 / 320 / 0 | 100.2 | 0 | 236 / 299 |
| jittery-uniform-1k #1 | 99.80 / 0.03 / 0.00 | 81.11 / 18.65 / 0.06 | 16.61 / 82.67 / 0.53 | 10 / 332 / 5315 | 101.9 | 0 | 217 / 261 |
| jittery-blob-1k #1 | 98.24 / 1.57 / 0.00 | 82.99 / 15.08 / 1.41 | 0.00 / 0.00 / 0.00 | 80 / 333 / 0 | 101.2 | 0 | 233 / 267 |

## Network

netem delays each direction once, so the round trip is about twice the delay. Times in ms.

| run | link (one way) | input -> applied p50 / p99 | round trip p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections (per bot-minute) | near decode errors | resyncs | input clock extra / skipped |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | clean | 55.0 / 67.0 | 67.0 / 100.0 | 52.9 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1000 / 251 |
| clean-blob-1k #1 | clean | 53.0 / 67.0 | 67.0 / 100.0 | 49.8 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1029 / 276 |
| lan-uniform-1k #1 | delay 15ms 2ms distribution normal | 65.0 / 82.0 | 100.0 / 100.0 | 47.7 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1641 / 666 |
| lan-blob-1k #1 | delay 15ms 2ms distribution normal | 69.0 / 87.0 | 100.0 / 133.0 | 50.3 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 2763 / 1795 |
| typical-uniform-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 86.0 / 113.0 | 133.0 / 167.0 | 44.4 | 260 / 0 | 259 / 0 | 13 (0.013) | 0 | 0 | 1624 / 840 |
| typical-blob-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 99.0 / 118.0 | 167.0 / 167.0 | 56.1 | 374 / 0 | 373 / 0 | 12 (0.012) | 0 | 0 | 5958 / 5271 |
| far-uniform-1k #1 | delay 75ms 10ms distribution normal loss 1% | 133.0 / 159.0 | 233.0 / 267.0 | 56.4 | 355 / 0 | 352 / 0 | 20 (0.020) | 0 | 0 | 1527 / 508 |
| far-blob-1k #1 | delay 75ms 10ms distribution normal loss 1% | 132.0 / 164.0 | 233.0 / 267.0 | 55.8 | 1060 / 0 | 1050 / 0 | 42 (0.042) | 0 | 0 | 6654 / 5605 |
| lossy-uniform-1k #1 | delay 40ms 5ms distribution normal loss 5% | 95.0 / 134.0 | 167.0 / 200.0 | 53.3 | 4047 / 5 | 3810 / 0 | 155 (0.155) | 0 | 0 | 4817 / 3951 |
| lossy-blob-1k #1 | delay 40ms 5ms distribution normal loss 5% | 96.0 / 137.0 | 167.0 / 200.0 | 53.3 | 4162 / 16 | 3931 / 0 | 183 (0.183) | 0 | 0 | 5287 / 4398 |
| jittery-uniform-1k #1 | delay 40ms 20ms distribution normal | 91.0 / 141.0 | 167.0 / 200.0 | 54.5 | 3370 / 7 | 3376 / 0 | 148 (0.148) | 0 | 0 | 7978 / 7027 |
| jittery-blob-1k #1 | delay 40ms 20ms distribution normal | 90.0 / 137.0 | 167.0 / 200.0 | 55.1 | 2660 / 3 | 2663 / 0 | 130 (0.130) | 0 | 0 | 6796 / 5743 |
