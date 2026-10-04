# Hardware-level optimisation of the per-client k-nearest search (cache, layout, SIMD, k-selection)

Scope: constant-factor (hardware) costs of the inner loop in `sim/src/server.rs` `assemble()` (~lines 990-1060) and `sim/src/grid.rs` `walk_rings`. Algorithmic fixes for the quadratic pile (finer or adaptive grids, sharing work between clients) belong to other notes. What these notes show is that hardware tuning gives about 3-5x on this loop, but cannot remove the O(N) cost per query in a 10k pile.

**Local measurement used throughout ("local bench").** The bench is a standalone Rust program, saved as `research_notes/Nearest player search algorithms/cache_simd_selection_bench.rs`. It was run on 2026-10-04 on the WSL2 dev box (AMD Ryzen 7 5700X3D, Zen 3), built with `rustc -C opt-level=3 -C target-cpu=native` and also without `target-cpu`.
- Setup: one query against n candidates with k=100. Bodies are a 28 B AoS struct, the same size as the sim's `Body`. Item ids are a random permutation of the body array, which mimics the grid's id indirection. Each figure is the mean over many queries.
- Caveats: this is not the target Zen 4c box, it is WSL, and it is single-threaded, so there is no contention from 64 cores sharing L3 or memory. Treat the ratios as indicative. The absolute ns will differ on the EPYC 8534P, which runs at a lower clock.

| variant (n = 600 / 3000 / 10000 candidates) | ns per query | ns per candidate |
|---|---|---|
| A current: AoS gather via id, `(f32,f32,u16)` tuples, `select_nth_unstable_by(total_cmp)` x2 | 3,988 / 19,072 / 65,199 | ~6.4-6.7 |
| distance only, AoS gather via id -> d2[] | 370 / 2,234 / 7,721 | 0.62-0.77 |
| distance only, SoA x[]/y[] contiguous in item order -> d2[] | 31 / 212 / 854 | 0.05-0.09 |
| B SoA + packed u64 key `(d2.to_bits()<<32 \| id)` + one `select_nth_unstable` on u64 | 1,500 / 6,585 / 21,496 | 2.2-2.5 |
| C SoA + u64 key + running threshold, buffer 4k, select-and-truncate when full | 1,961 / 8,474 / 17,574 | 1.8-3.3 |
| C' as C, but keys computed branchlessly into an array first, then threshold-filtered | 1,638 / 6,043 / 13,366 | 1.3-2.7 |
| E SoA + u32 key (u16 quantized d2 \| u16 id) + select | 1,891 / 7,226 / 23,537 | 2.4-3.2 |
| F SoA + u16 quantized d2[] + 256-bin histogram select | 1,559 / 5,415 / 17,142 | 1.7-2.6 |
| D SoA + `BinaryHeap<u64>` bounded to k | 6,055 / 13,374 / 24,362 | 2.4-10 |

With the generic `x86-64` target (no `target-cpu`), the scalar and selection rows were within ~10% of the native build. The SoA distance-only row was 1.5-2.3x slower (73 vs 31 ns at n=600, 1,150 vs 854 ns at n=10k) because the build falls back to SSE2.

## Data layout: positions inline in the grid (SoA / quantized, cell-ordered) vs id indirection

### Takeaway
Storing positions in the grid's CSR arrays makes distance computation 9-12x faster than gathering `bodies[j].state.pos` per item in the local bench: SoA `x[]`/`y[]` in item order, written during the counting-sort scatter. Distance computation is not the dominant cost today, though. Selection is: ~0.7 ns of the ~6.5 ns spent per candidate is the gather. Sorting entities each tick in Morton/Z or cell order is the classic MD/SPH way to get the same effect, at the price of a permutation step.

### Cited Findings
- **Tiered cache on the target CPU.**
  - The Zen 4c CCD packs two 8-core CCXs, and each CCX shares 16 MB of L3 among its 8 cores. That is 2 MB of L3 per core. The 64-core EPYC 8534P has 128 MB of L3 in total, which is consistent with 4 CCDs x 32 MB. — [TechPowerUp EPYC 8534P spec](https://www.techpowerup.com/cpu-specs/epyc-embedded-8534p.c3909), [OpenBenchmarking 8534P](https://openbenchmarking.org/s/AMD%20EPYC%208534P%2064-Core)
  - Zen 4's L2 is 1 MB per core. — [Chips and Cheese, Zen 4 part 2: memory subsystem](https://chipsandcheese.com/p/amds-zen-4-part-2-memory-subsystem-and-conclusion)
- **SPH neighbour search reference.** Ihmsen, Akinci, Becker and Teschner (CGF 30, 2011, "A Parallel SPH Implementation on Multi-Core CPUs") propose optimised uniform-grid variants for parallel neighbour queries, "spatial hashing and index sort". For shared-memory multi-CPU systems they use techniques "with improved cache-hit rate and reduced memory transfer", chiefly Z-index ordering of the grid and the particles. — [Eurographics DL entry](https://diglib.eg.org/handle/10.1111/v30i1pp099-112?show=full)
  - I could not extract the paper's numeric speedups because the PDF text was unreadable to the tools. See Gaps.
  - CompactNSearch (InteractiveComputerGraphics) is a maintained open-source implementation of this line of work that supports Z-order point sorting. — [CompactNSearch GitHub](https://github.com/InteractiveComputerGraphics/CompactNSearch)
- **GROMACS cluster-pair scheme.**
  - Páll and Hess (Comput. Phys. Commun. 184, 2641-2650, 2013) group a fixed number of particles (2, 4 or 8) into spatial clusters and compute all interactions between pairs of clusters. This "improves data reuse compared to the traditional scheme and results in a more efficient SIMD parallelization".
  - The cluster size is matched to the SIMD width. — [LAMMPS abstract page](https://www.lammps.org/abstracts/abstract.3709.html), [arXiv 1306.1737 summary](https://api.emergentmind.com/papers/1306.1737)
- **MD-Bench.** A proxy-app that implements the LAMMPS-style Verlet-list kernels and the GROMACS cluster-pair kernels side by side, for in-core performance analysis. — [MD-Bench, arXiv 2302.14660](https://arxiv.org/abs/2302.14660)
  - I could not extract its detailed numbers on gathers versus cluster layouts (PDF unreadable).
- **Local bench: distance computation.**
  - Over n candidates, the AoS-gather distance loop cost 0.62-0.77 ns per candidate. The SoA contiguous loop cost 0.05-0.09 ns per candidate, a 9-12x difference.
  - In both cases the 10k x 28 B body array (280 KB) is cache-resident: it is smaller than the 512 KB L2 of the Zen 3 dev box, and much smaller than Zen 4c's 1 MB L2. The gap is therefore the cost of the gather and of the lost vectorisation, not of DRAM misses. — local bench (above)

### Inferences
- **Concrete change.** In `Grid::rebuild`, scatter `(x, y)` alongside the id: `items: Vec<u32>`, `xs: Vec<f32>`, `ys: Vec<f32>`, or a parallel `Vec<[f32;2]>`.
  - The rebuild already touches every entity once, so the cost is two more 4 B stores per entity per tick: 80 KB of writes at 10k.
  - The `walk_rings` visitor should then receive a cell slice `(&[u32], &[f32], &[f32])`, not one item per closure call, so the compiler can vectorise the per-cell loop.
- **Squad filter.** The per-item squad check also gathers `bodies[j].squad`.
  - Option 1: store the squad id inline as a fourth SoA lane.
  - Option 2: filter squadmates after selection. They are added separately anyway, so take k + |squad| nearest and drop squadmates.
  - Either way the inner loop no longer needs the body array.
- **Morton ordering of the body array.** Re-sorting entities in Morton order each tick would make even the AoS gather nearly sequential. It is unnecessary once positions are inline in the grid, because the grid's item order already is cell order. Cell order within a row is as good as Morton for a 3x3-to-9x9 cell window, since each row of cells is contiguous in CSR.
- **Quantised positions.** Positions as u16/i16 relative to the cell would halve the bytes again. At 10k entities the working set (80 KB as f32) already fits in L1+L2, so quantisation buys little for this query. It mainly costs conversion work. Keep f32.
- **Working set.** 10k x 8 B = 80 KB of positions: this fits in Zen 4c's 1 MB L2 with room to spare, and each 8-core CCX shares 16 MB of L3. Memory bandwidth and NUMA are not the bottleneck at this scale. Instruction count, branch mispredicts and the gather's lost SIMD are.

### Gaps
- The SPH paper's quantitative cache-hit and speedup numbers for Z-index sort: the PDF could not be parsed.
- No public benchmark found specifically for Bevy or flecs grid queries with inline positions versus by-entity lookups.

## SIMD distance computation and branchless or compress-store filtering (AVX2/AVX-512, Rust)

### Takeaway
With contiguous SoA spans, plain Rust iterator loops auto-vectorise: 0.05 ns per candidate with `target-cpu=native`, 1.5-2.3x slower on baseline SSE2. Distance becomes nearly free. For the filter-and-append step on Zen 4, AVX-512 `VPCOMPRESSD/Q` must not use a memory destination: compress into a register, then store.

### Cited Findings
- **The Zen 4 compress-store penalty.** A Go runtime commit (`internal/runtime/gc/scan`) found that on AMD Genoa (Zen 4), `VPCOMPRESSQ` with a memory destination "imposes a severe performance penalty".
  - Replacing it with `VPCOMPRESSQ Z1, K1, Z2` followed by `VMOVDQU64 Z2, (mem)` cut time by 70.16% (geomean 1.039 µs -> 310.1 ns) and raised throughput by 278%.
  - On Turin (Zen 5), the same change was a 2.3% regression, because Zen 5 fixed the penalty. — [golang/go commit 041f564b3e](https://github.com/golang/go/commit/041f564b3e), mirrored at [remotebranch.eu](https://remotebranch.eu/Stowage/go/commit/041f564b3e)
  - uops.info has measured Zen 4 latency and throughput for the register form `VPCOMPRESSD ZMM{k}, ZMM`. — [uops.info VPCOMPRESSD Zen 4](https://uops.info/html-tp/ZEN4/VPCOMPRESSD_ZMM_K_ZMM-Measurements.html)
- **Local bench: vectorisation.** SoA distance loop at n=10k: 854 ns with `target-cpu=native` versus 1,150 ns on generic x86-64. At n=600: 31 vs 73 ns. — local bench
- **Local bench: branchless key generation.** Computing all keys branchlessly into an array and then filtering (C') beat the fused branchy push loop (C) at n=3000 and n=10k: 1.34 vs 1.76 ns per candidate at 10k. — local bench

### Inferences
- **Recommended inner loop per cell span:**
  1. compute `d2` for 8 (AVX2) or 16 (AVX-512) lanes;
  2. compare against `min(r2, current_kth)` to get a mask;
  3. compress the passing `(d2_bits<<32 | id)` keys, or the d2 and id lanes, into the candidate buffer.
- **Stable Rust, three options:**
  - Write the loop as a chunked, branchless fill: append the key unconditionally, then advance the length by `(key < thr) as usize`. This is the "branchless append" idiom; LLVM vectorises the distance part, though usually not the compress.
  - Use explicit `core::arch::x86_64` AVX-512 intrinsics behind `is_x86_feature_detected!("avx512f")`, using `_mm512_maskz_compress_epi32` + `_mm512_storeu`. Avoid `_mm512_mask_compressstoreu_*` on Zen 4.
  - Use the `wide` crate for portable 8-lane f32. `std::simd` still needs nightly.
- **Rough budget.** In the pile, the candidate count per query is ~N whatever the layout. Vectorised distance plus branchless filtering approaches ~0.1-0.3 ns per candidate, versus ~6.5 ns today. With a tight running threshold, most candidates fail the compare and never reach selection, so selection runs on O(k) survivors per refill, not on N.
- **Projected pile cost.** That is roughly 10k x 10k x ~0.3 ns ≈ 30 ms of CPU per tick, ≈ 0.5 ms wall on 64 cores. This is an extrapolation, not a measurement.

### Gaps
- I did not benchmark explicit AVX-512 compress code in Rust on Zen 4 hardware: no Zen 4 box was available during research.
- I did not find the exact uops.info latency and throughput numbers for `VPCOMPRESSD` mem vs reg on Zen 4. The page was found but not fetched.

## Fast k-selection: select_nth_unstable_by(total_cmp) vs heap, threshold, radix, SIMD select, packed keys

### Takeaway
The biggest single win is to stop selecting over `(f32, f32, u16)` tuples with a `total_cmp` closure. Use one packed integer key (`(d2.to_bits() as u64) << 32 | id`; non-negative f32 bits order like integers), select once rather than twice, and keep a running k-th threshold so most candidates are never stored. In the local bench this is 3-5x faster than the current code. A bounded `BinaryHeap` is the slowest option (branchy, log k). Integer-quantised u32 keys and histogram select did not beat the u64 threshold approach in Rust. Intel's AVX-512 `qselect` claims up to 15x over `std::nth_element` for 32-bit keys, but it is C++.

### Cited Findings
- **Rust's implementation.** Since ~1.77, `select_nth_unstable` uses introselect based on ipnsort (Bergdoll and Peters). The fallback is median of medians with a Tukey ninther pivot, giving O(n) worst case. — [Rust core `slice/sort/select.rs`](https://doc.rust-lang.org/src/core/slice/sort/select.rs.html)
- **x86-simd-sort (Intel, used by NumPy and PyTorch).**
  - It provides AVX-512 and AVX2 `qsort`, `qselect`, `partial_qsort`, `keyvalue_select` and `argselect`, choosing the ISA at runtime.
  - Its AVX-512 quickselect "performs a lot faster than std::nth_element. For smaller values of K, it is up to 6x faster for 64-bit data, up to 15x faster for 32-bit data and up to 7x faster for 16-bit data."
  - Version 6.0 added `qselect` and partial sort for key-value types. — [x86-simd-sort GitHub](https://github.com/intel/x86-simd-sort), [Phoronix on 6.0](https://phoronix.com/news/x86-simd-sort-6.0)
  - It is a C++ template library with no Rust binding mentioned. — [x86-simd-sort GitHub](https://github.com/intel/x86-simd-sort)
- **Google Highway vqsort** (Blacher, Giesen, Sanders, Wassenberg, 2022). It is "up to 20 times as fast as the sorting algorithms implemented in standard libraries" with AVX-512, and portable across seven ISAs. — [arXiv 2205.05982](https://arxiv.org/abs/2205.05982)
- **Branch mispredictions in quicksort.** Mispredictions dominate quicksort-family performance. Better pivots reduce instruction count but raise mispredict probability, which motivated BlockQuickSort. — [Brodal, AU lecture slides on branch mispredictions](https://cs.au.dk/~gerth/ae15/slides/branch-mispredictions.pdf)
- **Local bench: selection variants.**
  - Current A: 6.4-6.7 ns per candidate. Packed u64 + one select (B): 2.2-2.5 ns (2.6-3.0x faster).
  - Threshold buffer with branchless keys (C'): 1.34 ns at n=10k (4.9x faster than A) and 2.0 ns at n=3000 (3.2x).
  - BinaryHeap (D): 2.4-10 ns, the worst at small n.
  - u32 quantised keys (E): no better than u64 (2.35 ns at 10k).
  - 256-bin histogram select (F): 1.7 ns at 10k, close to C' but more complex. — local bench

### Inferences
- **Concrete code changes in `assemble`:**
  1. Replace `near_raw: Vec<(f32,f32,u16)>` with `Vec<u64>` keys. The duplicated d2 field exists only so squad entries can sort first with key -1. Squad entries can instead be appended after selection, which the code already does.
  2. Keep `thr` = current k-th key. Initialise it to `r2` and lower it whenever the buffer (cap ~2k-4k) fills, by running `select_nth_unstable(k-1)` and then truncating to k. Only push keys below `thr`.
  3. At each ring end, if `len >= k`, do one `select_nth_unstable(k-1)`, truncate to k, and test `kth <= bound²`. Do not re-select the whole vector, and drop the trailing second `select_nth_unstable_by(k, …)`.
- **The redundant ring-end select.** Today the ring-end select runs over the entire accumulated vector at every ring once `len >= k`, which is quadratic in the number of rings visited. With truncate-to-k plus the threshold, each ring-end select is O(k + ring items below thr).
- **The mid tier.** Apply the same `(d2_bits<<32 | id)` + threshold pattern. `nearest()` likely sorts or selects on tuples as well.
- **Sorted output.** If a sorted top-k is ever needed (the near accumulator does not appear to need one), sorting 100 u64 keys is trivial: the std sort's small-sort network covers it.
- **C++ libraries.** Calling x86-simd-sort or vqsort from Rust over FFI is not worth it for k=100 over a few hundred candidates per refill. Once the threshold filter is in place, selection is no longer the bottleneck. Reconsider only if a measured profile still shows select above ~20%.

### Gaps
- No published Rust benchmark comparing `select_nth_unstable` on u64 against tuples with `total_cmp`. The local bench is the only evidence.
- I did not fetch vqsort's exact GB/s figures on Zen 4.
- I did not find a Daniel Lemire post specifically on top-k by heap vs nth_element (search returned none).

## Avoiding redundant work: repeated ring-end selection, incremental approaches

### Takeaway
The ring-end `select_nth_unstable_by` on the whole, ever-growing vector is the main redundancy. Truncating to k after each select and filtering on a running threshold makes it incremental: total selection work becomes O(candidates) plus O(k) per refill, rather than O(candidates x rings).

### Cited Findings
- The current code (`sim/src/server.rs` ~995-1015, `grid.rs` `walk_rings`) pushes every in-radius item. At each ring end with `len >= k` it selects over the whole vector without truncating, then selects again at the end. — [repo source, local](file:///home/oguzhan/code/lattice-net/sim/src/server.rs)
- Local bench: the threshold buffer (C, C') outperformed a single select over everything (B) at n=10k by 18-38%. The threshold quickly converges to the true k-th distance, so most candidates are rejected by one compare. — local bench

### Inferences
- **Order of visits.** Cells in ring 0 and ring 1 hold the nearest entities, so visiting them first gives a tight threshold early. Within one cell, order does not matter for correctness.
- **Pile limit.** In a 25 m pile every entity sits in 1-4 cells, so even a perfect threshold still computes and compares N distances per query, which is still O(N²) per tick. The per-candidate floor drops from ~6.5 ns to ~0.1-0.3 ns (vectorised compare). Beyond that, an algorithmic change is needed.
- **Temporal coherence (unmeasured idea).** Seed `thr` from last tick's k-th distance plus 2 x max speed x dt. If the seed turns out too small (fewer than k candidates found), fall back to r². In a pile this lets even ring 0 reject most candidates immediately.

### Gaps
- No external source found on incremental k-NN with temporal coherence for game servers specifically.

## Prefetching, false sharing, per-thread scratch, NUMA/CCX locality, and where the cost really is

### Takeaway
At 10k entities the whole position set (80 KB) fits in each core's L2. The cost is in compute and branches, not memory: `total_cmp` closure comparisons on 12-byte tuples, mispredicted partition branches, and the scalar gather that blocks vectorisation. Prefetching and NUMA tuning are low-value here. Per-thread scratch already exists (`Scratch`) and should stay.

### Cited Findings
- **Cache sizes.** Zen 4c has 1 MB of L2 per core and 16 MB of L3 per 8-core CCX. — [Chips and Cheese Zen 4 memory](https://chipsandcheese.com/p/amds-zen-4-part-2-memory-subsystem-and-conclusion), [TechPowerUp 8534P](https://www.techpowerup.com/cpu-specs/epyc-embedded-8534p.c3909)
- **Profile.** The repo's bare-metal profile attributes 46% of CPU to `select_nth_unstable` and 14% to the grid walk in the pile. — repo CLAUDE.md, 2026-10-04 limits baseline
- **Local bench: the gather.** The gather costs 0.6-0.8 ns per candidate even with everything in L2, about 10% of the current per-candidate total. Selection plus tuple handling is the remaining ~85-90%. — local bench
- **Mispredicts.** Mispredictions are the dominant hidden cost of quickselect/quicksort partitioning on random keys. — [Brodal AU slides](https://cs.au.dk/~gerth/ae15/slides/branch-mispredictions.pdf)

### Inferences
- **False sharing.** Per-client output (`ClientSlot`, `snaps`) is written per client. As long as each rayon task owns whole `ClientSlot`s and the scratch is per-thread, false sharing is limited to slot boundaries. Make `ClientSlot` 64 B-aligned only if a profile shows HITM events.
- **NUMA.** The 8534P is single-socket. The grid is read-only after rebuild and is replicated through L3 per CCX: 80 KB of positions + 40 KB of ids + cell starts (a 250x250 grid at 32 m is 62,500 x 4 B = 250 KB). It fits each CCX's 16 MB L3 many times over.
- **`start[]`.** The cell-start array is larger than the positions. A query touches only a few rows of it, so this is fine.
- **Prefetch.** Software prefetch is unnecessary: the hardware prefetchers handle contiguous cell spans, and the data is L2-resident.
- **Epoch stamps.** `stamp: Vec<u32>` (40 KB) is per-thread scratch with random access per mid-tier candidate. That is fine at L1/L2 sizes.

### Gaps
- No perf-counter data (branch-misses, L2 misses) from the target box. The next bare-metal session should run `perf stat -e branch-misses,cache-misses,instructions` on the pile to confirm the split between mispredicts and memory.

## Tick-coherent ordering: processing clients in spatial order

### Takeaway
Iterating clients in grid/cell order, so consecutive queries on a thread hit the same cells, improves reuse of L1 (and of L2 under contention). Since the data is L2-resident at 10k, the expected gain is modest: single-digit to low tens of percent. It is cheap to do, because the grid rebuild already produces a cell-ordered entity list.

### Cited Findings
- Ihmsen et al. 2011 and CompactNSearch process particles in Z-order to raise cache-hit rates during neighbour queries. — [Eurographics DL](https://diglib.eg.org/handle/10.1111/v30i1pp099-112?show=full), [CompactNSearch](https://github.com/InteractiveComputerGraphics/CompactNSearch)
- GROMACS processes clusters of spatially adjacent particles together for data reuse. — [Páll & Hess abstract](https://www.lammps.org/abstracts/abstract.3709.html)

### Inferences
- **Cheap version.** Drive the client loop from `grid.items` order: each rayon chunk then gets spatially coherent clients. Map entity to `ClientSlot` through an index, or keep the slot order and sort a permutation each tick, which costs ~10k u32s.
- **Side benefit.** Spatial order also balances rayon work better. Pile clients would then be contiguous, though, so use small chunks or work-stealing (rayon's default) to avoid one chunk getting all the expensive queries.
- **Sharing work.** With spatially ordered clients, consecutive clients in the same cell could share one candidate scan (compute the cell's 3x3-neighbourhood candidate set once, then rank it per client). This shades into algorithmic territory.
- **Unmeasured.** I did not measure the L1/L2 gain of client ordering. Because the positions already fit in L2, expect it to matter less than the layout and selection changes above.

### Gaps
- No quantitative source found for client-order locality gains in game-server interest management. Measure locally.
