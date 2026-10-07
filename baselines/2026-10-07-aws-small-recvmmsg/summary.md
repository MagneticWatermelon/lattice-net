# Baseline: aws-small-recvmmsg, 2026-10-07

- **Machine:** AMD EPYC 9R14, 8 (1 socket(s) x 8 cores x 1 threads); Ubuntu 24.04.5 LTS, kernel 7.0.0-1014-aws, virt: amazon
- **Commit:** 1311636 (uncommitted changes)
- **Runs:** 2 scenarios x 1, 20 s each, interleaved. Server 8 threads, bots 4 threads, bots on a second machine (AMD EPYC 9R14), server listening on 172.31.27.101. Took 0 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 1000 | L0 (30 Hz; L0:508) | 2.07 / 2.21 / 2.29 | 0 | 30 | 1.00 | 245 | 58.8 | 0 / 0 |
| blob-1k-gso #1 | 1000 | L0 (30 Hz; L0:508) | 3.75 / 3.93 / 3.96 | 0 | 60 | 2.00 | 1481 | 356.1 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 0.21 / 0.23 | 0.18 / 0.21 | 0.22 / 0.24 | 0.16 / 0.18 | 0.05 / 0.07 | 0.04 / 0.06 | 0.43 / 0.48 | 0.23 / 0.27 | 0.54 / 0.62 |
| blob-1k-gso #1 | 0.21 / 0.24 | 0.17 / 0.19 | 0.15 / 0.17 | 0.15 / 0.16 | 0.07 / 0.09 | 0.04 / 0.04 | 1.57 / 1.70 | 0.53 / 0.60 | 0.82 / 0.90 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 8 | 2.07 | 0.21 / 0.03 / 0.15 | 0.43 / 0.07 / 0.36 | 0.23 / 0.05 / 0.20 | 0.54 / 0.10 / 0.44 | 0.26 | 0.34 |
| blob-1k-gso #1 | 8 | 3.75 | 0.21 / 0.04 / 0.15 | 1.57 / 0.35 / 1.43 | 0.53 / 0.14 / 0.49 | 0.82 / 0.19 / 0.74 | 0.32 | 0.32 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| uniform-1k #1 | recvmmsg (1000 us) | 1 | 30 | 46.78 | 2.6% / 2.6% |
| blob-1k-gso #1 | recvmmsg (1000 us) | 1 | 30 | 40.09 | 2.7% / 2.7% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 1000 / 1000 | 557 | 40.0 / 71.0 | 38.1 | 0 / 0 | 0 / 0 | 0 | 16 | 0 | 7% |
| blob-1k-gso #1 | 1000 / 1000 | 591 | 41.0 / 71.0 | 38.0 | 0 / 0 | 0 / 0 | 0 | 9435 | 0 | 13% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| uniform-1k #1 | 99.67 / 0.00 / 0.00 | 98.99 / 0.11 / 0.01 | 39.39 / 59.19 / 0.45 | 0 / 0 / 3634 | 92.1 | 0 | 133 / 195 | 267 / 300 |
| blob-1k-gso #1 | 99.24 / 0.21 / 0.00 | 90.23 / 2.17 / 5.69 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 105.7 | 0 | 133 / 175 | 267 / 267 |
