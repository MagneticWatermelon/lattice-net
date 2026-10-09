# Baseline: wsl2-input-clock-before, 2026-10-09

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** b28543d (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 8 threads (kept awake through ticks: on; sending during assembly: on; worker CPUs -, receive CPUs -), bots 8 threads, on the same machine over loopback. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1709) | 6.20 / 7.00 / 8.76 | 0 | 61 | 2.02 | 1639 | 393.6 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 6.25 / 7.02 / 8.47 | 0 | 60 | 2.00 | 1539 | 369.6 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L5 (30 Hz; L2:19,L3:447,L4:39,L5:577,L6:416) | 25.18 / 34.77 / 69.40 | 11 | 131 | 1.00 | 306 | 64.4 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.64 / 0.91 | 0.00 / 0.00 | 0.27 / 0.34 | 0.19 / 0.27 | 0.04 / 0.05 | 0.05 / 0.10 | 4.39 / 4.99 | 0.00 / 0.00 | 0.03 / 0.07 |
| fight-blob-1k #1 | 0.65 / 0.96 | 0.00 / 0.00 | 0.25 / 0.33 | 0.23 / 0.30 | 0.04 / 0.06 | 0.07 / 0.10 | 4.43 / 5.00 | 0.00 / 0.00 | 0.03 / 0.06 |
| fight-uniform-5k #1 | 2.41 / 3.80 | 0.00 / 0.00 | 0.54 / 0.90 | 0.36 / 0.43 | 0.04 / 0.08 | 0.05 / 0.10 | 20.26 / 28.81 | 0.00 / 0.00 | 0.03 / 0.32 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread. Sending during assembly (`send_during_assembly=on`), each shard's assembly task also frames and sends: assembly's columns cover all three, and transport and egress show only their (near-zero) wall time.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 8 | 6.20 | 0.64 / 0.12 / 0.53 | 4.39 / 0.86 / 4.01 | 0.00 | 0.03 | 0.49 | 0.19 |
| fight-blob-1k #1 | 8 | 6.25 | 0.65 / 0.11 / 0.51 | 4.43 / 0.85 / 4.01 | 0.00 | 0.03 | 0.56 | 0.23 |
| fight-uniform-5k #1 | 8 | 25.18 | 2.41 / 0.38 / 2.12 | 20.26 / 3.40 / 19.31 | 0.00 | 0.03 | 1.24 | 0.37 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | recvmmsg (1000 us) | 1 | 30 | 29.64 | 4.4% / 4.4% |
| fight-blob-1k #1 | recvmmsg (1000 us) | 1 | 30 | 29.65 | 4.5% / 4.5% |
| fight-uniform-5k #1 | recvmmsg (1000 us) | 1 | 149 | 53.85 | 16.7% / 16.7% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 627 | 100.0 / 142.0 | 50.2 | 0 / 0 | 0 / 0 | 0 | 25775 | 0 | 37% |
| fight-blob-1k #1 | 1000 / 1000 | 626 | 101.0 / 141.0 | 50.3 | 0 / 0 | 0 / 0 | 0 | 35910 | 0 | 41% |
| fight-uniform-5k #1 | 5000 / 5000 | 1027 | 102.0 / 154.0 | 51.3 | 1433 / 0 | 1433 / 0 | 30 | 1237 | 0 | 59% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.79 / 0.03 / 0.00 | 96.90 / 0.94 / 1.25 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 84.5 | 0 | 242 / 342 | 367 / 433 |
| fight-blob-1k #1 | 96.75 / 1.26 / 1.25 | 90.88 / 3.23 / 3.46 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 89.0 | 0 | 242 / 333 | 367 / 433 |
| fight-uniform-5k #1 | 99.56 / 0.22 / 0.06 | 92.54 / 6.45 / 0.27 | 22.92 / 68.85 / 6.45 | 0 / 482 / 13254 | 88.4 | 0 | 233 / 320 | 367 / 435 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 21.3 | 65405 | 64999 | 99.4 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 101.3 | 64675 | 64289 | 99.4 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 151.3 | 64958 | 63662 | 98.0 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 21.4 | 33455 | 7558 | 22.6 | 1446 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 101.5 | 33342 | 6660 | 20.0 | 1370 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 151.5 | 31081 | 5839 | 18.8 | 1198 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 22.7 | 40176 | 7540 | 18.8 | 1480 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 102.8 | 38637 | 6881 | 17.8 | 1340 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 152.8 | 38743 | 6631 | 17.1 | 1299 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 275204 | 1562 / 199388 | 127 | 0 | 183398 | 0 | 0.27 / 0.48 | 6.20 / 7.00 | 0 |
| fight-blob-1k #1 | 149592 | 2118 / 28518 | 1055 | 10013 | 97875 | 4108 | 0.29 / 0.48 | 6.25 / 7.02 | 0 |
| fight-uniform-5k #1 | 588888 | 1428 / 22924 | 401 | 2537 | 382879 | 4159 | 1.03 / 2.11 | 25.18 / 34.77 | 30 |
