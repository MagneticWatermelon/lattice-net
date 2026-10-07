# Baseline: aws-limits, 2026-10-07

- **Machine:** AMD EPYC 9R14, 64 (1 socket(s) x 64 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** af340e4 (uncommitted changes)
- **Runs:** 4 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 28 threads, bots on a second machine (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), server listening on 172.31.17.93. Took 16 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 10000 | L0 (30 Hz; L0:7107) | 13.96 / 25.21 / 61.76 | 1 | 354 | 2.00 | 1544 | 370.5 | 0 / 0 |
| pile-25m-10k #1 | 10000 | L0 (30 Hz; L0:7107) | 14.01 / 25.66 / 62.42 | 1 | 354 | 2.00 | 1544 | 370.5 | 0 / 0 |
| disk-200m-10k-noladder #1 | 10000 | L0 (30 Hz; L0:7108) | 13.49 / 23.35 / 26.37 | 0 | 354 | 2.00 | 1503 | 360.8 | 0 / 0 |
| uniform-20k-noladder #1 | 20000 | L0 (30 Hz; L0:6794) | 21.51 / 44.21 / 48.60 | 1782 | 596 | 1.81 | 1609 | 369.0 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 0.91 / 2.09 | 2.76 / 5.73 | 0.65 / 1.14 | 0.41 / 0.68 | 0.77 / 1.27 | 0.25 / 0.44 | 3.82 / 7.46 | 1.45 / 2.84 | 2.75 / 5.16 |
| pile-25m-10k #1 | 0.92 / 2.00 | 2.82 / 5.94 | 0.66 / 1.19 | 0.42 / 0.70 | 0.78 / 1.35 | 0.25 / 0.47 | 3.78 / 7.53 | 1.47 / 2.88 | 2.82 / 5.24 |
| disk-200m-10k-noladder #1 | 0.91 / 2.13 | 2.80 / 5.74 | 0.66 / 1.18 | 0.30 / 0.50 | 0.57 / 1.04 | 0.24 / 0.48 | 3.48 / 6.98 | 1.49 / 2.58 | 2.88 / 5.03 |
| uniform-20k-noladder #1 | 1.50 / 3.40 | 5.41 / 10.07 | 1.10 / 1.98 | 0.52 / 0.93 | 0.56 / 1.16 | 0.33 / 0.71 | 4.11 / 11.14 | 2.42 / 5.22 | 5.15 / 14.49 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 64 | 13.96 | 0.91 / 0.45 / 0.34 | 3.82 / 2.68 / 2.02 | 1.45 / 1.10 / 0.82 | 2.75 / 2.62 / 1.30 | 2.08 | 3.19 |
| pile-25m-10k #1 | 64 | 14.01 | 0.92 / 0.45 / 0.34 | 3.78 / 2.63 / 2.03 | 1.47 / 1.11 / 0.82 | 2.82 / 2.67 / 1.32 | 2.13 | 3.26 |
| disk-200m-10k-noladder #1 | 64 | 13.49 | 0.91 / 0.45 / 0.34 | 3.48 / 2.41 / 1.87 | 1.49 / 1.15 / 0.86 | 2.88 / 2.75 / 1.33 | 2.00 | 3.12 |
| uniform-20k-noladder #1 | 64 | 21.51 | 1.50 / 0.94 / 0.72 | 4.11 / 2.94 / 2.24 | 2.42 / 2.06 / 1.59 | 5.15 / 4.95 / 2.35 | 2.29 | 5.96 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | recvmmsg (1000 us) | 1 | 177 | 52.98 | 18.1% / 18.1% |
| pile-25m-10k #1 | recvmmsg (1000 us) | 1 | 177 | 53.25 | 18.3% / 18.3% |
| disk-200m-10k-noladder #1 | recvmmsg (1000 us) | 1 | 177 | 52.80 | 18.2% / 18.2% |
| uniform-20k-noladder #1 | recvmmsg (1000 us) | 1 | 303 | 54.24 | 32.5% / 32.5% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 10000 / 10000 | 165 | 58.0 / 77.0 | 49.9 | 0 / 0 | 0 / 0 | 0 | 41181825 | 0 | 14% |
| pile-25m-10k #1 | 10000 / 10000 | 165 | 58.0 / 77.0 | 49.9 | 0 / 0 | 0 / 0 | 0 | 41182879 | 0 | 14% |
| disk-200m-10k-noladder #1 | 10000 / 10000 | 165 | 57.0 / 76.0 | 49.9 | 0 / 0 | 0 / 0 | 0 | 4534399 | 0 | 13% |
| uniform-20k-noladder #1 | 20000 / 20000 | 221 | 67.0 / 115.0 | 55.4 | 3799217 / 1573449 | 3479338 / 0 | 1303603 | 38575 | 0 | 28% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 98.11 / 0.54 / 0.00 | 52.59 / 17.55 / 17.64 | 0.00 / 0.00 / 0.00 | 0 / 118 / 0 | 87.0 | 0 | 133 / 200 | 267 / 300 |
| pile-25m-10k #1 | 98.11 / 0.54 / 0.00 | 52.67 / 17.52 / 17.61 | 0.00 / 0.00 / 0.00 | 0 / 119 / 0 | 87.2 | 0 | 133 / 200 | 267 / 300 |
| disk-200m-10k-noladder #1 | 99.76 / 0.11 / 0.00 | 89.60 / 3.89 / 3.67 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 83.3 | 0 | 133 / 175 | 267 / 300 |
| uniform-20k-noladder #1 | 99.89 / 0.01 / 0.00 | 99.34 / 0.22 / 0.12 | 40.18 / 58.75 / 0.67 | 0 / 0 / 3884 | 89.0 | 0 | 149 / 181 | 282 / 307 |

## Ramps

Players join at a steady rate, then hold. One row per step of players (the first 5 s window that reaches it). Times in ms; phases are p50.

### pile-25m-10k-noladder #1 (10000 players, 50 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 1223 | L0 (30 Hz) | 3.8 / 4.5 | 0.6 | 0.5 | 0.8 | 0.3 | 0.6 |
| 2224 | L0 (30 Hz) | 5.8 / 6.7 | 0.6 | 1.0 | 1.3 | 0.6 | 1.0 |
| 3224 | L0 (30 Hz) | 7.9 / 8.8 | 0.7 | 1.4 | 1.9 | 0.8 | 1.5 |
| 4224 | L0 (30 Hz) | 10.0 / 11.1 | 0.8 | 1.9 | 2.5 | 1.0 | 1.9 |
| 5228 | L0 (30 Hz) | 11.9 / 13.2 | 0.8 | 2.3 | 3.1 | 1.2 | 2.4 |
| 6227 | L0 (30 Hz) | 14.0 / 15.6 | 0.9 | 2.7 | 3.9 | 1.4 | 2.8 |
| 7231 | L0 (30 Hz) | 16.0 / 17.0 | 1.0 | 3.2 | 4.6 | 1.6 | 3.2 |
| 8228 | L0 (30 Hz) | 18.3 / 19.6 | 1.0 | 3.7 | 5.3 | 1.9 | 3.7 |
| 9233 | L0 (30 Hz) | 20.5 / 22.8 | 1.1 | 4.2 | 6.0 | 2.1 | 4.2 |
| 10000 | L0 (30 Hz) | 22.6 / 24.6 | 1.2 | 4.7 | 6.6 | 2.3 | 4.8 |

- **Within budget** all the way to 10000 players (p99 25.2 ms at the peak).

### pile-25m-10k #1 (10000 players, 50 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 1223 | L0 (30 Hz) | 3.8 / 4.5 | 0.6 | 0.5 | 0.8 | 0.3 | 0.6 |
| 2224 | L0 (30 Hz) | 5.9 / 6.5 | 0.6 | 1.0 | 1.3 | 0.6 | 1.1 |
| 3224 | L0 (30 Hz) | 7.9 / 8.9 | 0.7 | 1.4 | 1.9 | 0.9 | 1.5 |
| 4224 | L0 (30 Hz) | 10.0 / 11.0 | 0.8 | 1.9 | 2.4 | 1.0 | 2.0 |
| 5226 | L0 (30 Hz) | 12.2 / 13.4 | 0.8 | 2.3 | 3.2 | 1.3 | 2.4 |
| 6225 | L0 (30 Hz) | 14.0 / 15.6 | 0.9 | 2.8 | 3.8 | 1.4 | 2.8 |
| 7229 | L0 (30 Hz) | 16.4 / 18.2 | 1.0 | 3.3 | 4.6 | 1.7 | 3.3 |
| 8228 | L0 (30 Hz) | 18.6 / 20.2 | 1.1 | 3.8 | 5.3 | 1.9 | 3.8 |
| 9233 | L0 (30 Hz) | 20.8 / 22.5 | 1.2 | 4.2 | 6.0 | 2.1 | 4.3 |
| 10000 | L0 (30 Hz) | 23.0 / 25.5 | 1.2 | 4.7 | 6.8 | 2.3 | 4.8 |

- **Within budget** all the way to 10000 players (p99 25.3 ms at the peak).

### disk-200m-10k-noladder #1 (10000 players, 50 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 1223 | L0 (30 Hz) | 3.6 / 4.3 | 0.5 | 0.5 | 0.8 | 0.3 | 0.6 |
| 2224 | L0 (30 Hz) | 5.5 / 6.2 | 0.6 | 1.0 | 1.1 | 0.6 | 1.0 |
| 3226 | L0 (30 Hz) | 7.5 / 8.4 | 0.7 | 1.4 | 1.6 | 0.9 | 1.5 |
| 4225 | L0 (30 Hz) | 9.4 / 10.6 | 0.8 | 1.9 | 2.1 | 1.1 | 2.0 |
| 5228 | L0 (30 Hz) | 11.5 / 12.6 | 0.8 | 2.3 | 2.8 | 1.3 | 2.4 |
| 6227 | L0 (30 Hz) | 13.5 / 14.8 | 0.9 | 2.8 | 3.5 | 1.5 | 2.9 |
| 7229 | L0 (30 Hz) | 15.5 / 17.0 | 1.0 | 3.2 | 4.2 | 1.7 | 3.3 |
| 8227 | L0 (30 Hz) | 17.6 / 19.3 | 1.0 | 3.7 | 4.9 | 1.9 | 3.8 |
| 9233 | L0 (30 Hz) | 20.0 / 21.5 | 1.1 | 4.3 | 5.6 | 2.1 | 4.2 |
| 10000 | L0 (30 Hz) | 22.0 / 24.1 | 1.2 | 4.7 | 6.3 | 2.3 | 4.7 |

- **Within budget** all the way to 10000 players (p99 23.8 ms at the peak).

### uniform-20k-noladder #1 (20000 players, 100 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 2457 | L0 (30 Hz) | 4.4 / 5.0 | 0.6 | 1.0 | 0.6 | 0.3 | 0.6 |
| 4462 | L0 (30 Hz) | 7.1 / 7.8 | 0.8 | 1.9 | 1.0 | 0.7 | 1.0 |
| 6463 | L0 (30 Hz) | 10.0 / 11.3 | 0.9 | 2.8 | 1.7 | 1.1 | 1.6 |
| 8463 | L0 (30 Hz) | 14.2 / 15.6 | 1.1 | 3.8 | 2.4 | 1.6 | 2.9 |
| 10463 | L0 (30 Hz) | 18.2 / 20.3 | 1.2 | 4.7 | 3.3 | 2.1 | 4.2 |
| 12466 | L0 (30 Hz) | 22.9 / 25.7 | 1.9 | 5.5 | 4.5 | 2.6 | 5.6 |
| 14465 | L0 (30 Hz) | 27.2 / 29.2 | 2.2 | 6.3 | 5.6 | 3.1 | 6.8 |
| 16467 | L0 (30 Hz) | 31.7 / 36.4 | 2.4 | 7.2 | 6.9 | 3.6 | 8.2 |
| 18472 | L0 (30 Hz) | 36.9 / 41.2 | 2.5 | 7.9 | 8.3 | 4.3 | 9.8 |
| 20000 | L0 (30 Hz) | 41.1 / 46.7 | 2.4 | 8.0 | 9.8 | 4.9 | 11.8 |

- **Over budget:** tick p99 first exceeded its period (33.3 ms at 30 Hz) with 15961 players.

## Profiles

- `pile-25m-10k-1/perf.txt`
- `pile-25m-10k-noladder-1/perf.txt`
