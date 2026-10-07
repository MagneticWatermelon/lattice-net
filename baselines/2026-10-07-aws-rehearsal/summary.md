# Baseline: aws-rehearsal, 2026-10-07

- **Machine:** AMD EPYC 9R14, 8 (1 socket(s) x 8 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** d7902fa (uncommitted changes)
- **Runs:** 2 scenarios x 1, 20 s each, interleaved. Server 8 threads, bots 4 threads, bots on a second machine (AMD EPYC 9R14), server listening on 172.31.24.165. Took 0 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 1000 | L0 (30 Hz; L0:508) | 2.24 / 2.42 / 2.54 | 0 | 30 | 1.00 | 245 | 58.9 | 0 / 0 |
| blob-1k-gso #1 | 1000 | L0 (30 Hz; L0:509) | 3.87 / 4.18 / 4.37 | 0 | 60 | 2.00 | 1482 | 356.3 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 0.27 / 0.32 | 0.24 / 0.27 | 0.24 / 0.27 | 0.17 / 0.20 | 0.04 / 0.07 | 0.05 / 0.07 | 0.46 / 0.56 | 0.22 / 0.27 | 0.53 / 0.61 |
| blob-1k-gso #1 | 0.23 / 0.28 | 0.23 / 0.26 | 0.15 / 0.18 | 0.15 / 0.17 | 0.08 / 0.11 | 0.04 / 0.05 | 1.63 / 1.90 | 0.54 / 0.61 | 0.80 / 0.89 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 8 | 2.24 | 0.27 / 0.04 / 0.16 | 0.46 / 0.07 / 0.36 | 0.22 / 0.04 / 0.19 | 0.53 / 0.10 / 0.46 | 0.31 | 0.41 |
| blob-1k-gso #1 | 8 | 3.87 | 0.23 / 0.04 / 0.16 | 1.63 / 0.30 / 1.48 | 0.54 / 0.13 / 0.49 | 0.80 / 0.17 / 0.73 | 0.34 | 0.38 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 1000 / 1000 | 458 | 41.0 / 69.0 | 40.0 | 0 / 0 | 0 / 0 | 0 | 28 | 0 | 7% |
| blob-1k-gso #1 | 1000 / 1000 | 423 | 61.0 / 88.0 | 58.6 | 0 / 0 | 0 / 0 | 0 | 8509 | 0 | 13% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 99.66 / 0.01 / 0.00 | 98.89 / 0.10 / 0.01 | 39.40 / 59.11 / 0.42 | 0 / 0 / 3663 | 70.2 | 0 | 133 / 167 | 267 / 300 |
| blob-1k-gso #1 | 99.19 / 0.46 / 0.00 | 90.93 / 1.89 / 5.47 | 0.00 / 0.00 / 0.00 | 3 / 0 / 0 | 111.3 | 0 | 133 / 200 | 267 / 300 |
