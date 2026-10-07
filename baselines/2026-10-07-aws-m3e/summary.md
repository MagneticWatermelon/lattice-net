# Baseline: aws-m3e, 2026-10-07

- **Machine:** AMD EPYC 9R14, 64 (1 socket(s) x 64 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** af340e4 (uncommitted changes)
- **Runs:** 8 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 28 threads, bots on a second machine (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), server listening on 172.31.17.93. Took 8 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 10000 | L0 (30 Hz; L0:1708) | 21.05 / 24.06 / 27.74 | 0 | 545 | 1.82 | 1432 | 343.4 | 0 / 0 |
| m3e-blob-3k-fight #1 | 3000 | L0 (30 Hz; L0:1709) | 17.55 / 20.52 / 61.26 | 1 | 180 | 2.00 | 1634 | 392.0 | 0 / 0 |
| m3e-blob-3k-classes #1 | 3000 | L0 (30 Hz; L0:1708) | 9.74 / 10.85 / 11.60 | 0 | 180 | 2.00 | 1554 | 373.2 | 0 / 0 |
| m3e-uniform-10k-sockets-1 #1 | 10000 | L0 (30 Hz; L0:1708) | 16.98 / 18.42 / 23.00 | 0 | 526 | 1.75 | 1344 | 322.5 | 0 / 0 |
| m3e-uniform-10k-sockets-4 #1 | 10000 | L0 (30 Hz; L0:1709) | 15.87 / 17.39 / 18.16 | 0 | 525 | 1.75 | 1343 | 322.4 | 0 / 0 |
| m3e-uniform-10k-sockets-8 #1 | 10000 | L0 (30 Hz; L0:1709) | 16.20 / 17.66 / 21.31 | 0 | 526 | 1.75 | 1343 | 322.5 | 0 / 0 |
| m3e-uniform-10k-sockets-16 #1 | 10000 | L0 (30 Hz; L0:1708) | 16.43 / 18.01 / 21.57 | 0 | 526 | 1.75 | 1343 | 322.5 | 0 / 0 |
| m3e-uniform-10k-recvfrom #1 | 10000 | L0 (30 Hz; L0:1708) | 18.45 / 20.04 / 22.85 | 0 | 526 | 1.75 | 1344 | 322.6 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 1.27 / 2.72 | 4.02 / 5.07 | 1.01 / 1.24 | 0.47 / 0.57 | 0.57 / 1.10 | 0.34 / 0.51 | 3.51 / 4.39 | 2.16 / 2.74 | 4.24 / 4.79 |
| m3e-blob-3k-fight #1 | 0.66 / 1.01 | 1.28 / 1.58 | 0.47 / 0.57 | 0.23 / 0.29 | 0.38 / 0.49 | 0.19 / 0.34 | 6.49 / 8.73 | 1.73 / 2.24 | 1.53 / 1.83 |
| m3e-blob-3k-classes #1 | 0.67 / 1.07 | 1.25 / 1.59 | 0.44 / 0.52 | 0.23 / 0.29 | 0.35 / 0.41 | 0.18 / 0.22 | 1.89 / 2.75 | 0.82 / 1.20 | 1.43 / 1.78 |
| m3e-uniform-10k-sockets-1 #1 | 1.20 / 1.82 | 3.98 / 5.21 | 0.99 / 1.31 | 0.47 / 0.68 | 0.56 / 0.91 | 0.31 / 0.41 | 3.19 / 3.90 | 2.00 / 2.40 | 3.94 / 4.44 |
| m3e-uniform-10k-sockets-4 #1 | 0.92 / 1.43 | 3.93 / 4.81 | 0.98 / 1.29 | 0.47 / 0.60 | 0.54 / 0.97 | 0.30 / 0.41 | 3.15 / 3.84 | 1.97 / 2.33 | 3.32 / 3.94 |
| m3e-uniform-10k-sockets-8 #1 | 0.96 / 1.45 | 4.07 / 5.05 | 1.01 / 1.22 | 0.47 / 0.56 | 0.54 / 0.88 | 0.29 / 0.55 | 3.22 / 3.97 | 2.03 / 2.41 | 3.39 / 3.98 |
| m3e-uniform-10k-sockets-16 #1 | 0.84 / 1.21 | 4.27 / 5.38 | 0.99 / 1.30 | 0.47 / 0.63 | 0.54 / 0.88 | 0.29 / 0.40 | 3.29 / 4.12 | 2.06 / 2.46 | 3.37 / 3.96 |
| m3e-uniform-10k-recvfrom #1 | 1.37 / 1.79 | 4.12 / 5.11 | 1.04 / 1.37 | 0.47 / 0.58 | 0.57 / 1.00 | 0.31 / 0.43 | 3.38 / 4.17 | 2.16 / 2.95 | 4.58 / 5.89 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 64 | 21.05 | 1.27 / 0.78 / 0.60 | 3.51 / 2.35 / 1.83 | 2.16 / 1.78 / 1.36 | 4.24 / 4.02 / 1.99 | 2.25 | 4.51 |
| m3e-blob-3k-fight #1 | 64 | 17.55 | 0.66 / 0.21 / 0.14 | 6.49 / 6.18 / 2.93 | 1.73 / 1.41 / 0.71 | 1.53 / 1.34 / 0.64 | 1.27 | 1.52 |
| m3e-blob-3k-classes #1 | 64 | 9.74 | 0.67 / 0.21 / 0.15 | 1.89 / 1.41 / 0.99 | 0.82 / 0.52 / 0.36 | 1.43 / 1.33 / 0.64 | 1.34 | 1.49 |
| m3e-uniform-10k-sockets-1 #1 | 64 | 16.98 | 1.20 / 0.74 / 0.60 | 3.19 / 2.10 / 1.69 | 2.00 / 1.66 / 1.26 | 3.94 / 3.75 / 1.87 | 2.08 | 4.47 |
| m3e-uniform-10k-sockets-4 #1 | 64 | 15.87 | 0.92 / 0.47 / 0.37 | 3.15 / 2.18 / 1.68 | 1.97 / 1.64 / 1.26 | 3.32 / 3.10 / 1.50 | 1.97 | 4.42 |
| m3e-uniform-10k-sockets-8 #1 | 64 | 16.20 | 0.96 / 0.50 / 0.35 | 3.22 / 2.33 / 1.72 | 2.03 / 1.67 / 1.28 | 3.39 / 3.15 / 1.51 | 1.95 | 4.56 |
| m3e-uniform-10k-sockets-16 #1 | 64 | 16.43 | 0.84 / 0.41 / 0.32 | 3.29 / 2.20 / 1.74 | 2.06 / 1.71 / 1.30 | 3.37 / 3.13 / 1.50 | 2.11 | 4.76 |
| m3e-uniform-10k-recvfrom #1 | 64 | 18.45 | 1.37 / 0.78 / 0.59 | 3.38 / 2.30 / 1.72 | 2.16 / 1.80 / 1.32 | 4.58 / 4.33 / 1.90 | 2.28 | 4.62 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | recvmmsg (1000 us) | 1 | 300 | 57.31 | 29.9% / 29.9% |
| m3e-blob-3k-fight #1 | recvmmsg (1000 us) | 1 | 90 | 46.50 | 25.8% / 25.8% |
| m3e-blob-3k-classes #1 | recvmmsg (1000 us) | 1 | 90 | 45.39 | 9.8% / 9.8% |
| m3e-uniform-10k-sockets-1 #1 | recvmmsg (1000 us) | 1 | 300 | 60.00 | 31.0% / 31.0% |
| m3e-uniform-10k-sockets-4 #1 | recvmmsg (1000 us) | 4 | 300 | 39.50 | 8.2% / 8.2% |
| m3e-uniform-10k-sockets-8 #1 | recvmmsg (1000 us) | 8 | 300 | 28.42 | 4.8% / 4.7% |
| m3e-uniform-10k-sockets-16 #1 | recvmmsg (1000 us) | 16 | 300 | 19.03 | 3.0% / 2.8% |
| m3e-uniform-10k-recvfrom #1 | recvfrom | 1 | 300 | 1.00 | 59.2% / 59.2% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 10000 / 10000 | 1516 | 60.0 / 78.0 | 50.4 | 0 / 0 | 0 / 0 | 33 | 6297 | 0 | 32% |
| m3e-blob-3k-fight #1 | 3000 / 3000 | 669 | 58.0 / 76.0 | 50.2 | 0 / 0 | 0 / 0 | 0 | 324754 | 0 | 10% |
| m3e-blob-3k-classes #1 | 3000 / 3000 | 1227 | 105.0 / 146.0 | 50.3 | 2 / 0 | 2 / 0 | 177 | 184369 | 0 | 33% |
| m3e-uniform-10k-sockets-1 #1 | 10000 / 10000 | 1552 | 58.0 / 75.0 | 50.3 | 0 / 0 | 0 / 0 | 73 | 6279 | 0 | 27% |
| m3e-uniform-10k-sockets-4 #1 | 10000 / 10000 | 1514 | 57.0 / 74.0 | 49.9 | 0 / 0 | 0 / 0 | 92 | 6125 | 0 | 27% |
| m3e-uniform-10k-sockets-8 #1 | 10000 / 10000 | 1489 | 57.0 / 74.0 | 49.8 | 0 / 0 | 0 / 0 | 50 | 6104 | 0 | 27% |
| m3e-uniform-10k-sockets-16 #1 | 10000 / 10000 | 1486 | 57.0 / 74.0 | 49.6 | 0 / 0 | 0 / 0 | 94 | 6455 | 0 | 27% |
| m3e-uniform-10k-recvfrom #1 | 10000 / 10000 | 1553 | 58.0 / 74.0 | 49.4 | 0 / 0 | 0 / 0 | 29 | 5558 | 0 | 27% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 99.81 / 0.05 / 0.00 | 99.50 / 0.15 / 0.01 | 40.00 / 58.97 / 0.57 | 0 / 0 / 3666 | 87.0 | 0 | 164 / 200 | 292 / 300 |
| m3e-blob-3k-fight #1 | 94.58 / 2.20 / 2.07 | 84.13 / 5.51 / 6.28 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 96.8 | 0 | 140 / 200 | 267 / 300 |
| m3e-blob-3k-classes #1 | 80.54 / 14.14 / 3.51 | 70.69 / 16.28 / 8.58 | 0.00 / 0.00 / 0.00 | 315 / 670 / 0 | 131.7 | 0 | 267 / 367 | 367 / 433 |
| m3e-uniform-10k-sockets-1 #1 | 99.85 / 0.02 / 0.00 | 99.52 / 0.13 / 0.01 | 40.11 / 58.88 / 0.56 | 0 / 0 / 3673 | 70.9 | 0 | 133 / 167 | 267 / 300 |
| m3e-uniform-10k-sockets-4 #1 | 99.85 / 0.02 / 0.00 | 99.52 / 0.13 / 0.01 | 40.11 / 58.91 / 0.53 | 0 / 0 / 3672 | 70.2 | 0 | 133 / 167 | 267 / 300 |
| m3e-uniform-10k-sockets-8 #1 | 99.85 / 0.02 / 0.00 | 99.52 / 0.13 / 0.01 | 40.05 / 58.98 / 0.52 | 0 / 0 / 3678 | 72.5 | 0 | 133 / 175 | 267 / 300 |
| m3e-uniform-10k-sockets-16 #1 | 99.85 / 0.02 / 0.00 | 99.52 / 0.13 / 0.01 | 40.07 / 58.97 / 0.52 | 0 / 0 / 3678 | 73.0 | 0 | 133 / 167 | 267 / 300 |
| m3e-uniform-10k-recvfrom #1 | 99.85 / 0.02 / 0.00 | 99.52 / 0.13 / 0.01 | 40.11 / 58.93 / 0.51 | 0 / 0 / 3678 | 73.4 | 0 | 133 / 167 | 267 / 300 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 0 | ? ms | 200 | 19.4 | 68322 | 13166 | 19.3 | 2591 |
| m3e-blob-3k-fight #1 | 0 | ? ms | 300 | 17.5 | 73503 | 23566 | 32.1 | 5405 |
| m3e-blob-3k-classes #1 | 0 | 10 ms | 500 | 30.5 | 93597 | 25433 | 27.2 | 5090 |
| m3e-blob-3k-classes #1 | 1 | 50 ms | 500 | 110.4 | 92366 | 21047 | 22.8 | 4497 |
| m3e-blob-3k-classes #1 | 2 | 75 ms | 500 | 160.9 | 92117 | 19883 | 21.6 | 4273 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| m3e-uniform-10k-fire20 #1 | 1135081 | 1300 / 16397 | 365 | 1122 | 92 | 2713 | 3.22 / 4.08 | 21.05 / 24.06 | 33 |
| m3e-blob-3k-fight #1 | 1185324 | 7970 / 54452 | 1920 | 10250 | 0 | 10852 | 4.46 / 5.12 | 17.55 / 20.52 | 0 |
| m3e-blob-3k-classes #1 | 416888 | 9571 / 102702 | 4017 | 42217 | 273856 | 14513 | 2.27 / 2.79 | 9.74 / 10.85 | 177 |
| m3e-uniform-10k-sockets-1 #1 | 0 | 0 / 0 | 0 | 0 | 0 | 0 | 0.07 / 0.27 | 16.98 / 18.42 | 73 |
| m3e-uniform-10k-sockets-4 #1 | 0 | 0 / 0 | 0 | 0 | 0 | 0 | 0.07 / 0.10 | 15.87 / 17.39 | 92 |
| m3e-uniform-10k-sockets-8 #1 | 0 | 0 / 0 | 0 | 0 | 0 | 0 | 0.07 / 0.12 | 16.20 / 17.66 | 50 |
| m3e-uniform-10k-sockets-16 #1 | 0 | 0 / 0 | 0 | 0 | 0 | 0 | 0.07 / 0.27 | 16.43 / 18.01 | 94 |
| m3e-uniform-10k-recvfrom #1 | 0 | 0 / 0 | 0 | 0 | 0 | 0 | 0.07 / 0.26 | 18.45 / 20.04 | 29 |

## Profiles

- `m3e-blob-3k-fight-1/perf.txt`
- `m3e-uniform-10k-fire20-1/perf.txt`
