# Interest management / AOI neighbor selection in dense crowds: shipped games, frameworks and heuristics

Research date: 2026-10-04. Context: lattice-net `sim/src/server.rs` ~975-1065 does, per client per tick, a k-nearest ring walk over a 32 m grid (`walk_rings`), collecting every in-radius non-squad entity into `near_raw` and `select_nth_unstable_by(k-1)` (k = `near_candidates`, ~100), then the same for mid (k = `mid_per_tick`, ~86) on `mid_grid`. In a 25 m pile, the first ring (one or a few 32 m cells) already holds ~all 10k players, so each query is O(N) and the tick O(N^2).

Note on method: about 30 tool calls. Several primary sources (Epic tech blog 2018, the Boulanger PDF body, PC Gamer article) could not be fetched (403, binary PDF, truncated page), so some engine details below are cited only to API/docs pages that confirm the feature exists. Anything from background knowledge and not verified this session is marked **[unverified]**.

## 1. What shipped games and engines do for AOI in dense crowds

### Takeaway
No shipped engine found does exact per-client k-nearest selection. The common pattern has three parts: (1) a coarse **shared** spatial structure (grid cells, rooms, bubbles) gives a candidate set, kept across frames and shared by connections; (2) a cheap **per-object priority** (distance falloff plus boosts) feeds a **priority accumulator** that a per-connection budget/count limiter drains; (3) when that still isn't enough, the engines degrade globally (EVE time dilation, PS2-style render-distance caps, Roblox radius shrink). Under density the selection becomes approximate: "send the highest accumulated priority that fits", not "the exact k nearest".

### Cited Findings

**Unreal Engine Replication Graph (Fortnite, 2018+)**
- Built because Fortnite Battle Royale "starts each game with 100 connected players and about 50,000 replicated Actors", and per-actor, per-connection relevancy evaluation "performs poorly in cases like this and will bottleneck the server's CPU." — [Epic: Replication Graph docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/replication-graph-in-unreal-engine)
- The core idea is persistent nodes that let "data to be stored across multiple frames and shared between client connections". The world is divided into grid spaces for battle royale / MOBA / MMORPG genres, or rooms/zones for dungeon crawlers, and nodes give "persistent actor lists for specific zones". Dormant static actors (a tree) need no updates until they change. — [Epic: Replication Graph docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/replication-graph-in-unreal-engine)
- `UReplicationGraphNode_GridSpatialization2D` is the 2D grid node. It has `CellSize` and `SpatialBias` properties (grid origin offset) and per-cell actor lists (`FActorCellInfo`). — [UE API: GridSpatialization2D](https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/ReplicationGraph/UReplicationGraphNode_GridSpatia-); [UE 4.26 FActorCellInfo](https://docs.unrealengine.com/4.26/en-US/API/Plugins/ReplicationGraph/UReplicationGraphNode_GridSpatia-/FActorCellInfo/index.html)
- `UReplicationGraphNode_DynamicSpatialFrequency` has `FDynamicSpatialFrequency_SortedItem` with an `EnableFastSharedPath` flag. Its settings include `BucketThresholds`, "for dynamically balancing buckets based on number of actors in a node, with more buckets when there are more actors." This is a density-adaptive stagger: the more actors in a node, the more frequency buckets they are split across, so each is updated less often. — [UE 4.26 API: FDynamicSpatialFrequency](https://docs.unrealengine.com/4.26/en-US/API/Plugins/ReplicationGraph/UReplicationGraphNode_DynamicSpa-/FDynamicSpatialF-/index.html); [UE API: ActorListFrequencyBuckets FSettings](https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Plugins/ReplicationGraph/UReplicationGraphNode_ActorListF-/FSettings)
- Third-party guides show `CellSize` values like 10000 uu (100 m) in examples, and note that an actor whose `NetCullDistanceSquared` is small relative to the cell size can fail to reach clients. — [studyraid: GridSpatialization2D](https://app.studyraid.com/en/read/98378/4423800/grid-spatialization-2d-for-efficient-actor-discovery); [bugnet: spatial grid not loading](https://bugnet.io/blog/fix-unreal-replication-graph-spatial-grid-not-loading) (secondary sources, low authority)
- **[unverified, background knowledge]** In the ShooterGame / Fortnite-style setup, each connection gathers the actor lists of the grid cell(s) it is in. Distance culling (`NetCullDistanceSquared`) and prioritization run over that gathered list. The "fast shared path" serializes a shared, connection-independent bunch once per frame and reuses it for every connection that needs it. Epic's 2018 tech blog "Replication Graph Overview and Proper Replication Methods" ([link](https://www.unrealengine.com/en-US/tech-blog/replication-graph-overview-and-proper-replication-methods)) covers this but returned HTTP 403.

**Unreal Iris (UE5 replacement for the replication path)**
- Pipeline: filtering (which objects a connection may see) then prioritization (order). The built-in dynamic filters include `UNetObjectGridFilter`: "Divide the game world into cells and only replicate objects in cells near the player's view." There is also `UNetObjectParentRelevancyGridFilter`. — [Epic: Iris filtering](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-filtering-in-unreal-engine); [UE API: ParentRelevancyGridFilter](https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/IrisCore/UNetObjectParentRelevancyGridFil-)
- The sphere prioritizer uses two concentric spheres. Inside the inner sphere priority = InnerPriority; between the spheres it falls off on squared distance; outside it is OutsidePriority. Defaults: InnerRadius 1000 uu (10 m), OuterRadius 5000 uu (50 m), InnerPriority 1.0, OuterPriority 0.2, OutsidePriority 0.1. `SphereWithOwnerBoost` adds `OwnerPriorityBoost` for the owning connection. — [Epic: Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine)
- "Replication priority accumulates across network ticks until an object is replicated and its priority is reset", so low-priority objects eventually reach >= 1.0 and get sent. — [Epic: Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine)
- `NetObjectCountLimiter` caps how many objects of a class are considered per frame. Defaults: MaxObjectCount 2, Priority 1.0, OwningConnectionPriority 1.0, bEnableOwnedObjectsFastLane true. — [Epic: Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine)
- "Prioritization is accomplished with batches of objects to encourage minimal code and data cache misses". No numbers are published. — [Epic: Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine)
- Note: priority here is a cheap scalar function of distance, evaluated per (object, connection) inside the filtered set. Nothing is sorted to find the exact k nearest. Ordering comes from the accumulator plus a byte budget.

**Gaffer On Games priority accumulator (reference pattern)**
- One float per object, kept across frames. Each frame the current priority is added, objects are sorted by accumulator, and the packet is filled until the byte budget runs out. Sent objects reset to 0; skipped objects keep their value, so they "are first in line" next packet. Demo: 901 cubes, at most 64 state updates per packet; player cube priority 1,000,000, interacting 100, at rest 1. — [Gaffer On Games: State Synchronization](https://gafferongames.com/post/state_synchronization/)

**Mirror (Unity)**
- Distance interest management is described as brute force ("checking every entity against every connection is relatively expensive"). It rebuilds on a `rebuildInterval` (in seconds), not every tick, and static objects get a much slower static rebuild interval. — [Mirror: Distance IM](https://mirror-networking.gitbook.io/docs/manual/interest-management/distance)
- Spatial hashing puts entities into a grid and sends each connection everything in its cell's "8-neighbor" grid entries (3x3). That is a square, cell-granular AOI, with no per-entity distance test or sort. "In early uMMORPG tests, Spatial Hashing was 30x faster than distance checking." Mirror now recommends Hex Spatial Hashing for performance. — [Mirror: Spatial Hashing](https://mirror-networking.gitbook.io/docs/manual/interest-management/spatial-hashing); [Mirror: Hex Spatial Hashing](https://mirror-networking.gitbook.io/docs/manual/interest-management/hex-spatial-hashing)

**Photon Fusion**
- Grid AOI: the world is split into fixed-size cells, and each client subscribes to the set of cells around its position. Default AOI radius 32 and cell size 32 (in Shared Mode the radius is capped at 300 and the cell size can't be changed). Default grid 1024^3. "Lowering cell size better reflects designed shapes and generally reduces network traffic but requires more CPU power." — [Photon Fusion: Interest area](https://doc.photonengine.com/fusion-godot/v3-client-server/manual/interest-area); [Fusion 2 AOI sample](https://doc.photonengine.com/fusion/v2/technical-samples/fusion-multi-peer-area-of-interest); [Fusion IM addon architecture](https://doc.photonengine.com/fusion/v2/addons/interest-management/architecture)

**SpatialOS / Improbable (Query-Based Interest)**
- Workers declare queries like `RelativeSphereConstraint(20)`. Queries can carry a **frequency** per interest region, so a worker can take less frequent updates for less time-sensitive regions. The runtime has to match queries against changing entities efficiently to update a routing table. — [US patent 10579434, Improbable](https://patents.google.com/patent/US10579434); [USPTO 10534649](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/10534649)
- No public per-query cost numbers were found.

**Star Citizen (server meshing / entity streaming)**
- Entities are in a client's or server's streaming bubble if their **projected screen size on a virtual 1080p plane is > 5 pixels** at the player's distance. So bubble membership is a size-over-distance (angular size) test, not a fixed radius: a moon is in from far away, a ship only when close. — [CIG Server Meshing & Persistent Streaming Q&A (Comm-Link 18397)](https://api.star-citizen.wiki/comm-links/18397)
- Only one server node has authority over an entity. Authority moves when the entity leaves the authoritative server's bubble, and can be moved on demand to load-balance. — [same](https://api.star-citizen.wiki/comm-links/18397)

**EVE Online (grids + time dilation)**
- TiDi (CCP Veritas, 2011-04-22) slows the game clock in proportion to load, so the tasklet queue stays minimal. The blog's hypothetical: ~1,600 players, dilation ~5% on warp-in, ~30% in steady fighting, ~10% on retreat. There is "a hard limit on how far we're willing to dilate." Rationale: most load in large fights is clock-driven (modules, physics, travel, warp-outs), so slowing time lowers it proportionally. — [CCP: Introducing Time Dilation](https://www.eveonline.com/news/view/introducing-time-dilation-tidi)
- TiDi starts when a node can't process its queue within EVE's 1 s tick. Big fights get a reinforced node that handles only that system. Fights of 6,000+ ships have been dilated to the 10% floor (1 game second = 10 real seconds). — [EVE University wiki: Time Dilation](https://wiki.eveuniversity.org/Time_Dilation); [DataCenterKnowledge](https://datacenterknowledge.com/servers/experiencing-heavy-server-load-just-slow-down-time) (one search snippet said "20 seconds", which contradicts the 10% floor; treat it as unreliable)
- EVE's AOI is the "grid": everything on the same grid is relevant to everyone on it, so EVE does not do per-client neighbor selection in fights at all. It accepts O(N^2) broadcast and pays with TiDi. **[unverified detail; consistent with the TiDi sources]**

**PlanetSide 2 (Forgelight, Daybreak)**
- 2023 high-pop performance investigation: the culprits were character "creation" in a zone (happening a little more than once per second from players trickling into active combat zones) and NPCs (for example CTF NPCs) sweeping large areas for nearby players. The fix: NPCs do "more focused checks on who is interacting with them" instead of querying "any character in range, even those not intending to use them or those not within line-of-sight". New profiling zones were added. — [MassivelyOP, March 2023](https://massivelyop.com/?p=429961); [PS2 patch notes Mar 8 2023](https://www.planetside2.com/patch-notes/mar-8-2023); [PTS Jan 31 2023](https://www.planetside2.com/patch-notes/pts-update-jan-31-23)
- PS2 caps infantry render (replication) distance, and the cap was raised in a later test ("Snipers rejoice"). The exact meters could not be fetched. — [PC Gamer](https://pcgamer.com/planetside-2-infantry-render-distance)
- No public GDC talk on PS2's interest management with numbers was found.

**Roblox (instance streaming)**
- `StreamingMinRadius` (default 64 studs): content streams in at highest priority and never streams out. `StreamingTargetRadius` (default 1024): the maximum stream-in distance; a smaller value "reduces server workload". Content beyond the target can be kept if memory allows, which is a hysteresis-like retention. The focus is the character or `Player.ReplicationFocus`. — [Roblox: Instance streaming](https://create.roblox.com/docs/workspace/streaming)

**Minecraft**
- `entity-broadcast-range-percentage` scales each entity type's default tracking range (50 = half range) to trade visibility distance for traffic. Ranges are per entity type, not per crowd. — [Minecraft 20w18a technical changes](https://minecraft.net/en-us/article/minecraft-snapshot-20w18a)

**MAG (Zipper, PS3, 2010, 256 players)**
- 256 players in an 8-player squad / 4-squad platoon / 4-platoon company hierarchy, on "a new server architecture". The organization mostly keeps contact within the squad. — [Wikipedia: MAG](https://en.wikipedia.org/wiki/MAG_(video_game)); [Engadget hands-on](https://www.engadget.com/2009-09-05-hands-on-mags-256-man-multiplayer.html)
- No technical detail on MAG's relevancy system was found.

**Foxhole, WoW, Hytale, Battlefield**
- No primary technical sources on their AOI algorithms were found this session (Foxhole dev blogs cover regions and queues, not networking internals). See Gaps.

### Inferences
- Every shipped system found works in two stages: a **cheap, coarse, shared** candidate set (cell lists, built once per tick or kept across frames), then **per-connection ranking by an accumulated scalar**, cut by a byte or count budget. The ranking key is never computed by an exact kNN. When the cell holds more than the budget, the accumulator plus budget decides who is sent this tick, and everyone in the candidate set rotates through over a few ticks.
- In the coarse-cell designs (Mirror 3x3, Fusion cell subscriptions), every client in the same cell gets the **same candidate set**. Per-client work goes into ranking or budgeting, not into the spatial search.
- Star Citizen's screen-size criterion and Iris's two-sphere falloff both say the same thing: relevance is a smooth, monotone function of distance (and size). An exact rank order isn't required.
- EVE and PS2 both show that the real killer in big fights is O(N^2) **per-entity sweeps** (NPC proximity, zone entry), not bandwidth. This matches lattice-net's own finding that `select_nth_unstable` is 46% of CPU in the pile.

### Gaps
- No published per-connection CPU cost for Replication Graph or Iris at 100 players, and no published Fortnite CellSize. The Epic 2018 tech blog 403'd.
- No primary technical source for PS2's (Forgelight) server-side interest system, the actual infantry render distance, or how it degrades in 300+ player fights.
- Nothing found for WoW layering/phasing internals, Hytale, Battlefield 2042 (128 players), or Foxhole's networking.

## 2. Heuristics that avoid exact per-client kNN

### Takeaway
There are well-precedented ways to replace "exact k nearest per client per tick". In order of how directly they are backed by shipped practice: (a) cell-granular candidate sets shared by every client in the cell, with no per-entity sort (Mirror, Fusion, Replication Graph); (b) a priority function plus accumulator that is evaluated only over a bounded candidate set and drained by a budget (Iris, Gaffer); (c) density-adaptive staggering, with more frequency buckets the more actors a node has (Replication Graph `BucketThresholds`); (d) a rebuild interval: recompute membership every N ticks or on cell change (Mirror `rebuildInterval`); (e) hard count caps per class or cell (Iris `NetObjectCountLimiter`); (f) global degradation (TiDi). Distance-bucket radix selection and random sampling are sound engineering, but no shipped game was found that documents them.

### Cited Findings
- **Shared cell candidate lists:** Mirror sends all entities in the 3x3 neighborhood of the client's cell, with no distance check, 30x faster than distance checks in uMMORPG tests. — [Mirror: Spatial Hashing](https://mirror-networking.gitbook.io/docs/manual/interest-management/spatial-hashing). Fusion clients subscribe to whole cells around them. — [Photon Fusion](https://doc.photonengine.com/fusion-godot/v3-client-server/manual/interest-area). Replication Graph nodes share actor lists "between client connections" across frames. — [Epic RG docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/replication-graph-in-unreal-engine)
- **Quantized priority instead of exact rank:** Iris uses an inner plateau (all objects within 10 m get priority 1.0, so the order among them is set by accumulated age, not distance), a falloff band, and a constant outside value. — [Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine). This amounts to distance bucketing: inside a plateau the distance ordering is deliberately thrown away.
- **Temporal reuse / rebuild interval:** Mirror recomputes visibility every `rebuildInterval` seconds, with a slower static interval. — [Mirror: Distance IM](https://mirror-networking.gitbook.io/docs/manual/interest-management/distance). Replication Graph keeps node data "across multiple frames". — [Epic RG docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/replication-graph-in-unreal-engine)
- **Density-adaptive frequency:** Replication Graph's dynamic spatial frequency / frequency-bucket nodes add buckets as the actor count grows ("more buckets when there are more actors"). — [UE API FSettings](https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Plugins/ReplicationGraph/UReplicationGraphNode_ActorListF-/FSettings); [UE 4.26 DynamicSpatialFrequency](https://docs.unrealengine.com/4.26/en-US/API/Plugins/ReplicationGraph/UReplicationGraphNode_DynamicSpa-/FDynamicSpatialF-/index.html)
- **Count caps:** Iris `NetObjectCountLimiter`, MaxObjectCount (default 2) per frame, with an owner fast lane. — [Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine)
- **Interaction-based relevance instead of range sweeps:** PS2's 2023 fix replaced "any character in range" sweeps with checks limited to characters actually interacting / in line of sight. — [MassivelyOP](https://massivelyop.com/?p=429961)
- **Owner/interaction boosts:** `SphereWithOwnerBoost` (Iris) and the Gaffer 1,000,000 / 100 / 1 tiers (self, interacting, at rest). — [Iris](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine); [Gaffer](https://gafferongames.com/post/state_synchronization/)
- **Retention hysteresis:** Roblox never streams out within MinRadius and may keep content beyond TargetRadius. — [Roblox streaming](https://create.roblox.com/docs/workspace/streaming)
- **Crowd aggregation:** in VON, under crowding, "a capable superpeer or the server would be called in as an 'aggregator' to take care of the management of several overloaded cells". — [gamedev.net VON thread](https://gamedev.net/forums/topic/468156-voronoi-state-management-for-p2p-mmogs/)

### Inferences (applied to lattice-net; engineering judgement, not sourced practice)
- **Per-cell shared near candidates.** Build the k nearest (or a distance-bucketed set) **once per occupied cell** around the cell centre, using a finer cell than 32 m where it's dense (or a fixed-count k-d/quadtree leaf). Each client then refines only that list of ~k + margin by its own distance plus squad. Cost per tick becomes O(occupied cells x local density) + O(N x k) instead of O(N x density). In a 25 m pile there may be only 1-4 cells, so the expensive part runs a handful of times rather than 10k times. Error: a client at a cell edge misses at most the ring of entities between its true k-ball and the cell-centred ball. Tolerable if the margin is ~1 cell or the neighbors' lists are unioned (Mirror's 3x3 idea).
- **Distance-bucket (radix) selection instead of `select_nth_unstable`.** Squared distance quantized into 16-32 bins (log or linear up to near_radius) gives a counting pass that finds the cutoff bin, then takes whole bins, with only the boundary bin partially taken. This is O(candidates) with tiny constants and no swaps; ties are broken by the accumulator anyway, as Iris's plateau does. It doesn't fix O(N) candidates per query in the pile, so pair it with the per-cell sharing above.
- **Temporal reuse.** Recompute a client's near candidate set only every 2-4 ticks (staggered by client id, like the existing mid/far stagger), or when it has moved more than ~X m or the local density has changed. Between recomputes, update distances only for the cached ~100. That gives Mirror-style rebuild intervals, a /2-/4 cost cut, plus implicit hysteresis against flicker.
- **Density-adaptive k or cap.** Replication Graph adds frequency buckets as density grows. The analogue: when a cell's count exceeds a threshold, sample the candidates (stratified by sub-cell, or by entity-id hash mod M, like the mid/far stagger) instead of scanning all of them. Every entity is still seen within M ticks, and the accumulator handles fairness.
- **Interaction first.** Squad, recent damage dealers or targets, and whoever is in the scope/aim cone should be added explicitly (lattice-net already does squad). Then the distance kNN only has to fill the remaining slots, and in a pile those are "anyone close", for which exact order hardly matters.
- These heuristics fit the CLAUDE.md degradation ladder: a pile can trip a "candidate sampling" level before radii shrink.

### Gaps
- No shipped game was found that documents random/stratified sampling of crowd members for AOI, or distance-histogram selection. Both are inferences.
- No public source on how Fortnite behaves in late-game piles (bots plus 100 players in a small circle) in terms of relevancy CPU.

## 3. Replication Graph / Iris scaling and per-connection cost figures

### Takeaway
Epic documents the *architecture* that scales (shared spatial nodes, frequency buckets, fast shared serialization, priority accumulation, batched prioritization), but this session found no public per-connection microsecond figures. The only hard Fortnite number in the docs is 100 players / ~50,000 replicated actors.

### Cited Findings
- 100 connected players and ~50,000 replicated actors per Fortnite BR match; the traditional path "will bottleneck the server's CPU". — [Epic RG docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/replication-graph-in-unreal-engine)
- Iris prioritization is batched for cache efficiency; no numbers published. — [Iris prioritization](https://dev.epicgames.com/documentation/en-us/unreal-engine/iris-prioritization-in-unreal-engine)
- Replication Graph is disabled on console builds (no splitscreen support). — [Epic RG docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/replication-graph-in-unreal-engine)

### Inferences
- Fortnite's problem is the inverse of lattice-net's: few connections (100) and many actors (50k), mostly static or dormant. lattice-net has 10k connections that are each also an entity. Lessons that carry over: share spatial results across connections, serialize once (lattice-net already serializes once per tier), and use frequency buckets. Fortnite's per-connection cost figures wouldn't transfer anyway.

### Gaps
- Per-connection CPU for RG/Iris; Fortnite grid cell size and frequency-bucket config (the Epic tech blog 403'd; [Epic 2018 tech blog](https://www.unrealengine.com/en-US/tech-blog/replication-graph-overview-and-proper-replication-methods) is the place to look).

## 4. Quality metrics (pop-in, churn/flicker, fairness) and how they're measured

### Takeaway
The academic metric with a name is **neighborship consistency** (fraction of the true AOI members that a node knows). Boulanger et al. measured IM algorithms by update-message count against an ideal visibility baseline. No shipped game was found publishing churn or pop-in metrics. Gaffer's accumulator implies a bounded staleness metric: max ticks between updates per entity.

### Cited Findings
- Neighborship consistency = known nodes / actual nodes within the AOI. Adaptive AOI buffers and critical-node detection were proposed to improve it in VON. — [Jiang et al., Enhancing Neighborship Consistency for P2P NVEs](https://staff.csie.ncu.edu.tw/jrjiang/publication/Enhancing%20Neighborship%20Consistency%20for%20p2p%20distributed%20VE.pdf); [JIT article](https://jit.ndhu.edu.tw/article/view/477)
- Boulanger, Kienzle, Verbrugge (NetGames '06) compared 8 IM algorithms on real player traces in the game Mammoth: 3 radius-based, 5 that also account for obstacles. Obstacle-aware IM cut update messages by **up to 6x**, "computationally inexpensive tile-based interest management algorithms can approximate ideal visibility-based interest management at very low cost", and random-action bots approximate human traces if their starting positions are chosen well. — [Boulanger et al. 2006 PDF](https://www.sable.mcgill.ca/~clump/papers/boulanger-06-comparing.pdf); [NUS mirror](https://www.comp.nus.edu.sg/~cs4344/0607s1/netgames06/s01Conf96_a32.pdf); [Boulanger thesis](https://www.cs.mcgill.ca/~jboula2/thesis.pdf)
- The accumulator guarantees that skipped objects are first in line next packet, which bounds starvation. — [Gaffer](https://gafferongames.com/post/state_synchronization/)

### Inferences
- Metrics worth adding to lattice-sim for any approximate selector, measured against the exact kNN on the same tick (offline or sampled 1-in-N clients):
  - **recall@k**: |approx ∩ exact| / k, overall and for the nearest 16;
  - **max missed rank**: the closest entity missing from the near set;
  - **churn**: near-set entries added or removed per client per second, which is the flicker driver;
  - **staleness**: max ticks since the last update per in-radius entity;
  - **combat fairness**: P(shooter or target is not in the other's near tier).
- The "nearest 16 must be exact" class matters most for combat. Recall over the far end of k matters little in a pile, where everyone is within a few meters.
- Boulanger's finding that tile-based IM approximates ideal visibility "at very low cost" is the academic justification for cell-granular selection.

### Gaps
- No shipped game publishes churn/pop-in metrics. The Bungie GDC 2011 talk "I Shot You First" (Halo Reach networking, David Aldridge) covers priority and bandwidth but couldn't be fetched (video). — [Game Developer](https://www.gamedeveloper.com/business/gdc-vault-adds-free-crawford-i-halo-reach-i-maxis-lectures)

## 5. Academic AOI work (aura/nimbus, VON, surveys, extreme density)

### Takeaway
Academic work splits AOI into region-based (cells/tiles, cheap, shared) and object/aura-based (per-pair distance, exact, expensive). Under crowding, the standard answer is aggregation: a server or superpeer takes over overloaded cells. Hierarchical, octree-based multicast groups are the other classic answer. Nothing found addresses 10k entities within 25 m directly. The literature assumes per-node AOI caps or aggregation.

### Cited Findings
- VON (Hu, Chen, Chen) keeps the P2P topology via Voronoi diagrams so each node finds its AOI neighbors. Neighbor discovery is the core problem. — [VON: A Scalable P2P Network for Virtual Environments](https://teacher.tku.edu.tw/StfFdDtl.aspx?tid=341689); [VON procedures](https://vast.sourceforge.net/docs/pub/VON-procedures.pdf)
- Under crowding, an aggregator (superpeer or server) manages several overloaded cells. — [gamedev.net VON thread](https://gamedev.net/forums/topic/468156-voronoi-state-management-for-p2p-mmogs/)
- Surveys: "Interest Management for Distributed Virtual Environments: A Survey" (Liu & Theodoropoulos, ACM CSUR) — [IBM Research](https://researcher.ibm.com/publications/interest-management-for-distributed-virtual-environments-a-survey); Glasgow survey (Trinder group) — [PDF](https://www.dcs.gla.ac.uk/~trinder/papers/Survey_camera_ready.pdf)
- Three-tiered IM for large-scale VEs with dynamic multicast groups on a load-balanced octree (Zyda group, VRST '98). — [PDF](https://mikezyda.com/resources/pubs/VRST98.pdf)
- MOPAR: cell-based P2P IM in which players in a cell connect to the cell's master node and master nodes connect to adjacent cells. — [MOPAR PDF](https://www.comp.nus.edu.sg/~bleong/hydra/related/yu05mopar.pdf)
- Frequency-competitive query strategies for maintaining low "congestion potential" among moving entities (arXiv 2022). This is a theory paper on how often to re-query moving entities' neighborhoods, relevant to the temporal-reuse heuristic. — [arXiv 2205.09243](https://arxiv.org/pdf/2205.09243)

### Inferences
- The aura/nimbus model (Benford & Fahlén, DIVE/MASSIVE) separates what you can perceive (nimbus, set by the target: size or loudness) from your focus (by the observer: view direction). Star Citizen's screen-size test is effectively a nimbus. A lattice-net analogue: vehicles or MAXes get larger near radii than infantry, and the observer's aim cone boosts priority. **[aura/nimbus definitions from background knowledge; no source fetched]**
- The academic consensus that region-based IM is cheap and approximately good backs per-cell shared candidate lists. Aggregation of overloaded cells maps onto computing the near set once per dense cell.

### Gaps
- Morgan/Lu/Storey interest management middleware paper: not fetched.
- No paper found that evaluates AOI selection quality vs cost at extreme densities (thousands within tens of meters). That regime seems unstudied publicly; most work targets P2P topology maintenance.
