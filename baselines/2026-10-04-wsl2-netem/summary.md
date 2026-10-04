# Baseline: wsl2-netem, 2026-10-04

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** 6a9168c (uncommitted changes)
- **Runs:** 12 scenarios x 1, 60 s each, interleaved. Server 8 threads, bots 8 threads, on the same machine over loopback. Took 14 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 2.85 / 3.72 / 17.07 | 0 | 30 | 1.00 | 226 | 54.2 | 0 / 0 |
| clean-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 5.64 / 6.87 / 13.02 | 0 | 60 | 2.00 | 1398 | 335.8 | 0 / 0 |
| lan-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1710) | 3.66 / 4.44 / 9.73 | 0 | 30 | 1.00 | 226 | 54.2 | 0 / 0 |
| lan-blob-1k #1 | 1000 | L0 (30 Hz; L0:1710) | 7.33 / 8.62 / 19.14 | 0 | 60 | 2.00 | 1402 | 336.5 | 0 / 0 |
| typical-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 3.74 / 4.57 / 5.24 | 0 | 30 | 1.00 | 227 | 54.4 | 0 / 0 |
| typical-blob-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 7.71 / 9.37 / 31.16 | 0 | 60 | 2.00 | 1419 | 340.6 | 0 / 0 |
| far-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1705) | 3.85 / 4.64 / 12.04 | 0 | 30 | 1.00 | 228 | 54.8 | 0 / 0 |
| far-blob-1k #1 | 1000 | L0 (30 Hz; L0:1705) | 8.06 / 9.45 / 23.48 | 0 | 60 | 2.00 | 1450 | 348.2 | 0 / 0 |
| lossy-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1706) | 3.71 / 4.42 / 10.14 | 0 | 30 | 1.00 | 227 | 54.5 | 0 / 0 |
| lossy-blob-1k #1 | 1000 | L0 (30 Hz; L0:1707) | 7.62 / 8.97 / 10.37 | 0 | 60 | 2.00 | 1420 | 341.0 | 0 / 0 |
| jittery-uniform-1k #1 | 1000 | L0 (30 Hz; L0:1710) | 3.83 / 4.64 / 18.80 | 0 | 30 | 1.00 | 227 | 54.4 | 0 / 0 |
| jittery-blob-1k #1 | 1000 | L0 (30 Hz; L0:1710) | 8.06 / 9.34 / 15.44 | 0 | 60 | 2.00 | 1420 | 340.8 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 0.34 / 0.56 | 0.09 / 0.15 | 0.17 / 0.28 | 0.07 / 0.12 | 0.04 / 0.07 | 0.60 / 0.99 | 0.34 / 0.70 | 1.11 / 1.58 |
| clean-blob-1k #1 | 0.35 / 0.66 | 0.09 / 0.17 | 0.16 / 0.25 | 0.06 / 0.07 | 0.04 / 0.07 | 2.28 / 2.85 | 0.94 / 1.25 | 1.74 / 2.29 |
| lan-uniform-1k #1 | 0.33 / 0.65 | 0.09 / 0.21 | 0.17 / 0.29 | 0.06 / 0.15 | 0.04 / 0.13 | 0.65 / 1.06 | 0.36 / 0.71 | 1.88 / 2.38 |
| lan-blob-1k #1 | 0.35 / 0.73 | 0.09 / 0.20 | 0.16 / 0.27 | 0.06 / 0.12 | 0.04 / 0.09 | 2.23 / 2.75 | 0.93 / 1.27 | 3.43 / 4.05 |
| typical-uniform-1k #1 | 0.33 / 0.53 | 0.09 / 0.16 | 0.17 / 0.24 | 0.05 / 0.09 | 0.04 / 0.07 | 0.72 / 1.10 | 0.36 / 0.70 | 1.93 / 2.39 |
| typical-blob-1k #1 | 0.35 / 0.57 | 0.10 / 0.17 | 0.17 / 0.22 | 0.06 / 0.08 | 0.04 / 0.06 | 2.26 / 2.78 | 0.91 / 1.18 | 3.87 / 4.75 |
| far-uniform-1k #1 | 0.34 / 0.52 | 0.09 / 0.16 | 0.17 / 0.22 | 0.06 / 0.09 | 0.04 / 0.06 | 0.81 / 1.13 | 0.36 / 0.72 | 1.98 / 2.46 |
| far-blob-1k #1 | 0.35 / 0.55 | 0.10 / 0.17 | 0.17 / 0.22 | 0.06 / 0.08 | 0.04 / 0.06 | 2.37 / 2.90 | 0.89 / 1.23 | 4.07 / 4.82 |
| lossy-uniform-1k #1 | 0.33 / 0.52 | 0.09 / 0.18 | 0.17 / 0.27 | 0.06 / 0.09 | 0.04 / 0.07 | 0.76 / 1.08 | 0.36 / 0.73 | 1.86 / 2.29 |
| lossy-blob-1k #1 | 0.35 / 0.77 | 0.09 / 0.22 | 0.17 / 0.37 | 0.05 / 0.13 | 0.04 / 0.10 | 2.31 / 2.75 | 0.92 / 1.24 | 3.67 / 4.45 |
| jittery-uniform-1k #1 | 0.34 / 0.52 | 0.10 / 0.18 | 0.17 / 0.23 | 0.06 / 0.08 | 0.04 / 0.07 | 0.78 / 1.12 | 0.36 / 0.74 | 1.97 / 2.45 |
| jittery-blob-1k #1 | 0.36 / 0.76 | 0.11 / 0.19 | 0.17 / 0.25 | 0.05 / 0.08 | 0.04 / 0.06 | 2.29 / 2.81 | 0.90 / 1.22 | 4.11 / 4.89 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | 1000 / 1000 | 358 | 52.0 / 69.0 | 50.2 | 0 / 0 | 0 / 0 | 0 | 0 | 8% |
| clean-blob-1k #1 | 1000 / 1000 | 362 | 54.0 / 70.0 | 50.0 | 0 / 0 | 0 / 0 | 0 | 0 | 11% |
| lan-uniform-1k #1 | 1000 / 1000 | 595 | 68.0 / 85.0 | 51.5 | 0 / 0 | 0 / 0 | 0 | 0 | 6% |
| lan-blob-1k #1 | 1000 / 1000 | 662 | 69.0 / 85.0 | 51.0 | 0 / 0 | 0 / 0 | 0 | 0 | 8% |
| typical-uniform-1k #1 | 1000 / 1000 | 625 | 93.0 / 117.0 | 51.1 | 425 / 0 | 422 / 0 | 14 | 0 | 6% |
| typical-blob-1k #1 | 1000 / 1000 | 591 | 93.0 / 117.0 | 51.1 | 469 / 0 | 468 / 0 | 10 | 0 | 10% |
| far-uniform-1k #1 | 1000 / 1000 | 725 | 129.0 / 166.0 | 53.4 | 1691 / 0 | 1683 / 0 | 41 | 0 | 6% |
| far-blob-1k #1 | 1000 / 1000 | 775 | 129.0 / 165.0 | 53.6 | 1696 / 1 | 1687 / 0 | 71 | 0 | 11% |
| lossy-uniform-1k #1 | 1000 / 1000 | 650 | 96.0 / 138.0 | 53.8 | 5769 / 17 | 5504 / 0 | 195 | 0 | 6% |
| lossy-blob-1k #1 | 1000 / 1000 | 658 | 96.0 / 139.0 | 54.1 | 5884 / 18 | 5636 / 0 | 190 | 0 | 9% |
| jittery-uniform-1k #1 | 1000 / 1000 | 558 | 92.0 / 144.0 | 55.9 | 5504 / 6 | 5510 / 0 | 175 | 0 | 6% |
| jittery-blob-1k #1 | 1000 / 1000 | 562 | 90.0 / 141.0 | 55.8 | 5469 / 8 | 5477 / 0 | 180 | 0 | 11% |

## Network

netem delays each direction once, so the round trip is about twice the delay. Times in ms.

| run | link (one way) | input -> applied p50 / p99 | round trip p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections (per bot-minute) | near decode errors | resyncs | input clock extra / skipped |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-1k #1 | clean | 52.0 / 69.0 | 67.0 / 100.0 | 50.2 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1874 / 878 |
| clean-blob-1k #1 | clean | 54.0 / 70.0 | 67.0 / 100.0 | 50.0 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 1230 / 239 |
| lan-uniform-1k #1 | delay 15ms 2ms distribution normal | 68.0 / 85.0 | 100.0 / 133.0 | 51.5 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 2682 / 1661 |
| lan-blob-1k #1 | delay 15ms 2ms distribution normal | 69.0 / 85.0 | 100.0 / 133.0 | 51.0 | 0 / 0 | 0 / 0 | 0 (0.000) | 0 | 0 | 2095 / 1089 |
| typical-uniform-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 93.0 / 117.0 | 167.0 / 167.0 | 51.1 | 425 / 0 | 422 / 0 | 14 (0.014) | 0 | 0 | 6522 / 5484 |
| typical-blob-1k #1 | delay 40ms 5ms distribution normal loss 0.5% | 93.0 / 117.0 | 167.0 / 167.0 | 51.1 | 469 / 0 | 468 / 0 | 10 (0.010) | 0 | 0 | 7444 / 6426 |
| far-uniform-1k #1 | delay 75ms 10ms distribution normal loss 1% | 129.0 / 166.0 | 233.0 / 267.0 | 53.4 | 1691 / 0 | 1683 / 0 | 41 (0.041) | 0 | 0 | 10310 / 9286 |
| far-blob-1k #1 | delay 75ms 10ms distribution normal loss 1% | 129.0 / 165.0 | 233.0 / 267.0 | 53.6 | 1696 / 1 | 1687 / 0 | 71 (0.071) | 0 | 0 | 10570 / 9549 |
| lossy-uniform-1k #1 | delay 40ms 5ms distribution normal loss 5% | 96.0 / 138.0 | 167.0 / 200.0 | 53.8 | 5769 / 17 | 5504 / 0 | 195 (0.195) | 0 | 0 | 9198 / 8229 |
| lossy-blob-1k #1 | delay 40ms 5ms distribution normal loss 5% | 96.0 / 139.0 | 167.0 / 200.0 | 54.1 | 5884 / 18 | 5636 / 0 | 190 (0.190) | 0 | 0 | 9477 / 8520 |
| jittery-uniform-1k #1 | delay 40ms 20ms distribution normal | 92.0 / 144.0 | 167.0 / 200.0 | 55.9 | 5504 / 6 | 5510 / 0 | 175 (0.175) | 0 | 0 | 13531 / 12548 |
| jittery-blob-1k #1 | delay 40ms 20ms distribution normal | 90.0 / 141.0 | 167.0 / 200.0 | 55.8 | 5469 / 8 | 5477 / 0 | 180 (0.180) | 0 | 0 | 13618 / 12662 |
