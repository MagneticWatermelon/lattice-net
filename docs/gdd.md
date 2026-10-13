# Game Design Document

*Working title **Lattice**. Draft v0.1, 2026-10-09. The game, and the Unity client built on lattice-net.*

**Status tags.**
- **Exists:** built and measured in this repo.
- **Decided:** decided earlier (see CLAUDE.md).
- **Proposed:** this document's recommendation. Accept, change or reject it.
- **Open:** needs your decision. Each one has a recommended answer in §22.

Numbers marked as existing come from the code (`game/`, `client-core/`, `sim/`) and CLAUDE.md. When the code changes, the code wins: update this file.

## Key proposals at a glance

1. **The Unity client embeds the Rust client stack as a native plugin** (`lattice-net`, `lattice-game`, `lattice-client-core` behind a C ABI). C# only draws, plays sound, reads input and shows UI. (§12)
2. **Pump mode first, then a net thread owned by the plugin** (U3). Unity's main thread can stall longer than the connection's 5 s timeout while it loads a continent. (§12.3)
3. **The Unity project lives on the Windows side, in its own repository.** This repo builds the plugin and the generated C# bindings, then copies them over. (§18)
4. **Bring the Unity client to the Bevy client's level (U0–U2) before vehicles (M4).** (§20)
5. **Continents are authored in Unity and exported as a continent package** that both the server and the client load. The procedural world becomes the first package. (§16)
6. **Collision grows before the first authored base:** ceilings, rotated boxes, ramps and faction shields (built: C1, with holes and force volumes, §6.1).
7. **An original setting:** the planet Tessel, its lattice, and three factions (the Ordinate, the Free Compact, the Resonance). (§2)
8. **Six classes, sidegrade progression, and one resource (Mass).** (§4, §8)
9. **Lattice transit time** stops cross-map redeploys. Factions are capped per continent, and surges lock continents. (§3)
10. **Vehicles are visible by their size**, with longer ranges and faster far updates for aircraft. **Scopes promote targets** to the near tier. (§5, §10)

## Contents

**Part I: the game.** 1. Vision and pillars · 2. Setting and factions · 3. The war · 4. The soldier · 5. Vehicles · 6. Bases and level design · 7. Squads and communication · 8. Progression and economy · 9. Player experience · 10. Fairness and net feel

**Part II: the Unity client.** 11. What lattice-net gives a client today · 12. Architecture · 13. The frame · 14. Drawing the world and the crowd · 15. Combat presentation · 16. Continents as content · 17. Shell, services and sessions · 18. Tooling, tests and the dev setup · 19. Client budgets

**Part III: the plan.** 20. Milestones · 21. Server work the client needs · 22. Open questions · 23. Risks · Appendix A. Numbers · Appendix B. Glossary

---

# Part I: the game

## 1. Vision and pillars

**The pitch.** Thousands of soldiers fight one war on one continent. Three factions fight over a planet whose buried lattice brings the dead back, at whichever node their faction holds. Every base you take moves the front, and every front has a third party.

**The pillars:**
1. **One battlefield.** Up to 10,000 players share a continent: one server process (decided). Nothing is instanced and there are no rounds. The fight you see on the horizon is real, and you can drive to it.
2. **The front matters.** Bases are linked into a lattice, and you can only attack bases linked to ones you hold. Taking ground changes where everyone can spawn, what they can pull and, in the end, who owns the continent.
3. **Fair gunplay at any scale.**
   - The server decides every hit; there's no client-side hit detection.
   - It compensates lag fully up to 100 ms RTT.
   - Shooting stays skill-expressive: ~0.5 s to kill, a cone of fire, bloom, recoil and bullet drop.
4. **Roles, not loadout soup.** Six classes have clear jobs. A squad that combines them beats a crowd that doesn't.
5. **Performance is a feature.** A 1,000-player fight must play smoothly on a mid-range PC. Every system has a budget: server tick, bytes per client, client frame. Degradation under load is designed, not accidental.

**Kept from PlanetSide:** the persistent territory war, three factions, combined arms, classes, bases with spawn logistics, and continent locks.

**PlanetSide 2's pain points, and our answers:**

| problem | our answer |
|---|---|
| Big fights ran badly on server and client | One process per continent with tiered interest (exists). A crowd-rendering budget per tier (§14). Designed degradation (exists, §10). |
| New players die without knowing why | A death recap, damage direction (exists), directed deploy into balanced fights, sidegrade progression (§4.4, §8, §9). |
| Instant reinforcement from anywhere ("redeploy-side") | Lattice transit time: spawning far from where you died takes longer (§3.4). |
| Spawn camping and one-door spawn rooms | Base design rules (§6.3). |
| Population imbalance | Per-faction caps per continent, and underdog bonuses (§3.7). |
| Enemies popping in at render range | Explicit tiers with a render delay per tier, and distant fights drawn from activity data (exists). |
| Doubts about hit registration | Full lag compensation to 100 ms RTT, and a net graph anyone can open (exists). |

## 2. Setting and factions

*Proposed. All names are working names: run a trademark check before anything goes public.*

**Tessel.** People settled this world two centuries ago, and they didn't find it empty. Under its crust lies the **lattice**, a planet-wide crystalline network older than anything alive on Tessel:
- It records the minds of the people linked to it, and regrows their bodies at its **wells**.
- The colonists were linked when they landed.
- So death became a delay, not an end, for as long as a well you're attuned to stands within reach.

Whoever holds the wells decides who comes back, and where. That's the war. It never ends, because nobody stays dead, and it's about territory, because ground means life.

**The factions split over what the lattice is for:**

| | **The Ordinate** | **The Free Compact** | **The Resonance** |
|---|---|---|---|
| belief | The lattice is infrastructure: it must be governed, rationed and defended, under one authority. | The lattice belongs to whoever works it: contracts, not commands. | The lattice is alive, and humanity's future is to join it. |
| who | The colonial administration's successor state | Frontier holds, miners, charter companies | Colonists linked the longest, who listen to the lattice and remake themselves with it |
| look | Heavy plate, hard angles, unit markings; graphite and amber | Rugged, modular, patched gear; khaki and jade | Smooth shells grown from lattice crystal with a faint inner glow; bone and magenta |
| feel | Discipline: stable recoil, big magazines, sturdy vehicles | Punch: hard-hitting, slower-firing guns; fast, light vehicles | Precision: flatter-shooting weapons, lattice tricks |

- **Asymmetry is flavor, not counters (proposed).**
  - Each class gets one faction-unique weapon.
  - Each faction gets one unique main battle tank and one unique fighter.
  - Everything else is shared and reskinned. Balancing three fully different arsenals is the most expensive way to make a game like this, and a small team can't afford it.
- **Colors must stay distinct at 300 m, and to colorblind players.**
  - The default is faction colors: amber, jade and magenta.
  - An **IFF mode** colors by relation instead: your faction always blue-white, the two enemies orange and magenta. Proposed as the default for new players.
  - The Bevy client's placeholders (red, blue, purple) are PlanetSide 2's. Don't carry them over.
- **Tone:** grounded military sci-fi. Soldiers are tired professionals, not superheroes, and the lattice is uncanny.

**The fiction maps onto the mechanics:**

| fiction | mechanic |
|---|---|
| A well regrows you | spawning and respawn timers |
| Re-keying a base's lattice tap | capturing a control point |
| Lattice conduits between bases | lanes: you can attack only what you're linked to |
| A faction's Landing on a continent | its home base, which can't be captured (PlanetSide's warpgate) |
| An Anchor's field relay | the mobile spawn vehicle |
| Mass, printed into gear | the resource for vehicles and consumables |
| A lattice surge | a timed continent-wide event (an alert) that can lock the continent |

## 3. The war: continents, territory, spawns

### 3.1 Continents

- **A continent is one map on one server process** (decided): an 8,192 m square (exists: `WORLD_SIZE`), with up to 10,000 players (decided ceiling).
- **The wire formats bound the map:** 8,192 m across (512 m cells, 4 bits per axis) and heights of 0–655.35 m (exists). A bigger map or higher ground needs a protocol change.
- **Launch with one continent, Kiln (proposed, [docs/kiln.md](kiln.md)).** A desert of canyons, mesas and glass: three rifts meeting in a central crater, five layers from tunnels to spires 600 m up, 67 sites. Its blockout (K0) exists: `lattice-server --world kiln` (world id `world::KILN`). **Harrow** stays the procedural test map: the temperate hills from world seed 1, about 90 m of relief, 16 walled bases and 35 outposts (exists).
- **Plan for four:** Kiln, Harrow, Rime (arctic) and Mire (marsh and jungle), with 2–3 open at a time.
- **Design for 2,000–6,000 players per continent as the usual case, and 10,000 as the peak (proposed).** A continent with 300 players must still be fun: open fewer continents (§3.6), don't spread players thin.

### 3.2 Regions, sites and lanes

- **Each region holds one site**, one of:
  - a **Landing**: a faction's home, which can't be captured;
  - a **major facility**: large, with a unique benefit;
  - a **base**: walled, ~90 m (exists);
  - an **outpost**: open, ~40 m (exists).
- **Lanes link regions.** You can only capture a site in a region linked to one your faction holds.
- **Supply.** A region cut off from its faction's Landing loses its benefits (vehicle terminals, capture bonuses) until it's reconnected.
- **Region shapes** are the sites' Voronoi cells, clipped by authored borders where needed. Holding the site holds the region.
- **Harrow v0's lattice (proposed):**
  - its sites are `world.rs`'s 16 bases and 35 outposts;
  - three of the bases, near the map's edge and roughly 120° apart around its center, become Landings;
  - each site links to its 2–4 nearest neighbors, as a planar graph built deterministically from the seed;
  - most lanes run 600–1,700 m (median about 1 km): one to three minutes of sprinting, so vehicles matter.

### 3.3 Capturing

- **Control points.** A site has 1–3 control points: outposts 1, bases 2–3, major facilities 3–4.
  - A point belongs to the faction with players on it while the others are absent.
  - A contested point freezes.
- **The control meter.** Holding more points than the owner moves the site's control meter toward you. It flips in 3–4 min for a base and 2 min for an outpost, uncontested. When the meter flips, the site and its region flip.
- **A site that just flipped can't be attacked for 3 min (proposed).** That ends ping-pong and gives the defenders a window.
- **Empty sites can be taken ("ghost capping") at the normal speed, for little XP.** PlanetSide 2 tried both extremes; tune it in playtests.

### 3.4 Spawning

**Spawn points, from the safest to the most forward:**

| spawn | where | who can use it |
|---|---|---|
| Landing | the faction's home | everyone, always |
| spawn room ("well") | every base and facility the faction holds | everyone, unless the site's meter is past halfway to the attacker |
| Anchor, deployed | the mobile spawn APC, parked in the field | the faction; one per 200 m |
| Ark, deployed | the air transport, hovering or landed | its pilot's squad and platoon |
| relay beacon | dropped by a squad leader | that squad; one per squad, 30 s cooldown |

- **Lattice transit time (proposed).** Spawning takes 6 s, plus 2 s per lane between the region you died in and the spawn's, up to 20 s. Reinforcing a fight nearby is quick; crossing the continent in one jump isn't. This is our answer to redeploy-side.
- **Spawn rooms are shielded.** Enemies can't walk in or shoot through the shield. That needs faction-filtered collision (§6.1).
- **Spawning is budgeted.** Daybreak's 2023 profiling found that zone entry and spawn cost, not bandwidth, killed PlanetSide 2's big fights. Cap spawns per tick the way the server already caps accepts (`max_accepts_per_tick`), so a mass redeploy waits a second instead of spiking the tick.
- **Respawn today (exists)** is a load-test rule: 5 s after death, the server puts you back into the scenario's fight. The game replaces it with the deploy screen (§9.2) and medic revives (§4.4).

### 3.5 Resources: Mass

- **Mass is the only resource.** It buys vehicles (§5), Exo suits, grenades and deployables.
- **A player holds up to 750.** It accrues at a base rate, plus a share for the regions your faction holds: more territory, more vehicles.
- **Proposed rates (to tune):** +50/min base, plus up to +50/min from territory.
- **Proposed costs:** light tank ~150, Anchor ~200, Exo ~300, fighter ~300, main battle tank ~350.

### 3.6 Surges, locks and rotation

- **A lattice surge** (an alert) starts when a faction holds 45% or more of a continent's regions, or on a schedule.
  - For 60 min the factions fight for territory.
  - The leader at the end **locks** the continent: it closes, and its players move to the next open continent.
  - The winner gets a faction-wide bonus until the next lock.
- **Population decides how many continents are open (proposed):** one until ~3,000 players are online, two until ~7,000, three beyond. A war service runs this (§17).

### 3.7 Population balance

- **Each faction is capped at 36% of a continent's cap.** Over it, you queue or pick another continent.
- **The underdog bonus** adds XP and Mass, scaled by how outnumbered your faction is on that continent, recomputed every minute.
- **Capture speed scales with numbers only up to 2:1,** so a 3:1 zerg is no faster than 2:1.

## 4. The soldier

### 4.1 Movement

**Exists** (`game/src/movement.rs`):

| | value |
|---|---|
| run / sprint / aiming down sights | 6 / 9 / 3 m/s |
| acceleration, on the ground / in the air | 60 / 20 m/s² |
| jump | 4.5 m/s, ~0.68 m high; gravity 15 m/s² |
| step up / step down | 0.45 / 0.6 m |
| steepest walkable slope | 45°: steeper is a wall, in the air too, and on a cliff (a 4 m height cell spanning more than 6 m, `World::cliff`) no move rises at all within a jump of the ground, so cliffs can't be climbed slantwise or jumped up a step at a time (exists since 2026-10-13, for Kiln) |
| collider | a cylinder: radius 0.4 m, height 1.8 m |
| crouch (while `BUTTON_CROUCH` is held; exists since 2026-10-12) | 3 m/s; height 1.2 m, eyes at 1.15 m; no sprint or jump; the cone ×0.75 |

- Movement is deterministic. Prediction matches the server bit for bit, and it's checked (exists).
- Crouched, a player passes under cover whose underside is above 1.2 m, and stays crouched there until there's room to stand (C1, exists since 2026-10-13).
- **Ceilings** (C1, exists): a jump stops at a box's underside; a walk won't step up under one too low (the head's height over the new ground); under the terrain's surface (in a tunnel) the surface is a ceiling too.
- **Force volumes** (K1, exists): a lift carries up at its speed (30 m/s² to it; tall lifts 30 m/s, short ones 20, slowing to 4 m/s over their last stretch so riders bob over the top and step off); a drop shaft slows a fall to 6 m/s; a spring throws whoever stands on it. The stick steers in all of them as in the air.
- **Proposed additions,** each a change to `movement.rs` and predicted like the rest:
  - **fall damage** past 8 m;
  - **jump jets** for the Skirmisher: fuel-limited vertical thrust;
  - **an Exo body:** radius 0.6 m, height 2.4 m, 4 m/s.
- **Not proposed:** prone (it breaks hitboxes and crowds), mantling (later, if levels want it), slides.

### 4.2 Health and damage

- **100 health** (decided). Others see it as 7 bits in the near tier and in 15ths in mid and far (exists).
- **No regenerating shield in v1 (proposed).** Revisit after playtests: a shield layer changes the time to kill, and it's one more value to replicate.
- **Health regenerates only out of combat** (after 8 s untouched, +5/s), or from medics.
- **Hitboxes** (exists, `game/src/hit.rs`):
  - a body capsule, radius 0.4 m around a segment 0.4–1.1 m above the feet, so it covers 0–1.5 m;
  - a head sphere, radius 0.2 m at 1.6 m, so 1.4–1.8 m;
  - crouched: the segment's top at 0.7 m (the body covers 0–1.1 m) and the head at 1.15 m (0.95–1.35 m). The head is centered on the eyes in both stances, so crouching never sees over cover with less head showing than standing does. The server's history keeps each tick's stance, so a shot is tested against the stance the shooter saw.
  - A headshot doubles damage (decided for the rifle: 20 body, 40 head).
- **Explosions** deal radius damage with falloff. Cover blocks them: a target takes damage only with a line of sight from the blast's center.

### 4.3 Classes

*Proposed.*

| class | job | weapons | tools | ability |
|---|---|---|---|---|
| Assault | hold the line, kill armor | assault rifles, LMGs | rocket launcher, frag grenades | **Bulwark:** −40% damage taken for 6 s, slower |
| Medic | keep the squad alive | assault rifles, carbines | heal tool, revive | **Mend field:** heals an area for 8 s |
| Engineer | vehicles, supply, fortification | carbines, SMGs, shotguns | repair tool, ammo pack, AT mines | **Turret:** a deployable anti-infantry gun |
| Recon | information, precision | sniper rifles, DMRs, SMGs | recon dart, hacking | **Cloak** (runs on energy) |
| Skirmisher | flank, harass, take the high ground | carbines, SMGs, shotguns | C4, smoke | **Jump jets** |
| Exo | heavy armor, slow, costs Mass | two arm weapons (anti-infantry or anti-armor) | none | **Lock-down:** immobile, fires faster |

- **The Exo** has 300 armor. Only Engineers can repair it; medics can't heal it. It costs Mass and has a 3 min cooldown.
- **The server enforces cloak, not just the client's shader.** Enemies beyond 15 m of a cloaked Recon aren't sent it at all: you can't wallhack what you never received.
- **Every class can capture points and use terminals.**

### 4.4 Death, revive, recap

- **The body stays where it fell** (exists): the dead don't move, aren't pushed and don't block, and corpses keep their aim.
- **A medic can revive you within 30 s.** You stand up at your body with 50 health. A revive is a life event, like a respawn: never a correction (exists: the life counter).
- **The death recap (proposed message)** shows who killed you, with what, from how far, how much health they had left, and the damage you took in your last 5 s. The server builds it from its own hit records.
- **After death,** wait for a revive or release to the deploy screen.

### 4.5 Weapons

- **Shots are projectiles the server simulates (exists, M3d):**
  - they fly with gravity;
  - they leave from where the shooter's eyes were at the sub-tick moment of the trigger pull;
  - they're tested against targets rewound to what the shooter saw.
- **Accuracy (exists; numbers in Appendix A):** a cone of fire (tight down the sights, wide from the hip, wider moving or in the air), bloom per shot, and recoil on the view.
- **The rifle that exists** (`game/src/weapon.rs`): 600 m/s, 10 rounds/s, 20 body / 40 head, a 2 s flight (~1.2 km), ~0.4 s to kill with body shots.

**Proposed weapon families** (all numbers to tune):

| family | rate (rounds/s) | damage, body/head | velocity (m/s) | notes |
|---|---|---|---|---|
| assault rifle | 10 | 20/40, falling to 14/28 by 60 m | 600 | exists, plus falloff |
| carbine | 12 | 18/36 → 12/24 | 550 | |
| LMG | 10 | 20/40 → 15/30 | 620 | 100-round box, slow reload |
| SMG | 15 | 15/30 → 8/16 past 25 m | 400 | |
| shotgun | 1.5 | 8 pellets × 14 | 350 | the server spreads the pellets from one shot |
| DMR | 4 | 34/68 | 750 | |
| sniper rifle | 0.8 | 80/160 | 900 | a headshot kills; its scope promotes targets (§10) |
| pistol | 6 | 20/40 | 400 | |
| rocket launcher | 0.5 | 120 against armor, 60 splash | 120 | visible and dodgeable |
| grenade | | 90 at the center, 6 m radius | thrown | bounces |

- **The wire limits fire rates.** A shot rides an input, and an input is one 1/30 s step, so a weapon fires at most 30 rounds/s. Rates in between (12/s) work through each shot's sub-step timing.
  - The server's rate check becomes a per-weapon interval, in 1/256 steps.
  - An Exo's two arms share the limit (in the same step, they alternate), and each shot names its weapon.
- **Effective ranges follow the tiers.**
  - Infantry precision reaches 500 m: the mid tier, 10 Hz, interpolated.
  - Targets beyond are in the far tier (2 Hz): visible, but not reliably hittable.
  - Scopes fix that by promoting what's in the scope's cone (§10).
- **Ammo (exists for the rifle, 2026-10-12).** A 30-round magazine and 180 rounds carried, refilled on respawning. A reload takes 2.2 s, or 2.7 s from empty (a round to chamber); the last round starts one by itself, and `BUTTON_RELOAD` starts one early. Nothing fires while it runs.
  - **Predicted like movement:** each input's step counts the reload down, fires its shot if a round is ready, then takes the reload button (`weapon::Ammo::step`). The own snapshot carries the magazine, the reserve and the reload's steps left; a mismatch is replayed like a movement correction (`ammo_corrections`).
  - **The server refuses a shot without a round** (`shots_refused`), and a client never claims one: it checks the round it will have at the shot's step.
  - Others see a reload as a near flag (for the animation).
  - **Unlimited rounds** is a server rule for tests (`--unlimited-rounds`; implied by `--immortal`, where nobody dies to be refilled): the rifle is full again after every step, on the server and in prediction (the Welcome announces it). Load baselines run with it, so held triggers fire at 10 shots/s as in those from before rounds.
  - *Proposed:* reserves refill at terminals and ammo packs; magazine sizes per weapon and loadout.

## 5. Vehicles

*M4. Proposed scope.*

| role | seats | working name | notes |
|---|---|---|---|
| ATV | 1 + 1 | | fast flanking, no armor |
| light tank | 1 | | |
| APC, mobile spawn | 1 + 1 gunner + 10 | **Anchor** | deploys a field relay: a spawn for its faction |
| main battle tank | 1 + 1 | one per faction | the faction's signature vehicle |
| fighter | 1 | one per faction | |
| gunship | 1 + 2 | | |
| air transport | 1 + 2 + 10 | **Ark** | can deploy as an airborne squad spawn |

- **The physics is deterministic, in `lattice-game`** (decided), so a driver predicts bit-exact like a soldier.
- **Seats, not positions.** A seated player has no position of their own on the wire: the vehicle's state and a seat index place them. A full Ark is one entity, not 13.
- **Gunners aim with their input's yaw and pitch,** and their shots ride their inputs, like a soldier's.
- **Visibility by size (proposed, a server change).**
  - At 1080p and 90° FOV, a soldier at 1.5 km is ~1 px tall, a tank ~5 px long, and a gunship ~13 px across.
  - So vehicles get a longer far radius than soldiers: ground vehicles 3 km, aircraft 4 km.
  - Aircraft also get faster far updates (5 Hz or more, with velocity). At 2 Hz a 100 m/s jet moves 50 m between updates, which extrapolation can't hide.
- **Altitude.** Blobs carry 0–655 m today. Aircraft need more, say 12 bits at 50 cm (2 km): a format change for vehicle blobs only.
- **Order: ground first (M4a), air second (M4b).** Aircraft touch everything: movement in 3D, interest at speed, anti-air balance.

## 6. Bases and level design rules

### 6.1 What collision can express

Everything that blocks movement or shots must exist in the shared world (`game/src/world.rs`): the server simulates against it, and the client predicts with it.

**Today (exists):** a heightmap (4 m samples, heights in cm), with holes, and boxes. A box's top can be stood on, and its body blocks only between its bottom and its top. **C1 (exists since 2026-10-13, K1):**
- **boxes turned** about the vertical (`CoverBox::turned`: an axis, half-sizes along and across it);
- **wedges** (`CoverBox::wedge`): a box whose top rises evenly along its axis, walked up like the ground (ramps; authors keep them under 45°);
- **ceilings:** an underside stops a jump, a walk up under one too low, and standing up (§4.1);
- **shields** (`CoverBox::shield`, `Kind::Shield`): block every faction's movement, shots and sight but their own, and are never stood on. Static for now (a Landing's spawn room is its sector's faction's); owned by a site's holder with U5;
- **holes in the heightmap** (a bit per 4 m cell): the terrain is a shell open at its holes. Over a hole there's no ground; under the surface (feet more than 2 m below it) the surface is a ceiling and boxes are the floor, and moving under it is only from under it or from a hole, with room for the body. Shots cross the surface only through holes. Every hole's edges must be lined with walls below the surface (or the void under it shows);
- **force volumes** (`Volume`: lifts, drop shafts, springs; §4.1).

All of it in `movement.rs` and `hit.rs`, deterministic and checked Linux against Windows (`scripts/determinism.sh` on the kit's test level, `lattice_game::kit`, and on Kiln), tested against shots, sight and tracers too. Walls, crates, buildings, stairs, ramps, interiors with ceilings, tunnels, shielded doors and lifts all work. **It doesn't work yet:** doors that open; a spawn room's shields changing hands (U5); timed volumes (K4's clockwork).

**Next steps:**
- **C2 (only if needed):** convex hulls or static triangle meshes, behind a deterministic BVH.

### 6.2 Dimensions from the game's constants

| rule | value | why |
|---|---|---|
| door or corridor width | ≥ 1.4 m; ≥ 2.2 m for two to pass | the collider is 0.8 m wide |
| ceiling height | ≥ 2.4 m; ≥ 2.6 m where players jump | 1.8 m tall, plus a 0.68 m jump |
| stair riser | ≤ 0.4 m | step up is 0.45 m |
| ledge a player can jump onto | ≤ 1.1 m | a 0.68 m jump, plus 0.45 m of ledge catch |
| low cover | 1.0–1.1 m | a crouching player's eyes are at 1.15 m: they shoot over it, the head showing as a standing player's does over 1.5 m; up to 1.35 m hides all but the top of the head, from 1.4 m all of them |
| full cover | ≥ 1.9 m | the head's top is at 1.8 m |
| heights to avoid | 1.4–1.65 m | the eyes (1.6 m) see over while the body (up to 1.5 m) hides: head-only peeking |
| walkable slope | ≤ 45° | `MAX_SLOPE` |

### 6.3 Base design rules

*Proposed.*
- **A spawn room has at least three exits.** One is covered (a tunnel or a bridge) and leads to a different part of the base.
- **No control point is in sight of a spawn room's exit.** Points are 25–60 m apart, and at least one is indoors.
- **Attackers have natural Anchor spots with cover, 150–300 m from the walls.** Defenders' vehicle pads are inside the walls, out of direct fire.
- **Infantry interiors are vehicle-proof:** doorways narrower than any vehicle.
- **Every base has at least one high point,** reachable two ways.
- **Every base gives both sides a reason to go somewhere specific:** a generator powers the shields, a terminal makes vehicles.

### 6.4 Site kinds

| kind | size | points | special |
|---|---|---|---|
| Landing | ~300 m | none | can't be captured; every terminal |
| major facility | 200–300 m | 3–4 | a continent-wide benefit, e.g. air pads or cheaper vehicles |
| base | ~90 m (exists) | 2–3 | spawn room, vehicle terminal |
| outpost | ~40 m (exists) | 1 | spawn room only |

## 7. Squads, platoons, outfits, communication

- **Group sizes (proposed):** squads of 12 (the sim uses 4), platoons of up to 4 squads, and outfits (guilds) of any size, with tags.
- **Squadmates are always in each other's near tier,** at any distance (exists: the interaction clause). A 12-player squad takes up to 11 of a client's near slots.
- **Platoon members show on the map** from a low-rate positions message (proposed: 1 Hz, ~4 B each).
- **Leaders get:** the relay beacon, waypoints and markers (reliable messages), and "follow me".
- **Spotting (proposed):**
  - A key marks the enemy under your crosshair.
  - The server checks the line of sight, then adds the enemy to your faction's spotted list for 6 s.
  - Each client gets that list's ids within its mid radius, 2 B each, a few times a second. A per-faction list keeps the tier data serialized once per entity: one bit per entity would differ for each faction.
- **Text chat and voice run outside the game connection** (decided: QUIC or another service for traffic that isn't tick-paced). Voice has proximity, squad and platoon channels: Vivox or self-hosted (open).
- **The kill feed is local:** kills within 150 m and in your squad. Today Kill events reach both sides and their squads; the proposal also sends them to everyone within 150 m of the victim. Continent news (captures, surges) goes in its own feed.

## 8. Progression and economy

*Proposed.*
- **Rank** comes from XP. **Merits** unlock weapons, attachments, tools and abilities per class.
- **Sidegrades, not upgrades.**
  - Unlocks are options with tradeoffs, and a new player's default gear is competitive.
  - A class's full power ceiling should take ~10 hours to reach, not hundreds.
- **XP for everything that wins fights:** kills, assists, revives, heals, repairs, resupply, spawns on your Anchor, captures, defense. Rewards arrive as reliable events, so the UI shows "+100 Revive" at once.
- **Characters are bound to a faction.** An account can have one character per faction. Switching factions has a cooldown, which deters spying.
- **Monetization (open):** if any, cosmetics only. Never power.

## 9. Player experience

### 9.1 The first session

1. **Log in and pick a faction.** The underpopulated faction is suggested, and earns the bonus.
2. **A short training range at the Landing:** move, shoot, revive, capture a point.
   - The server is one binary, so the range could also run offline: a local server with bots, shipped with the game.
3. **Directed deploy.** The game suggests a squad, and a fight of 24–96 players with balanced numbers, from the server's activity data (the fight finder).
4. **Every death shows the recap.** The first five also show a hint, like "You were shot from the ridge, 140 m north."

### 9.2 Screens

| screen | what's on it |
|---|---|
| deploy | the continent map, your squad, spawn options with their transit times, the loadout, recommended fights |
| map (M) | regions and owners, lanes, where the fighting is, squad and platoon, waypoints |
| loadout | class, weapons, attachments, tools, cosmetics |
| death | the recap, revive status, release |
| social | squad, platoon, outfit, chat, voice |
| settings | video presets (a "Battle" preset that guarantees the frame time), FOV, sensitivity per zoom level, keys, IFF and colorblind modes, audio, the net graph |

### 9.3 HUD

- **Exists in the Bevy client:**
  - the crosshair, sized to the cone;
  - hit markers: white for the body, orange for the head, red for a kill;
  - damage direction;
  - the kill feed;
  - the net graph (N).
- **Exists in the Unity client,** besides those: health, and the rounds (2026-10-12): the cell's count over what's carried, a segment per round that refills as the reload runs, RELOAD under the reticle when a quarter or less is left, NO AMMO when there's nothing to reload with (and as the trigger clicks dry). The rifle's ammo cell glows with the rounds left: the gauge is on the gun too.
- **Proposed:**
  - ability and grenades;
  - a compass with squadmates, waypoints and spotted enemies;
  - a minimap with every friendly the client knows about within 500 m (free, from the entity store);
  - a capture panel inside a site: points, owners, meter;
  - a reward ticker;
  - a connection warning: over 100 ms RTT, "shots may need leading";
  - a server-load icon, shown when the ladder level is above 0.

### 9.4 Audio

- **Near shots.** Every shot by a near-tier shooter is a positioned one-shot sound (from the Shots message), filtered by distance and delayed by the speed of sound.
- **Distant fights.** The activity cells (exists: shots per 128 m cell per faction every 0.5 s) drive a layered battle bed positioned at each cell: a crackle for a small fight, a roar for a big one. Beyond the near tier, there are no sounds per shot.
- **Callouts:** squad leader orders, "base under attack", surge news.

### 9.5 Accessibility

- IFF colors and colorblind palettes.
- FOV from 70° to 110°.
- Raw mouse input, with a sensitivity per zoom level.
- Hold or toggle for aiming and sprinting.
- Subtitles for callouts.

## 10. Fairness and net feel

- **Lag compensation (decided).** It's full for RTT up to 100 ms. Rewind is capped at 300 ms for near targets and 367 ms for mid and far ones; beyond the caps, players lead their shots.
- **Render delays (exists):** near 67 ms (adapting up to 133 ms in crowds), mid and far 200 ms.
- **What players see about it:** the net graph, a warning above 100 ms RTT, and a server-load indicator.
  - A loaded server shows fewer distant players and slower far updates long before it slows the game. That's the ladder, by design.
- **Scopes promote targets (proposed, a server change).** The interest system's interaction clause gains the scope's cone: aim down a scope of 4× or more, and the enemies in its cone become near-tier for you. A sniper at 400 m then sees them at 30 Hz.
- **Anti-cheat.**
  - The server decides hits, the cone's spread (secret picks per shot), fire rates, render-time claims (the render floor and the backtrack bound) and movement (exists).
  - The session log flags statistical outliers (exists).
  - Client-side anti-cheat (EAC or BattlEye) is open; not before a public test.
- **Don't send what can't be seen.** Cloaked Recons beyond 15 m, and anything beyond the tiers, never reach the client.

---

# Part II: the Unity client

## 11. What lattice-net gives a client today

The whole client-side network stack exists in Rust. The bots and the Bevy client use it every day.

**The crates:**
- **`lattice-net`** (the root crate): the UDP transport, sans-IO.
  - Connect tokens, sealed packets, reliable and unreliable channels.
  - The client side is `Client::receive / update / recv / send / flush / drain_outgoing`.
  - A connection times out after 5 s of silence.
- **`lattice-game`**: the rules both sides run bit for bit: movement, the world, the rifle and its projectiles, hit geometry, the message formats.
- **`lattice-client-core`**: the client the bots and humans both run, `ClientCore`:

| call | what it does |
|---|---|
| `on_message(bytes, now)` | takes everything the server sends: Welcome, Snapshot, Near, Entities, Events, Shots, Activity |
| `fire(now, yaw, pitch, ads)` | pulls the trigger; returns the direction for our own tracer |
| `tick_inputs(now, source)` | runs the input clock by elapsed time; returns input batches to send unreliably |
| `render(now, f)` | draws every other entity at the render step as a `RenderState` (pos, yaw, pitch, airborne, health, dead, ads, crouched, reloading, tier, how, at). Call it once per frame: it also adapts the near delay |
| `own_render(now)` | where to draw our own player: between predicted steps, with corrections smoothed away |
| `drain_news`, `drain_ambient` | combat events; others' shots, for tracers; ambient shots for distant fights |
| `cone`, `health`, `is_dead`, `server_own`, `stats` | the reticle, vitals, the server ghost, the net graph |

**The Bevy client** (`client/`):
- `net.rs`'s `Session` is a UDP socket plus `Client` plus `ClientCore`, polled once per frame. It has no Bevy in it, and it's tested against an in-process server.
- Coordinate conversions are in `coords.rs`, tracer bookkeeping in `controls.rs`, and the HUD and gun feel in `hud.rs` and `gun.rs`.

**What the server sends** (exists; formats in `game/src/msg.rs`, `delta.rs`, `events.rs`, `activity.rs`):

| message | channel | when, and what |
|---|---|---|
| Welcome | reliable | once: entity id, spawn, the world's id (`world::KILN`, or a seed for Harrow) and its digest (a client that builds a different world knows: `world_mismatch`) |
| Snapshot | unreliable | every tick: own state (full f32, stance), acked input, queue depth, pace, levels, health, life, rounds (magazine, reserve, reload) |
| Near | unreliable | every tick: up to 64 entities within 150 m (plus squad and contacts), as deltas against acked baselines |
| Entities | unreliable | mid (500 m, 10 Hz) and far (1.5 km, 2 Hz) blobs, 11 B each |
| Events | reliable | Hit, Hurt, Kill |
| Shots | unreliable | near shooters' shots, for tracers |
| Activity | unreliable | distant fights per 128 m cell, every 0.5 s |

**What a client sends:** input batches (unreliable, each input 3× redundant). Each input carries the stick, yaw, pitch, buttons and the render step, plus a shot when the trigger was pulled.

**Constraints the client inherits:**
- Entity ids are u16, and an entity's faction is its id mod 3.
- Game time is 1/30 s steps.
- The map is 8,192 m across, with heights of 0–655 m.
- Positions on the wire are in game space: x east, y north, z up.

## 12. Architecture: a Rust plugin under Unity

*Proposed.* **The Unity client embeds the Rust client stack as a native plugin.** C# draws, plays sound, reads the mouse and keyboard, and shows UI. Everything that touches the wire or predicts movement stays in Rust.

**Why:**
- **Prediction must match the server bit for bit.** The same Rust code compiled into the plugin gets that for free, and the determinism check already covers Windows. A C# port would need its own proof, and would drift.
- **One protocol implementation.** That's why the bots are Rust (decided in M1). A C# port would double every protocol change.
- **`client-core` holds hard-won timing logic:** the input clock, resyncs, adaptive delays, staying inside the render floor. That's ~2,700 lines with tests, and porting it is all risk.

**The rule:** if it affects the wire, prediction or a hit, it's Rust. If it's only seen or heard, it's C#.

```
 Unity (C#)                                 lattice-client-ffi (Rust cdylib)
+----------------------------+   C ABI    +-----------------------------------+
| input -> intent, trigger   | ---------> | Session: a UDP socket             |
| camera, viewmodel, recoil  |            |   lattice_net::Client (crypto)    |
| crowd renderer (by tier)   | <--------- |   ClientCore (prediction, clock,  |
| VFX, audio, HUD, menus     |   flat     |     render timeline, news)        |
| services (HTTPS)           |   arrays   |   World (terrain, cover, sites)   |
+----------------------------+            +----------------+------------------+
                                                           | UDP, sealed
                                                           v
                                              the continent server (lattice-server)
```

### 12.1 The plugin crate

- **A new crate, `client-ffi/`** (`lattice-client-ffi`, library `lattice_client`), in the root workspace, so `cargo test` and `clippy` cover it. It builds as a `cdylib` for Unity and an `rlib`, so the Bevy client can share its `Session`.
- **`Session` moves there** from `client/src/net.rs`, with its test.
- **What the Bevy client decides about *what* to draw moves into `client-core`:**
  - the input encoding: `coords.rs`'s `yaw_u16`, `pitch_i16` and `stick`;
  - the tracer bookkeeping in `controls.rs`: a seen shot's tracer starts when its shooter is drawn firing, and our own tracer is re-aimed along the server's real shot.
  - Unity then only flies and draws tracers.
- **Constants are generated, never retyped:** weapon, movement, hitbox and tier values go to C# through `cargo run -p lattice-client-ffi --bin gen-cs`.
- **An API version,** `lt_version()`, which the C# side checks when it loads the plugin.

### 12.2 The C API (a sketch: names and fields will change)

```c
// One session per connection. Positions are game space (x east, y north, z up), meters.
typedef struct LtSession LtSession;

typedef struct {
    float yaw, pitch;        // radians: yaw counterclockwise from east, pitch up
    float forward, right;    // -1..1, from the keys or a stick
    uint8_t buttons;         // SPRINT, JUMP, ADS, CROUCH, RELOAD (later USE...)
} LtIntent;

typedef struct {
    uint16_t id;
    uint8_t faction, tier, how, flags;   // flags: airborne, dead, ads, crouched, reloading
    uint8_t health;
    float pos[3], yaw, pitch;
    double at;                           // the game step it's drawn at
} LtEntity;

typedef struct {
    float pos[3];            // own_render: where to draw us
    uint8_t health, dead, life;
    float cone_deg;          // the reticle
    float server_pos[3];     // the server ghost
    uint8_t crouched, mag;   // predicted, like the position
    uint16_t reserve;
    float reload;            // seconds left of a reload; 0: none
} LtOwn;

uint32_t   lt_version(void);
LtSession* lt_connect(const char* addr, const uint8_t* token, uint32_t len, const LtConfig* cfg);
int        lt_state(LtSession*);                    // connecting, connected, denied, timed out
double     lt_begin_frame(LtSession*);              // takes "now" once per frame
void       lt_poll(LtSession*);                     // receive and deliver
int        lt_fire(LtSession*, float yaw, float pitch, int ads, float dir_out[3]);
void       lt_inputs(LtSession*, const LtIntent*);  // the input clock, then send
int        lt_render(LtSession*, LtEntity* out, int cap, double* step_out);
void       lt_own(LtSession*, LtOwn* out);
int        lt_drain_events(LtSession*, LtEvent* out, int cap);
int        lt_drain_tracers(LtSession*, LtTracer* out, int cap);  // seen, own (re-aimed), ambient
void       lt_flush(LtSession*);
void       lt_stats(LtSession*, LtStats* out);
const LtWorldView* lt_world(LtSession*);            // heights, boxes, sites, once welcomed
void       lt_close(LtSession*);                    // disconnect and free
// LtConfig, LtEvent, LtTracer, LtStats and LtWorldView follow the same pattern.
```

- **Buffers belong to the caller** (`NativeArray<T>` on the C# side), and the structs are blittable. Nothing allocates per frame on either side.
- **Time is the plugin's monotonic clock,** read once per frame by `lt_begin_frame`. Unity's `Time` never reaches the netcode.
- **For development:** the plugin also mints dev tokens (`--token-key`, like the bots) and reads playtest invites (`client-core/src/invite.rs`), and a test build adds `lt_*_at(..., now)` variants so tests drive time.
- **The session keeps news** (`keep_news(true)`): a game shows it, while a bot only counts it.

### 12.3 Threading

- **U1–U2: pump mode.** The main thread calls the plugin in the frame order of §13. That's exactly what the Bevy client does today, and its tests prove it.
- **U3: a net thread (exists, 2026-10-12; `client-ffi/src/net_thread.rs`, `LT_CONFIG_NET_THREAD`).**
  - The session runs on a plugin thread about a thousand times a second (receive, the input clock with the latest intent, send), behind the session's lock. On Windows it sleeps on a high-resolution waitable timer (the default 15.6 ms timer tick would hold inputs back) at above-normal priority.
  - The main thread sets the intent, pulls the trigger and reads render states. Its calls that read the clocks (`lt_fire`, `lt_own`, `lt_render`, `lt_combat`) take the time as they run, under the lock: the thread may have run the input and render clocks past the frame's start, and a call at an earlier time would be held at theirs (a judder of up to a thread tick, in our own camera most of all). Lock order keeps every claim increasing, so the server's render floor never holds a shot.
  - The trigger takes the call's time, not yet the Input System's event time (proposed).
  - **Measured** (2026-10-12): the C API test freezes the frames for 100 ms, 300 ms and 1 s while running and firing, then for 10 s: 0 stand-ins, late inputs, refused, held or trimmed shots, resyncs and corrections, and the drawn speed within ±1% frame to frame; the same freezes pumped cost 39 stand-ins and 3 resyncs. The Unity player in a 300-bot fight, same freezes: 0 stand-ins and resyncs, 324 shots none refused, held or trimmed, connected through the 10 s freeze; pumped, 195 stand-ins, 7 resyncs, and the connection lost.
- **Why Unity needs it:**
  - The connection times out after 5 s, and Unity's main thread can stall longer than that while it loads a continent. It also stalls for hundreds of milliseconds on shader compiles and garbage collection.
  - In pump mode, a stalled main thread means stand-ins, resyncs and trimmed shots: the input clock catches up 300 ms at most.
- **The API stays the same in both modes.**

### 12.4 C# layers

| layer | job |
|---|---|
| `Lattice.Native` | P/Invoke declarations, structs, generated constants, a dynamic loader (in the editor, reloads a rebuilt DLL without restarting Unity) |
| `Lattice.Net` | `NetDriver`: owns the session and runs the frame order (PlayerLoop) |
| `Lattice.World` | terrain and props from the world (v0) or the continent scene (v1) |
| `Lattice.Players` | the crowd: entity views by band, animation, faction looks |
| `Lattice.LocalPlayer` | input mapping, camera, viewmodel, recoil, aiming down sights |
| `Lattice.Combat` | tracers, muzzle flashes, impacts, hit markers, damage direction, death |
| `Lattice.Audio` | near shots, battle beds, callouts |
| `Lattice.UI` | the HUD and screens |
| `Lattice.Services` | login, the war map, characters (HTTPS), invites |
| `Lattice.Diagnostics` | the net graph, ghosts, tier colors, self-check flags, the report file |

### 12.5 Coordinates: game and Unity

- **Game space is right-handed with z up; Unity is left-handed with y up.** Swapping y and z converts one to the other.
- **The map is centered on Unity's origin** for float precision: 0.5 mm or better at the edges.

| | game | Unity |
|---|---|---|
| position | (x, y, z) | (x − 4096, z, y − 4096) |
| yaw θ (counterclockwise from east) | θ | `Quaternion.Euler(0, 90° − θ, 0)` |
| view (yaw θ, pitch φ up) | (θ, φ) | `Quaternion.Euler(−φ, 90° − θ, 0)` |
| east, north, up | +x, +y, +z | +x, +z, +y |

- **The view's canonical form is the game's (θ, φ).** Unity rotations are derived from it, never the other way round.
- **Mouse look:** moving right decreases θ. Unity's mouse delta y is positive upward (Bevy's is positive downward), so moving the mouse up increases φ.
- **The movement stick** comes from θ, forward f and right r: `(f cos θ + r sin θ, f sin θ − r cos θ)`, scaled to ±127. Rust computes it (`stick`), not C#.
- **`Coords.cs` has EditMode tests** that mirror `client/src/coords.rs`'s.

## 13. The frame

```
EarlyUpdate (inserted)         lt_begin_frame, lt_poll
Update (NetDriver, first)      read the actions into the intent
                               trigger held: lt_fire, then the recoil kicks the view
                               lt_inputs(intent)
LateUpdate                     lt_own: camera and viewmodel
                               lt_render: the crowd (jobs)
                               lt_drain_events, lt_drain_tracers: VFX, audio, HUD
PostLateUpdate (inserted,      lt_flush
  before rendering)
```

- **Fire before inputs,** as the Bevy client does: the shot rides the input the clock makes this frame. The shot carries the view before its own recoil kick.
- **A held trigger fires whenever the weapon is ready.** The plugin rate-limits it the way the server does.
- **`lt_render` runs once per frame, and its output drives everything drawn.** Nothing reads entity positions anywhere else.
- **With the net thread (U3),** `lt_poll`, `lt_inputs` and `lt_flush` move to the thread. The frame keeps `lt_begin_frame`, the intent, `lt_fire`, `lt_render` and the drains.

## 14. Drawing the world and the crowd

### 14.1 Engine setup

*Proposed.*
- **Unity 6 LTS** (pin the exact version), with:
  - URP (Forward+), the GPU Resident Drawer and GPU occlusion culling;
  - Burst, Jobs and Collections;
  - the Input System;
  - VFX Graph, Animation Rigging and glTFast (for Meshy's GLB files);
  - Addressables for streaming continents;
  - UI Toolkit.
- **URP, not HDRP:** it scales to the mid-range PCs this game needs. A 1,000-player battle spends the GPU on crowds and effects, not on lighting.
- **Not used:** Netcode for GameObjects or Entities, and Unity Physics or PhysX for anything that matters to gameplay. PhysX is only for cosmetic ragdolls and debris.

### 14.2 Terrain and sky

- **v0 (proposed):** Unity Terrain tiles, 4 × 4 tiles of 2,048 m with 513 samples each (4 m spacing), filled at load from the plugin's heights. Texture by height and slope, as the Bevy client colors its vertices.
- **v1:** authored Unity Terrain, exported to the game (§16).
- **The drawn ground must match `World::terrain`.** Unity draws two triangles per 4 m cell; the game interpolates bilinearly. Measure the gap on Harrow. If feet visibly float or sink (more than 5 cm), switch `World::terrain` to the same triangle split: a deterministic server change, after which what you see is exactly what you stand on and what stops bullets.
- **The horizon:** terrain reaches the map's edge (8 km), with aerial perspective. Players exist only within 1.5 km, where they're ~1 px tall, so the cutoff can't be seen.
- **Day and night (proposed)** follow the server's step clock, so everyone sees the same sky. Nights stay readable: darkness that hides players makes the fight about gamma settings.

### 14.3 Props and buildings

- **v0:** one prefab per cover kind, fitted exactly to its box (as the Bevy client does: each model is normalized to a unit box). The GPU Resident Drawer instances them; Harrow v0 has about 17,000.
- **The rule, from the Bevy client: what you see is what blocks you.** Within a player's reach, no art geometry may look solid and not be, or be solid and not look it.

### 14.4 The crowd

**How tall a soldier looks** at 1080p and 90° FOV: ~35 px at 50 m, ~12 px at 150 m, ~3.5 px at 500 m, ~1 px at 1.5 km. So the crowd is drawn in bands:

| band | how it's drawn | count |
|---|---|---|
| rich (to ~60 m, nearest first) | full skinned mesh, Animator, aim IK, rifle in the hands, shadows | ≤ 96 |
| crowd (beyond) | GPU-instanced meshes with baked animation (vertex animation textures), LODs by screen size | ≤ ~1,250 |
| far (beyond ~600 m) | an impostor or a lit capsule, in the faction's color, with muzzle flashes | |

- **Bands go by screen size, not distance,** so a 4–12× scope brings the rich band out with the zoom.
- **Animation comes from `RenderState`:**
  - velocity from the drawn positions, smoothed;
  - airborne, aiming down sights, crouched, reloading and dead from its flags;
  - firing from the tracer feed.
- **Death:** a ragdoll or a clip, then hold still: the server's corpse doesn't move.
- **Transforms for the rich band update in a Burst job** over a `TransformAccessArray`. The crowd band is only an instance buffer.
- **Entity views are pooled.** Nothing is instantiated during a fight.
- **Faction looks:** one material with a team mask, colored by faction or by IFF mode.
- **Tier colors (T) and server ghosts (G) carry over from the Bevy client,** for debugging.

## 15. Combat presentation

- **Own shots:**
  - a tracer leaves at once along `lt_fire`'s direction: in the real cone, but along our own pick;
  - it's re-aimed when the server's shot comes back (exists in the Bevy client; moves into `client-core`, §12.1);
  - a muzzle flash, the viewmodel's kick, and recoil on the view (constants from `weapon.rs`);
  - the reticle is sized to `cone_deg`.
- **Others' shots:** each one starts when its shooter is drawn firing, from the drawn muzzle, along the server's aim.
- **Tracers fly the server's arc** (600 m/s, 9.81 m/s²), in VFX Graph, thousands at once.
- **Distant fights:** the plugin's ambient shots (up to 600/s), drawn the same way, plus flashes and the battle bed (§9.4).
- **Impacts:** decals and sparks where tracers hit the terrain or cover, from a cosmetic raycast against the plugin's world.
- **Hit feedback:** markers and sounds from Hit events, damage direction from Hurt events (`dir`), and the kill feed from Kill events.
- **Death:** the camera moves to a death view, and shows the recap once that message exists.

## 16. Continents as content: the authoring pipeline

*Proposed.* A real continent is authored, not generated: sculpted and painted terrain, bases, roads, foliage, lighting. But the server can't read a Unity scene, and the client must collide with exactly what the server collides with. So:

```
the continent's scene, in the Unity editor
    |  Lattice Exporter
    v
continent package (.lcp) ----> the server:        World::load(package)
    |                          the client plugin: World::load(package)
    +-- content hash --------> sent in Welcome; the client checks its own copy
```

- **The package holds what the game needs, nothing visual:**
  - heights: u16 cm on the 4 m grid, maybe 2 m later;
  - collision primitives: boxes, turned boxes, wedges and shields (C1), holes, force volumes;
  - gameplay markup: sites, control points, spawn rooms and spawn spots, terminals, vehicle pads, shield volumes, Landings, lanes, region borders, no-deploy zones.
- **Designers mark gameplay objects in Unity** with components (`LatticeCollider`, `ControlPoint`, `SpawnRoom`, and so on).
- **The exporter refuses to export a broken map.** It checks:
  - that ramps are walkable (wedges under 45°) and every hole's edge is walled;
  - §6.2's dimensions;
  - reachability: every control point can be reached from every spawn room.
- **The procedural world becomes a package too.** `world.rs` writes Harrow v0 as one, so the server and the client load every continent the same way.
- **Welcome sends a continent id and a content hash.** Exists for generated worlds (2026-10-13): the world's id and `World::digest`; a client whose world differs flags it (`world_mismatch`, `LtStats`). For packages the id becomes the package's. On a mismatch, the client asks the player to update (proposed).
- **Art and collision come from the same scene,** so they can't drift apart.

## 17. Shell, services and sessions

**A session, start to end:**
1. Log in (HTTPS).
2. Pick a character.
3. The war map shows the open continents, their populations and their fights. Pick a continent.
4. The login service mints a connect token for that continent's server. It expires in tens of seconds.
5. Connect (on the net thread) while the continent loads.
6. Welcome.
7. The deploy screen.
8. Deploy, and play.
9. Switching continents is a disconnect and a new token.

**Details:**
- **The token's 32 B of user data carries the character id and faction.** The login service seals it, so the client can't change who it is. The server already receives the token's user data (exists: `ServerEvent::Connected`).
- **Deploying (a proposed protocol change):**
  - On the deploy screen, a connected player is a dead entity with no body. The input clock keeps running, as it does for a dead player today.
  - `Deploy { spawn, loadout }` (reliable) puts the player into the world, as a life event.
- **Playtests keep invites** (`--invite FILE`, exists). The Unity client reads the same file, and spends one token per launch.
- **Disconnects:** a replaced connection (the same user connected elsewhere) says so. A timeout offers to reconnect with a new token.
- **Services needed (none exist yet):** login and token minting, characters and loadouts, the war service (continents, surges, queues), chat, voice.

## 18. Tooling, tests and the dev setup

- **Where the Unity project lives (proposed):** on the Windows side (e.g. `C:\code\lattice-unity`), in its own git repository.
  - The Unity editor works poorly on a project inside WSL's filesystem (slow imports, unreliable file watching), and this repo must stay in WSL.
  - This repo builds the plugin and its generated C#, and copies them in (`scripts/unity-plugin.sh`, like `client-windows.sh`). The API version check catches a mismatch.
  - Large source art stays out of git until there's a remote with LFS, like `meshy_output/` today.
- **Building the plugin:** cross-compiled from WSL (`x86_64-pc-windows-gnullvm`, as for the Bevy client) or natively with MSVC. Run `scripts/determinism.sh` with the plugin's target and profile.
- **Unity builds and tests run from WSL,** through WSL interop: the editor's `-batchmode -executeMethod` for builds, and `-runTests` for EditMode and PlayMode tests.
- **Tests:**
  - **Rust:** the C API against an in-process `SimServer` (the Bevy `Session` test, moved), and every exported struct's layout.
  - **C#:** EditMode tests for coordinates and the generated constants, and a PlayMode test that joins a local server with bots and checks for 0 corrections.
- **Self-checks carry over from the Bevy client:** `--autoplay`, `--autofire`, `--screenshot PATH --after S`, `--exit-after S`, and the summary appended to `lattice-report.txt` on exit. Claude Code can then run and judge the Windows build from WSL.
- **Keep the Bevy client** as the reference client until Unity passes U2's bars. Freeze its features now.

## 19. Client budgets

*Proposed targets.*
- **Frame rate:**
  - on a mid-range PC (RTX 3060 / RX 6600 class), 1080p at 144 fps or more (6.9 ms) in a 300-player fight;
  - on a low-end PC (GTX 1060 class), 60 fps or more at low settings;
  - in a 1,000-player blob, 60 fps or more at medium.
- **The main thread, per frame:**
  - the plugin's calls take 0.5 ms or less: poll, inputs, rendering ~1,300 entities, the drains;
  - entity views update in 1 ms or less (jobs);
  - nothing allocates on the managed heap during a fight.
- **Bandwidth:** 0.7–1.5 Mbps down (exists; the server's budget is 1.5 Mbps), ~25–30 kbps up (inputs, 3× redundant).
- **Reference:** on the dev desktop, the Bevy client draws a 1,000-bot blob at p50 3.5 / p99 4.7 ms with capsules, and 6.5 / 8.0 ms with soldiers. Unity should match that at U1–U2.

---

# Part III: the plan

## 20. Milestones

| milestone | contents | pass bars |
|---|---|---|
| **U0: plugin** | `client-ffi` (`Session`, the C API, generated C#); a Unity scene that connects with a dev token or an invite and shows the net graph | Rust: the C API joins an in-process server and predicts with 0 corrections. Unity: connects to a local server; 0 corrections over 60 s; no managed allocations per frame |
| **U1: walk** (the Bevy client's M3c; built, 2026-10-12) | terrain, props, soldiers by band, cameras (first person, chase, spectator), net graph, ghosts, tier colors | M3c's bars: 0 corrections walking alone; smoothness per tier matches the Bevy client on the same link (near ≥ 99%; mid within a point of Bevy's); render time never goes backwards; frame p99 under the refresh interval with a 1k blob in view on the Windows desktop; smooth by eye. **Frame bar met** (2026-10-12): p99 4.6 ms with 1,000 bots fighting in view, against a 180 Hz display's 5.56 ms (IL2CPP player; the static props through one BatchRendererGroup) |
| **U2: fight** (the Bevy client's M3d) | firing, aiming down sights, cone, recoil, tracers (own re-aimed), hit markers, damage direction, death and respawn, kill feed, distant fights, battle audio | an honest Unity client is never trimmed or held (`rewinds_trimmed` and `renders_held` 0 in the session log); a 300-bot fight within budget; you play it and it feels right |
| **U3: net thread** (built, 2026-10-12) | the session on a plugin thread | with the main thread frozen for 100, 300 and 1,000 ms: no resyncs or trims (like the Bevy client's freeze tests); a 10 s scene load keeps the connection. **Met** (§12.3) |
| **U4: shell** | login or invite, deploy screen, map, loadout (stubbed), settings, death recap | |
| **U5: territory** | sites, points, the capture panel, lanes on the map, spawn rooms | needs §21's server work |
| **U6: classes** (crouch and the rifle's rounds built, 2026-10-12) | the six classes, their tools and abilities. Built: crouch (Ctrl held, C toggled; the view eases down; soldiers kneel or crouch-walk, the crowd from baked crouched poses) and the rifle's rounds (a first-person reload that swaps its ammo cell, timed by the predicted reload; the HUD's rounds; handling sounds, near soldiers' reloads heard) | crouched heads drawn on their hitbox; prediction exact. **Measured:** a crouched fight (`--autotarget --crouch`, 1,000 bots): 0 corrections, 0 ammo corrections, 6 reloads; frame p99 4.7 ms, no managed allocations |
| **U7: vehicles** (M4) | ground, then air | |
| **U8: the first authored continent** | the exporter and package; *proposed* ([docs/kiln.md](kiln.md)): prove them on Kiln's blockout rather than rebuilding Harrow by hand (it stays procedural) | |
| **Kiln** ([docs/kiln.md](kiln.md): K0 blockout, K1 collision kit, K2 great sites, K3 bases and tunnels, K4 the living world, K5 art) | **K0, K1 and K2 built** (2026-10-13): the whole continent in rough shapes, generated (`game/src/kiln/`), every site as boxes, 117 lanes; C1, holes and force volumes, used by Kiln (square decks, lifts and drop shafts, a tunnel a sector, shielded spawn rooms); the great sites in graybox (the Heart chamber and its tunnels and shafts, the Spans' galleries, perches and wall tunnels, the Cliffholds' lift towers, keep and deep gallery, the Geodes' wells and springs) | **K0 met:** every route walked end to end by the game's movement and by bots over the server, 0 corrections; Landing to the Hollow ~353 s in every sector (within 0.2%), every kind of site within 7.2% of the same walk from its Landing. **K1 met:** `scripts/determinism.sh` agrees Linux/Windows on the kit's level and on Kiln, both with tunnels, lifts and vents. **K2:** 4,000 fighting in the Hollow tick at p50 22.3 / p99 24.6 ms on the 8-core dev box with the bots on it (Kiln costs nothing over Harrow; assembly is the work); the run on server hardware is still to do |
| after that | squads, platoons and outfits; progression; surges and rotation; more continents | |

**Recommended order change: build U0–U2 before M4.** Vehicles need a client, and building them in Bevy first would be thrown away.

## 21. Server work the client needs

| for | server work | protocol |
|---|---|---|
| U0–U3 | none | none |
| U4 | the deploy flow (no body until deployed), death recap, reward events, a continent-wide fight list every 5 s | Deploy (C→S reliable); Recap, Reward, Fights (S→C) |
| U5 | territory: sites, capture, lanes, spawn rooms, Landings; shields owned by their site's holder (C1's shields exist, static) | Territory (reliable: the full state at join, then changes); site state for nearby sites at 1–2 Hz |
| U6 | classes and loadouts; crouch (built); ammo and reloads (built for the rifle); jump jets; Exo bodies; cloak in interest; healing and revives; deployables (proximity checks on the shared grid); spotting; appearance | more input buttons and a weapon slot (+1 B per input); wider near and far flags; appearance records for entities entering a client's near or mid set; deployables (reliable, static); the spotted list |
| U7 | vehicles (M4): physics, seats, interest by size, faster far updates for aircraft | vehicle states, seat changes |
| U8 | continent packages | Welcome v2: continent id and content hash |
| the shell | login, characters, the war service, chat, voice | HTTPS or QUIC, outside the game connection |

**Every message stays under one packet** until the transport can fragment (its step 3).

## 22. Open questions

1. **Names** (the game, the planet, the factions, the continents). *Recommended:* keep the working names internally, and run a trademark check before anything public.
2. **Shields.** *Recommended:* none in v1 (100 health plus medics); revisit after playtests.
3. **Squad size.** *Recommended:* 12, after checking its near-tier cost in the blob.
4. **Where the Unity project lives.** *Recommended:* its own repository on the Windows side, rather than inside this repo.
5. **Audio middleware.** *Recommended:* FMOD (hundreds of voices, priorities and distance layers), over Unity's audio or Wwise.
6. **Voice.** Vivox or self-hosted.
7. **The business model:** free-to-play with cosmetics, or buy-to-play. It changes the shell and the services, not the client's core.
8. **AI soldiers for low population.** The bots already aim, lead and fire. Clearly marked AI could fill an outnumbered faction's front on an early-access continent. *Recommended:* decide after the first public playtest.
9. **Anti-cheat:** EAC or BattlEye before a public test, or later.
10. **Platforms.** *Recommended:* Windows first, Linux through Proton, and macOS only after the determinism check passes on ARM.

## 23. Risks

| risk | mitigation |
|---|---|
| Unity's main-thread stalls starve the inputs | the net thread (U3), tested with frozen frames |
| 1,300 animated soldiers in Unity | bands by screen size and baked-animation instancing; measure at U1 |
| Authored maps outgrow the collision vocabulary | C1 before U8; the exporter refuses what the game can't express |
| The plugin and the Unity project drift apart in their two repositories | generated bindings, the API version check, one script that copies both |
| Content at this scale (four continents of 64 km²) | modular kits, Meshy for props, procedural placement inside authored layouts |
| The fantasy needs thousands of players | one continent until it's full, the fight finder, optional AI (open question 8) |
| Scope, for a tiny team | the milestone order above: nothing in Part I ships before its milestone |

---

## Appendix A. Numbers

| what | value | where |
|---|---|---|
| tick and game step | 30 Hz and 1/30 s (20 Hz under load, dilation down to 0.8) | `movement.rs`, `sim/src/ladder.rs` |
| map | an 8,192 m square; height samples every 4 m; heights 0–655.35 m, in cm | `movement.rs`, `world.rs`, `msg.rs` |
| players per continent | 10,000 | CLAUDE.md |
| near tier | 150 m (plus squad, plus contacts for 5 s); up to 64 of 100 candidates per tick; 30 Hz | `sim/src/interest.rs` |
| mid tier | 500 m; ~256 at 10 Hz | |
| far tier | 1,500 m; ~1,000 at 2 Hz | |
| bandwidth budget | 1.5 Mbps per client | |
| render delay | near 67–133 ms; mid and far 200 ms | `client-core` |
| rewind cap | 300 ms near; 367 ms mid and far | CLAUDE.md |
| connection timeout | 5 s | `src/connection.rs` |
| rifle | 600 m/s, gravity 9.81 m/s², 10 rounds/s, 20 body / 40 head, 2 s flight | `weapon.rs` |
| cone of fire | down the sights 0.1° (0.4° moving); from the hip 2° (2.75° moving); ×2.5 in the air | |
| bloom | down the sights +0.04° per shot, up to +0.5°; from the hip +0.1°, up to +1.5°; recovers after 133 ms, at 12°/s | |
| recoil | 0.32° up per shot (the first ×1.75); 0.16° left to 0.22° right; drifts back down after 0.12 s, at 9°/s | |
| aiming down sights | 0.2 s to raise; 1.35× zoom; half speed | `client/src/gun.rs`, `weapon.rs` |
| hitboxes | body capsule, radius 0.4, 0.4–1.1 m; head sphere, radius 0.2, at 1.6 m; crouched 0.4–0.7 m and 1.15 m | `hit.rs` |
| eye height | 1.6 m; crouched 1.15 m | `weapon.rs` |
| crouch | 3 m/s, 1.2 m tall, the cone ×0.75, no sprint or jump | `movement.rs`, `weapon.rs` |
| rifle rounds | 30 in the magazine, 180 carried; reload 2.2 s, 2.7 s from empty | `weapon.rs` |
| health and respawn | 100; 5 s | `faction.rs` |
| ambient shots | up to 600/s | `client-core/src/distant.rs` |

## Appendix B. Glossary

- **Step:** 1/30 s of game time, the timeline everything is drawn on.
- **Tier:** near, mid or far: how often and how precisely the server tells a client about an entity.
- **Render delay:** how far behind the newest server state other entities are drawn.
- **Rewind, lag compensation:** the server tests a shot against targets where the shooter saw them.
- **Stand-in:** the input the server applies when yours didn't arrive in time.
- **Resync:** the client giving up on inputs the server has already stood in for.
- **Life event:** a death, respawn or revive. Prediction rebases without counting a correction.
- **Ladder:** the server's levels of degradation under load.
- **Site, region, lane, Landing:** a base, its territory, a link between two regions, a faction's home.
- **Band:** how the client draws an entity (rich, crowd or far), chosen by its size on screen.
