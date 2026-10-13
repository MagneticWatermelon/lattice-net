//! Deterministic player movement, shared by the server and the clients' prediction.
//!
//! Both sides run exactly this code on exactly these f32s (no FMA contraction in
//! Rust, `sqrt` is correctly rounded) against the same `World`, so replaying an
//! input stream reproduces the server's state bit-for-bit. Any mismatch a client
//! sees is a real misprediction (or a push from a crowd; see the server).
//!
//! 2.5D: players walk on the terrain (`World::terrain`), jump and fall under
//! gravity, can't climb slopes steeper than `MAX_SLOPE`, step up onto low cover
//! and slide along the rest. The horizontal plane is the world's (x, y); `z` is up.

use crate::world::World;

pub const TICK_HZ: u32 = 30;
pub const DT: f32 = 1.0 / TICK_HZ as f32;
/// Continent edge length in meters. The world spans [0, WORLD_SIZE] on x and y.
pub const WORLD_SIZE: f32 = 8192.0;
pub const RUN_SPEED: f32 = 6.0;
pub const SPRINT_SPEED: f32 = 9.0;
/// Crouching (`BUTTON_CROUCH`): this speed, no sprint and no jump; the
/// collider's height and the eyes' (`weapon::CROUCH_EYE_HEIGHT`) lower.
pub const CROUCH_SPEED: f32 = 3.0;
pub const CROUCH_HEIGHT: f32 = 1.2;
/// Max change of velocity per second (m/s²); a third of it in the air.
const ACCEL: f32 = 60.0;
const AIR_ACCEL: f32 = ACCEL / 3.0;
const GRAVITY: f32 = 15.0;
pub const JUMP_SPEED: f32 = 4.5;
/// Rise over run a player can still walk up (45°).
pub const MAX_SLOPE: f32 = 1.0;
/// Highest ledge a walking player steps up onto, and how far down the ground
/// may drop under a walking player before they fall instead.
pub const STEP_UP: f32 = 0.45;
const STEP_DOWN: f32 = 0.6;
/// The player's collision cylinder.
pub const RADIUS: f32 = 0.4;
pub const HEIGHT: f32 = 1.8;

pub const BUTTON_SPRINT: u8 = 1;
pub const BUTTON_JUMP: u8 = 2;
/// On the wire only: this input carries a shot (`weapon::Shot`). Movement
/// ignores it; decoded inputs never have it set.
pub const BUTTON_FIRE: u8 = 4;
/// Aiming down sights: slower (`weapon::ADS_SPEED`), no sprint, a tighter
/// cone of fire.
pub const BUTTON_ADS: u8 = 8;
/// Crouching while held: lower (`CROUCH_HEIGHT`), slower (`CROUCH_SPEED`),
/// no sprint or jump, a tighter cone of fire (`weapon::CONE_CROUCH`).
pub const BUTTON_CROUCH: u8 = 16;
/// Reload the rifle (`weapon::Ammo`); movement ignores it.
pub const BUTTON_RELOAD: u8 = 32;

/// One tick of player intent. `move_*` is a world-space stick in [-127, 127].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Input {
    pub move_x: i8,
    pub move_y: i8,
    pub yaw: u16,
    /// Up/down aim, -32767 (straight down) to 32767 (straight up).
    pub pitch: i16,
    pub buttons: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MoveState {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    /// Height of the feet.
    pub z: f32,
    pub vz: f32,
    pub grounded: bool,
    /// Crouching: its input held `BUTTON_CROUCH`.
    pub crouched: bool,
}

impl MoveState {
    /// Standing at (x, y): pushed out of any cover, on the ground.
    pub fn standing(world: &World, pos: [f32; 2]) -> Self {
        let mut s = MoveState { pos, vel: [0.0; 2], z: f32::MAX, vz: 0.0, grounded: true, crouched: false };
        s.z = world.terrain(pos[0], pos[1]);
        collide(world, &mut s);
        s.z = world.ground(s.pos[0], s.pos[1], s.z, STEP_UP);
        s
    }

    /// The collider's height: standing or crouched.
    pub fn height(&self) -> f32 {
        if self.crouched {
            CROUCH_HEIGHT
        } else {
            HEIGHT
        }
    }
}

/// What a dead player's inputs move: nothing; its body settles under gravity
/// like any other. (Yaw and pitch are kept for its own camera; the server
/// keeps the body's aim as it died, so others see a still corpse.)
/// The server and the client's prediction both apply this while dead.
pub fn dead_input(i: Input) -> Input {
    Input { yaw: i.yaw, pitch: i.pitch, ..Input::default() }
}

/// Starved steps whose stand-in repeats the last input before movement
/// freezes (`stand_in`).
pub const GRACE_TICKS: u32 = 2;

/// What the server applies for a seq whose input came too late or never,
/// the `run`th such seq in a row (from 1), given the last real input it
/// applied: that input again for `GRACE_TICKS` (a lost packet costs nothing),
/// then a frozen one, no movement with the facing kept (holding packets back
/// buys no movement). A client predicts the same for the seqs it leaves to
/// stand-ins after a resync.
pub fn stand_in(last: Input, run: u32) -> Input {
    if run <= GRACE_TICKS {
        last
    } else {
        Input { yaw: last.yaw, ..Input::default() }
    }
}

pub fn step(world: &World, s: MoveState, input: Input) -> MoveState {
    let mut wish = [input.move_x as f32 / 127.0, input.move_y as f32 / 127.0];
    let len = (wish[0] * wish[0] + wish[1] * wish[1]).sqrt();
    if len > 1.0 {
        wish = [wish[0] / len, wish[1] / len];
    }
    let crouched = input.buttons & BUTTON_CROUCH != 0;
    let speed = if crouched {
        CROUCH_SPEED
    } else if input.buttons & BUTTON_ADS != 0 {
        RUN_SPEED * crate::weapon::ADS_SPEED
    } else if input.buttons & BUTTON_SPRINT != 0 {
        SPRINT_SPEED
    } else {
        RUN_SPEED
    };

    let mut dv = [wish[0] * speed - s.vel[0], wish[1] * speed - s.vel[1]];
    let dv_len = (dv[0] * dv[0] + dv[1] * dv[1]).sqrt();
    let max_dv = if s.grounded { ACCEL * DT } else { AIR_ACCEL * DT };
    if dv_len > max_dv {
        let k = max_dv / dv_len;
        dv = [dv[0] * k, dv[1] * k];
    }

    let mut n = s;
    n.crouched = crouched;
    n.vel = [s.vel[0] + dv[0], s.vel[1] + dv[1]];
    if s.grounded && input.buttons & BUTTON_JUMP != 0 && !crouched {
        n.vz = JUMP_SPEED;
        n.grounded = false;
    }
    if !n.grounded {
        n.vz -= GRAVITY * DT;
    }

    // Horizontal: the world's edges, then slopes too steep to walk up.
    n.pos = [s.pos[0] + n.vel[0] * DT, s.pos[1] + n.vel[1] * DT];
    for i in 0..2 {
        if n.pos[i] < 0.0 || n.pos[i] > WORLD_SIZE {
            n.pos[i] = n.pos[i].clamp(0.0, WORLD_SIZE);
            n.vel[i] = 0.0;
        }
    }
    // A slope too steep to walk up is a wall where it rises past the feet:
    // walking, and in the air too (landing on it would climb a cliff a jump
    // at a time); not under someone on a deck above it.
    let feet = if n.grounded { s.z } else { s.z + n.vz * DT };
    n.pos = terrain_move(world, s.pos, n.pos, feet, &mut n.vel);
    collide(world, &mut n);

    // Vertical: follow the ground while walking, fall and land otherwise.
    if n.grounded {
        let g = world.ground(n.pos[0], n.pos[1], n.z, STEP_UP);
        if g >= n.z - STEP_DOWN {
            n.z = g;
        } else {
            n.grounded = false; // walked off a ledge
            n.vz = 0.0;
        }
    } else {
        n.z += n.vz * DT;
        // Falling, the feet catch a ledge within a step (how a jump gets
        // onto a crate); rising, only what's below them.
        let reach = if n.vz <= 0.0 { STEP_UP } else { 0.0 };
        let g = world.ground(n.pos[0], n.pos[1], s.z.max(n.z), reach);
        if n.z <= g {
            n.z = g;
            n.vz = 0.0;
            n.grounded = true;
        }
    }
    n
}

/// The server moving a player by `delta` outside of their inputs (a crowd's
/// separation push): kept in the world and out of cover, and on the ground if
/// they were on it. Clients can't predict these; they reconcile.
pub fn nudge(world: &World, s: MoveState, delta: [f32; 2]) -> MoveState {
    let mut n = s;
    n.pos = [(s.pos[0] + delta[0]).clamp(0.0, WORLD_SIZE), (s.pos[1] + delta[1]).clamp(0.0, WORLD_SIZE)];
    // Never up a cliff: a crowd at its foot would push its edge up it.
    let mut vel = n.vel;
    n.pos = terrain_move(world, s.pos, n.pos, s.z, &mut vel);
    collide(world, &mut n);
    if n.grounded {
        let g = world.ground(n.pos[0], n.pos[1], n.z, STEP_UP);
        if g >= n.z - STEP_DOWN {
            n.z = g;
        } else {
            n.grounded = false;
            n.vz = 0.0;
        }
    }
    n
}

/// Whether moving from `from` to `to` climbs terrain steeper than
/// `MAX_SLOPE` where it rises past the feet (at `feet`): a wall. Steeper
/// along the move; or any rise at all from ground that steep onto ground
/// that steep, so a slope too steep can't be climbed slantwise either (a
/// slide along a cliff while jumping at it would creep up it). (Falling
/// past a cliff's foot lands there; on a box above it, it's not in the way.)
fn steep(world: &World, from: [f32; 2], to: [f32; 2], feet: f32) -> bool {
    let t = world.terrain(to[0], to[1]);
    let rise = t - world.terrain(from[0], from[1]);
    // Not a climb, or the ground's well under the feet (a deck over it).
    if rise <= 0.0 || t + 1.0 <= feet {
        return false;
    }
    let run = ((to[0] - from[0]) * (to[0] - from[0]) + (to[1] - from[1]) * (to[1] - from[1])).sqrt();
    // On the ground (or within a jump of it), on a cliff any rise at all is
    // a wall, slantwise too and with the feet still above it: else a slide
    // along a cliff, or each jump at it, would creep a little higher up it.
    let on_terrain = feet < world.terrain(from[0], from[1]) + 1.0;
    (rise > MAX_SLOPE * run && t > feet) || (on_terrain && world.cliff(from[0], from[1]) && world.cliff(to[0], to[1]))
}

/// Where a move from `from` toward `to` ends against the terrain: all the
/// way, or, into a slope too steep (`steep`), slid along it (the part of the
/// move up the slope taken away, from the move and from `vel`), or not at
/// all if that's still a climb.
fn terrain_move(world: &World, from: [f32; 2], to: [f32; 2], feet: f32, vel: &mut [f32; 2]) -> [f32; 2] {
    if !steep(world, from, to, feet) {
        return to;
    }
    let g = world.gradient(to[0], to[1]);
    let len = (g[0] * g[0] + g[1] * g[1]).sqrt();
    if len > 0.0 {
        let (nx, ny) = (g[0] / len, g[1] / len);
        let d = [to[0] - from[0], to[1] - from[1]];
        let into = d[0] * nx + d[1] * ny;
        if into > 0.0 {
            let slid = [(to[0] - into * nx).clamp(0.0, WORLD_SIZE), (to[1] - into * ny).clamp(0.0, WORLD_SIZE)];
            if !steep(world, from, slid, feet) {
                let v = vel[0] * nx + vel[1] * ny;
                if v > 0.0 {
                    *vel = [vel[0] - v * nx, vel[1] - v * ny];
                }
                return slid;
            }
        }
    }
    *vel = [0.0; 2];
    from
}

/// Pushes the player's circle out of every cover box their body overlaps in
/// height and can't step onto, taking away the velocity into the box (so they
/// slide along it). Boxes come in the index's fixed order.
fn collide(world: &World, s: &mut MoveState) {
    let height = s.height();
    for _ in 0..2 {
        world.boxes_near(s.pos[0], s.pos[1], RADIUS, |b| {
            if b.top <= s.z + STEP_UP || b.bottom >= s.z + height {
                return; // below the feet's reach, or above the head
            }
            let cx = s.pos[0].clamp(b.min[0], b.max[0]);
            let cy = s.pos[1].clamp(b.min[1], b.max[1]);
            let (dx, dy) = (s.pos[0] - cx, s.pos[1] - cy);
            let d2 = dx * dx + dy * dy;
            if d2 >= RADIUS * RADIUS {
                return;
            }
            if d2 > 0.0 {
                // Outside the box: out along the line to its nearest point.
                let d = d2.sqrt();
                let (nx, ny) = (dx / d, dy / d);
                s.pos = [cx + nx * RADIUS, cy + ny * RADIUS];
                let into = s.vel[0] * nx + s.vel[1] * ny;
                if into < 0.0 {
                    s.vel = [s.vel[0] - into * nx, s.vel[1] - into * ny];
                }
            } else {
                // Center inside: out through the nearest face.
                let faces = [s.pos[0] - b.min[0], b.max[0] - s.pos[0], s.pos[1] - b.min[1], b.max[1] - s.pos[1]];
                let i = (0..4).fold(0, |m, i| if faces[i] < faces[m] { i } else { m });
                match i {
                    0 => (s.pos[0], s.vel[0]) = (b.min[0] - RADIUS, s.vel[0].min(0.0)),
                    1 => (s.pos[0], s.vel[0]) = (b.max[0] + RADIUS, s.vel[0].max(0.0)),
                    2 => (s.pos[1], s.vel[1]) = (b.min[1] - RADIUS, s.vel[1].min(0.0)),
                    _ => (s.pos[1], s.vel[1]) = (b.max[1] + RADIUS, s.vel[1].max(0.0)),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;
    use std::sync::Arc;

    fn world() -> Arc<World> {
        World::shared(1)
    }

    /// A spot with no cover within `r` and gentle terrain.
    fn open_spot(w: &World, r: f32) -> [f32; 2] {
        for i in 0..10_000 {
            let p = [600.0 + (i % 100) as f32 * 61.0, 600.0 + (i / 100) as f32 * 59.0];
            let mut any = false;
            w.boxes_near(p[0], p[1], r, |_| any = true);
            let flat = (w.terrain(p[0] + r, p[1]) - w.terrain(p[0] - r, p[1])).abs() < 0.2 * r;
            if !any && flat {
                return p;
            }
        }
        panic!("no open spot");
    }

    #[test]
    fn walks_on_the_terrain_at_run_speed_and_stops() {
        let w = world();
        let p = open_spot(&w, 20.0);
        let fwd = Input { move_x: 127, ..Default::default() };
        let mut s = MoveState::standing(&w, p);
        for _ in 0..TICK_HZ {
            s = step(&w, s, fwd);
            assert!(s.grounded);
            assert_eq!(s.z, w.ground(s.pos[0], s.pos[1], s.z, STEP_UP), "feet on the ground");
        }
        assert!((s.vel[0] - RUN_SPEED).abs() < 1e-4, "{s:?}");
        assert!(s.pos[0] - p[0] > 4.0 && s.pos[0] - p[0] < 6.0, "{s:?}");
        for _ in 0..TICK_HZ {
            s = step(&w, s, Input::default());
        }
        assert_eq!(s.vel, [0.0, 0.0]);
    }

    #[test]
    fn diagonal_is_not_faster() {
        let w = world();
        let s = MoveState::standing(&w, open_spot(&w, 20.0));
        let diag = Input { move_x: 127, move_y: 127, buttons: BUTTON_SPRINT, ..Default::default() };
        let mut t = s;
        for _ in 0..TICK_HZ {
            t = step(&w, t, diag);
        }
        let v = (t.vel[0] * t.vel[0] + t.vel[1] * t.vel[1]).sqrt();
        assert!((v - SPRINT_SPEED).abs() < 1e-3, "{v}");
    }

    #[test]
    fn jumps_land_back_on_the_ground() {
        let w = world();
        let s = MoveState::standing(&w, open_spot(&w, 20.0));
        let mut t = step(&w, s, Input { buttons: BUTTON_JUMP, ..Default::default() });
        assert!(!t.grounded && t.z > s.z);
        let mut peak = t.z;
        for _ in 0..60 {
            t = step(&w, t, Input::default());
            peak = peak.max(t.z);
            if t.grounded {
                break;
            }
        }
        assert!(t.grounded && t.z == s.z, "landed where it jumped: {t:?}");
        let height = peak - s.z;
        assert!(height > 0.55 && height < 0.75, "jump height {height}");
    }

    #[test]
    fn aiming_down_sights_is_slower_and_never_sprints() {
        let w = world();
        let p = open_spot(&w, 30.0);
        let run = |buttons| {
            let mut s = MoveState::standing(&w, p);
            for _ in 0..30 {
                s = step(&w, s, Input { move_x: 127, buttons, ..Default::default() });
            }
            (s.vel[0] * s.vel[0] + s.vel[1] * s.vel[1]).sqrt()
        };
        let ads = run(BUTTON_ADS | BUTTON_SPRINT);
        assert!((ads - RUN_SPEED * crate::weapon::ADS_SPEED).abs() < 1e-3, "{ads}");
        assert!((run(BUTTON_SPRINT) - SPRINT_SPEED).abs() < 1e-3);
    }

    #[test]
    fn crouching_is_slow_low_and_never_sprints_or_jumps() {
        let w = world();
        let p = open_spot(&w, 30.0);
        let mut s = MoveState::standing(&w, p);
        for _ in 0..30 {
            s = step(&w, s, Input { move_x: 127, buttons: BUTTON_CROUCH | BUTTON_SPRINT, ..Default::default() });
        }
        let v = (s.vel[0] * s.vel[0] + s.vel[1] * s.vel[1]).sqrt();
        assert!((v - CROUCH_SPEED).abs() < 1e-3, "{v}");
        assert!(s.crouched && s.height() == CROUCH_HEIGHT);
        let t = step(&w, s, Input { buttons: BUTTON_CROUCH | BUTTON_JUMP, ..Default::default() });
        assert!(t.grounded && t.vz == 0.0, "no jump while crouched: {t:?}");
        // Let go: standing again at once.
        let t = step(&w, t, Input::default());
        assert!(!t.crouched && t.height() == HEIGHT);
        // The dead don't crouch.
        assert!(!step(&w, s, dead_input(Input { buttons: BUTTON_CROUCH, ..Default::default() })).crouched);
    }

    #[test]
    fn cover_blocks_and_slides() {
        let w = world();
        // A tall wall, approached head-on along x then diagonally.
        let b = *w.boxes().iter().find(|b| b.top - b.bottom > 2.5 && b.max[1] - b.min[1] > 6.0).expect("a long wall along y");
        let (mid_y, x0) = ((b.min[1] + b.max[1]) / 2.0, b.min[0] - 3.0);
        let mut s = MoveState::standing(&w, [x0, mid_y]);
        for _ in 0..60 {
            s = step(&w, s, Input { move_x: 127, ..Default::default() });
        }
        assert!(s.pos[0] <= b.min[0] - RADIUS + 1e-3, "stopped at the wall: {s:?}");
        // Diagonally: pressed against the face, it slides along y.
        let face = b.min[0] - RADIUS;
        let mut t = MoveState::standing(&w, [x0, mid_y]);
        let mut contact: Option<f32> = None;
        let mut slid = 0.0f32;
        for _ in 0..60 {
            t = step(&w, t, Input { move_x: 127, move_y: 127, ..Default::default() });
            // Alongside the flat face (around its ends the circle rounds the corner).
            let beside = t.pos[1] > b.min[1] && t.pos[1] < b.max[1];
            assert!(!beside || t.pos[0] <= face + 1e-3, "never through the wall: {t:?}");
            if beside && (t.pos[0] - face).abs() < 1e-3 {
                let y0 = *contact.get_or_insert(t.pos[1]);
                slid = slid.max(t.pos[1] - y0);
            }
        }
        assert!(slid > 1.0, "slid {slid} m along the wall");
    }

    /// On Kiln, a rift's wall: (a point on its floor at its foot, the
    /// direction into the wall).
    fn rift_wall() -> (Arc<World>, [f32; 2], [f32; 2]) {
        let w = World::shared(crate::world::KILN);
        let rift = crate::kiln::Rift::new(0);
        // Out along the floor toward the wall until the ground starts to rise.
        let mut across = 0.0;
        let base = w.terrain(rift.at(3200.0, 0.0)[0] as f32, rift.at(3200.0, 0.0)[1] as f32);
        loop {
            let p = rift.at(3200.0, -across);
            if w.terrain(p[0] as f32, p[1] as f32) > base + 3.0 {
                break;
            }
            across += 1.0;
        }
        let p = rift.at(3200.0, -(across - 12.0));
        let into = [-rift.normal[0] as f32, -rift.normal[1] as f32];
        (w, [p[0] as f32, p[1] as f32], into)
    }

    #[test]
    fn cliffs_are_walls_even_to_jumps() {
        let (w, foot, into) = rift_wall();
        let mut s = MoveState::standing(&w, foot);
        let start = s.z;
        let push = Input { move_x: (into[0] * 127.0) as i8, move_y: (into[1] * 127.0) as i8, ..Default::default() };
        // Walking into the wall, then jumping at it for 20 s: never up it.
        for i in 0..600 {
            let jump = if i > 60 { BUTTON_JUMP } else { 0 };
            s = step(&w, s, Input { buttons: BUTTON_SPRINT | jump, ..push });
            assert!(s.z < start + 12.0, "climbed the wall to {} (from {start})", s.z);
        }
        let t = w.terrain(s.pos[0], s.pos[1]);
        assert!(s.z - t < 1.0 || !s.grounded, "standing on the ground at the wall's foot: z {} terrain {t}", s.z);
    }

    #[test]
    fn slides_along_cliffs() {
        let (w, foot, into) = rift_wall();
        let mut s = MoveState::standing(&w, foot);
        // Diagonally into the wall: along it, not stopped.
        let along = [into[1], -into[0]];
        let d = [(into[0] + along[0]) * 0.7, (into[1] + along[1]) * 0.7];
        for _ in 0..150 {
            s = step(&w, s, Input { move_x: (d[0] * 127.0) as i8, move_y: (d[1] * 127.0) as i8, ..Default::default() });
        }
        let moved = (s.pos[0] - foot[0]) * along[0] + (s.pos[1] - foot[1]) * along[1];
        assert!(moved > 10.0, "slid {moved} m along the wall in 5 s");
    }

    #[test]
    fn steps_onto_crates_by_jumping() {
        let w = world();
        // A crate no more than ~1 m above the ground it's approached from.
        let c = *w
            .boxes()
            .iter()
            .find(|b| {
                let y = (b.min[1] + b.max[1]) / 2.0;
                b.top - b.bottom < 1.2 && b.max[0] - b.min[0] > 1.6 && b.top - w.terrain(b.min[0] - 1.0, y) < 1.0
            })
            .expect("a crate");
        let y = (c.min[1] + c.max[1]) / 2.0;
        let mut s = MoveState::standing(&w, [c.min[0] - 2.0, y]);
        let mut on_top = false;
        for i in 0..90 {
            let jump = if i > 4 { BUTTON_JUMP } else { 0 };
            s = step(&w, s, Input { move_x: 60, buttons: jump, ..Default::default() });
            on_top |= s.grounded && s.z == c.top;
        }
        assert!(on_top, "jumped onto the crate: {s:?} vs top {}", c.top);
    }

    #[test]
    fn replays_are_bit_exact() {
        let w = world();
        let p = open_spot(&w, 30.0);
        let inputs: Vec<Input> = (0..300u32)
            .map(|i| Input {
                move_x: ((i * 37) % 255) as i8,
                move_y: ((i * 91) % 255) as i8,
                yaw: (i * 999) as u16,
                pitch: 0,
                buttons: (i % 3) as u8 & (BUTTON_SPRINT | BUTTON_JUMP) | if i % 7 < 2 { BUTTON_CROUCH } else { 0 },
            })
            .collect();
        let run = || inputs.iter().fold(MoveState::standing(&w, p), |s, &i| step(&w, s, i));
        let (a, b) = (run(), run());
        assert_eq!(a.pos[0].to_bits(), b.pos[0].to_bits());
        assert_eq!(a.z.to_bits(), b.z.to_bits());
        assert_eq!(a.crouched, b.crouched);
    }
}
