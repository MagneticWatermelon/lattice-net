# Baseline: aws-netem-10k, 2026-10-07

- **Machine:** AMD EPYC 9R14, 64 (1 socket(s) x 64 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** af340e4 (uncommitted changes)
- **Runs:** 12 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 28 threads, bots on a second machine (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), server listening on 172.31.17.93. Took 14 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-10k #1 | 10000 | L0 (30 Hz; L0:1709) | 17.36 / 19.34 / 20.82 | 0 | 526 | 1.75 | 1344 | 322.6 | 0 / 0 |
| clean-blob-10k #1 | 10000 | L0 (30 Hz; L0:1709) | 17.06 / 18.66 / 19.67 | 0 | 451 | 1.50 | 1383 | 332.1 | 0 / 0 |
| lan-uniform-10k #1 | 10000 | L0 (30 Hz; L0:1709) | 17.18 / 18.86 / 20.10 | 0 | 527 | 1.76 | 1349 | 323.9 | 0 / 0 |
| lan-blob-10k #1 | 10000 | L0 (30 Hz; L0:1708) | 17.15 / 18.86 / 20.87 | 0 | 452 | 1.51 | 1394 | 334.7 | 0 / 0 |
| typical-uniform-10k #1 | 10000 | L0 (30 Hz; L0:1706) | 17.26 / 19.02 / 20.05 | 0 | 529 | 1.76 | 1356 | 325.5 | 0 / 0 |
| typical-blob-10k #1 | 10000 | L0 (30 Hz; L0:1707) | 17.01 / 18.95 / 19.93 | 0 | 454 | 1.51 | 1403 | 336.7 | 0 / 0 |
| far-uniform-10k #1 | 10000 | L0 (30 Hz; L0:1706) | 17.25 / 18.97 / 21.02 | 0 | 530 | 1.77 | 1365 | 327.7 | 0 / 0 |
| far-blob-10k #1 | 10000 | L0 (30 Hz; L0:1706) | 17.20 / 18.94 / 19.94 | 0 | 456 | 1.52 | 1420 | 340.9 | 0 / 0 |
| lossy-uniform-10k #1 | 10000 | L0 (30 Hz; L0:1707) | 17.02 / 18.64 / 20.05 | 0 | 528 | 1.76 | 1356 | 325.4 | 0 / 0 |
| lossy-blob-10k #1 | 10000 | L0 (30 Hz; L0:1706) | 16.73 / 18.45 / 19.30 | 0 | 454 | 1.51 | 1404 | 337.1 | 0 / 0 |
| jittery-uniform-10k #1 | 10000 | L0 (30 Hz; L0:1709) | 17.10 / 18.82 / 19.94 | 0 | 528 | 1.76 | 1356 | 325.5 | 0 / 0 |
| jittery-blob-10k #1 | 10000 | L0 (30 Hz; L0:1709) | 16.85 / 18.52 / 19.30 | 0 | 454 | 1.51 | 1404 | 337.1 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-10k #1 | 1.26 / 2.51 | 4.08 / 5.40 | 1.01 / 1.33 | 0.47 / 0.60 | 0.55 / 1.03 | 0.31 / 0.56 | 3.23 / 4.14 | 2.03 / 2.40 | 4.06 / 4.68 |
| clean-blob-10k #1 | 1.22 / 2.31 | 4.02 / 5.08 | 1.00 / 1.32 | 0.48 / 0.66 | 0.59 / 1.06 | 0.32 / 0.53 | 3.59 / 4.37 | 2.03 / 2.45 | 3.47 / 4.11 |
| lan-uniform-10k #1 | 1.18 / 2.42 | 4.13 / 5.48 | 1.01 / 1.26 | 0.47 / 0.59 | 0.55 / 0.84 | 0.31 / 0.43 | 3.15 / 3.98 | 2.03 / 2.44 | 4.05 / 4.57 |
| lan-blob-10k #1 | 1.18 / 2.40 | 4.05 / 5.37 | 1.03 / 1.25 | 0.48 / 0.60 | 0.57 / 0.95 | 0.31 / 0.56 | 3.72 / 4.55 | 2.02 / 2.34 | 3.48 / 4.15 |
| typical-uniform-10k #1 | 1.19 / 2.44 | 4.02 / 5.18 | 0.99 / 1.27 | 0.47 / 0.60 | 0.56 / 1.04 | 0.31 / 0.60 | 3.31 / 4.09 | 2.03 / 2.42 | 4.04 / 4.82 |
| typical-blob-10k #1 | 1.18 / 2.38 | 4.02 / 5.13 | 1.00 / 1.24 | 0.48 / 0.59 | 0.57 / 0.88 | 0.31 / 0.40 | 3.73 / 4.46 | 2.02 / 2.39 | 3.42 / 4.43 |
| far-uniform-10k #1 | 1.17 / 2.42 | 3.92 / 5.17 | 0.99 / 1.20 | 0.47 / 0.58 | 0.56 / 0.88 | 0.31 / 0.47 | 3.41 / 4.21 | 2.06 / 2.43 | 4.05 / 4.91 |
| far-blob-10k #1 | 1.18 / 2.42 | 3.89 / 4.82 | 1.00 / 1.19 | 0.48 / 0.59 | 0.57 / 0.96 | 0.31 / 0.54 | 3.85 / 4.77 | 2.10 / 2.48 | 3.47 / 4.63 |
| lossy-uniform-10k #1 | 1.14 / 2.17 | 3.92 / 4.96 | 1.00 / 1.21 | 0.47 / 0.57 | 0.55 / 0.82 | 0.31 / 0.40 | 3.23 / 4.04 | 2.02 / 2.42 | 4.09 / 4.93 |
| lossy-blob-10k #1 | 1.14 / 2.29 | 3.87 / 4.90 | 1.00 / 1.21 | 0.48 / 0.58 | 0.56 / 0.98 | 0.30 / 0.56 | 3.69 / 4.34 | 2.02 / 2.36 | 3.37 / 4.41 |
| jittery-uniform-10k #1 | 1.18 / 2.43 | 3.92 / 4.93 | 0.98 / 1.23 | 0.47 / 0.58 | 0.56 / 0.82 | 0.31 / 0.40 | 3.24 / 4.09 | 2.04 / 2.40 | 4.09 / 4.98 |
| jittery-blob-10k #1 | 1.18 / 2.39 | 3.86 / 4.87 | 0.99 / 1.18 | 0.48 / 0.58 | 0.57 / 0.87 | 0.31 / 0.51 | 3.74 / 4.59 | 2.02 / 2.38 | 3.40 / 4.46 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-10k #1 | 64 | 17.36 | 1.26 / 0.77 / 0.60 | 3.23 / 2.23 / 1.71 | 2.03 / 1.66 / 1.27 | 4.06 / 3.87 / 1.92 | 2.05 | 4.57 |
| clean-blob-10k #1 | 64 | 17.06 | 1.22 / 0.76 / 0.61 | 3.59 / 2.35 / 1.89 | 2.03 / 1.69 / 1.30 | 3.47 / 3.28 / 1.66 | 2.23 | 4.52 |
| lan-uniform-10k #1 | 64 | 17.18 | 1.18 / 0.74 / 0.58 | 3.15 / 2.24 / 1.72 | 2.03 / 1.67 / 1.28 | 4.05 / 3.87 / 1.94 | 1.89 | 4.62 |
| lan-blob-10k #1 | 64 | 17.15 | 1.18 / 0.74 / 0.60 | 3.72 / 2.46 / 1.96 | 2.02 / 1.66 / 1.30 | 3.48 / 3.29 / 1.69 | 2.25 | 4.56 |
| typical-uniform-10k #1 | 64 | 17.26 | 1.19 / 0.73 / 0.58 | 3.31 / 2.27 / 1.75 | 2.03 / 1.67 / 1.29 | 4.04 / 3.85 / 1.92 | 2.05 | 4.51 |
| typical-blob-10k #1 | 64 | 17.01 | 1.18 / 0.73 / 0.59 | 3.73 / 2.48 / 1.97 | 2.02 / 1.67 / 1.31 | 3.42 / 3.24 / 1.66 | 2.23 | 4.52 |
| far-uniform-10k #1 | 64 | 17.25 | 1.17 / 0.73 / 0.57 | 3.41 / 3.00 / 1.82 | 2.06 / 1.70 / 1.33 | 4.05 / 3.87 / 1.92 | 1.39 | 4.41 |
| far-blob-10k #1 | 64 | 17.20 | 1.18 / 0.73 / 0.58 | 3.85 / 2.79 / 2.05 | 2.10 / 1.73 / 1.35 | 3.47 / 3.28 / 1.67 | 2.07 | 4.40 |
| lossy-uniform-10k #1 | 64 | 17.02 | 1.14 / 0.70 / 0.55 | 3.23 / 2.30 / 1.74 | 2.02 / 1.67 / 1.29 | 4.09 / 3.90 / 1.94 | 1.91 | 4.41 |
| lossy-blob-10k #1 | 64 | 16.73 | 1.14 / 0.70 / 0.56 | 3.69 / 2.46 / 1.96 | 2.02 / 1.66 / 1.31 | 3.37 / 3.19 / 1.64 | 2.21 | 4.37 |
| jittery-uniform-10k #1 | 64 | 17.10 | 1.18 / 0.73 / 0.57 | 3.24 / 2.36 / 1.75 | 2.04 / 1.68 / 1.29 | 4.09 / 3.90 / 1.94 | 1.88 | 4.41 |
| jittery-blob-10k #1 | 64 | 16.85 | 1.18 / 0.73 / 0.58 | 3.74 / 2.55 / 2.00 | 2.02 / 1.66 / 1.30 | 3.40 / 3.22 / 1.65 | 2.18 | 4.36 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| clean-uniform-10k #1 | recvmmsg (1000 us) | 1 | 300 | 58.74 | 29.6% / 29.6% |
| clean-blob-10k #1 | recvmmsg (1000 us) | 1 | 300 | 58.80 | 30.2% / 30.2% |
| lan-uniform-10k #1 | recvmmsg (1000 us) | 1 | 300 | 58.97 | 29.8% / 29.8% |
| lan-blob-10k #1 | recvmmsg (1000 us) | 1 | 300 | 58.90 | 29.4% / 29.4% |
| typical-uniform-10k #1 | recvmmsg (1000 us) | 1 | 298 | 58.79 | 29.3% / 29.3% |
| typical-blob-10k #1 | recvmmsg (1000 us) | 1 | 298 | 58.75 | 29.7% / 29.7% |
| far-uniform-10k #1 | recvmmsg (1000 us) | 1 | 296 | 58.54 | 29.5% / 29.5% |
| far-blob-10k #1 | recvmmsg (1000 us) | 1 | 296 | 58.78 | 29.5% / 29.5% |
| lossy-uniform-10k #1 | recvmmsg (1000 us) | 1 | 284 | 58.01 | 28.2% / 28.2% |
| lossy-blob-10k #1 | recvmmsg (1000 us) | 1 | 284 | 57.86 | 28.1% / 28.1% |
| jittery-uniform-10k #1 | recvmmsg (1000 us) | 1 | 299 | 58.70 | 29.9% / 29.9% |
| jittery-blob-10k #1 | recvmmsg (1000 us) | 1 | 299 | 58.83 | 29.4% / 29.4% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-10k #1 | 10000 / 10000 | 1486 | 58.0 / 74.0 | 49.6 | 0 / 0 | 0 / 0 | 85 | 5845 | 0 | 27% |
| clean-blob-10k #1 | 10000 / 10000 | 1522 | 58.0 / 75.0 | 50.0 | 0 / 0 | 0 / 0 | 45 | 245209 | 0 | 25% |
| lan-uniform-10k #1 | 10000 / 10000 | 1731 | 74.0 / 93.0 | 50.7 | 257 / 8 | 1145 / 0 | 1314 | 4308 | 0 | 26% |
| lan-blob-10k #1 | 10000 / 10000 | 1668 | 75.0 / 93.0 | 50.7 | 88 / 2 | 130 / 0 | 3007 | 194023 | 0 | 27% |
| typical-uniform-10k #1 | 10000 / 10000 | 2052 | 100.0 / 124.0 | 51.5 | 6395 / 2139 | 9682 / 0 | 3158 | 4505 | 0 | 27% |
| typical-blob-10k #1 | 10000 / 10000 | 1845 | 100.0 / 124.0 | 51.3 | 6584 / 3277 | 11751 / 0 | 5924 | 191258 | 0 | 27% |
| far-uniform-10k #1 | 10000 / 10000 | 2395 | 134.0 / 171.0 | 53.2 | 22791 / 5059 | 29659 / 0 | 6592 | 4182 | 0 | 26% |
| far-blob-10k #1 | 10000 / 10000 | 2119 | 135.0 / 171.0 | 53.4 | 22746 / 8945 | 34262 / 0 | 10787 | 183980 | 0 | 27% |
| lossy-uniform-10k #1 | 10000 / 10000 | 2044 | 102.0 / 144.0 | 53.4 | 66217 / 5346 | 70739 / 0 | 7937 | 4172 | 0 | 26% |
| lossy-blob-10k #1 | 10000 / 10000 | 1864 | 103.0 / 145.0 | 53.9 | 64056 / 2022 | 64607 / 0 | 6210 | 191338 | 0 | 27% |
| jittery-uniform-10k #1 | 10000 / 10000 | 2061 | 95.0 / 148.0 | 55.2 | 69126 / 1794 | 71082 / 0 | 4872 | 4400 | 0 | 25% |
| jittery-blob-10k #1 | 10000 / 10000 | 1852 | 97.0 / 149.0 | 55.4 | 63198 / 335 | 63794 / 0 | 4275 | 173528 | 0 | 26% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| clean-uniform-10k #1 | 99.86 / 0.02 / 0.00 | 99.52 / 0.13 / 0.01 | 40.06 / 58.97 / 0.52 | 0 / 0 / 3677 | 74.9 | 0 | 133 / 167 | 267 / 300 |
| clean-blob-10k #1 | 99.75 / 0.06 / 0.00 | 95.25 / 1.51 / 1.94 | 40.09 / 58.88 / 0.55 | 0 / 0 / 3691 | 77.8 | 0 | 133 / 183 | 267 / 300 |
| lan-uniform-10k #1 | 95.54 / 4.27 / 0.05 | 81.25 / 15.66 / 2.28 | 23.82 / 53.16 / 19.92 | 88 / 1600 / 11001 | 128.8 | 0 | 167 / 264 | 300 / 333 |
| lan-blob-10k #1 | 92.28 / 7.29 / 0.24 | 85.21 / 10.58 / 2.69 | 28.00 / 54.85 / 14.86 | 227 / 970 / 9009 | 129.7 | 0 | 167 / 266 | 300 / 333 |
| typical-uniform-10k #1 | 96.33 / 3.50 / 0.03 | 83.02 / 14.33 / 1.94 | 24.69 / 53.52 / 18.86 | 75 / 1492 / 10761 | 129.4 | 0 | 233 / 300 | 366 / 369 |
| typical-blob-10k #1 | 93.60 / 6.04 / 0.18 | 86.76 / 9.45 / 2.42 | 28.70 / 55.13 / 14.03 | 182 / 898 / 8670 | 129.6 | 0 | 233 / 300 | 366 / 369 |
| far-uniform-10k #1 | 94.99 / 4.81 / 0.06 | 79.80 / 16.64 / 2.69 | 23.00 / 52.79 / 21.01 | 133 / 1716 / 11342 | 132.2 | 0 | 283 / 367 | 414 / 442 |
| far-blob-10k #1 | 92.26 / 7.29 / 0.26 | 84.96 / 10.91 / 2.72 | 27.41 / 54.73 / 15.49 | 250 / 1078 / 9259 | 131.7 | 0 | 285 / 367 | 416 / 443 |
| lossy-uniform-10k #1 | 95.62 / 4.19 / 0.05 | 81.09 / 15.78 / 2.34 | 23.39 / 52.97 / 20.48 | 99 / 1639 / 11120 | 131.2 | 0 | 233 / 300 | 367 / 400 |
| lossy-blob-10k #1 | 92.21 / 7.36 / 0.24 | 85.51 / 10.61 / 2.50 | 27.57 / 54.67 / 15.40 | 246 / 1007 / 9075 | 131.2 | 0 | 233 / 300 | 367 / 400 |
| jittery-uniform-10k #1 | 92.09 / 7.63 / 0.14 | 75.10 / 20.19 / 3.59 | 20.77 / 53.03 / 22.94 | 221 / 1950 / 11836 | 133.1 | 0 | 202 / 276 | 335 / 371 |
| jittery-blob-10k #1 | 88.94 / 10.45 / 0.42 | 80.87 / 14.33 / 3.31 | 24.77 / 55.12 / 17.61 | 352 / 1327 / 10101 | 133.3 | 0 | 202 / 276 | 335 / 371 |

## Network

netem delays each direction once, so the round trip is about twice the delay. Times in ms.

| run | link (one way) | input -> applied p50 / p99 | round trip p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections (per bot-minute) | near decode errors | resyncs | input clock extra / skipped |
|---|---|---|---|---|---|---|---|---|---|---|
| clean-uniform-10k #1 | clean | 58.0 / 74.0 | 67.0 / 100.0 | 49.6 | 0 / 0 | 0 / 0 | 85 (0.009) | 0 | 14 | 14092 / 423 |
| clean-blob-10k #1 | clean | 58.0 / 75.0 | 67.0 / 100.0 | 50.0 | 0 / 0 | 0 / 0 | 45 (0.004) | 0 | 270 | 23004 / 10163 |
| lan-uniform-10k #1 | delay 15ms 2ms distribution normal | 74.0 / 93.0 | 100.0 / 133.0 | 50.7 | 257 / 8 | 1145 / 0 | 1314 (0.131) | 0 | 0 | 28854 / 14235 |
| lan-blob-10k #1 | delay 15ms 2ms distribution normal | 75.0 / 93.0 | 100.0 / 133.0 | 50.7 | 88 / 2 | 130 / 0 | 3007 (0.301) | 0 | 62 | 32381 / 16927 |
| typical-uniform-10k #1 | delay 40ms 5ms distribution normal loss 0.5% | 100.0 / 124.0 | 167.0 / 200.0 | 51.5 | 6395 / 2139 | 9682 / 0 | 3158 (0.316) | 0 | 0 | 53745 / 38294 |
| typical-blob-10k #1 | delay 40ms 5ms distribution normal loss 0.5% | 100.0 / 124.0 | 167.0 / 200.0 | 51.3 | 6584 / 3277 | 11751 / 0 | 5924 (0.592) | 0 | 0 | 59518 / 42160 |
| far-uniform-10k #1 | delay 75ms 10ms distribution normal loss 1% | 134.0 / 171.0 | 233.0 / 267.0 | 53.2 | 22791 / 5059 | 29659 / 0 | 6592 (0.659) | 0 | 0 | 80775 / 63675 |
| far-blob-10k #1 | delay 75ms 10ms distribution normal loss 1% | 135.0 / 171.0 | 233.0 / 267.0 | 53.4 | 22746 / 8945 | 34262 / 0 | 10787 (1.079) | 0 | 0 | 89198 / 70247 |
| lossy-uniform-10k #1 | delay 40ms 5ms distribution normal loss 5% | 102.0 / 144.0 | 167.0 / 200.0 | 53.4 | 66217 / 5346 | 70739 / 0 | 7937 (0.794) | 0 | 0 | 73921 / 57044 |
| lossy-blob-10k #1 | delay 40ms 5ms distribution normal loss 5% | 103.0 / 145.0 | 167.0 / 200.0 | 53.9 | 64056 / 2022 | 64607 / 0 | 6210 (0.621) | 0 | 0 | 81810 / 65541 |
| jittery-uniform-10k #1 | delay 40ms 20ms distribution normal | 95.0 / 148.0 | 167.0 / 233.0 | 55.2 | 69126 / 1794 | 71082 / 0 | 4872 (0.487) | 0 | 0 | 101985 / 86828 |
| jittery-blob-10k #1 | delay 40ms 20ms distribution normal | 97.0 / 149.0 | 167.0 / 233.0 | 55.4 | 63198 / 335 | 63794 / 0 | 4275 (0.427) | 0 | 0 | 111861 / 96722 |
