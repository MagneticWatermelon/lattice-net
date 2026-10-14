//! The collision kit's test level (K1): flat ground at 100 m with one of
//! everything C1, holes and force volumes add, so the tests (and
//! `examples/determinism`, Linux against Windows) cover each. Its world id is
//! `KIT`; `World::build(KIT)` builds it, so a server can run it too.
//!
//! Laid out west to east along y = 1000 (all within 400 m of (1200, 1000)):
//! - a wall turned 30° (x 1000);
//! - a wedge ramp up to a platform (x 1020-1050);
//! - a slab to jump into (x 1060), a crawlway (x 1080);
//! - a wall with a gap shielded for faction 1 (y 1030);
//! - a tunnel: a ramp down a hole in the ground, 100 m under it, a ramp up
//!   another hole (x 1100-1248);
//! - a tower with a lift up its side and a drop shaft down the other (x 1300);
//! - a spring (x 1350).

use crate::world::{hole_bits, Continent, CoverBox, Force, Kind, Lane, Parts, Volume, World, ALWAYS, TERRAIN_N, TERRAIN_RES, UNTURNED};

/// The kit level's world id ("KIT" in ASCII).
pub const KIT: u64 = 0x4B_4954;
/// Its ground's height.
pub const GROUND: f32 = 100.0;

/// The turned wall: center, half-sizes, axis (30°).
pub const TURNED: ([f32; 2], [f32; 2], f32) = ([1000.0, 1000.0], [10.0, 0.5], 30.0);
/// The wedge ramp, x from..to (rising east), its top; the platform past it.
pub const RAMP: (f32, f32, f32) = (1020.0, 1040.0, 106.6);
pub const PLATFORM: (f32, f32) = (1040.0, 1050.0);
/// The slab's underside (2.2 m up: under a jump's reach) and the crawlway's (1.5 m).
pub const SLAB: (f32, f32, f32) = (1060.0, 1070.0, 102.2);
pub const CRAWL: (f32, f32, f32) = (1080.0, 1090.0, 101.5);
/// The shielded gap: x from..to in the wall along y = `GAP_Y`, letting `GAP_PASS` through.
pub const GAP: (f32, f32) = (1010.0, 1014.0);
pub const GAP_Y: f32 = 1030.0;
pub const GAP_PASS: u8 = 1;
/// The tunnel: the holes' x ranges (ramps down, then up), its floor and
/// its ceiling's underside; y from 996 to 1004.
pub const TUNNEL_IN: (f32, f32) = (1100.0, 1124.0);
pub const TUNNEL_OUT: (f32, f32) = (1224.0, 1248.0);
pub const TUNNEL_FLOOR: f32 = 92.0;
pub const TUNNEL_CEILING: f32 = 96.0;
/// The tower's footprint (x, y ranges) and top; its lift's and drop shaft's columns.
pub const TOWER: ([f32; 2], [f32; 2], f32) = ([1300.0, 1310.0], [990.0, 1000.0], 140.0);
pub const LIFT_X: [f32; 2] = [1311.0, 1315.0];
pub const DROP_X: [f32; 2] = [1295.0, 1299.0];
/// The spring's pad (x range) and its throw.
pub const SPRING: ([f32; 2], [f32; 3]) = ([1350.0, 1352.0], [7.0, 0.0, 12.0]);

/// Builds the kit level.
pub fn level() -> World {
    let g = GROUND;
    let mut boxes = Vec::new();
    let (c, half, deg) = TURNED;
    let a = (deg as f64).to_radians();
    let axis = [libm::cos(a) as f32, libm::sin(a) as f32];
    boxes.push(CoverBox::turned(c, half, axis, g - 0.3, g + 4.0, Kind::Wall, 1));

    let (x0, x1, top) = RAMP;
    boxes.push(CoverBox::new([x0, 998.0], [x1, 1002.0], g - 0.3, top, Kind::Block, 0).wedge(g));
    boxes.push(CoverBox::new([PLATFORM.0, 996.0], [PLATFORM.1, 1004.0], g - 0.3, top, Kind::Block, 0));

    let (x0, x1, under) = SLAB;
    boxes.push(CoverBox::new([x0, 995.0], [x1, 1005.0], under, under + 0.4, Kind::Block, 0));
    let (x0, x1, under) = CRAWL;
    boxes.push(CoverBox::new([x0, 995.0], [x1, 1005.0], under, under + 0.4, Kind::Block, 0));

    boxes.push(CoverBox::new([1000.0, GAP_Y - 0.3], [GAP.0, GAP_Y + 0.3], g - 0.3, g + 4.0, Kind::Wall, 1));
    boxes.push(CoverBox::new([GAP.1, GAP_Y - 0.3], [1024.0, GAP_Y + 0.3], g - 0.3, g + 4.0, Kind::Wall, 1));
    boxes.push(CoverBox::new([GAP.0, GAP_Y - 0.2], [GAP.1, GAP_Y + 0.2], g - 0.3, g + 4.0, Kind::Shield, 1).shield(GAP_PASS));

    // The tunnel: ramps (wedges) in the holes, walls lining the holes and the
    // tube, its floor and its ceiling.
    let (f, ceil) = (TUNNEL_FLOOR, TUNNEL_CEILING);
    let (i0, i1) = TUNNEL_IN;
    let (o0, o1) = TUNNEL_OUT;
    let mid = |a: f32, b: f32| [(a + b) * 0.5, 1000.0];
    boxes.push(CoverBox::turned(mid(i0, i1), [(i1 - i0) * 0.5, 4.0], [-1.0, 0.0], f - 1.0, g, Kind::Block, 0).wedge(f));
    boxes.push(CoverBox::new([o0, 996.0], [o1, 1004.0], f - 1.0, g, Kind::Block, 0).wedge(f));
    boxes.push(CoverBox::new([i1, 996.0], [o0, 1004.0], f - 1.0, f, Kind::Block, 0));
    for y in [995.4, 1004.0] {
        boxes.push(CoverBox::new([i0 - 0.6, y], [o1 + 0.6, y + 0.6], f - 1.0, g, Kind::Block, 0));
    }
    for x in [i0 - 0.6, o1] {
        boxes.push(CoverBox::new([x, 995.4], [x + 0.6, 1004.6], f - 1.0, g, Kind::Block, 0));
    }
    boxes.push(CoverBox::new([i1, 995.4], [o0, 1004.6], ceil, ceil + 0.5, Kind::Block, 0));
    let cell = |v: f32| (v / TERRAIN_RES) as usize;
    let mut cells = Vec::new();
    for (a, b) in [TUNNEL_IN, TUNNEL_OUT] {
        for cy in cell(996.0)..cell(1004.0) {
            for cx in cell(a)..cell(b) {
                cells.push((cx, cy));
            }
        }
    }

    let (tx, ty, ttop) = TOWER;
    boxes.push(CoverBox::new([tx[0], ty[0]], [tx[1], ty[1]], g - 0.3, ttop, Kind::Block, 0));
    let volumes = vec![
        Volume { min: [LIFT_X[0], ty[0], g], max: [LIFT_X[1], ty[0] + 4.0, ttop + 1.0], force: Force::Lift, vel: [0.0, 0.0, 12.0], when: ALWAYS, phase: 0, axis: UNTURNED, half: [0.0; 2] },
        Volume { min: [DROP_X[0], ty[0], g], max: [DROP_X[1], ty[1], ttop + 2.0], force: Force::Drop, vel: [0.0, 0.0, -5.0], when: ALWAYS, phase: 0, axis: UNTURNED, half: [0.0; 2] },
        Volume { min: [SPRING.0[0], 996.0, g - 0.5], max: [SPRING.0[1], 1004.0, g + 0.5], force: Force::Spring, vel: SPRING.1, when: ALWAYS, phase: 0, axis: UNTURNED, half: [0.0; 2] },
    ];

    // A route through all of it, for walking: down the tunnel and out.
    let lane = Lane {
        a: 0,
        b: 0,
        path: vec![[1094.0, 1000.0, g], [i0 + 1.0, 1000.0, g], [i1 + 2.0, 1000.0, f], [o0 - 2.0, 1000.0, f], [o1 - 1.0, 1000.0, g], [1260.0, 1000.0, g]],
        one_way: false,
        when: ALWAYS,
    };
    World::from_parts(Parts {
        id: KIT,
        continent: Continent::Harrow,
        heights: vec![(g * 100.0) as u16; TERRAIN_N * TERRAIN_N],
        sites: Vec::new(),
        boxes,
        ground: Vec::new(),
        lanes: vec![lane],
        points: Vec::new(),
        holes: hole_bits(&cells),
        volumes,
        rooms: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit;
    use crate::movement::{step, Input, MoveState, BUTTON_CROUCH, BUTTON_JUMP, BUTTON_SPRINT, HEIGHT, RADIUS};
    use std::sync::Arc;

    fn kit() -> Arc<World> {
        World::shared(KIT)
    }

    /// The stick toward `to` from `s`.
    fn toward(s: &MoveState, to: [f32; 2]) -> (i8, i8) {
        let (dx, dy) = (to[0] - s.pos[0], to[1] - s.pos[1]);
        let d = (dx * dx + dy * dy).sqrt().max(1e-3);
        ((dx / d * 127.0) as i8, (dy / d * 127.0) as i8)
    }

    fn walk(w: &World, mut s: MoveState, to: [f32; 2], buttons: u8, steps: usize, faction: u8) -> MoveState {
        for _ in 0..steps {
            let (mx, my) = toward(&s, to);
            s = step(w, s, Input { move_x: mx, move_y: my, buttons, ..Default::default() }, faction);
        }
        s
    }

    #[test]
    fn turned_walls_block_and_slide() {
        let w = kit();
        let b = w.boxes()[0];
        assert!(b.is_turned());
        // From 5 m out on its face's normal, walking into it: stopped at
        // its face (in its frame), never through.
        let n = b.world_dir([0.0, 1.0]);
        let start = [b.center[0] + n[0] * 5.0, b.center[1] + n[1] * 5.0];
        let mut s = MoveState::standing(&w, start);
        for _ in 0..90 {
            s = step(&w, s, Input { move_x: (-n[0] * 127.0) as i8, move_y: (-n[1] * 127.0) as i8, ..Default::default() }, 0);
            assert!(b.local(s.pos[0], s.pos[1])[1] >= b.half[1] + RADIUS - 1e-3, "through the turned wall: {s:?}");
        }
        assert!((b.local(s.pos[0], s.pos[1])[1] - (b.half[1] + RADIUS)).abs() < 1e-3, "against its face");
        // Slantwise: it slides along the wall.
        let along = b.world_dir([1.0, 0.0]);
        let d = [(-n[0] + along[0]) * 0.7, (-n[1] + along[1]) * 0.7];
        let before = b.local(s.pos[0], s.pos[1])[0];
        for _ in 0..30 {
            s = step(&w, s, Input { move_x: (d[0] * 127.0) as i8, move_y: (d[1] * 127.0) as i8, ..Default::default() }, 0);
        }
        assert!(b.local(s.pos[0], s.pos[1])[0] - before > 2.0, "slid along it");
        // Shots stop at it, and its normal faces back out.
        let (p0, p1) = ([start[0], start[1], GROUND + 1.5], [b.center[0], b.center[1], GROUND + 1.5]);
        let t = hit::solid(p0, p1, &b).expect("a shot hits it");
        assert!((t * 5.0 - (5.0 - b.half[1])).abs() < 1e-3, "at its face: {t}");
        let nrm = hit::solid_normal(p0, p1, t, &b);
        assert!((nrm[0] - n[0]).abs() < 1e-4 && (nrm[1] - n[1]).abs() < 1e-4, "{nrm:?}");
    }

    #[test]
    fn wedges_are_walked_up_like_ground() {
        let w = kit();
        let (x0, x1, top) = RAMP;
        let mut s = MoveState::standing(&w, [x0 - 4.0, 1000.0]);
        for _ in 0..150 {
            s = step(&w, s, Input { move_x: 127, ..Default::default() }, 0);
            if s.pos[0] > x0 + 0.5 && s.pos[0] < x1 - 0.5 {
                let on = GROUND + (top - GROUND) * (s.pos[0] - x0) / (x1 - x0);
                assert!(s.grounded && (s.z - on).abs() < 0.01, "on the ramp's slope: {s:?} vs {on}");
            }
        }
        assert!(s.pos[0] > PLATFORM.0 + 2.0 && s.z == top, "up onto the platform: {s:?}");
        // From the side, its high end is a wall; its low end is stepped onto.
        let side = walk(&w, MoveState::standing(&w, [x1 - 2.0, 995.0]), [x1 - 2.0, 1000.0], 0, 60, 0);
        assert!(side.pos[1] < 998.0 - RADIUS + 1e-3 && side.z == GROUND, "the ramp's side stops a walk at its high end: {side:?}");
        let low = walk(&w, MoveState::standing(&w, [x0 + 0.5, 995.0]), [x0 + 0.5, 1000.0], 0, 60, 0);
        assert!(low.pos[1] > 999.0, "its low end is stepped onto: {low:?}");
        // A shot over its low end passes; one into its slope stops there.
        let wedge = w.boxes()[1];
        assert!(hit::solid([x0 - 5.0, 1000.0, GROUND + 0.5], [x0 + 1.0, 1000.0, GROUND + 0.5], &wedge).is_none());
        let t = hit::solid([x0 + 15.0, 1000.0, GROUND + 8.0], [x0 + 15.0, 1000.0, GROUND], &wedge).expect("down into the slope");
        let at = GROUND + 8.0 - 8.0 * t;
        assert!((at - (GROUND + (top - GROUND) * 0.75)).abs() < 1e-3, "on its slope: {at}");
        let n = hit::solid_normal([x0 + 15.0, 1000.0, GROUND + 8.0], [x0 + 15.0, 1000.0, GROUND], t, &wedge);
        assert!(n[2] > 0.9 && n[0] < -0.2, "the slope's normal: {n:?}");
    }

    #[test]
    fn ceilings_stop_heads() {
        let w = kit();
        let (x0, x1, under) = SLAB;
        let mid = (x0 + x1) / 2.0;
        // A jump under the slab: the head stops at its underside.
        let mut s = MoveState::standing(&w, [mid, 1000.0]);
        s = step(&w, s, Input { buttons: BUTTON_JUMP, ..Default::default() }, 0);
        let mut peak = s.z;
        for _ in 0..40 {
            s = step(&w, s, Input::default(), 0);
            peak = peak.max(s.z);
        }
        assert!(s.grounded && s.z == GROUND, "{s:?}");
        assert!((peak + HEIGHT - under).abs() < 1e-3, "the head stopped at the slab: peak {peak}");
        // Crouched under the crawlway; it won't stand up until out of it.
        let (c0, c1, low) = CRAWL;
        let mut s = MoveState::standing(&w, [c0 - 2.0, 1000.0]);
        s = walk(&w, s, [c0 + 5.0, 1000.0], BUTTON_CROUCH, 60, 0);
        assert!(s.pos[0] > c0 + 3.0 && s.crouched, "crawled in: {s:?}");
        let up = step(&w, s, Input::default(), 0);
        assert!(up.crouched && up.z + up.height() <= low, "no room to stand: {up:?}");
        let out = walk(&w, up, [c1 + 4.0, 1000.0], 0, 120, 0);
        assert!(out.pos[0] > c1 + 1.0 && !out.crouched, "stands once out: {out:?}");
        // Standing, it's a wall.
        let blocked = walk(&w, MoveState::standing(&w, [c0 - 2.0, 1000.0]), [c0 + 5.0, 1000.0], 0, 60, 0);
        assert!(blocked.pos[0] <= c0 - RADIUS + 1e-3, "{blocked:?}");
    }

    #[test]
    fn shields_let_their_own_through() {
        let w = kit();
        let mid = (GAP.0 + GAP.1) / 2.0;
        for faction in 0..3u8 {
            let s = walk(&w, MoveState::standing(&w, [mid, GAP_Y - 5.0]), [mid, GAP_Y + 5.0], 0, 90, faction);
            assert_eq!(s.pos[1] > GAP_Y, faction == GAP_PASS, "faction {faction}: {s:?}");
        }
        // Shots and sight: theirs through, others' stopped.
        let (p0, p1) = ([mid, GAP_Y - 5.0, GROUND + 1.5], [mid, GAP_Y + 5.0, GROUND + 1.5]);
        assert!(hit::line_clear(&w, p0, p1, GAP_PASS));
        assert!(!hit::line_clear(&w, p0, p1, 0));
        assert!(!hit::line_clear(&w, p0, p1, crate::movement::NO_FACTION));
        // Nobody stands on one.
        assert_eq!(w.ground(mid, GAP_Y, GROUND + 5.0, 1.0, crate::clock::Live::OFF), GROUND);
    }

    #[test]
    fn holes_lead_under_the_ground() {
        let w = kit();
        let lane = &w.lanes()[0];
        let mut s = MoveState::standing(&w, [lane.path[0][0], lane.path[0][1]]);
        let mut next = 1;
        let mut lowest = f32::MAX;
        let mut in_tunnel = 0;
        for _ in 0..30 * 60 {
            let Some((mx, my)) = lane.steer([s.pos[0], s.pos[1], s.z], &mut next, false) else { break };
            s = step(&w, s, Input { move_x: mx, move_y: my, buttons: BUTTON_SPRINT, ..Default::default() }, 0);
            lowest = lowest.min(s.z);
            if w.under(s.pos[0], s.pos[1], s.z) && !w.hole(s.pos[0], s.pos[1]) {
                in_tunnel += 1;
                assert!(s.z >= TUNNEL_FLOOR - 1e-3 && s.z + s.height() <= TUNNEL_CEILING + 1e-3, "inside the tube: {s:?}");
            }
        }
        assert!(next >= lane.path.len(), "through the tunnel and out: {s:?} (waypoint {next})");
        assert!(lowest == TUNNEL_FLOOR && in_tunnel > 300, "walked its floor ({lowest}, {in_tunnel} steps under)");
        assert_eq!(s.z, GROUND, "back on the ground");
        // In the tunnel a jump stops at its ceiling, and the ground above is
        // no floor to anyone in it.
        let x = (TUNNEL_IN.1 + TUNNEL_OUT.0) / 2.0;
        let mut t = MoveState::standing_at(&w, [x, 1000.0], TUNNEL_FLOOR);
        assert_eq!(t.z, TUNNEL_FLOOR);
        t = step(&w, t, Input { buttons: BUTTON_JUMP, ..Default::default() }, 0);
        for _ in 0..30 {
            t = step(&w, t, Input::default(), 0);
            assert!(t.z + HEIGHT <= TUNNEL_CEILING + 1e-3);
        }
        assert!(t.grounded && t.z == TUNNEL_FLOOR);
        // Up on the ground, beside the holes: solid, not a way down.
        let beside = walk(&w, MoveState::standing(&w, [x, 1010.0]), [x, 1000.0], 0, 60, 0);
        assert_eq!(beside.z, GROUND, "the ground over the tunnel holds: {beside:?}");
        // Shots: from the tube up, the ceiling; down a hole, the ramp; across
        // the ground over the tube, nothing; into the ground, the ground.
        let t = hit::terrain(&w, [x, 1000.0, TUNNEL_FLOOR + 1.5], [x, 1000.0, GROUND + 20.0]).expect("the shell from below");
        assert!((TUNNEL_FLOOR + 1.5 + t * (GROUND + 20.0 - TUNNEL_FLOOR - 1.5) - GROUND).abs() < 0.05);
        let hole = (TUNNEL_IN.0 + TUNNEL_IN.1) / 2.0;
        assert!(hit::terrain(&w, [hole, 1000.0, GROUND + 10.0], [hole, 1000.0, GROUND - 6.0]).is_none(), "down through the hole");
        assert!(hit::terrain(&w, [x, 1000.0, GROUND + 10.0], [x, 1000.0, GROUND - 6.0]).is_some(), "into the ground");
        assert!(hit::terrain(&w, [x - 20.0, 1000.0, TUNNEL_FLOOR + 1.5], [x + 20.0, 1000.0, TUNNEL_FLOOR + 1.5]).is_none(), "along the tube");
    }

    #[test]
    fn lifts_lift_drops_slow_springs_throw() {
        let w = kit();
        let (tx, ty, top) = TOWER;
        // Into the lift's column, then onto the tower's top.
        let mut s = MoveState::standing(&w, [LIFT_X[1] + 3.0, ty[0] + 2.0]);
        s = walk(&w, s, [(LIFT_X[0] + LIFT_X[1]) / 2.0, ty[0] + 2.0], 0, 20, 0);
        let mut t = 0;
        while s.z < top + 1.0 && t < 30 * 10 {
            s = step(&w, s, Input::default(), 0);
            t += 1;
        }
        assert!(s.z >= top + 1.0, "lifted: {s:?}");
        assert!((t as f32) / 30.0 < 40.0 / 12.0 + 1.0, "at about its speed: {t} steps");
        s = walk(&w, s, [tx[1] - 3.0, ty[0] + 2.0], 0, 90, 0);
        assert!(s.grounded && s.z == top, "on the tower's top: {s:?}");
        // Off its far edge into the drop shaft: down at its speed, landed.
        let mut fell = 0;
        while s.grounded && fell < 120 {
            s = walk(&w, s, [DROP_X[0] + 2.0, ty[0] + 2.0], 0, 1, 0);
            fell += 1;
        }
        fell = 0;
        for _ in 0..30 * 12 {
            s = walk(&w, s, [DROP_X[0] + 2.0, ty[0] + 2.0], 0, 1, 0);
            assert!(s.vz >= -5.0 - 1e-4, "slowed: {s:?}");
            fell += 1;
            if s.grounded {
                break;
            }
        }
        assert!(s.grounded && s.z == GROUND && fell > 30 * 7, "down slowly: {fell} steps, {s:?}");
        // The spring: thrown up and along.
        let (pad, vel) = SPRING;
        let mut s = walk(&w, MoveState::standing(&w, [pad[0] - 3.0, 1000.0]), [pad[0] + 1.0, 1000.0], 0, 25, 0);
        // (Holding forward: in the air, the stick steers as ever.)
        let mut peak = s.z;
        for _ in 0..60 {
            s = step(&w, s, Input { move_x: 127, ..Default::default() }, 0);
            peak = peak.max(s.z);
        }
        assert!(peak > GROUND + vel[2] * vel[2] / 30.0 - 0.5, "thrown up: {peak}");
        assert!(s.pos[0] > pad[1] + 8.0 && s.grounded, "and along: {s:?}");
    }

    #[test]
    fn the_kit_replays_bit_exact() {
        let w = kit();
        let run = || {
            let mut s = MoveState::standing(&w, [990.0, 1000.0]);
            let mut h = 0u64;
            for i in 0..3000u32 {
                let a = i as f32 * 0.013;
                let input = Input {
                    move_x: (libm::cosf(a) * 127.0) as i8,
                    move_y: (libm::sinf(a * 0.7) * 40.0) as i8,
                    buttons: if i % 40 < 3 { BUTTON_JUMP } else if i % 90 < 20 { BUTTON_CROUCH } else { BUTTON_SPRINT },
                    ..Default::default()
                };
                s = step(&w, s, input, (i / 500 % 3) as u8);
                h = h.rotate_left(5) ^ s.pos[0].to_bits() as u64 ^ (s.z.to_bits() as u64) << 32;
            }
            h
        };
        assert_eq!(run(), run());
    }
}
