# Neighbor search in physics and crowd simulation, and how it maps to per-client kNN

Context: lattice-net sim, `sim/src/grid.rs` (uniform 32 m grid, counting-sort CSR rebuild every tick) and `sim/src/server.rs` ~L980-1060 (Chebyshev ring walk; every in-radius candidate pushed into a vec; `select_nth_unstable_by` over the whole vec at the end of each ring; stop once k-th <= ring bound). Near: k=100 within 150 m. Mid: k~86 within 500 m. A 25 m pile of 10k gives 44 ms/tick, with select_nth at 46% CPU.

Why the pile is quadratic, read from the code: a 25 m pile fits in one or two 32 m cells, so ring 0 pushes about 10k candidates for every client. `Ring::Done(0)` then runs select_nth over about 10k, and the k-th distance (~1.3 m, below) is larger than the ring-0 bound of 0, so ring 1 is walked as well and select_nth runs again. That is about 10k x 10k = 1e8 distance computations and partition steps per tick. The grid's cell size is about 25x too coarse for the local density, which is exactly the failure that simulation codes avoid by tying cell size to density or cutoff.

Worked density numbers (my arithmetic, not from a source):
- **Pile:** 10k players in a 25 m-diameter disk (~491 m²) is ~20 players/m². The 100th-nearest radius is sqrt(100 / (20π)) ≈ 1.26 m, and the 150th is ≈ 1.55 m.
- **Uniform:** 10k in 8 km x 8 km is 1.56e-4 /m². The expected count within 150 m is π·150²·1.56e-4 ≈ 11, so the near tier is radius-limited (k is never reached), and the 100th neighbor would be at ~450 m.
- **Movement per tick:** running at 6.5 m/s and 30 Hz is ~0.22 m/tick.

---

## 1. Verlet neighbor lists with a skin: rebuild rules, cost, and a kNN analogue

### Takeaway
MD codes build a list with cutoff r_c + skin, reuse it for many steps, and rebuild only when some particle has moved more than skin/2. GROMACS extends this to an outer list built rarely plus a cheaply pruned inner list. A kNN analogue is sound (keep k+m candidates plus a lower bound on everything outside them, and re-validate with a displacement bound), but it buys little in a pile, because at 20 players/m² the gap between the k-th and (k+m)-th neighbor is smaller than one tick of movement. It pays off in sparse and medium density, where the problem is fixed-radius anyway.

### Cited Findings
- LAMMPS: the neighbor cutoff is the force cutoff plus the skin. A larger skin means less frequent rebuilds but more pairs checked every step — [LAMMPS neighbor command](https://docs.lammps.org/neighbor.html)
- LAMMPS `neigh_modify every M check yes`: a rebuild is attempted every M steps but only happens if at least one atom has moved more than half the skin since the last build — [LAMMPS neigh_modify](https://doc.lammps.org/neigh_modify.html)
- GROMACS has two modes. In the classical mode, a list containing at least all pairs up to rlistOuter is built every nstlist steps. In dynamic pruning, an outer list up to rlistOuter is built every nstlist steps and pruned to an inner list (rlistInner) every nstlistPrune steps — [GROMACS pairlist_tuning.cpp docs](https://manual.gromacs.org/current/doxygen/html-full/pairlist__tuning_8cpp.xhtml)
- With dynamic pruning, nstlist can rise from 10–20 to 100+ steps, and pruning costs "typically <1% of the total runtime". On GPU the pruning is rolling: part of the list is pruned every (second) step and overlaps other work — [Páll et al. 2020, Heterogeneous parallelization… in GROMACS, arXiv:2006.09167](https://ar5iv.labs.arxiv.org/html/2006.09167); [GROMACS docs](https://manual.gromacs.org/current/doxygen/html-full/pairlist__tuning_8cpp.xhtml)
- In GROMACS the buffer is sized from a tolerated energy drift (verlet-buffer-tolerance), so lists are deliberately allowed to be slightly wrong within a statistical tolerance rather than kept exact — [GROMACS docs/forum](https://gromacs.bioexcel.eu/t/rlist-nstlist-tuning-and-verlet-buffer-tolerance-rvdw-and-rcoulomb/9865)
- Páll & Hess 2013: r_list = r_c + r_b. The optimal list update interval is "between 10 and 50" steps — [Páll & Hess 2013, arXiv:1306.1737](https://ar5iv.labs.arxiv.org/html/1306.1737)
- Kinetic data structures (computational geometry) exist that maintain all k-nearest neighbors of moving points for any k ≥ 1. They assume polynomial trajectories and event queues, with O(n log^(d+1) n + kn) space — [Kinetic k-semi-Yao graph, arXiv:1412.5697](https://arxiv.org/pdf/1412.5697); see also the [Kinetic reverse kNN paper, arXiv:1406.5554](https://ar5iv.arxiv.org/html/1406.5554)

### Inferences
- **A sound kNN-Verlet rule.** At build time, store per client the candidate set C of its k+m nearest, and R = a lower bound on the distance of every non-candidate (for example the (k+m+1)-th distance, or the ring bound reached). Track δ = max displacement since the build, over all players including the querier (a global max, or a per-cell max for a tighter bound). Every non-candidate is then at least R − 2δ away. Each tick, recompute the exact current distances of the k+m candidates (O(k+m), already contiguous) and select k. If the k-th of those is ≤ R − 2δ, the result is exactly the true kNN; otherwise rebuild that client's list. This is the LAMMPS skin/2 criterion generalized from a fixed cutoff to a rank. It also handles radius-limited (sparse) cases by setting R = r_near + skin.
- **Pile arithmetic (my calculation).** With k=100 and m=50 the gap is ~1.55 − 1.26 = 0.29 m, but 2δ after one running tick is ~0.43 m, so the list is invalid after a single tick. Even m=k (k+m=200, radius ~1.78 m, gap 0.52 m) survives only ~1 tick at full running speed. In the pile, exact kNN-Verlet gives roughly nothing; the ranks churn every tick.
- **Sparse and medium arithmetic.** With a 10 m skin at 6.5 m/s, the rebuild criterion (some player moved > 5 m) is hit every ~23 ticks. Per-client lists with per-client displacement checks (only the querier and its candidates matter, plus a global or per-cell max for outsiders) could amortize the walk over ~10–20 ticks for the uniform and hotspot cases. These are not currently the bottleneck.
- **What makes Verlet-style reuse work in a pile is GROMACS's attitude:** tolerate bounded error. Interest management already ranks with a priority accumulator, so an ε-approximate kNN (accept if k-th ≤ (1+ε)·bound, or accept a stale list for N ticks inside a dense cluster) is probably acceptable. Near-tier membership needs no exact rank 100 vs 101 at 0.25 m spacing.
- Kinetic kNN structures are event-driven and assume known trajectories. Player input is unpredictable, so they don't transfer to a 30 Hz tick; the discrete displacement-bound check above is the practical form.

### Gaps
- I found no published "approximate kNN with Verlet-like skin" for a fixed neighbor count in MD/SPH. The closest is GADGET-style adaptive smoothing lengths (section 6), which target a neighbor count through a radius, not a rank list.
- No source gives the neighbor-rank churn rate for dense crowds; the pile arithmetic above is mine.

---

## 2. Cell lists sized to the cutoff, and adaptive/hierarchical structures for non-uniform density

### Takeaway
Every SPH/MD code ties cell size to the interaction length (cell = support radius h, giving ~30–40 neighbors in 3D) or to particle density. Here the effective "interaction length" is the k-th neighbor distance, which varies over ~2.5 orders of magnitude (1.3 m in a pile vs >150 m uniform). So one fixed cell size can't serve both: the grid needs a density-adaptive level, at least locally, or density-derived cell sizes like GROMACS's.

### Cited Findings
- SPH (Koschier et al. tutorial, which summarizes Ihmsen et al. 2011): a uniform grid with cell size equal to the kernel support radius; a query visits the occupied cell plus 26 neighbors; the method is O(n) — [Koschier et al., SPH Techniques…, arXiv:2009.06944](https://ar5iv.labs.arxiv.org/html/2009.06944)
- A fully populated SPH neighborhood holds ~30–40 particles in 3D, with h = 2× the particle radius — [arXiv:2009.06944](https://ar5iv.labs.arxiv.org/html/2009.06944)
- Compact hashing (Ihmsen et al. 2011) stores only populated cells, so memory scales with particle count rather than domain volume. Non-empty cells are sorted along a Z-curve for cache coherence, and the Z-sort is refreshed only at fixed intervals (e.g., every 1000th step), because temporal coherence keeps the order nearly intact — [arXiv:2009.06944](https://ar5iv.labs.arxiv.org/html/2009.06944); [Ihmsen et al. 2011, A Parallel SPH Implementation on Multi-Core CPUs, CGF 30(1):99–112](https://cg.informatik.uni-freiburg.de/publications/2011_CGF_dataStructuresSPH.pdf)
- Hoetzlein 2014 (NVIDIA GTC, "Fast Fixed-Radius Nearest Neighbors: Interactive Million-Particle Fluids"): a uniform bin grid built by counting sort instead of radix sort, since a full sort on the key isn't needed, with atomics. Cost is O(Nk) — [Hoetzlein 2014 slides](https://ramakarl.com/pdfs/2014_Hoetzlein_FastFixedRadius_Neighbors.pdf); [Wikipedia: Fixed-radius near neighbors](https://en.wikipedia.org/wiki/Fixed-radius_near_neighbors). lattice-net's `Grid::rebuild` is already this algorithm on CPU.
- GROMACS sizes its grid from density, not from the cutoff. The x/y spacing is (max(M,N)/ρ)^(1/3), so each z-column cell holds a fixed number of particles (a cluster) — [Páll & Hess 2013](https://ar5iv.labs.arxiv.org/html/1306.1737). The 2020 paper describes the same thing as a regular x/y grid whose z columns are binned "into cells with fixed number of particles" — [arXiv:2006.09167](https://ar5iv.labs.arxiv.org/html/2006.09167)
- The SPH tutorial does not discuss adaptive or multi-resolution particle sizes — [arXiv:2009.06944](https://ar5iv.labs.arxiv.org/html/2009.06944). (I couldn't extract the 2011 paper's timing tables: the PDF wasn't text-extractable in this environment.)

### Inferences
- **The 2D analogue of GROMACS's density-sized columns: bin by fixed count, not fixed size.** Within each 32 m cell (or each x-strip), sort the members by y (or by a Morton code within the cell) and chunk them into fixed-size runs of 8–16 with stored bounding boxes. The counting-sort CSR already gives each cell a contiguous slice, so this adds one per-cell sort (or an incremental insertion sort, section 5) only for cells over a threshold, say >64 occupants.
- **Two-level grid.** Keep the 32 m grid as the shared index, which the CLAUDE.md "one index shared by all systems" decision needs. For any cell with occupancy > T, build a sub-grid with cell size s = sqrt(c·k / ρ_cell), so the k-th neighbor lies within a ring or two of subcells (c ≈ 1/4 → ~25 per subcell). At 20/m², s ≈ 1.1 m. The ring walk then descends: for queries inside a dense cell, walk subcell rings; for queries outside it, treat it as a bulk cell. This is the "multi-level grid" idea; the user's prior decision is "a better algorithm, not a finer grid", and this makes the finer grid local and automatic rather than global.
- The Z-order "refresh rarely" result from SPH is directly applicable: players' order within a cell changes slowly, so re-sorting each dense cell's slice by insertion sort from last tick's order is near-linear.

### Gaps
- No quantitative CPU timings from Ihmsen 2011 could be extracted (PDF parsing unavailable); the per-method speedups (compact hashing vs index sort) remain unverified here.
- No source found on SPH octree neighbor search timings for extreme density ratios (I did not reach an octree-SPH paper in this budget).

---

## 3. Cluster-pair approaches (GROMACS, Páll & Hess 2013): can 8–16-player clusters resolve a pile cheaply?

### Takeaway
GROMACS groups 4–8 particles into spatial clusters and tests cluster-vs-cluster bounding boxes: one box check covers M×N pairs, at the cost of computing extra pairs (86% extra for 4×4 at a 1 nm cutoff) that SIMD makes cheap. For kNN, the big win is clustering on the query side: queriers in the same small region share one candidate superset. In the pile that is roughly a 10–20x reduction in candidate work (my estimate).

### Cited Findings
- Fixed-size clusters of 2, 4 or 8 particles; computing all interactions between a pair of clusters improves data reuse and maps onto SIMD width — [Páll & Hess 2013, CPC 184:2641–2650, arXiv:1306.1737](https://ar5iv.labs.arxiv.org/html/1306.1737)
- "One bounding box-pair distance check for M×N particle pairs" — [Páll & Hess 2013](https://ar5iv.labs.arxiv.org/html/1306.1737)
- 4×4 adds 86% extra pair interactions beyond the cutoff, but is still 1.8× faster than 1×1 (reaction-field). Measured: 223 vs 76 pairs/kcycle (RF) and 139 vs 63 (Ewald) on Sandy Bridge AVX-256; 1151 pairs/kcycle for 8×4 on Kepler — [Páll & Hess 2013](https://ar5iv.labs.arxiv.org/html/1306.1737)
- Current GROMACS: CPU SIMD uses 8×4 (or 4×4 or 4×8 depending on SIMD width); GPU uses 8×4 with 8-way super-clustering (about half of inner-loop checks are skips, an estimated 8–12% cost); Intel GPU uses 4×2. Clusters also give an "implicit buffer": the same accuracy with a 0.105 nm explicit buffer vs 0.218 nm for 1×1 — [arXiv:2006.09167](https://ar5iv.labs.arxiv.org/html/2006.09167)

### Inferences
- **Query-side clusters (the "i-cluster").** Take a cluster Q of g queriers (8–16, consecutive in the cell's sorted slice) with center c and radius ρ (max distance of a member from c). For every member q, d_k(q) ≤ d_k(c) + ρ, and q's true k nearest all lie within d_k(c) + 2ρ of c (triangle inequality). So the cluster does one walk from c with radius d_k(c) + 2ρ to gather a superset S, and each member selects its k from S with SIMD distance computations. Pile numbers (mine): g=16 at 20/m² spans ρ ≈ 0.5 m, so the superset radius is ~1.26 + 1.0 ≈ 2.3 m, |S| ≈ 330. Total work is ~10k × 330 ≈ 3.3e6 distance+select operations vs ~1e8 now: about 30x less, plus one walk per cluster instead of per client.
- **Candidate-side clusters (the "j-cluster").** Clusters of 8 with bounding boxes let a query skip whole clusters whose box min-distance exceeds the current k-th bound (as RVO2's kd-tree does with node boxes, section 4). On AVX2, 8 f32 x/y distances per cluster are one SIMD op, which fits the "86% extra but faster" finding.
- Positions are currently read indirectly (`self.bodies[j]`). The GROMACS lesson is that clusters store coordinates contiguously (SoA x[8], y[8]); copying positions into the grid's CSR order during rebuild would remove the gather from the inner loop.
- The result is exact, not approximate. The superset bound is a strict triangle inequality, so near-tier correctness is unchanged.

### Gaps
- No source applies cluster-pair to kNN (as opposed to fixed cutoff); the query-cluster bound is my derivation and should be checked by a test against brute force (the existing `ring_walk_finds_the_k_nearest_and_stops_early` pattern).

---

## 4. Crowd simulation (RVO2/ORCA, DetourCrowd, Unity DOTS boids): structures and caps

### Takeaway
Crowd systems almost universally cap the neighbor count small (RVO2's `maxNeighbors`, DetourCrowd's fixed neighbor array, Unity boids' per-cell averages) and use a bounded sorted insert that shrinks the search radius once the list is full. That bounded-heap / shrinking-radius kNN is exactly what lattice-net's ring walk lacks: it pushes every in-radius candidate and partitions afterwards.

### Cited Findings
- RVO2 (van den Berg et al.) rebuilds an agent kd-tree each step (`buildAgentTree`) with `RVO_MAX_LEAF_SIZE = 10`. Queries recurse into child nodes only when their bounding-box distance is within the current range, and the range is passed by reference and shrinks as neighbors are found — [RVO2 KdTree.cc](https://github.com/snape/RVO2/blob/main/src/KdTree.cc)
- RVO2 `insertAgentNeighbor`: accepts if distSq < rangeSq, appends while size < maxNeighbors, insertion-shifts into ascending order, and once full sets `rangeSq = agentNeighbors_.back().first` (the k-th distance) — [RVO2 Agent.cc](https://github.com/snape/RVO2/blob/main/src/Agent.cc)
- DetourCrowd (Recast/Detour, Mononen): a `dtProximityGrid` initialized with `init(maxAgents*4, maxAgentRadius*3)` (cell size ~3 agent radii). `getNeighbours` queries items in a square range, retrieving at most 32 (`MAX_NEIS`), filters by height and distance, and `addNeighbour` inserts by distance into a capped list. Neighbor queries run every frame, while topology optimization is throttled to every 0.5 s (`OPT_TIME_THR`) — [DetourCrowd.cpp](https://github.com/recastnavigation/recastnavigation/blob/main/DetourCrowd/Source/DetourCrowd.cpp)
- Unity ECS Boids sample: agents are hashed into grid cells and cohesion and alignment are computed per grid cell (cell averages) rather than per pair, which removes repeated per-boid averaging — [Unleashing massive flocks with Unity](https://levelup.gitconnected.com/unleashing-massive-flocks-with-unity-30ef13aea78b); [Unity ECS samples](https://docsearch.algolia.com/mcp/docs/repo/unity-technologies/entitycomponentsystemsamples)

### Inferences
- **Immediate, low-risk fix.** Replace "push all in-radius, select_nth at each ring end" with a bounded max-heap (or a sorted small array, RVO2-style, though for k=100 a binary heap or a two-pass threshold is better) of size k whose top is the current k-th distance. Reject any candidate with d² ≥ top before touching the heap. This alone cuts the select cost from O(cell occupancy · log) repeats to O(occupancy) comparisons plus O(k log k) insertions in expectation, but it is still O(occupancy) distance computations per query in a pile. So it removes the 46% select_nth share but not the 1e8 distance computations; it must be combined with sections 2–3.
- **Cheaper variant.** Keep the vec, but before pushing, compare against a threshold initialized from last tick's k-th distance of this client plus 2·max displacement (temporal coherence): typically ~k+few candidates survive, and select_nth runs on a ~k-sized vec. If fewer than k survive, fall back. This exploits that the k-th distance changes by at most 2δ per tick (exact bound, my derivation).
- **Crowd sims' small caps (RVO2's default maxNeighbors is often ~10; DetourCrowd's at most 32 raw) are a design choice of "we only need the closest few".** The near tier's k=100 is 3–10x larger, so per-agent costs from crowd papers won't transfer directly.
- **Unity's per-cell averaging is the "aggregate the far part" trick.** For the mid and far tiers in a pile, a dense subcell could be represented by one aggregate, but the network tiers need individual entities, so this applies only to non-networking consumers (NPC proximity, AoE).

### Gaps
- I didn't find quantitative per-agent kNN costs for 10k–100k agents from RVO2, DetourCrowd, Unity DOTS or the AC Unity crowd talks within the search budget. Continuum Crowds (Treuille 2006) avoids per-agent neighbor search entirely (density/potential fields) and wasn't researched further.
- RVO2's default maxNeighbors value wasn't verified from source in this session (commonly 10 in examples).

---

## 5. Physics-engine broadphase: frame-to-frame coherence (SAP, Box2D dynamic tree, PhysX MBP)

### Takeaway
Broadphases exploit coherence in two ways: fat/enlarged bounds that absorb small motion without structural updates (Box2D), and incrementally re-sorted axis lists (insertion sort on nearly sorted data, SAP). Both transfer: a "fat" candidate radius is the Verlet skin (section 1), and per-cell slices can be insertion-sorted from last tick's order instead of re-sorted.

### Cited Findings
- Box2D dynamic tree: a binary AABB tree. Proxy AABBs are enlarged ("fat") so objects can move small amounts without triggering a tree update. The tree can be rebuilt while keeping unchanged subtrees, and the optimal bottom-up rebuild is "very expensive" and used only for testing — [Box2D 2.4.1 b2_dynamic_tree.h](https://box2d.org/doc_version_2_4/b2__dynamic__tree_8h_source.html); [Box2D 3.1 tree docs](https://box2d.org/documentation/group__tree.html)
- Sweep-and-prune sorts projected AABB extents per axis and updates the order incrementally. Insertion sort suits this because bodies move little between steps, so the lists are nearly sorted — [Wikipedia: Sweep and prune](https://en.wikipedia.org/wiki/Sweep_and_prune); [Tracy, Buss & Woods, Efficient large-scale sweep and prune](https://mathweb.ucsd.edu/~sbuss/ResearchWeb/EnhancedSweepPrune/SAP_paper_online.pdf)
- PhysX offers SAP and MBP (multi box pruning). MBP "does not suffer from the same performance issues as SAP when all objects are moving or when inserting large numbers of objects" — [PxBroadPhaseType reference](https://docs.nvidia.com/gameworks/content/gameworkslibrary/physx/apireference/files/structPxBroadPhaseType.html)
- Parallel SAP can use temporal coherence to get near-optimal load balancing in its sort stage — [Capannini & Larsson 2016, EG PGV](https://diglib.eg.org/handle/10.2312/pgv20161177?show=full)

### Inferences
- **SAP's known failure mode is the pile.** Many objects clustered on an axis make swaps explode, which is why PhysX added MBP (a grid of SAP regions). That is the same lesson: partition space first, then use coherent sorting inside small regions. For lattice-net, dense cells could keep their members in a 1D order (x-sorted, or Morton within the cell) that is insertion-sorted each tick from the previous order. That is O(n + swaps) with swaps ~ n·(movement/spacing), and it supplies both GROMACS-style clusters and a sweep axis for kNN (expand outward from the query's position in the sorted x-list while |Δx| < current k-th distance).
- **Box2D's fat-AABB idea maps to per-client fat query radius:** cache "the k nearest are within R_fat of my position at build time", and recheck only when the querier has moved more than (R_fat − d_k)/2 and no candidate outsider could have entered (section 1's bound).
- **Rebuild vs refit:** lattice-net's counting-sort rebuild is already O(N) and parallel-friendly. Coherence matters inside dense cells (ordering within the cell), not for the top-level grid.

### Gaps
- No Catto blog post with quantitative broadphase-coherence numbers was retrieved; Box2D v3's exact margin constant wasn't verified.
- No quantitative MBP-vs-SAP benchmark under all-moving dense scenes was found.

---

## 6. Explicit handling of extreme density (neighbor caps, sampling, adaptive radius)

### Takeaway
Simulation codes handle neighbor explosion by (a) hard caps with errors or overflow (LAMMPS per-atom limits, DetourCrowd's fixed arrays), (b) adapting the radius so the neighbor count stays roughly constant (astrophysical SPH such as GADGET), and (c) aggregation per cell (boids). Approach (b) is the closest physics analogue to budgeted interest-management kNN, and its standard trick is to start from last step's radius.

### Cited Findings
- LAMMPS `neigh_modify` has `one` (the maximum number of neighbors of one atom) and `page` settings, and runs error out if exceeded rather than silently truncating — [LAMMPS neigh_modify](https://doc.lammps.org/neigh_modify.html) (I didn't verify the exact default values in this session; commonly cited as one=2000, page=100000.)
- DetourCrowd retrieves at most 32 raw items per query and keeps a capped, distance-sorted neighbor list — [DetourCrowd.cpp](https://github.com/recastnavigation/recastnavigation/blob/main/DetourCrowd/Source/DetourCrowd.cpp)
- GADGET-2 doesn't define the smoothing length as the distance to the N-th neighbor. It solves a constraint that the kernel volume holds a constant mass (a weighted, float "number of neighbours", NumNgb = 4π/3 h³ Σ w_j), so h adapts with density — [GADGET mailing list, Springel](https://wwwmpa.mpa-garching.mpg.de/gadget/gadget-list/0064.html); [Springel 2005, GADGET-2, astro-ph/0505010](https://arxiv.org/abs/astro-ph/0505010)
- Unity boids aggregate cohesion and alignment per cell rather than per neighbor — [levelup.gitconnected](https://levelup.gitconnected.com/unleashing-massive-flocks-with-unity-30ef13aea78b)

### Inferences
- **The adaptive-radius approach transfers directly as "radius-first kNN".** Keep each client's last k-th distance r_prev. This tick, query a fixed radius r_prev + 2δ_max (exactly guaranteed to contain all k by the displacement bound, provided at least k were within r_prev last tick), collect with a cheap d² < (r_prev+2δ)² filter, and select k from the ~k·((r+2δ)/r)² survivors. In the pile: (1.26 + 0.43)/1.26 squared ≈ 1.8, so ~180 candidates instead of 10k. But the filter still requires scanning the cell unless the cell is subdivided (sections 2–3). Combined with a ~1 m sub-grid, the scan is ~9 subcells × ~20 = ~180 items: O(k) per client, about 1.8e6 operations per tick in the 10k pile vs ~1e8.
- **Approximate kNN is defensible at extreme density.** At 0.25 m mean spacing, the 100th vs 150th nearest differ by 0.3 m, which is invisible to gameplay. Options: cap the scanned candidates per subcell (sampling, as DetourCrowd's raw cap of 32 does), or treat a dense subcell as "all in or all out". Either bounds worst-case cost regardless of density, which fits the degradation-ladder philosophy (CLAUDE.md). The priority accumulator downstream already reorders within the near set.

### Gaps
- No source quantifies the cost of SPH adaptive-h iteration vs fixed h at extreme density ratios.
- I didn't find any game-industry (MMO) publication describing kNN interest management in piles; adjacent researchers may cover that.

---

## Summary mapping (ranked by expected payoff for the 25 m / 10k pile)

1. **Density-adaptive local grid** (GROMACS density-sized cells; SPH cell = interaction length): subdivide cells over a threshold to ~1 m subcells at 20/m². This turns the per-query scan from ~10k into ~O(k) items. [Section 2]
2. **Temporal bound on the k-th distance** (Verlet skin / GADGET's start-from-last-h): filter with (r_prev + 2δ)² before pushing, which shrinks select_nth's input from ~10k to ~180 (the cell scan remains O(occupancy) without item 1). [Sections 1 and 6]
3. **Bounded heap with a shrinking range** (RVO2): removes the repeated full select_nth at each ring end. [Section 4]
4. **Query-side clustering** (GROMACS i-clusters): one walk per 8–16 queriers with a superset radius of d_k(c) + 2ρ, for exact results at ~1/16 of the walks. [Section 3]
5. **Contiguous SoA positions in CSR order, and insertion-sorted dense-cell slices** (GROMACS clusters, SAP coherence). [Sections 3 and 5]
6. **Approximate kNN or sampling in extremely dense subcells** as the last-resort cap. [Section 6]
