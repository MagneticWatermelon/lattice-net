# Baseline: aws-fight-trims, 2026-10-07

- **Machine:** AMD EPYC 9R14, 64 (1 socket(s) x 64 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** af340e4 (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 28 threads, bots on a second machine (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), server listening on 172.31.17.93. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1708) | 7.21 / 7.97 / 8.28 | 0 | 60 | 2.01 | 1640 | 393.8 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1709) | 5.09 / 6.61 / 7.08 | 0 | 60 | 2.00 | 1536 | 368.9 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L0 (30 Hz; L0:1708) | 9.87 / 10.85 / 11.30 | 0 | 158 | 1.05 | 811 | 194.6 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.52 / 0.75 | 0.44 / 0.55 | 0.34 / 0.38 | 0.18 / 0.22 | 0.15 / 0.18 | 0.06 / 0.10 | 3.18 / 3.72 | 0.68 / 0.97 | 0.57 / 0.77 |
| fight-blob-1k #1 | 0.53 / 0.65 | 0.43 / 0.56 | 0.33 / 0.37 | 0.18 / 0.22 | 0.15 / 0.39 | 0.06 / 0.10 | 1.36 / 2.62 | 0.33 / 0.54 | 0.55 / 0.78 |
| fight-uniform-5k #1 | 0.81 / 1.13 | 2.05 / 2.58 | 0.62 / 0.87 | 0.35 / 0.42 | 0.34 / 0.65 | 0.21 / 0.27 | 1.35 / 1.87 | 0.92 / 1.26 | 1.31 / 1.61 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 64 | 7.21 | 0.52 / 0.07 / 0.05 | 3.18 / 2.83 / 1.84 | 0.68 / 0.56 / 0.34 | 0.57 / 0.48 / 0.21 | 1.01 | 0.62 |
| fight-blob-1k #1 | 64 | 5.09 | 0.53 / 0.07 / 0.05 | 1.36 / 1.01 / 0.53 | 0.33 / 0.18 / 0.11 | 0.55 / 0.47 / 0.21 | 1.04 | 0.62 |
| fight-uniform-5k #1 | 64 | 9.87 | 0.81 / 0.35 / 0.27 | 1.35 / 0.85 / 0.65 | 0.92 / 0.61 / 0.44 | 1.31 / 1.20 / 0.58 | 1.38 | 2.41 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | recvmmsg (1000 us) | 1 | 30 | 28.41 | 9.2% / 9.2% |
| fight-blob-1k #1 | recvmmsg (1000 us) | 1 | 30 | 28.05 | 5.2% / 5.2% |
| fight-uniform-5k #1 | recvmmsg (1000 us) | 1 | 150 | 51.69 | 15.0% / 15.0% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 653 | 105.0 / 145.0 | 51.3 | 0 / 0 | 0 / 0 | 0 | 25419 | 0 | 10% |
| fight-blob-1k #1 | 1000 / 1000 | 613 | 103.0 / 143.0 | 49.5 | 0 / 0 | 0 / 0 | 0 | 35077 | 0 | 12% |
| fight-uniform-5k #1 | 5000 / 5000 | 1191 | 105.0 / 146.0 | 50.5 | 0 / 0 | 0 / 0 | 0 | 1035 | 0 | 15% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.74 / 0.08 / 0.00 | 96.87 / 0.95 / 1.29 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 86.5 | 0 | 242 / 342 | 367 / 433 |
| fight-blob-1k #1 | 96.58 / 1.36 / 1.28 | 90.77 / 3.26 / 3.51 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 92.4 | 0 | 242 / 342 | 367 / 433 |
| fight-uniform-5k #1 | 98.31 / 1.50 / 0.03 | 90.72 / 7.95 / 0.85 | 30.18 / 55.54 / 12.37 | 2 / 767 / 7971 | 120.3 | 0 | 233 / 367 | 367 / 433 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 27.0 | 66574 | 66178 | 99.4 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 107.0 | 65269 | 64837 | 99.3 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 156.9 | 65556 | 63661 | 97.1 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 25.0 | 31903 | 7210 | 22.6 | 1403 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 105.0 | 32429 | 6767 | 20.9 | 1345 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 155.1 | 33488 | 6118 | 18.3 | 1300 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 30.1 | 33672 | 6026 | 17.9 | 1181 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 110.0 | 37423 | 6436 | 17.2 | 1261 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 160.6 | 38378 | 6328 | 16.5 | 1219 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 279586 | 1564 / 201287 | 148 | 0 | 185448 | 0 | 1.04 / 1.23 | 7.21 / 7.97 | 0 |
| fight-blob-1k #1 | 149425 | 2077 / 28911 | 961 | 10200 | 99342 | 4157 | 1.09 / 1.41 | 5.09 / 6.61 | 0 |
| fight-uniform-5k #1 | 568210 | 1276 / 20235 | 436 | 2022 | 379357 | 3696 | 1.71 / 2.14 | 9.87 / 10.85 | 0 |
