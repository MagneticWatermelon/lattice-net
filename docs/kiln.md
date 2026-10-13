# Kiln, the desert continent

*Map design, 2026-10-13 (from the design doc "Lattice map design: Kiln"). Every number is proposed, sized to the engine's current limits. The last section, **K0 as built**, says what exists (`game/src/kiln/`) and where it departs from this.*

Kiln is the launch continent: an 8 km square of desert, canyon and glass under a ringed sky, built around three rifts that meet in a central crater. Fights happen on five layers, from tunnels under the canyon floors to spires 600 m up, and every base spans at least three of them. It takes over from Harrow as the launch continent; Harrow stays the procedural test map.

## At a glance

| what | Kiln |
|---|---|
| Size | an 8,192 m square, the wire format's limit. A 400 m escarpment, the Rim, bounds about 55 km² of play |
| Heights | 10–620 m, inside the 0–655 m the wire carries: about 600 m of relief |
| Layers | five: Deep, Floor, Plain, Mesa and Spire, plus the air above |
| Shape | three rifts up to 320 m deep, meeting in the Hollow, a crater 1.6 km wide |
| Sites | 67: 3 Landings, the Hollow, 9 major facilities, 18 bases, 36 outposts |
| Lanes | about 110, 2–4 per site, mostly 500–1,500 m long |
| Tunnels | about 30 km: approaches into bases, and the Undervein between sites |
| Fight sizes | outposts 24–150, bases 100–600, majors 400–2,000, the Hollow up to 4,000 |
| Day | a 2-hour cycle: 70 min of day, 10 of dusk, 30 of night, 10 of dawn |

## What grand means on Kiln

Grand comes from scale you can see and use: landmarks, long sightlines, sudden drops and fights in the distance. Size alone doesn't do it; an empty 8 km map is just a long walk. So:

- **Landmarks to steer by.** The Heartwell, a spire rising to about 610 m out of the Hollow, can be seen from anywhere. The Span arches, the Geodes' glow and the great arches of the Glasslands come next, so a player knows roughly where they are without opening the map.
- **Scale by contrast.** Tight spaces open onto huge ones: a 4 m tunnel ends on a ledge 300 m above a rift floor.
- **Fights you can watch.** From the Mesa layer you can watch battles 1–3 km away, drawn from the activity cells the server already sends (shots per 128 m cell per faction every 0.5 s): a vista costs nothing extra on the wire. At night they're brighter.
- **Every base is a set piece.** Each has its own silhouette and spans at least three layers: taking one means a climb, a descent, or both.
- **Travel is a journey.** Every lane changes layer or biome on the way. Vehicles, infantry and aircraft get different routes.
- **Alive, not empty.** Plants and creatures move, glow and change with the time of day.
- **Readable at a glance.** Each layer has its own materials, the lattice lights the routes, and nights stay bright enough to fight in.
- **Fair for three.** Each faction gets the same kinds of sites at the same lane distances, within 10%, and the Landings rotate between factions each time Kiln reopens after a lock.

## The shape

Three rifts cut out of one crater run from the Hollow, at the center, most of the way to the Rim, and split the continent into three sectors, one per faction. Each rift is the border between two factions' sectors, so every rift is a front; the Hollow, where all three meet, is everyone's front.

- Rifts at 90°, 210° and 330° (north up); sectors centered at 30° (**I, the Glasslands**), 150° (**II, the Dune sea**) and 270° (**III, the Dry Sea**).
- **The land rises toward the center.** Outer plains 160–240 m; the mesa country, 1.4–2.6 km from the center, climbs to 300–460 m; the Crown, the plateau round the Hollow, stands at about 420 m. Heading for the middle means climbing all the way, then looking down 340 m into the Hollow.
- **The rifts deepen as they go in:** about 250 m wide and 100 m deep at the outer end, 600 m wide and 320 m deep where they open into the Hollow. Their floors are the low road to the center; the Crown is the high one.
- **Every sector has the same skeleton:** a Landing near the Rim, 3.3 km from the center; a Geode in its mesa country, 2 km out; a Cliffhold in the wall of one of its border rifts, near the outer end.
- **Every rift has a Span** halfway along, 2 km from the center: a bridge fortress, and the main way across.
- **The outer biomes differ by sector:** fused glass plains (I), a dune sea (II), a dry sea bed (III). The mesa country, the rifts and the Crown run through all three.
- **The Landings rotate.** Each time Kiln reopens after a lock, every faction moves one Landing over.
- **Bases and outposts fill in between:** 18 and 36, six and twelve per sector, on the lanes between the great sites.

## Biomes

| biome | where | layers | terrain and features | how it plays |
|---|---|---|---|---|
| The Hollow | the center, 1.6 km across | all five | a crater 340 m deep with terraced walls; the Heartwell spire in the middle, the Heart chamber under it | the mega-fight, every faction's front at once |
| The Crown | the plateau round the Hollow, to 1.4 km | Mesa, Spire | broken high ground at ~420 m: spires, outcrops, the upper lips of the rifts | infantry and aircraft hold the high ground over the rift mouths |
| The rifts | three, from the Hollow most of the way to the Rim | Deep to Mesa | sheer walls, side canyons, ledges, glowing veins, rockfalls | vehicles on the floor, infantry on the ledges, tunnels in the walls |
| Mesa country | a ring 1.4–2.6 km out | Plain, Mesa, Spire | mesas, buttes, hoodoos and natural arches; slot canyons 2–6 m wide and 30–80 m deep | snipers and aircraft on top, infantry in the slots, vehicles on the flats |
| The Glasslands | Sector I's outer plains | Deep, Plain | plains of fused glass, fulgurite forests, buried vaults | open vehicle country with long sightlines; infantry move through the forests and vaults |
| The Dune sea | Sector II's outer plains | Deep, Floor, Plain | dunes up to 60 m; cenotes 80–120 m deep that open into caverns; sandfalls | dunes break up vehicle sightlines; cenotes drop infantry into the caves |
| The Dry Sea | Sector III's outer plains | Deep, Floor, Plain | a salt-flat basin; petrified reefs 30–80 m tall full of caves and arches; skeletons of megafauna | the flats belong to vehicles, the reefs and bones to infantry |
| The Rim | the outer 400 m of the square | out of play | an escarpment rising to ~460 m | bounds play and makes the skyline |

The shapes are familiar desert forms, so players read the ground at a glance. What makes Kiln alien is the material and the life: glass where sand should be, veins of light in the rock, things that breathe and move, and a ring across the sky.

## Layers

| layer | heights | what's there | who moves there | looks like |
|---|---|---|---|---|
| Spire | 460–620 m | spire tops, arch crowns, the highest bridges, the Heartwell's galleries | infantry by lift or jump jet; aircraft | pale, wind-polished stone and crystal |
| Mesa | 300–460 m | mesa tops, the Crown, the Hollow's upper terraces, the Span decks | infantry and aircraft; vehicles up the ramped mesas | banded red and ochre rock |
| Plain | 160–240 m | the outer plains, the flats of mesa country, the rifts' outer rims | vehicles first, everyone else too | the sector's own ground: glass, dunes or salt |
| Floor | 60–120 m | rift floors, the Hollow's floor, cenote bottoms, the Dry Sea basin | vehicles and infantry | dark basalt and scree, veins in the walls |
| Deep | anywhere under the surface, down to 10 m | tunnels, caverns, the Heart chamber, base undercrofts | infantry only | rock lit by its veins, never dark |

Vehicles keep to the Plain and the Floor; vent lifts, switchbacks, drop shafts and tunnels carry infantry between all five. Rules for every layer:

- **Sites span layers.** Every base spans at least three, every outpost two; the majors and the Hollow four or five.
- **There's always a way up or down.** Inside a site, no spot is more than 150 m from a way to the next layer, and each pair of adjacent layers is joined at least twice, by two kinds of connector.
- **Vehicles stay in the middle:** the Plain and the Floor, plus ramps up some mesas. Never the Deep or the Spire.
- **Falls are a choice.** Fall damage starts past 8 m, so paths along higher edges get a lip or a rail. A drop of more than 30 m beside a site gets a drop shaft or a lift.
- **Every high point has a counter:** a second way up, cover against it, or a line of sight onto it from another high point.

| connector | what it is | climbs | who |
|---|---|---|---|
| Ramps and switchbacks | roads cut into slopes and walls | any height | vehicles and infantry |
| Rock stairs | stairs and ladders carved into the rock | up to ~60 m | infantry |
| Vent lifts | lattice vents whose updraft carries you up, on a 30 s cycle | 50–200 m | infantry |
| Drop shafts | updrafts that slow a fall; one way, down | up to 340 m | infantry |
| Lattice springs | jump pads grown from the lattice | 10–30 m, or across a gap | infantry |
| Lifts | elevators in bases, the Span pylons and the Heartwell | up to 300 m | infantry |

## Tunnels

Tunnels give infantry a way into every base that vehicles can't follow, and a way across the map that aircraft can't see. Four scales, about 30 km in all:

| kind | length | what it links | how many |
|---|---|---|---|
| Base tunnels | 50–150 m | a base's spawn room to its points, under the courtyard; one is the spawn room's covered exit | 2–3 per base |
| Approach tunnels | 150–300 m | the ground outside a base to its undercroft or a point | 1–2 per base |
| Great-site networks | 0.5–2 km each | the levels of a major facility or the Hollow (the Span's wall tunnels, the Cliffhold's galleries, the Heart chamber's three systems) | one per great site |
| The Undervein | 0.5–1.5 km per tube | site to site, through lava tubes and dead veins under the lanes | about 12: two under each rift, the rest under the outer biomes |

Each tunnel has chambers to fight in, junctions, shafts to the surface and, in the rift walls, galleries with windows over the drop. Rules: room to fight (≥ 2.2 m wide where two pass, ≥ 2.6 m high where players jump; main tubes 4–8 m wide and 4–10 m high, a 20–60 m chamber every 100–150 m); infantry only; no dead ends (two ways out, ≤ 400 m between junctions or exits); mouths fair (visible, defensible, marked, never inside a spawn room); never dark (veins light them); owned with their region (the spawn room's shield stops attackers at the inner end); sound follows the tubes. The continent package marks them as areas joined by portals: players in areas that can't see each other get the lowest priority, and players more than two portals apart aren't sent at all.

## A living world

Kiln's cycles (light, life, vents, bridges) run on the server's step clock and the continent's seed, so every client sees the same world without a byte on the wire.

- **The ground:** banded mesas, buttes and hoodoos, natural arches up to 300 m across; slot canyons; the Glasslands' fulgurite spikes 10–40 m tall; the Dune sea's cenotes with sandfalls; lava tubes under everything; the Dry Sea's petrified reefs and colossal skeletons, big enough to fight inside.
- **The life:** sunsails (membranes 20–40 m across on tall stalks: the stalks are cover, the membranes block nothing), glass reeds that chime, lattice coral that glows at night, sand-whales (a ridge of moving sand that never changes the ground), night flocks. Nothing cosmetic hides a player or stops a shot; anything that does is collision in the continent package.
- **The lattice made visible:** veins of light run through the rock along the lanes; wells glow in their owner's color; a captured site's veins shift to its new owner's; a lattice surge lights every vein at once.
- **Clockwork:** vent lifts run 20 s then rest 10 s; living bridges (coral spans) grow across chosen gaps at dusk and wither at dawn after a minute's warning, opening night-only routes; sandfalls shift every 20 minutes.

## Day, night and weather

| phase | length | what changes |
|---|---|---|
| Day | 70 min | sunsails open; the ring is a pale arc across the sky; glare off the glass and the salt |
| Dusk | 10 min | sunsails fold; living bridges grow; veins start to glow; long shadows across the rifts |
| Night | 30 min | the ring lights the land blue; veins, coral and wells glow; night flocks rise; living bridges are open |
| Dawn | 10 min | living bridges wither after a minute's warning; flocks settle; sunsails open |

Nights are bright (Tessel's ring reflects sunlight onto the night side); every soldier has a rim light and their faction's trim; nothing that hides a player is cosmetic, so settings never help; night changes routes, not visibility. **Glass storms** (proposed; decide after playtests): a storm of glass dust scheduled from the seed covers one region for 10–15 minutes a few times a cycle; visibility drops to ~150 m for everyone; the storm is a volume in the world, and the server shrinks the mid and far tiers for players inside it.

## Sites and fronts

| kind | count | where | size | points | fight size |
|---|---|---|---|---|---|
| Landing | 3 | one per sector, near the Rim, 3.3 km from the center | ~300 m | none | can't be taken |
| The Hollow | 1 | the center | a crater 1.6 km across | 5, on 3 layers | 1,500–4,000 |
| Span | 3 | halfway along each rift | ~500 m | 4 | 400–2,000 |
| Cliffhold | 3 | a rift wall near its outer end, one per sector | a wall ~140 m high, 7 terraces | 4 | 400–1,500 |
| Geode | 3 | each sector's mesa country | ~250 m | 4 | 400–1,500 |
| Base | 18 | 6 per sector | 90–200 m | 2–3 | 100–600 |
| Outpost | 36 | 12 per sector | 40–80 m | 1 | 24–150 |

The lanes: they follow the lattice (veins in the rock); about 110, 2–4 per site, 500–1,500 m, each changing layer or biome on the way. Each Landing links to three sites in its own sector. The Hollow has six lanes: one up each rift floor from its Span, one over the Crown from each sector. Each rift can be crossed at least five ways: its Span, two smaller bridges, a pair of ramps down to the floor and up the far wall, two Undervein tubes. From every Landing, the shortest path to the Hollow is three lanes and about 3 km, within 10% across the sectors; lattice transit adds 2 s of spawn time per lane. Supply works as the GDD says: on Kiln, cutting supply usually means taking a Span, or the bases where a rift meets the outer plains.

How the fronts form: each faction starts with its sector and meets its two neighbors along the rifts. Holding a Span carries you into a neighbor's sector; holding the Cliffhold on your side keeps your neighbor from doing the same close to your Landing. The third faction is never far from a rift fight: it can come down from the Crown or up through the Undervein. The Hollow is open to all three at once.

## The great sites

**The Hollow** (a crater 1.6 km across and 340 m deep, all five layers): the rim (~420 m, three fortified gates where the Crown lanes arrive); the terraces (broken rings of ledges down the crater wall, joined by ramps, stairs, vent lifts and drop shafts); the floor (~80 m, where the rifts open in); the Heartwell (a spire from the floor to ~610 m, the largest well, lifts to galleries near the top); the Heart chamber (Deep, under it, reached by three tunnel systems and shafts). Points: the three rift-mouth plazas, the Heart chamber, the Heartwell's gallery. The owner's spawn room is in the Heartwell's foot. Holding the Hollow makes its owner's spawns 2 s faster everywhere on Kiln.

**The Spans** (one per rift, 2 km out, where the rift is ~450 m wide and 220 m deep; ~500 m long, four layers): the deck (Mesa: a vehicle road, a gatehouse at each bridgehead), the gallery (an enclosed infantry level under the deck, windows on both sides), the pylons (two towers on the floor, lifts and stairs to the gallery and deck; the owner's spawn room at the top of one), the arches (two, ~150 m over the deck, stairs inside to perches), the rift floor (its road under the bridge; aircraft fly under the deck), the wall tunnels (Deep: from the floor to each bridgehead). Points: both bridgeheads, the gallery's middle, a pylon foot. Holding a Span gives a forward vehicle terminal on each bridgehead.

**The Cliffholds** (in a rift wall near its outer end, 3 km out, where the wall is ~140 m high; four layers): the floor gate (a gatehouse and vehicle bay), the terraces (seven, joined by stairwells and two lift shafts), the summit keep (on the rim; its tower rises ~80 m higher, the only high point for a kilometer), the deep galleries (halls behind the terraces, an approach tunnel from the plains). Points: the floor gate, the middle terrace, the summit keep, the inner gallery. The spawn room is mid-wall. Holding one gives air pads on the summit.

**The Geodes** (a crystal dome in each sector's mesa country, 2 km out: ~250 m across, crest ~150 m above the flats, floor ~100 m below; four layers): the crest (cracks look down inside), the cracks (the ways in, too narrow for vehicles), the crystal terraces (ledges round the walls, stairs and lattice springs), the well chamber (Deep). Points: the well chamber, a terrace, the crest, the main crack. At night a Geode lights its sector like a lantern. Holding one gives every player of its faction on Kiln 10 extra Mass a minute.

## Bases and outposts

| base kind | where | layers | the idea | ways in |
|---|---|---|---|---|
| Mesa citadel | on a mesa, any sector | Plain, Mesa, Deep | a walled town on a mesa top; one road climbs to it | the ramp road, a lift shaft and a cave stair, an approach tunnel, the air |
| Slot hold | across a knot of slot canyons | Plain, Mesa, Deep | walkways and bridges over the slots, rooms cut into their walls | the slots from three sides, the bridges on top, a drop from the mesa |
| Rift ledge | on a broad ledge halfway down a rift wall | Floor, Plain, Deep | a base over the drop | a lift or drop shaft from the rim, a switchback from the floor, the wall tunnels |
| Cenote | round a cenote, Dune sea | Plain, Floor, Deep | rings of buildings round the mouth and down the walls, a cave at the bottom | the dunes, lifts and the drop, caves from the Undervein |
| Reef warren | in a petrified reef, Dry Sea | Floor, Plain, Deep | corridors through the reef, rooms in its caves, a tower on its crest | the salt flats, cave mouths, the crest |
| Ossuary | inside a colossal skeleton, Dry Sea | Floor, Plain, Deep | a courtyard in the ribcage, a fort in the skull, a burial pit | between the ribs, through the jaw, up from the pit |
| Glass vault | round a buried vault, Glasslands | Floor, Plain, Deep | a gatehouse on the glass, a great hall sunk in a blast crater, halls underground, a fulgurite spire as its high point | the vault doors, the crater walls, approach tunnels |

Each sector's six are its two biome kinds, a Mesa citadel, a Slot hold and two Rift ledges (one on each border rift); Sector III has an Ossuary and a Reef warren. Every sector gets two large (150–200 m, 3 points), two medium (120–150 m, 3) and two small (90–120 m, 2). Outposts, two of each kind per sector: Arch (the point under a natural arch, a perch on its crown), Vent (the point at a vent lift's foot), Tunnel mouth (where an Undervein tube surfaces), Spire perch (on a hoodoo, a lift, a fortified foot), Well shrine (a small well in a hollow, a cave beneath), Wreck (a crashed lattice ship on its side, its decks stacked like floors). Rules: the GDD's base rules hold (§6.3); every base spans three layers, every outpost two; every site has a way in vehicles can't use; every base has a silhouette nameable from 1 km.

## What Kiln asks of the engine

| need | why | status |
|---|---|---|
| An 8,192 m map, heights 0–655 m | the size and the relief | fits today |
| Aircraft above 655 m | the Spire layer tops out at 620 m | proposed: a wider altitude field for vehicles, with M4 |
| Holes in the heightmap | tunnel mouths, cenotes and caves | exists (K1): a bit per 4 m cell; for the package, a hole mask |
| C1: ceilings, rotated boxes, wedges, faction shields | tunnels, ramps, every interior | exists (K1); shields change hands with U5 |
| C2: meshes behind a deterministic BVH | arches, reefs, skeletons, Geode shells, cliff faces, slots narrower than 4 m | needed (the GDD says "only if needed") |
| Force volumes | vent lifts, drop shafts, springs | exists (K1), always on; timed with the clockwork (K4) |
| Clockwork state | vents, living bridges, sandfalls | new: collision and volumes switched by the step and the seed |
| Areas and portals | interest in tunnels and layered bases | new |
| Storm cells | glass storms, if they ship | new, optional |

The exporter's reachability check must understand lifts, vents and drop shafts, and pass with every clockwork feature switched off. Budgets to measure before committing: the server (a 4,000 blob on Kiln's graybox before fixing the Hollow's size; a box budget per 32 m cell, at most 200, enforced by the exporter), the client (HLOD and impostors, occlusion culling in tunnels, lighting that blends through the day; within 500 m the terrain must match the heightmap closely), the wire (only the aircraft altitude), the bots (a navigation graph, or the scale tests can't run on Kiln).

## Build plan

| step | what | done when |
|---|---|---|
| K0: blockout | the whole continent in rough shapes: heightmap, rifts, the Hollow, every site as boxes; bots run the lanes to time them | lane times match across sectors within 10%, and a bot crosses Kiln on every route |
| K1: collision kit | C1, heightmap holes and force volumes, with the cross-platform determinism checks | the determinism script passes on a level with tunnels, lifts and vents |
| K2: great sites in graybox | the Hollow, one Span, one Cliffhold and one Geode, from boxes until C2 lands; a 3,000–4,000 bot blob in the Hollow | the tick holds its p99 budget with 4,000 in the Hollow |
| K3: bases, outposts, tunnels | the 18 bases, 36 outposts, the Undervein and the lanes, with territory from U5 | every site can be captured, and the exporter's reachability check passes |
| K4: the living world | the day cycle, the clockwork features, the life and the lattice glow | a full two-hour cycle with bots, identical on every client |
| K5: art, sound, playtests | final art and audio; a human playtest per sector | each sector played by people, and fixed |

K3 needs U5 (territory); K0 needs the U8 exporter and package format; K2 on needs C2 (graybox with boxes until then). Proposed GDD change: U8 plans to rebuild Harrow by hand as the first authored continent; prove the exporter on Kiln's blockout instead, and keep Harrow procedural.

**Open questions** (with recommendations): the name (a working name: trademark check before anything public); Kiln or Harrow at launch (Kiln: grand is the pitch); how symmetric (same structure everywhere, outer biomes differ; compare capture rates per sector once live); day length (two hours, revisit with playtest data); clockwork now or later (always-on vent lifts in the blockout, the cycles and living bridges in K4 if the static version plays well); glass storms (after playtests); site count (67 suits ~2,000–6,000 players; measure fight sizes at several populations in K3's bot tests); rotating Landings (rotate, and redress each Landing for its current owner).

---

## K0 as built (2026-10-13)

`game/src/kiln/` builds Kiln as world id `world::KILN` (`World::shared(KILN)`, `lattice-server --world kiln`): deterministic (f64 and `libm` only, checked Linux against Windows by `scripts/determinism.sh`), in ~0.6 s. Its layout is planned once in a sector's frame and turned into place, so the three sectors are the same but for their outer biomes. `game/examples/kiln_map` draws it from above; `kiln_walk` walks a lane; `kiln_probe` looks at a spot.

**Measured (the K0 bar):**
- **Every route walks.** The tests walk all 114 lanes both ways and the 42 routes inside sites (terraces, spirals, stairs) with the game's own movement and no jumping: 312 walks, all through (`kiln::tests::every_route_walks_end_to_end`).
- **Bots cross Kiln on every route.** `lattice-server --world kiln --spawn lanes --immortal` with `lattice-bots --world kiln --run-lanes --count 228`: 228 of 228 lane walks done (every lane, both ways), 0 corrections.
- **Fair within 10%.** From every Landing the Hollow is 3 lanes and ~353 s sprinting (~3.2 km): 353.0 / 353.7 / 353.3 s. Every kind of site is the same walk from its own Landing in all three sectors within 8.8% (the widest: the Spans, whose decks are axis-aligned, below).

**What's there:** the radial land (plains 192–236 m, mesa flats 236–250, the Crown at 420 with its 170 m escarpment, the Rim to 460); three rifts (600 → 256 m wide, floors 80 → 92 m, 6:1 walls); the Hollow (floor 80 m, five terraces of 68 m with switchback ramps from each rim gate, the three rift mouths, the Heartwell's blocks to 610 m); mesas and buttes in the mesa country, outcrops on the Crown; dunes to 55 m (II), the salt basin at 112 m and its reefs (III), glass undulations and blast scars (I); 67 sites in rough shapes with 130 control points; 114 lanes (graded roads ≤ 0.36, ramps ≤ 0.33 up the escarpment and the Span bridgeheads, ≤ 0.4 down rift walls to the ledges, level bridge decks); ~9,500 boxes (the busiest 32 m cell 161, under the 200 budget); the ground's material per height sample (sand, glass, salt, ochre, basalt, pale stone, road, pad, scree) for drawing.

**Movement, for cliffs** (`movement.rs`): a slope steeper than 45° is a wall in the air as well as on the ground (a jump used to land a little way up any cliff); a move into one slides along it; and on a cliff cell (a 4 m height cell spanning more than 6 m, `World::cliff`) no move rises at all within a jump of the ground, so a cliff can't be climbed slantwise either. Terrain under a deck (more than a meter below the feet) isn't in the way.

**Where K0 departs from the design** (each waits for what the build plan puts later):
- **No tunnels, caves, lifts, vent lifts, drop shafts or springs** (K1). Every climb is a ramp or a stair of boxes (≤ ~55 m, the cell budget): the Span's floor and deck connect through the Crown-side ledge and the bridgeheads' ramps, not pylon lifts; the Geode's crest and the Heartwell's gallery have points but no way up yet; the rift ledges are reached by ramps down the wall (the design's "lift or drop shaft from the rim"). *(K1 added lifts, drop shafts and the first tunnels: below.)*
- **The Undervein's two tubes per rift** come with tunnels: each rift is crossed by its Span, two small bridges, and the floor (the Cliffhold's terraces down, a ramp up the far wall to the far ledge).
- **Spans at 2.1 km**, not 2.0 (room for the ledge's ramps between the Hollow and the bridgehead); the Crown-side rift ledge at 250 m, the outer one at 165 m.
- **Decks are axis-aligned** (until C1's rotated boxes): rift I's crossings run east-west and square, the other two's north-south, 15% longer: the Spans' walks differ by up to 8.8% across sectors. *(K1: every deck is square to its rift.)*
- **114 lanes** (the design: about 110); a few run 1.7–1.9 km.
- **Slots are 16 m wide**, mesas' faces are heightmap (C2 for narrower slots, arches and overhangs); arches, ribs and hoodoos are boxes.

## K1 as built (2026-10-13)

**The collision kit** (`world.rs`, `movement.rs`, `hit.rs`; GDD §4.1, §6.1): boxes turned about the vertical, wedges (ramps), ceilings (jumps, walks up and standing up stop at undersides), shields that let one faction through (movement, shots, sight, tracers), holes in the heightmap (the terrain a shell: under it the surface is a ceiling and boxes the floor; holes are the only ways through it), and force volumes (lifts, drop shafts, springs). Movement takes the mover's faction now (`step(world, state, input, faction)`). Harrow's hashes didn't change: what it has moves bit for bit as before.

**The bar:** `scripts/determinism.sh` agrees Linux/Windows on a level with tunnels, lifts and vents, twice over: the kit's test level (`lattice_game::kit`, world id `KIT`, `lattice-server --world kit`: a turned wall, a ramp, a slab and a crawlway, a shielded gap, a tunnel 100 m long down holes at both ends, a tower with a lift and a drop shaft, a spring; 60 players of every faction roaming it, its route walked, 20,000 shots across it), and Kiln itself. `kit::tests` test each piece against movement and shots.

**Kiln with it:**
- **Square crossings.** The Spans' decks, galleries, rails, arches and the rifts' small bridges are turned boxes square to their rift: the Spans are no longer the outliers, and every kind of site is now the same walk from its Landing within **7.2%** across sectors (was 8.8%).
- **Lifts and drop shafts** (one-way routes, walked up and down by the tests): the Heartwell's gallery (a walkway out to a lift from the floor and a drop shaft on each sector's side, 470 m at 30 m/s), each Geode's crest (from the upper terrace, a walkway over to the crest), each Span's pylons (from the rift's floor to the deck, a gap in the rail), each rift ledge (from the shelf to the rim, a walkway over the wall's face; where the shelf's flat ends and the rim begins is found from the ground) and each Vent outpost (a vent lift up to a walkway onto a pillar, a drop shaft off its far side; the stairs are gone). Lifts slow over their last stretch so riders bob just over the top and step off.
- **The first tunnels.** Each sector's two Tunnel mouths (O5, O11) are joined by a tube ~1.4 km long: a trench down through holes in the ground (2 cells across, 10 along, axis-aligned so the holes are whole cells, a canopy over its head) to a 16 m chamber 12 m under the ground, then a 6 m tube level from chamber to chamber, dipping only where the ground does (keeping 8 m of rock over its floor). Its lane is a lane like any other (117 now): 234 of 234 walks by bots over the server, 0 corrections.
- **Shielded spawn rooms.** Each Landing has a roofed spawn room with a door at each end shielded for its sector's faction (sector k's Landing is faction k's until the Landings rotate).
- **What a ceiling taught:** K0's Wreck had a deck 1.87 m over the ground at one edge, which a walk only got through because standing into a slab pushed you sideways. Its decks now clear the highest ground under them.

**Measured:** all 59 lattice-game tests (354 Kiln walks, 48 of them one way); the plugin's and Unity's tests (the kit's world through the C API); Unity on Kiln with 300 bots: frame p50 3.8, p99 4.5 ms (as K0), 0 corrections, no managed allocations.

**Left for later:** timed volumes (K4's clockwork); shields changing hands (U5); the rest of the Undervein, caves and cenote caverns (K3); tunnels' own lighting (sunlight leaks onto the walls of shallow ones; K5); C2's meshes.

**In the Unity client** (the Lattice repo): Kiln is the default world (`--world kiln|harrow|N|none`), built on a thread at launch so the Welcome finds it ready (~2 s in). The terrain is splatted from the plugin's per-sample materials (`lt_world_materials`, nine layers, each 2 km tile with the few it uses); the sites' boxes are drawn in four blockout materials by kind; the lanes are veins of light, one additive ribbon per route on whatever it's walked on, with a ring at every control point (`World/LatticeVeins.cs`). No grass or rocks yet (K3/K5). Against a local server on Kiln with 300 bots: 0 corrections, 0 managed allocations per frame, uncapped frame p50 3.9 ms, p99 4.5 ms at 1080p fullscreen. Seen in shots: terrain textures stretch down cliffs (triplanar comes with K5's materials). K1: turned boxes are drawn along their axis, a wedge's slope as a prism on a box; shields as slabs of light in their faction's color, lifts and drop shafts as columns of light (`World/Connectors.cs`); holes are cut out of the terrain (`TerrainData.SetHoles`), and veins follow lanes down trenches and along tunnels.
