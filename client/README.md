# lattice-client: the first playable client

A Bevy window on `lattice-client-core`, the same client code the bots run: prediction, the render timeline, entity interpolation. It plays on a `lattice-server`, walking and jumping among the bots. There's no combat yet (M3d).

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

In WSL itself, `cargo run` in `client/` opens a window under WSLg, but it renders on the CPU (llvmpipe, 5–10 fps). That's good for checking what's drawn, not for playing.

## Controls

| key | |
|---|---|
| click / Esc | grab / release the mouse |
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
| `--near-ms`, `--mid-ms` | the render delays: near players 67 ms, mid and far 200 ms |
| `--view first\|chase\|spectator`, `--spectate X,Y,Z,YAW,PITCH` | the starting camera |
| `--no-vsync`, `--no-shadows`, `--ghosts` | rendering options |
| `--autoplay` | wander instead of reading the keyboard |
| `--screenshot PATH --after S` and `--exit-after S` | self-checks: save a frame, then quit with a summary (frame times, corrections, smoothness per tier) |

## Layout

| file | what's in it |
|---|---|
| `net.rs` | `Session`: UDP socket + transport + client core, polled and flushed each frame. No Bevy; tested against an in-process `SimServer` over loopback. |
| `coords.rs` | Game (x, y, z up) → Bevy (x, z, −y); yaw/pitch encodings; movement stick from WASD and view yaw. |
| `terrain.rs` | 64 × 64 chunks of 128 m: every 4 m sample within ~1 km of the camera, every 32 m beyond, skirts on every edge. Vertex colors by height and slope, ±8% noise per sample. |
| `scene.rs` | The world on Welcome (terrain, ~15k cover boxes, sun), terrain streaming, players and ghosts. |
| `controls.rs` | Mouse look, the frame's input to `tick_inputs`, cameras, toggles, autoplay. |
| `hud.rs` | The net graph, the crosshair and the help line. |

## Measured (2026-10-05)

Windows 10 desktop (AMD GPU), 1280×720, no vsync. The server and a 1,000-bot blob ran in WSL, with the client in the blob for 20 s.

| frame time p50 / p99 | corrections (push) | near interpolated | mid interpolated | render delay | resyncs |
|---|---|---|---|---|---|
| 3.4–3.5 / 4.2–4.7 ms (~285 fps) | 0 (8–12) | 99.9% | 97.3–97.5% | 100.2 ms, no snaps | 0 |

RTT across WSL's NAT is ~11 ms, and input → applied ~70 ms.
