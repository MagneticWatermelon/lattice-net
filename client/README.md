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

Everything visible beyond the terrain is a model in `assets/models/` (7.9 MB), made with Meshy and imported by `import-assets`.

| model | drawn as | triangles |
|---|---|---|
| `soldier` | every player within 120 m (at most 160, nearest first; capsules beyond), tinted by faction or tier, rigged | 15.5k |
| `anim_*` | its clips: idle, aim-walk ("Run and Shoot"), run ("Rifle Charge"), sprint, jump, death | — |
| `rifle` | in each soldier's right hand, and the first-person view model | 3.9k |
| `wall` | concrete blocks tiled along every wall (the scattered walls and base perimeters) | 0.9k |
| `crate`, `container`, `sandbags` | low cover and base cover | 0.9–1.8k |
| `post`, `command`, `bunker` | base buildings: guard posts at the gates, the command building, bunkers | 2.8–5.0k |
| `rocks` | small clusters scattered for scale, too low to look like cover; drawn within 300 m | 1.1k |

- **What you see is what blocks you.** Each prop is normalized to fill a unit box (x along its long side, y up, front at +z), so one transform puts it exactly in its collision box from `World::boxes()`. Antennas and the radar dish stick out above. Beyond 700 m (1.8 km for buildings) the plain boxes take over.
- **Soldiers:** a soldier's white armor takes the player's color. Its clip follows its drawn speed (idle < 0.4 m/s, aim-walk < 4.5, run < 7.5, sprint), played backward when it backs up. Death plays once and holds. The rifle is a child of the `RightHand` bone; `LATTICE_RIFLE="x,y,z,rx,ry,rz"` (cm, degrees) overrides its grip, for tuning.
- **First person:** the rifle is drawn by a second camera on render layer 1 after the world, with depth cleared, so it never sinks into a wall.

**Re-importing:** `cargo run --release --bin import-assets` turns Meshy's output (`assets/meshy_output/*/<name>.glb`, kept out of git, 152 MB) into `assets/models/`:
- it shrinks textures to 1024 px and makes materials single-sided;
- it normalizes the props;
- it cuts the clips down to skeleton and keyframes;
- it makes "Run and Shoot" and "Rifle Charge" run in place (they move the hips forward 1.1 and 2.5 m a cycle);
- it takes the jump's own rise out.

**Meshy tasks** (2026-10-05, 185 credits). Each prop is a smart-topology text-to-3d preview, refined with 2k PBR textures. The soldier is a `latest` preview remeshed to 15k triangles, refined, rigged at 1.8 m, plus 5 animations. Follow-ups (retexture, remesh, LODs, more clips) start from these, without regenerating:

| model | preview | refine |
|---|---|---|
| soldier | `01a10dc6-4f9c-72d6-97ba-764c06b004c3` | `01a10dc8-be91-7176-83a5-61c00d1fb163`, rig `01a10dcb-3213-765a-8fc8-af580f692d03` |
| rifle | `01a10dc6-6023-751a-bc97-a4790d5493f4` | `01a10dc8-ca95-7552-b442-697ee4819a38` |
| wall | `01a10dc6-730c-7437-b186-a0104fca3891` | `01a10dc8-d675-756e-bd59-2662e86b623d` |
| crate | `01a10dc5-c29f-729c-8154-d0a8d6a81872` | `01a10dc8-b18b-7193-b81c-b42d64defe2b` |
| container | `01a10dc6-86c6-72de-8a0f-cbc1ac991966` | `01a10dc8-e227-77fc-b6f0-cad6287916f4` |
| sandbags | `01a10dc6-958d-70fc-9824-f3c81c0f7aa5` | `01a10dc8-f07b-7155-b5fc-dba9b5609237` |
| post (asked for a tower) | `01a10dc6-b424-76e2-9ada-3e6371772951` | `01a10dc8-ff3b-740a-a787-c18ac7d8f344` |
| command | `01a10dc6-c1ba-73b4-8b69-18b51ea3b255` | `01a10dc9-0a8b-7107-b63c-2bb2827d9ae4` |
| bunker | `01a10dc6-cef7-7118-9a3a-3546b4bc827e` | `01a10dc9-17e8-704b-ba3d-22fe9e27c4dd` |
| rocks | `01a10dc6-ddb9-75b9-8503-449a7c5867fe` | `01a10dc9-2539-7160-bae0-b08de3b0d553` |

Animations (on the rig): idle `01a10dcd-48f7-7009-bd44-c939dd93a4e0`, Run and Shoot `01a10dcd-4e9c-76c3-a408-1d18283c2677`, Rifle Charge `01a10dcd-5456-723e-a781-e7a8ca1dd9bc`, Regular Jump `01a10dcd-59c7-7765-8243-4ac1787e059d`, Shot and Fall Backward `01a10dcd-5f47-751e-b6b0-ddb1dc05874d`. Walking and running came with the rig.

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
| `bin/import-assets.rs` | Meshy's output → `assets/models` (see Models). |
| `controls.rs` | Mouse look, the frame's input to `tick_inputs`, cameras, toggles, autoplay. |
| `hud.rs` | The net graph, the crosshair and the help line. |

## Measured (2026-10-05)

Windows 10 desktop (AMD GPU), 1280×720, no vsync. The server and a 1,000-bot blob ran in WSL, with the client in the blob for 20 s.

| frame time p50 / p99 | corrections (push) | near interpolated | mid interpolated | render delay | resyncs |
|---|---|---|---|---|---|
| 3.4–3.5 / 4.2–4.7 ms (~285 fps) | 0 (8–12) | 99.9% | 97.3–97.5% | 100.2 ms, no snaps | 0 |

RTT across WSL's NAT is ~11 ms, and input → applied ~70 ms.

**With the models** (2026-10-06, the same desktop and blob, vsync off): frame time p50 6.5 / p99 8.0 ms (~150 fps), 0 corrections. Near players were 99.96% interpolated and mid 97.8%. The capsules cost 3.5 / 4.7 ms, so the soldiers (up to 160 animated) and the cover models add about 3 ms. With 12 bots in view: 5.6 / 6.1 ms.
