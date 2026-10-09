# Baseline: wsl2-allowance-flags, 2026-10-09

- **Machine:** AMD Ryzen 7 5700X3D 8-Core Processor, 16 (1 socket(s) x 8 cores x 2 threads); Ubuntu 24.04.4 LTS, kernel 6.18.40.1-microsoft-standard-WSL2, virt: wsl
- **Commit:** 2227f37 (uncommitted changes)
- **Runs:** 3 scenarios x 1, 60 s each, interleaved. Server 8 threads (kept awake through ticks: on; sending during assembly: on; worker CPUs -, receive CPUs -), bots 8 threads, on the same machine over loopback. Took 3 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 | L0 (30 Hz; L0:1709) | 6.41 / 7.38 / 38.21 | 1 | 60 | 2.02 | 1642 | 394.3 | 0 / 0 |
| fight-blob-1k #1 | 1000 | L0 (30 Hz; L0:1708) | 6.41 / 7.44 / 27.40 | 0 | 60 | 2.00 | 1537 | 369.1 | 0 / 0 |
| fight-uniform-5k #1 | 5000 | L6 (20 Hz; L2:12,L3:150,L4:602,L5:41,L6:602) | 25.91 / 35.80 / 60.16 | 11 | 123 | 1.00 | 292 | 57.6 | 0 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | separate | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0.66 / 0.99 | 0.00 / 0.00 | 0.29 / 0.38 | 0.23 / 0.29 | 0.04 / 0.06 | 0.07 / 0.10 | 4.47 / 5.11 | 0.00 / 0.00 | 0.03 / 0.06 |
| fight-blob-1k #1 | 0.69 / 1.00 | 0.00 / 0.00 | 0.26 / 0.33 | 0.21 / 0.28 | 0.03 / 0.06 | 0.07 / 0.11 | 4.51 / 5.20 | 0.00 / 0.00 | 0.03 / 0.26 |
| fight-uniform-5k #1 | 2.74 / 4.91 | 0.00 / 0.00 | 0.62 / 0.96 | 0.37 / 0.45 | 0.04 / 0.08 | 0.05 / 0.10 | 20.22 / 29.23 | 0.00 / 0.00 | 0.04 / 0.59 |

## Phase breakdown (p50)

For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread. Sending during assembly (`send_during_assembly=on`), each shard's assembly task also frames and sends: assembly's columns cover all three, and transport and egress show only their (near-zero) wall time.

| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 8 | 6.41 | 0.66 / 0.12 / 0.53 | 4.47 / 0.88 / 4.06 | 0.00 | 0.03 | 0.54 | 0.23 |
| fight-blob-1k #1 | 8 | 6.41 | 0.69 / 0.12 / 0.54 | 4.51 / 0.84 / 4.08 | 0.00 | 0.03 | 0.58 | 0.21 |
| fight-uniform-5k #1 | 8 | 25.91 | 2.74 / 0.44 / 2.42 | 20.22 / 3.42 / 19.26 | 0.00 | 0.04 | 1.28 | 0.38 |

## Ingress

One receive thread per socket (`--sockets`). recvmmsg gathers for up to `--rx-gather-us` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up).

| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |
|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | recvmmsg (1000 us) | 1 | 30 | 29.87 | 4.5% / 4.5% |
| fight-blob-1k #1 | recvmmsg (1000 us) | 1 | 30 | 29.80 | 4.6% / 4.6% |
| fight-uniform-5k #1 | recvmmsg (1000 us) | 1 | 149 | 54.14 | 16.6% / 16.6% |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 1000 / 1000 | 654 | 100.0 / 141.0 | 50.4 | 130 / 5 | 135 / 0 | 10 | 27036 | 0 | 38% |
| fight-blob-1k #1 | 1000 / 1000 | 624 | 100.0 / 141.0 | 50.4 | 645 / 248 | 893 / 0 | 224 | 32762 | 0 | 42% |
| fight-uniform-5k #1 | 5000 / 5000 | 1006 | 102.0 / 154.0 | 52.2 | 1677 / 0 | 1663 / 0 | 2881 | 1371 | 0 | 58% |

## Smoothness

Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by.

| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 99.79 / 0.03 / 0.00 | 96.80 / 0.96 / 1.33 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 88.4 | 0 | 242 / 342 | 367 / 433 |
| fight-blob-1k #1 | 96.77 / 1.24 / 1.22 | 91.02 / 3.20 / 3.36 | 0.00 / 0.00 / 0.00 | 0 / 0 / 0 | 98.1 | 0 | 242 / 342 | 367 / 433 |
| fight-uniform-5k #1 | 99.50 / 0.26 / 0.09 | 89.52 / 9.40 / 0.33 | 18.64 / 71.94 / 7.63 | 0 / 586 / 14696 | 90.4 | 0 | 234 / 322 | 367 / 437 |

## Fights

Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop.

| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |
|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 0 | 10 ms | 167 | 21.3 | 68001 | 67597 | 99.4 | 0 |
| fight-blob-1k-immortal #1 | 1 | 50 ms | 166 | 101.4 | 66756 | 66270 | 99.3 | 0 |
| fight-blob-1k-immortal #1 | 2 | 75 ms | 167 | 151.3 | 66960 | 65199 | 97.4 | 0 |
| fight-blob-1k #1 | 0 | 10 ms | 167 | 21.5 | 33159 | 7390 | 22.3 | 1436 |
| fight-blob-1k #1 | 1 | 50 ms | 166 | 101.5 | 32981 | 6574 | 19.9 | 1342 |
| fight-blob-1k #1 | 2 | 75 ms | 167 | 151.6 | 33781 | 6300 | 18.6 | 1319 |
| fight-uniform-5k #1 | 0 | 10 ms | 167 | 22.7 | 38340 | 7013 | 18.3 | 1362 |
| fight-uniform-5k #1 | 1 | 50 ms | 167 | 102.6 | 39381 | 7109 | 18.1 | 1397 |
| fight-uniform-5k #1 | 2 | 75 ms | 166 | 152.7 | 38729 | 6449 | 16.7 | 1260 |

| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |
|---|---|---|---|---|---|---|---|---|---|
| fight-blob-1k-immortal #1 | 282880 | 1639 / 205608 | 142 | 0 | 186995 | 0 | 0.35 / 0.54 | 6.41 / 7.38 | 10 |
| fight-blob-1k #1 | 149556 | 2273 / 28883 | 1063 | 10306 | 98857 | 4193 | 0.31 / 0.53 | 6.41 / 7.44 | 224 |
| fight-uniform-5k #1 | 589661 | 1412 / 22292 | 425 | 2384 | 385058 | 4054 | 1.12 / 2.22 | 25.91 / 35.80 | 2881 |
