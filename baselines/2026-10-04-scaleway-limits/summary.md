# Baseline: scaleway-limits, 2026-10-04

- **Machine:** AMD EPYC 8534P 64-Core Processor, 128 (1 socket(s) x 64 cores x 2 threads); Ubuntu 24.04.3 LTS, kernel 6.8.0-88-generic, virt: none
- **Commit:** 5ad9714 (uncommitted changes)
- **Runs:** 4 scenarios x 1, 60 s each, interleaved. Server 64 threads, bots 24 threads, bots on a second machine (AMD EPYC 8224P 24-Core Processor), server listening on 172.16.16.2. Took 16 min.
- **Setup and checks:** `env.txt`. Raw logs, per-window CSVs and key=value summaries: one directory per run.

Steady state: from 3 s after the first client until clients start leaving. Times in ms.

## Server

| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |
|---|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 10000 | L0 (30 Hz; L0:6244) | 21.05 / 59.21 / 62.61 | 1966 | 284 | 2.00 | 1477 | 311.3 | 0 / 0 |
| pile-25m-10k #1 | 10000 | L0 (30 Hz; L0:3759,L1:49,L2:31,L3:31,L4:104,L5:41,L6:730,L7:181,L8:905) | 19.26 / 55.69 / 60.86 | 0 | 255 | 2.00 | 1404 | 276.5 | 0 / 0 |
| disk-200m-10k-noladder #1 | 10000 | L0 (30 Hz; L0:7108) | 11.41 / 18.95 / 20.23 | 0 | 354 | 2.00 | 1423 | 341.6 | 0 / 0 |
| uniform-20k-noladder #1 | 20000 | L0 (30 Hz; L0:6898) | 15.52 / 68.34 / 137.10 | 445 | 610 | 1.81 | 1585 | 369.2 | 327246 / 0 |

## Phases (p50 / p99)

| run | ingress | events | movement | grid | serialize | assembly | transport | egress |
|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 0.91 / 1.54 | 1.69 / 3.46 | 0.23 / 0.36 | 0.22 / 0.38 | 0.14 / 0.24 | 13.92 / 47.02 | 1.16 / 2.20 | 2.63 / 6.56 |
| pile-25m-10k #1 | 0.86 / 1.96 | 1.57 / 4.95 | 0.23 / 0.39 | 0.21 / 0.38 | 0.14 / 0.24 | 12.50 / 41.12 | 1.11 / 2.24 | 2.51 / 5.48 |
| disk-200m-10k-noladder #1 | 1.11 / 1.77 | 1.94 / 3.53 | 0.26 / 0.36 | 0.22 / 0.34 | 0.15 / 0.23 | 3.94 / 7.61 | 1.33 / 2.24 | 2.42 / 3.40 |
| uniform-20k-noladder #1 | 1.91 / 4.56 | 3.34 / 8.48 | 0.35 / 0.61 | 0.43 / 0.67 | 0.21 / 0.39 | 3.37 / 10.24 | 2.28 / 4.89 | 3.48 / 42.66 |

## Clients

| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | near decode errors | swarm busy |
|---|---|---|---|---|---|---|---|---|---|
| pile-25m-10k-noladder #1 | 10000 / 10000 | 243 | 83.0 / 159.0 | 65.7 | 0 / 0 | 0 / 0 | 0 | 0 | 10% |
| pile-25m-10k #1 | 10000 / 10000 | 259 | 71.0 / 128.0 | 57.3 | 8 / 0 | 8 / 0 | 0 | 0 | 9% |
| disk-200m-10k-noladder #1 | 10000 / 10000 | 165 | 58.0 / 77.0 | 49.7 | 0 / 0 | 0 / 0 | 0 | 0 | 11% |
| uniform-20k-noladder #1 | 20000 / 20000 | 242 | 66.0 / 199.0 | 52.8 | 847403 / 135406 | 973841 / 3 | 163353 | 0 | 22% |

## Ramps

Players join at a steady rate, then hold. One row per step of players (the first 5 s window that reaches it). Times in ms; phases are p50.

### pile-25m-10k-noladder #1 (10000 players, 50 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 1217 | L0 (30 Hz) | 4.0 / 4.5 | 0.4 | 0.3 | 1.7 | 0.4 | 0.9 |
| 2218 | L0 (30 Hz) | 6.9 / 7.8 | 0.5 | 0.7 | 3.3 | 0.6 | 1.5 |
| 3219 | L0 (30 Hz) | 10.6 / 11.9 | 0.6 | 0.9 | 5.8 | 0.8 | 2.2 |
| 4217 | L0 (30 Hz) | 14.7 / 15.6 | 0.8 | 1.3 | 8.8 | 1.0 | 2.1 |
| 5217 | L0 (30 Hz) | 19.6 / 21.6 | 0.9 | 1.6 | 12.7 | 1.1 | 2.5 |
| 6218 | L0 (30 Hz) | 25.3 / 27.7 | 1.0 | 1.9 | 17.6 | 1.3 | 2.9 |
| 7217 | L0 (30 Hz) | 31.8 / 34.5 | 1.1 | 2.3 | 23.0 | 1.4 | 3.3 |
| 8218 | L0 (30 Hz) | 39.5 / 41.6 | 1.2 | 2.6 | 29.7 | 1.6 | 3.5 |
| 9221 | L0 (30 Hz) | 46.9 / 52.5 | 1.3 | 2.9 | 35.7 | 1.8 | 4.4 |
| 10000 | L0 (30 Hz) | 55.5 / 60.2 | 1.3 | 3.1 | 43.1 | 2.1 | 4.9 |

- **Over budget:** tick p99 first exceeded its period (33.3 ms at 30 Hz) with 7217 players.

### pile-25m-10k #1 (10000 players, 50 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 1217 | L0 (30 Hz) | 3.9 / 4.4 | 0.4 | 0.3 | 1.6 | 0.4 | 0.9 |
| 2216 | L0 (30 Hz) | 6.9 / 8.2 | 0.5 | 0.6 | 3.3 | 0.6 | 1.5 |
| 3219 | L0 (30 Hz) | 10.6 / 11.7 | 0.7 | 0.9 | 5.9 | 0.8 | 1.8 |
| 4220 | L0 (30 Hz) | 14.5 / 15.9 | 0.8 | 1.3 | 9.0 | 1.0 | 1.9 |
| 5220 | L0 (30 Hz) | 19.5 / 21.5 | 0.9 | 1.6 | 12.8 | 1.1 | 2.5 |
| 6223 | L0 (30 Hz) | 25.1 / 26.8 | 1.0 | 1.9 | 17.4 | 1.3 | 2.8 |
| 7220 | L6 (20 Hz) | 30.3 / 32.7 | 1.5 | 2.9 | 20.6 | 1.5 | 3.5 |
| 8223 | L6 (20 Hz) | 37.7 / 40.2 | 1.6 | 3.5 | 26.6 | 1.6 | 3.7 |
| 9228 | L8 (20 Hz) | 45.3 / 49.4 | 1.7 | 3.9 | 32.7 | 1.8 | 4.2 |
| 10000 | L8 (20 Hz) | 53.4 / 57.3 | 1.8 | 4.2 | 39.4 | 2.1 | 4.9 |

- **Over budget:** tick p99 first exceeded its period (50.0 ms at 20 Hz) with 9729 players.
- **The ladder** left level 0 at 6470 players and was at level 8 at the peak.

### disk-200m-10k-noladder #1 (10000 players, 50 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 1217 | L0 (30 Hz) | 3.3 / 3.7 | 0.4 | 0.4 | 1.0 | 0.3 | 0.9 |
| 2216 | L0 (30 Hz) | 5.4 / 6.0 | 0.5 | 0.7 | 1.7 | 0.6 | 1.5 |
| 3219 | L0 (30 Hz) | 7.3 / 8.1 | 0.6 | 1.0 | 2.5 | 0.8 | 2.0 |
| 4224 | L0 (30 Hz) | 8.4 / 9.1 | 0.8 | 1.3 | 2.8 | 1.0 | 1.9 |
| 5227 | L0 (30 Hz) | 9.9 / 10.5 | 1.0 | 1.6 | 3.3 | 1.1 | 2.3 |
| 6226 | L0 (30 Hz) | 11.4 / 12.1 | 1.1 | 1.9 | 4.0 | 1.3 | 2.4 |
| 7230 | L0 (30 Hz) | 12.9 / 13.7 | 1.3 | 2.2 | 4.7 | 1.6 | 2.4 |
| 8230 | L0 (30 Hz) | 14.7 / 15.3 | 1.4 | 2.8 | 5.6 | 1.8 | 2.5 |
| 9228 | L0 (30 Hz) | 16.5 / 17.3 | 1.6 | 2.9 | 6.4 | 2.0 | 2.8 |
| 10000 | L0 (30 Hz) | 18.5 / 19.1 | 1.7 | 3.2 | 7.4 | 2.2 | 3.1 |

- **Within budget** all the way to 10000 players (p99 19.3 ms at the peak).

### uniform-20k-noladder #1 (20000 players, 100 joins/s)

| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |
|---|---|---|---|---|---|---|---|
| 2447 | L0 (30 Hz) | 3.4 / 3.8 | 0.5 | 0.7 | 0.6 | 0.3 | 0.9 |
| 4448 | L0 (30 Hz) | 5.6 / 6.3 | 0.8 | 1.2 | 1.0 | 0.6 | 1.5 |
| 6457 | L0 (30 Hz) | 7.4 / 7.9 | 1.1 | 1.9 | 1.4 | 0.9 | 1.2 |
| 8455 | L0 (30 Hz) | 10.2 / 11.1 | 1.5 | 2.2 | 2.0 | 1.4 | 2.1 |
| 10455 | L0 (30 Hz) | 13.4 / 14.3 | 1.8 | 3.0 | 2.7 | 2.0 | 3.0 |
| 12457 | L0 (30 Hz) | 16.3 / 17.3 | 2.0 | 3.7 | 3.5 | 2.4 | 3.7 |
| 14452 | L0 (30 Hz) | 19.0 / 19.9 | 2.3 | 4.1 | 4.5 | 2.8 | 4.2 |
| 16454 | L0 (30 Hz) | 22.4 / 23.7 | 2.2 | 4.8 | 5.3 | 3.2 | 5.8 |
| 18457 | L0 (30 Hz) | 27.1 / 58.4 | 2.2 | 5.3 | 6.5 | 3.8 | 7.6 |
| 20000 | L0 (30 Hz) | 31.1 / 86.9 | 2.3 | 5.1 | 7.6 | 4.5 | 9.4 |

- **Over budget:** tick p99 first exceeded its period (33.3 ms at 30 Hz) with 17954 players.

## Profiles

- `pile-25m-10k-1/perf.txt`
- `pile-25m-10k-noladder-1/perf.txt`
