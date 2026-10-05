# lattice-client: the first playable client

A Bevy window on `lattice-client-core`, the same client code the bots run: prediction, the render timeline, entity interpolation. It plays on a `lattice-server`: walk, jump and shoot among the bots (3 factions; you respawn 5 s after dying).

It's its own Cargo workspace, so the root's `cargo test` and `clippy` never build Bevy. Run Cargo inside `client/`.

## Play on Windows against a server in WSL

```sh
# WSL: a server with a crowd to walk into
cargo build --release -p lattice-sim
./target/release/lattice-server --spawn blob &
./target/release/lattice-bots --count 1000 --duration 600 &

# WSL: build the Windows client into C:\lattice (prints the server's address)
scripts/client-windows.sh
```

Then on Windows: `C:\lattice\lattice-client.exe --server <WSL IP>:40000`.
- **The build:** `scripts/client-windows.sh` cross-compiles from WSL for `x86_64-pc-windows-gnullvm`. It uses llvm-mingw unpacked in `~/.local/opt` (no sudo, no Windows toolchain); the script's header says where to get it.
- **The address:** the WSL IP is `hostname -I` in WSL. The server binds `0.0.0.0` by default.
- **The bundle:** the exe and `libunwind.dll`, 67 MB.

A native build on Windows works too, with Rust + MSVC: `cargo build --release` in `client\`. ring builds C, so it needs the MSVC C++ build tools.

In WSL itself, `cargo run` in `client/` opens a window under WSLg, but it renders on the CPU (llvmpipe, 5–10 fps). That's good for checking what's drawn, not for playing. Run the built binary directly with `BEVY_ASSET_ROOT=$PWD` (in `client/`), or Bevy looks for `assets/` beside the executable.

## Models

Everything visible beyond the terrain is a model in `assets/models/` (~25 MB), made with Meshy and imported by `tools/import-assets`. The soldier and the rifle are the user's own Meshy models (image-to-3d from concept art); the rest came from text prompts.

| model | drawn as | triangles |
|---|---|---|
| `soldier` | every drawn player (the nearest 1,200), tinted by faction or tier, rigged; `soldier_lod1`–`lod3` beyond 15 / 50 / 130 m, on the same skeleton | 60k / 15k / 4k / 1.3k |
| `anim_*` | its clips: idle, aim-walk ("Run and Shoot"), run ("Rifle Charge"), sprint, jump, death | — |
| `rifle`, `rifle_view` | in each soldier's hands (within 150 m); the first-person one | 10k; 40k |
| `wall` | concrete blocks tiled along every wall (the scattered walls and base perimeters) | 0.9k |
| `crate`, `container`, `sandbags` | low cover and base cover | 0.9–1.8k |
| `post`, `command`, `bunker` | base buildings: guard posts at the gates, the command building, bunkers | 2.8–5.0k |
| `rocks` | small clusters scattered for scale, too low to look like cover; drawn within 300 m | 1.1k |

- **What you see is what blocks you.** Each prop is normalized to fill a unit box (x along its long side, y up, front at +z), so one transform puts it exactly in its collision box from `World::boxes()`. Antennas and the radar dish stick out above. Beyond 700 m (1.8 km for buildings) the plain boxes take over.
- **Soldiers:** a soldier's white armor takes the player's color, dead or alive. Its clip follows its drawn speed (idle < 0.4 m/s, aim-walk < 4.5, run < 7.5, sprint), played backward when it backs up. Death plays once and holds.
- **Distance:** lighter meshes past 15, 50 and 130 m; no shadows past 90 m; past 250 m the pose freezes (its animation graph is removed, so nothing is evaluated). With all ~280 players of a 300-bot fight drawn as soldiers, frames take p50 6.6 / p99 7.5 ms in first person on the Windows desktop (chase view 8.6 / 9.4).
- **Rifles:** the clips hold rifles of different sizes at different angles, so a rifle follows the right hand's position but points where the player aims (`hold_rifles`, after the skeleton is posed). The dead drop theirs.
- **`--viewer`:** no game. It shows a row of soldiers (bind pose, then each clip) beside their collision capsules, for looking at the models.
- **First person:** the rifle is drawn by a second camera on render layer 1 after the world, with depth cleared, so it never sinks into a wall.

**Re-importing:** `tools/import-assets` (its own Cargo workspace; it uses meshoptimizer, which the client doesn't need). `cargo run --release` there turns Meshy's output (`assets/meshy_output/*/<name>.glb`, kept out of git) into `assets/models/`:
- it shrinks textures to 1024 px and makes materials single-sided;
- it normalizes the props;
- it cuts the clips down to skeleton and keyframes;
- it makes "Run and Shoot" and "Rifle Charge" run in place (they move the hips forward 1.1 and 2.5 m a cycle);
- it takes the jump's own rise out;
- it simplifies the rifle (~800k triangles) to 10k (held) and 40k (first person) with meshoptimizer, which keeps each vertex's UVs so the textures still fit;
- it builds the soldier's lighter meshes the same way, on its index buffer only: same vertices and skin, a quarter, a fifteenth and a fiftieth of the triangles.

**Models too detailed to rig:** Meshy rigs up to 300k faces. `cargo run --release -- decimate IN.glb OUT.glb TRIANGLES` simplifies a model first; the user's soldier (881k triangles, 8k textures) went to 60k (2.1% error) and was rigged from that file.

**Meshy tasks** (2026-10-05/06, 255 credits of ours; the user's soldier and rifle generations are theirs). Each prop is a smart-topology text-to-3d preview, refined with 2k PBR textures. The soldier is a `latest` preview remeshed to 15k triangles, refined, rigged at 1.8 m, plus 5 animations. Follow-ups (retexture, remesh, LODs, more clips) start from these, without regenerating.

**The first two soldiers were replaced.** The first (50 credits to replace) It had a carbine slung across its chest, and Meshy's auto-rig of it was broken: the collarbones owned 31% of the vertices (the chest and back), the spine bones 2%, and the spine ran along the front of the belly. The torso was squeezed whenever the arms moved. The second (no weapon) rigged cleanly, with joints centered in the limbs and the collarbones at 9%. The user's T-pose soldier replaced it (rig 5 + clips 15 credits), rigged as cleanly: collarbones 10%, torso 13%, left and right matching. Check a new rig the same way before buying clips.

| model | preview | refine |
|---|---|---|
| soldier (the user's) | image-to-3d `c44b834b-2c00-40f8-a9e7-ade09de1b73d` | decimated locally, rig `01a10e0e-1eea-7052-8dec-93a6361fcf1e` |
| rifle (the user's) | multi-image-to-3d `8a8338b0-c99b-44f9-ba63-b1f7229c4d0b` | — |
| wall | `01a10dc6-730c-7437-b186-a0104fca3891` | `01a10dc8-d675-756e-bd59-2662e86b623d` |
| crate | `01a10dc5-c29f-729c-8154-d0a8d6a81872` | `01a10dc8-b18b-7193-b81c-b42d64defe2b` |
| container | `01a10dc6-86c6-72de-8a0f-cbc1ac991966` | `01a10dc8-e227-77fc-b6f0-cad6287916f4` |
| sandbags | `01a10dc6-958d-70fc-9824-f3c81c0f7aa5` | `01a10dc8-f07b-7155-b5fc-dba9b5609237` |
| post (asked for a tower) | `01a10dc6-b424-76e2-9ada-3e6371772951` | `01a10dc8-ff3b-740a-a787-c18ac7d8f344` |
| command | `01a10dc6-c1ba-73b4-8b69-18b51ea3b255` | `01a10dc9-0a8b-7107-b63c-2bb2827d9ae4` |
| bunker | `01a10dc6-cef7-7118-9a3a-3546b4bc827e` | `01a10dc9-17e8-704b-ba3d-22fe9e27c4dd` |
| rocks | `01a10dc6-ddb9-75b9-8503-449a7c5867fe` | `01a10dc9-2539-7160-bae0-b08de3b0d553` |

Animations (on the rig): idle `01a10e0e-cc96-708b-8090-79afbad6b7b0`, Run and Shoot `01a10e0e-d22e-7127-a427-d17a1867058a`, Rifle Charge `01a10e0e-d7c1-758d-88dd-04993e92a9d0`, Regular Jump `01a10e0e-dd47-7182-b634-0600eb9ad798`, Shot and Fall Backward `01a10e0e-e299-7122-81d3-dfe20e1d100d`. Walking and running came with the rig.

## Controls

| key | |
|---|---|
| click / Esc | grab / release the mouse |
| left mouse (held) | fire the rifle: 10 shots/s, with a tracer (the grabbing click doesn't fire) |

**Combat feedback:**
- **The hit marker** is an X on the crosshair: white for a body hit, orange for a head hit, larger and red for a kill.
- **Red marks around the crosshair** point where hits came from, relative to your view.
- **The kill feed** is top right, as "Faction #id > Faction #id (head)".
- **Other players' tracers** are orange. Each starts when its shooter is drawn firing.
| WASD, Shift, Space | move, sprint, jump |
| V | view: first person → chase → spectator (free flight: WASD, Space/C, Shift) |
| G | server ghosts: blue = each player's newest server sample (no delay, no smoothing); pink = our own last server state |
| T | tier colors on/off |
| N | net graph |

**Tier colors:**
- green: near (30 Hz);
- amber: mid (10 Hz);
- red: far (2 Hz);
- grey: held (updates stopped);
- white: new (before its first sample's time).

**The net graph** shows:
- fps and frame time; RTT, loss and bandwidth;
- the server's step, level and pace;
- input → applied (RTT/2 + the server's reported wait);
- the actual render delay (near, and the mid/far lag), and clock snaps;
- players drawn per tier and the share interpolated over the last second;
- corrections and push corrections;
- sparklines of frame time and snapshot arrival gaps.

## Flags

`--help` lists them all:

| flag | what it does |
|---|---|
| `--server` | the server's address |
| `--user` | the user id |
| `--token-key`, `--server-id` | for the dev connect token, which the client mints itself like the bots do |
| `--near-ms`, `--near-max-ms`, `--mid-ms` | the render delays: near players 67 ms, growing up to 133 ms when near updates come less often (crowds); mid and far 200 ms |
| `--view first\|chase\|spectator`, `--spectate X,Y,Z,YAW,PITCH` | the starting camera |
| `--no-vsync`, `--no-shadows`, `--ghosts` | rendering options |
| `--autoplay`, `--autofire` | wander instead of reading the keyboard; hold the trigger |
| `--screenshot PATH --after S` and `--exit-after S` | self-checks: save a frame, then quit with a summary (frame times, corrections, smoothness per tier) |

## Layout

| file | what's in it |
|---|---|
| `net.rs` | `Session`: UDP socket + transport + client core, polled and flushed each frame. No Bevy; tested against an in-process `SimServer` over loopback. |
| `coords.rs` | Game (x, y, z up) → Bevy (x, z, −y); yaw/pitch encodings; movement stick from WASD and view yaw. |
| `terrain.rs` | 64 × 64 chunks of 128 m: every 4 m sample within ~1 km of the camera, every 32 m beyond, skirts on every edge. Vertex colors by height and slope, ±8% noise per sample. |
| `scene.rs` | The world on Welcome (terrain, cover, rocks, sun), terrain streaming, players (a root at the feet: capsule, or soldier when near) and ghosts. |
| `models.rs` | The models: loading, cover fitted to its boxes, rocks, soldiers (spawn, tint, animation, rifle in hand), the first-person rifle. |
| `../tools/import-assets` | Meshy's output → `assets/models` (see Models). |
| `controls.rs` | Mouse look, the frame's input to `tick_inputs`, cameras, toggles, autoplay. |
| `hud.rs` | The net graph, the crosshair and the help line. |

## Measured (2026-10-05)

Windows 10 desktop (AMD GPU), 1280×720, no vsync. The server and a 1,000-bot blob ran in WSL, with the client in the blob for 20 s.

| frame time p50 / p99 | corrections (push) | near interpolated | mid interpolated | render delay | resyncs |
|---|---|---|---|---|---|
| 3.4–3.5 / 4.2–4.7 ms (~285 fps) | 0 (8–12) | 99.9% | 97.3–97.5% | 100.2 ms, no snaps | 0 |

RTT across WSL's NAT is ~11 ms, and input → applied ~70 ms.

**With the models** (2026-10-06, the same desktop and blob, vsync off): frame time p50 6.5 / p99 8.0 ms (~150 fps), 0 corrections. Near players were 99.96% interpolated and mid 97.8%. The capsules cost 3.5 / 4.7 ms, so the soldiers (up to 160 animated) and the cover models add about 3 ms. With 12 bots in view: 5.6 / 6.1 ms.
