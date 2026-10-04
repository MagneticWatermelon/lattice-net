# Batched / all-points kNN over a per-tick-rebuilt 2D point set

Scope: algorithms for answering N simultaneous k-nearest-neighbour queries (every point is also a query) on a 2D set rebuilt every 33 ms tick. N = 1k–20k, k ≈ 100 (near tier, r ≤ 150 m) and k ≈ 86 (mid tier, r ≤ 500 m). Density ranges from a uniform spread to a 25 m pile of 7–10k. The target is CPU Rust on a 64-core Zen4.

System baseline, read from the code (`sim/src/grid.rs`, `sim/src/server.rs` ~L984–1062):
- `Grid` is a CSR uniform grid (32 m cells for near). It stores only ids; positions come from `bodies[j].state.pos`, which costs an indirection per item.
- `walk_rings` pushes every in-radius item's d² into `near_raw`. At the end of each ring with ≥k candidates it runs `select_nth_unstable_by(k-1)` over the whole vec, then once more at the end.
- In a pile, all N points share 1–4 cells. Each query therefore pushes about N items and runs one select of O(N) per ring (at least 2 rings, because the ring-0 bound is 0), so the total is Θ(N²).
- The radius bound `Done(k*cell)` is conservative. After ring k, everything unvisited is at least k·cell away, not (k+½)·cell, because the query can sit on a cell edge. So in a 32 m grid, the k-th neighbour at 1.3 m only becomes provable after ring 1. By then ring 1's items have been pushed, but in the pile they are mostly empty.

---

## 1. All-kNN algorithms (Vaidya, Callahan–Kosaraju WSPD, dual-tree, kNN join): what is practical, and is O(Nk) or O(N log N + Nk) cheaply achievable?

### Takeaway
In theory, all-kNN in fixed low dimension costs O(N log N + kN) through Vaidya's or Callahan–Kosaraju's algorithms. Their constants and complexity aren't worth it at N ≤ 20k. Vaidya's original analysis was also later shown to be flawed. In practice the same bound comes from something simple: sort the points by space (a Morton order, or a grid with density-adapted cells), then run each query from its own leaf with a k-th-distance bound. Expected work is O(k log k) or O(k·c) per point, independent of N. The practical gap in the current code is that the 32 m cell doesn't adapt to density, not the lack of an exotic algorithm.

### Cited Findings
- Vaidya's 1989 all-kNN algorithm claimed O(k·d^d·n log n). Ma & Li (2019) argue that it has a major mistake: computing one quantity, Est(r₀), takes Θ(n²) time, so the real bound is Ω(n²) + k·d^d·n log n. They propose O(k(k + (√d)^d)·n log n) instead. — [Ma & Li, "A True O(n log n) Algorithm for the All-k-Nearest-Neighbors Problem", arXiv:1908.00159 (2019)](https://ar5iv.labs.arxiv.org/html/1908.00159)
- Callahan & Kosaraju's WSPD work also has a parallel all-NN result: O(log n) time on O(n) processors in the CREW PRAM model. — [Ma & Li 2019, citing Callahan & Kosaraju](https://ar5iv.labs.arxiv.org/html/1908.00159)
- Dual-tree all-NN with cover trees has O(N) runtime guarantees for O(N) queries (Ram et al. 2009). Curtin et al. give a tree-independent meta-algorithm and a tighter kNN pruning bound. — [Curtin et al., "Plug-and-play dual-tree algorithm runtime analysis", JMLR 2015 / arXiv:1501.05222](https://ar5iv.labs.arxiv.org/html/1501.05222)
- Yesantharao et al. (2021) compare parallel kNN structures in low dimension: a kd-tree, a new "zd-tree" (a kd-tree split by Morton order), Chan's Morton method, Connor–Kumar's STANN and WSPD. Their findings:
  - Their best all-kNN (kNN-graph) variant is **leaf-based**: start each query at the leaf that contains it and walk upward. This skips the O(log n) root descent and gives expected **O(k log k) work per query**, so O(n) total for fixed k.
  - They pre-sort queries in Morton order for locality.
  - They report a 75× speedup on 144 hyperthreads (72 cores) for k=1 on 10 M 3D points, beating competitors "by close to an order of magnitude" in most cases.
  - Connor–Kumar's code needed ParlayLib changes to scale past 16 threads.
  — [Yesantharao, Wang, Dhulipala, Shun, "Parallel Nearest Neighbors in Low Dimensions with Batch Updates", arXiv:2111.04182 (2021)](https://ar5iv.labs.arxiv.org/html/2111.04182)
- Connor & Kumar build kNN graphs from Morton order on multi-core machines. Their stated advantages are speed, low space, cache efficiency and easy parallelisation (IEEE TVCG 2010). Chan's "minimalist" NN randomly shifts the points, sorts them by Morton order, and has O(n log n) preprocessing with O((1/ε) log n) expected time per query. — [Yesantharao et al. 2021, related-work summary](https://arxiv.org/pdf/2111.04182)
- "kNN join" in the database/GPU literature (Gowanlock's Hybrid KNN-Join) reduces kNN to an ε-range self-join on a grid with cell size ε:
  - ε is chosen by sampling 0.1% of the points so that each point probably gets ≥K neighbours.
  - Queries that find fewer than K are re-queued, and if ≥25% of a batch fails, the grid is rebuilt with a larger ε.
  - Dense regions go to the GPU (range queries) and sparse ones to a CPU kd-tree.
  - Tested on 10⁷ points in 2–6 D and 2.5×10⁷ 2D points (Gaia, OSM), with K=32.
  — [Gowanlock, "Hybrid KNN-Join", arXiv:1810.04758](https://ar5iv.labs.arxiv.org/html/1810.04758)
- Fixed-radius all-neighbours on a uniform grid ("sort into grid, scan neighbour cells") runs in O(kn), with k the average neighbour count (Green 2012). Hoetzlein (2014) speeds it up with counting sort and atomics. — [Wikipedia: Fixed-radius near neighbors](https://en.wikipedia.org/wiki/Fixed-radius_near_neighbors)

### Inferences
- O(N log N + Nk) is cheaply achievable here. At N=10k, a sort costs ~0.1–0.5 ms single-threaded and is parallelisable. Nk = 10⁶ "result slots", so even at 10 ns each the floor is ~10 ms of CPU, or ~0.16 ms wall on 64 cores. The current Θ(N²) pile cost (10⁸ distance computations plus repeated selects) is about 100× above that floor.
- The leaf-based search from Yesantharao et al. maps directly onto the current design: ring walking from the query's own cell already is leaf-based search. It is only O(k)-bounded when a leaf holds O(k) points, and a 32 m cell holds up to N in the pile. **Make the leaves density-adaptive** (Section 2) and the existing algorithm inherits the O(k log k) bound.
- WSPD and dual-tree are worth knowing for one idea: process a cell's queries together against a candidate set bounded by node-to-node distances (Section 3). Implementing them in full would be over-engineering at N ≤ 20k.

### Gaps
- No published constant-factor benchmark of Vaidya or WSPD all-kNN at N ≈ 10k in 2D was found. They are generally treated as theoretical.
- Yesantharao et al.'s 2D-specific all-kNN timings weren't extracted (the summary only says 2D was tested).

---

## 2. Adaptive spatial structures rebuilt per frame (quadtree, kd-tree, implicit/linear kd-tree, Morton-sorted arrays / LBVH, hierarchical grids): build vs query cost at these N

### Takeaway
A 2D kd-tree over 10k points builds in ~0.5 ms single-threaded with the fastest Rust/C++ libraries. On uniform data, a k=100 query costs a few µs. That is fine for uniform spread but brings no magic: per-query k=100 cost is dominated by maintaining the 100-result set, not by traversal. The cheapest adaptive structure that fits this codebase is a **Morton-sorted point array with positions stored inline (SoA)**, or equivalently a **two-level grid** (coarse 32 m cells subdivided to ~k/2–k points per leaf where dense). Either one is built by one radix or counting sort per tick (~0.1–0.3 ms at 10k) and queried leaf-first.

### Cited Findings
- **Kiddo / nabo / FNNTW / pykdtree**, from sdd/kd-tree-comparison raw results (`all-benchmarks.json`): Criterion, Ryzen 5900X (12C/24T), uniform random points.
  - **Build** of a 2D f32 tree of 10,000 points:

    | Library | Build time |
    |---|---|
    | Kiddo v5 immutable | 526 µs |
    | Kiddo v3 std | 441 µs |
    | Kiddo v2 | 536 µs |
    | nabo | 570 µs |
    | FNNTW (f64) | 850 µs |
    | pykdtree (f64) | 508 µs |
    | scipy (f64) | 1.82 ms |

    The 1,000-point build is ~31–39 µs for kiddo/nabo.
  - **Query Nearest 100**, 2D f32, tree of 10,000. Each sample is a batch of 1,000 queries run with `rayon::par_iter` (from the bench source: `QUERY_POINTS_PER_LOOP = 1_000`, `query_points.par_iter()`).

    | Library | Wall time per 1,000 queries | Core-time per query |
    |---|---|---|
    | Kiddo v5 immutable | 344 µs | ≈4–8 µs |
    | Kiddo v3 std | 360 µs | (same order) |
    | nabo | 396 µs | (same order) |
    | FNNTW (f64) | 591 µs | |
    | pykdtree | 1.32 ms | |
    | scipy | 7.7 ms | |

    The core-time column assumes the batch saturates 12–24 threads.
  - Nearest 10 on the same setup is ~66 µs per 1,000 queries for kiddo; Nearest 1 is ~36 µs.
  - Data from [sdd/kd-tree-comparison raw JSON](https://raw.githubusercontent.com/sdd/kd-tree-comparison/master/all-benchmarks.json) and [bench source nearest_n_kiddo_v5_immutable.rs](https://raw.githubusercontent.com/sdd/kd-tree-comparison/master/benches/nearest_n_kiddo_v5_immutable.rs). Machine from the [kd-tree-comparison README](https://github.com/sdd/kd-tree-comparison).
- Kiddo's tree types:
  - `KdTree` is modifiable.
  - `ImmutableKdTree` is for when all points are known up front. It was rewritten in v5, with up to 2× faster construction and better handling of duplicate coordinates. Duplicates matter for a pile with exact overlaps.
  - The query API has `nearest_n`, `within`, and `best_n_within` (a custom "best" ranking).
  - There is an optional nightly-only SIMD feature, and an experimental modified van Emde Boas node order that is more cache-friendly than Eytzinger.
  — [kiddo 5.3.3 docs](https://docs.rs/crate/kiddo/5.3.3)
- Kiddo (separately reported): 26 ms to build and 84 ms to query 1 M uniform points in the 3D unit cube on an EPYC 7502P. — [kiddo docs.rs (v3 page, via search summary)](https://docs.rs/crate/kiddo/^3.0.0)
- **LBVH / Karras 2012.** Sort primitives by Morton code, then build a binary radix tree in place, with every internal node built in parallel. The split comes from the highest differing bit of adjacent Morton codes. The same radix tree yields BVHs, octrees and kd-trees. — [Karras, "Maximizing Parallelism in the Construction of BVHs, Octrees, and k-d Trees", HPG 2012](https://research.nvidia.com/publication/2012-06_maximizing-parallelism-construction-bvhs-octrees-and-k-d-trees)
- Yesantharao et al.'s zd-tree is a kd-tree whose splits follow the Morton order, so it is built by sorting. Batch insertion of 100k points into 5 M costs ~10⁻⁷ s per element. — [arXiv:2111.04182](https://ar5iv.labs.arxiv.org/html/2111.04182)
- Uniform grids work well when queries and data are uniform, and cell size strongly affects kNN efficiency (Leite et al., a 3D voxel grid with brute force on GPU). — [search summary of GPU kNN grid literature, e.g. SSCAD paper](https://proceedings-sol.sbc.org.br/index.php/sscad/article/download/18509/18342)

### Inferences
- **Cell size for the pile.** 10k players in a 25 m disk is ~20 players/m². The 100th neighbour sits at r ≈ √(100/(20π)) ≈ 1.26 m. A 32 m cell is ~25× too coarse, so it gives no pruning at all. Leaves of ~1 m (or "≤ 32–64 points", whichever comes first) bring a query down to ~3×3 to 5×5 leaves, about 300–1,000 candidates. With a k-th-distance threshold (Section 6), most of those are rejected with a single compare.
- **Two-level grid (the least invasive change).** Keep the 32 m CSR grid as the shared index. For any cell whose count exceeds a threshold T (say 4k = 400), build a sub-grid inside the cell with side ≈ 32 m / ⌈√(count/32)⌉, using a second counting sort over just that cell's items. `walk_rings` then descends into sub-cells near the query. Cost: one extra counting sort over the dense cells' items. That is O(N) and well under 0.5 ms at 10k even single-threaded, and parallel per dense cell.
- **Morton-sorted SoA (a cleaner long-term layout).** Sort (morton_code, id) and store `xs[]`, `ys[]` in sorted order next to `ids[]`. This removes the `bodies[j]` indirection (one random cache miss per candidate today) and allows AVX2/AVX-512 distance computation over contiguous runs. Any quadtree level is then a contiguous range found by binary search or prefix, which makes this an implicit quadtree or LBVH with no pointers. The grid can stay as the cross-system index: it can be a coarse prefix of the Morton key.
- **kd-tree libraries.** With ~0.5 ms single-thread build plus ~4–8 µs core-time per k=100 query, 10k queries would cost ~40–80 ms of core-time, or ~1 ms wall on 64 cores, for uniform data. That is acceptable but probably no better than a well-tuned adaptive grid, and it would make the networking index differ from the grid shared with NPC/AoE systems.
- **Duplicates.** In a pile with many identical positions, kd-tree median splits degenerate. Kiddo v5 claims fixes for this, and grids don't care.

### Gaps
- Kiddo's benchmarks are uniform data only. No published numbers were found for kd-trees or grids on extreme clustered data (all points within 25 m) at 10k.
- No direct benchmark of LBVH or Morton-array kNN on CPU at N=10k was found.

---

## 3. Exploiting spatial coherence of queries (process all queries of a cell together, shared candidate set, cell-to-cell bounds)

### Takeaway
Group queries by leaf. For a leaf L with bounding box B, every query q in L has its k-th distance at most d_k(c) + h, where d_k(c) is the k-th distance from any reference point c in L and h is the distance from c to the farthest corner of B. So one candidate set S(L), all points within d_k(c) + 2h of c, contains every query's kNN. Each query then selects its k from |S(L)| instead of scanning the grid. This is the core idea behind dual-tree and kNN-join methods.

### Cited Findings
- Dual-tree algorithms prune query-node × reference-node pairs with node-to-node bounds. Curtin et al. derive a tighter kNN bound (B(N_q)) in a tree-independent framework. — [Curtin et al., JMLR 2015](https://ar5iv.labs.arxiv.org/html/1501.05222)
- The Hybrid KNN-Join grid method processes queries in batches per cell, with range queries against the 3^d neighbour cells. It also sorts points by estimated work (the neighbour count of the cell) for load balancing. — [arXiv:1810.04758](https://ar5iv.labs.arxiv.org/html/1810.04758)
- Morton-ordering the queries gives locality in tree traversal for every algorithm compared. — [arXiv:2111.04182](https://ar5iv.labs.arxiv.org/html/2111.04182)

### Inferences
- **Shared candidate set per leaf.** In the pile, with leaves of about 1 m holding ~20–60 players, S(L) is a few hundred points. Gather S(L) once, with positions copied contiguously, maybe sorted by distance to the leaf centre. Then for each q in L, compute |S| distances with SIMD and select k. That is ~300 × 8 B loaded once per leaf and reused by ~40 queries, so cost per query is roughly |S| × ~1 ns (SIMD distance) plus the select. Sorting S(L) by distance to c also lets each query stop early: with candidate d(c,s) sorted ascending and the triangle inequality, any s with d(c,s) − d(c,q) > current k-th distance can be cut off.
- **Even simpler for an all-in-one-cell pile.** If a cell has m ≫ k players, sort the cell's items once by (x, y) or Morton code. For each q, binary-search its rank and expand a window outward in x until |x_s − x_q| exceeds the current k-th distance. That is a 1D sweep with pruning (see Section 5).
- This also helps the mid tier (k≈86, 500 m on the coarser grid), where many clients in one area share near-identical candidate sets.
- The current per-client rayon parallelism stays intact if work is grouped into "leaf tasks", each emitting results for its member clients. The leaf→client mapping is the cell's item list.

### Gaps
- No source quantifies the speedup of shared per-cell candidate sets versus independent queries in 2D at this scale. It would need a microbenchmark.

---

## 4. Exploiting temporal coherence across ticks (incremental kNN, kinetic structures, last tick's k-th distance as initial radius, Verlet lists)

### Takeaway
Players move at most a few metres per tick: about 0.25 m at 7.5 m/s and 30 Hz, about 1–3 m for vehicles. Molecular-dynamics Verlet lists with a "skin" are the proven pattern: store neighbours within r + skin, and rebuild only when something has moved more than skin/2. For kNN, the cheap and robust use of coherence is to **seed each query with last tick's k-th distance plus 2·v_max·dt**. That gives a tight initial threshold, and the search becomes a bounded range query from the first ring.

### Cited Findings
- Verlet lists store the neighbours within cutoff + skin and reuse them across timesteps. The list must be rebuilt once any particle has moved more than half the skin distance since the last build. Skin size trades rebuild frequency against list efficiency. — [LAMMPS neighbor docs](https://doc.lammps.org/Developer_par_neigh.html); [neighbor_modify docs](https://www.columbia.edu/cu/civileng/yin/PDPS_Website/neighbor_modify.html); [TUM SCCS AutoPas Verlet lists talk](https://cs.cit.tum.de/sccs/aktuelles/sccs-kolloquium/sccs-colloquium/previous-talks-at-the-sccs-colloquium/article/luis-gall-an-exploration-of-different-approaches-for-implementing-verlet-lists-in-autopas)
- Half-lists store each pair once and use symmetry (Newton's third law). — [NVIDIA nvalchemi NeighborConfig docs](https://nvidia.github.io/nvalchemi-toolkit/modules/generated/nvalchemi.models.base.NeighborConfig.html)

### Inferences
- **Seeded threshold (low risk, exact).** Let t = d_k(prev)² inflated by (d_k + 2·v_max·dt)². Push only candidates with d² ≤ t. If fewer than k pass, fall back to the full walk (rare). This removes the "push everything, then select" behaviour: in the pile, ~100–300 of 10k pass. The scan still touches all items in the cells, though, so in the pile it is still O(N) distance computations per query. It is a ~10× constant-factor fix, not an asymptotic one, unless combined with smaller leaves.
- **Verlet-style kNN list (exact, with a sufficient-condition check).** Keep last tick's top k' = k + slack (say 150) per client with their distances. The current kNN is exactly the top k of the new distances to those k' candidates, provided no point outside the list could have entered. That holds when d_k'(prev) − d_k(prev) > 2·v_max·dt·(ticks since build). When the condition fails, do a fresh search. In a static or slow-moving crowd this is O(k') per query per tick with no grid walk. Fresh entries such as spawns need a separate check: a point that newly appears must be inserted, or must force a rebuild of nearby lists.
- **Kinetic data structures** (kinetic kNN, Delaunay) are theoretically elegant, but the event-driven bookkeeping for 10k × 100 neighbours with erratic player motion is unlikely to beat the per-tick rebuild. No CPU game-scale evidence was found.
- This interacts with interest management: the near tier already tolerates stale-ish membership (accumulator, ages), so a slightly stale kNN may be acceptable (see Section 6).

### Gaps
- No published work on Verlet-style kNN lists, as opposed to fixed-radius lists, for game interest management was found. The rebuild condition above is derived, not sourced.

---

## 5. Degenerate case: all N points within the near radius — the cheapest exact per-point kNN

### Takeaway
When every point is within r of every other, the problem is pure all-kNN on a small, dense patch. The fixed radius gives no pruning, so the structure has to adapt to the local scale (~1 m here). The cheapest exact options:
- a fine sub-grid or quadtree with leaves of ~k/2 points, plus a ring walk with a threshold;
- or a sort by x and a sweep window with pruning on |Δx| < current k-th distance.

Both give roughly O(N·(k + c)) work instead of O(N²).

### Cited Findings
- The uniform-grid fixed-radius method runs in O(kn) when cell size matches the interaction radius (Green 2012; Hoetzlein 2014). — [Wikipedia: Fixed-radius near neighbors](https://en.wikipedia.org/wiki/Fixed-radius_near_neighbors)
- Hybrid KNN-Join picks the grid cell size ε from a sample of the data's mean inter-point distances so that each point gets ~K neighbours, and grows ε on failure. That is a density-adaptive cell size. — [arXiv:1810.04758](https://ar5iv.labs.arxiv.org/html/1810.04758)
- Leaf-based Morton/zd-tree search gives expected O(k log k) per query independent of N. — [arXiv:2111.04182](https://ar5iv.labs.arxiv.org/html/2111.04182)

### Inferences
- **Sizing the sub-grid.** For a cell with m points and area A, choose a sub-cell side s ≈ √(A·k / (2m)), so each sub-cell holds about k/2 points. For the pile (m = 10k, the occupied area ~490 m², k = 100), s ≈ 1.6 m: about 300 sub-cells of ~33 points each. A query visits rings 0–2 (a 5×5 neighbourhood, ~800 candidates) and proves the 100th nearest by ring 2 (bound 2s ≈ 3.2 m > d_100 ≈ 1.3–1.8 m). That is ~800 distance computations versus ~10,000, plus one select over ≤ 800, or none with a threshold. **Expected ~12× fewer distance computations and ~100× less selection work in the pile.**
- **Tighter ring bound.** Computing the exact distance from q to the nearest unvisited cell boundary (min over the 4 sides) instead of k·cell often saves a whole ring. The true bound after ring k is min(q.x − x_lo, x_hi − q.x, q.y − y_lo, y_hi − q.y), where [x_lo, x_hi] × [y_lo, y_hi] is the visited box. That is ≥ k·cell, and up to (k+1)·cell when q sits mid-cell.
- **Pre-check by counts.** `start[c+1] − start[c]` gives each cell's population at no cost. Before computing any distance, the walk can sum counts per ring and skip the select until the cumulative count reaches k. The current code already avoids selects below k. Its main waste is selecting over everything collected, including far-away ring items.
- **x-sweep alternative.** Sort the dense cell's points by x once (O(m log m), shared). For each q, start at its rank and expand left and right, keeping a bounded top-k (a heap or a threshold buffer). Stop each side when (Δx)² > current k-th d². In a roughly isotropic blob of density ρ, a strip of width 2·d_k contains about 2·d_k·√(m/ρ)·ρ points. That is O(√m·k)-ish, worse than a 2D sub-grid but trivial to implement, and it needs no tuning.
- **Exact versus "everyone sees everyone".** In a 10k pile, every player's near tier is chosen from people within ~1.5 m, which is rather meaningless for gameplay. Approximate selection (Section 6) is arguably more appropriate there, but the exact sub-grid is cheap enough not to need it.

### Gaps
- No benchmark of these specific variants at N=10k / k=100 in 2D was found. A microbench against the current `walk_rings` (the existing test already builds a 3,000-point 200 m blob) would settle constants.

---

## 6. Approximate kNN with bounded error: which give big speedups, and what are the error characteristics?

### Takeaway
Classic (1+ε)-approximate kNN through Morton order (Chan's shifted-sort, Connor–Kumar) cuts constants by searching only a window of the sorted order. The error is bounded only in expectation, or after several shifted sorts. For interest management, a better approximation is "exact kNN on a coarser quantised distance" or "stochastic per-cell sampling", where the error is easy to state, e.g. "near tier = 100 entities, all within d_k + s of the true k-th".

### Cited Findings
- Chan's minimalist NN randomly shifts points, sorts them by Morton order, and answers (1+ε)-approximate queries in O((1/ε) log n) expected time after O(n log n) preprocessing. Its random shift defends against adversarial inputs. — [Yesantharao et al. 2021, describing Chan 2006](https://ar5iv.labs.arxiv.org/html/2111.04182)
- Connor & Kumar's Morton-order kNN-graph construction (STANN) achieves expected O(k log k) work per point under bounded expansion. — [Yesantharao et al. 2021](https://ar5iv.labs.arxiv.org/html/2111.04182)
- Hybrid KNN-Join chooses ε probabilistically to yield ≥K neighbours, and fixes failures by retrying. That makes it exact, with probabilistic work. — [arXiv:1810.04758](https://ar5iv.labs.arxiv.org/html/1810.04758)

### Inferences
- **Morton window (approximate).** Take the w = 2k–4k points around q's rank in Morton order and select k. This costs O(w) per query, about 200–400 distance computations at k=100, contiguous and SIMD-friendly. Points across a Morton seam can be missed. Using two orders (one shifted by half a cell) and merging the windows cuts misses sharply (Chan/Connor–Kumar). For a game, a missed true neighbour at rank ~90 is replaced by one at rank ~110: a slightly farther player, not a missing nearby enemy, since nearby enemies at rank ≤ 20 are essentially never missed.
- **Cell-quantised kNN (approximate, bounded).** Rank whole sub-cells by min-distance to q and take whole sub-cells until ≥k. Each chosen entity is then within d_k + diag(sub-cell) of q, so with s = 1.6 m the error is ≤ ~2.3 m. That means no per-entity select, only per-cell, and the per-cell order can be precomputed per leaf (Section 3) and shared by all queries in the leaf.
- **Quantised distance radix select.** Exact up to the bin width.
- Both are compatible with the near-tier accumulator, which already re-ranks the ≤100 candidates by priority and age. Candidate-set error at the boundary only changes which far-ish entities compete.

### Gaps
- No quantitative error versus speedup measurements for Morton-window kNN in 2D at k≈100 were extracted. The papers report it, but only abstract-level summaries were retrieved.

---

## 7. Selection: repeated `select_nth` vs bounded max-heap vs threshold / radix / histogram select vs per-ring population counts

### Takeaway
The profile's 46% in `select_nth_unstable` comes from re-selecting an O(N) vector at every ring end and again at the end, for every query. Two fixes:
- a **running threshold** (the k-th-best d² so far) that rejects most candidates with one compare before any push;
- a **buffered select**: append to a 2k–4k buffer, and when it fills, select_nth down to k and update the threshold.

Together these make selection cost O(m + k·log(m/k)) with tiny constants. A binary max-heap of size k is simpler but branchier, with log₂ 100 ≈ 7 levels per accepted insert. Histogram or radix select on quantised distances suits SIMD and needs no comparisons.

### Cited Findings
- Faiss's GPU k-selection, WarpSelect:
  - It keeps all state in registers (thread queues plus a warp-wide sorted list) and runs at up to 55% of theoretical peak.
  - It is 1.62× faster than the prior fgknn at k=100 and 2.01× at k=1000.
  - Its design premise: k-selection, not distance computation, was the bottleneck in prior GPU kNN.
  — [Johnson, Douze, Jégou, "Billion-scale similarity search with GPUs", arXiv:1702.08734 (2017)](https://ar5iv.labs.arxiv.org/html/1702.08734)
- Kiddo offers `nearest_n_within` with sorted and unsorted variants. Unsorted is substantially cheaper: in the 2D benchmarks, "within radius" sorted/unsorted at 10k is 1.40 ms vs 0.21 ms per batch for Kiddo v2. Skipping a final sort matters. — [kd-tree-comparison raw JSON](https://raw.githubusercontent.com/sdd/kd-tree-comparison/master/all-benchmarks.json)

### Inferences
- **The concrete fix in `walk_rings`' visitor:**
  - keep `tau = f32::INFINITY` until k items are held;
  - push only when d² < tau;
  - when `near_raw.len() >= 2k` (or 4k), `select_nth_unstable_by(k-1)`, truncate to k, and set tau = near_raw[k-1].0;
  - at `Done(bound)`, stop if len ≥ k and tau ≤ bound².

  The current code's per-ring select over everything collected becomes amortised O(1) per candidate. The truncation is what makes later selects cheap. The current code never truncates mid-walk, so each ring's select re-scans all earlier rings' candidates.
- **Avoid the second select.** After the walk, the vec has ≤2k items. Selecting k from them is cheap, or with the buffered approach, one final select on ≤2k.
- **Seed tau from last tick** (Section 4) to reject from the very first ring.
- **Radix or histogram select.** Distances within r = 150 m quantise to u16 bins, or a u32 key if the f32 d² bits are taken as an integer, which is monotone for non-negative floats. A 256-bin histogram on the top byte finds the bin that holds the k-th element in one pass, and a second pass emits. This is branch-free and AVX-512 friendly (for example VPCONFLICTD or compress-store approaches), and suits the pile, where m is in the thousands. With small leaves (m in the hundreds), the threshold buffer is simpler and just as fast.
- **Comparator cost.** `total_cmp` on f32 compiles to integer tricks. Comparing `d2.to_bits()` as u32 for non-negative values is equivalent and slightly cheaper. Packing (d2_bits << 32 | id) into a u64 makes select and sort operate on plain u64, which halves memory traffic versus (f32, f32, u16) tuples padded to 12 B.
- **Per-ring population counts.** Use `start[]` differences to skip the select until the count reaches k, and to choose a starting ring whose cumulative population ≥ k (or ≥ the seeded threshold). In uniform areas this avoids distance work on rings that can't matter. In the pile it doesn't help: one cell holds everything, which is why adaptive leaves are the real fix.
- **Positions inline.** The indirection `bodies[j].state.pos` (a large struct, one cache line per candidate) likely costs more than the arithmetic. Storing `xs`/`ys` beside the grid's `items` in CSR order, filled during the counting-sort scatter, makes the candidate loop a contiguous SIMD loop at ~1 ns per candidate or less.

### Gaps
- No CPU-specific benchmark comparing heap, threshold-buffer and quickselect for k=100 over m = 10²–10⁴ was found from a primary source. This needs a local microbenchmark.
- Rust's `select_nth_unstable` implementation details (introselect, and its fallback behaviour) weren't verified from source in this pass.
