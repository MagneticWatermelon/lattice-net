//! Kiln's sites in rough shapes (K0's "every site as boxes"): each kind's
//! ground (terraces, ramps, pits, mesas cut by slots) and its blockout
//! boxes, its control points, the routes inside it; the cover scattered
//! between sites; and lanes kept clear of boxes.
//!
//! Most boxes are axis-aligned, so buildings face the map's axes; decks and
//! bridges are turned (C1) to cross their rifts square. Climbs inside sites
//! are ramps, stairs of boxes no taller than ~60 m (a cover cell's box
//! budget), or lifts (force volumes, K1) with drop shafts beside them.

use super::land::{self, face_offset, fbm, mesa, mound, pad, Rng, DECK, HOLLOW_FLOOR, SALT, TERRACES, TERRACE_CLIFF};
use crate::world::CELLS;
use super::lanes::{stamp_ramp, Leg, LegKind, Route, ROAD_HALF};
use super::*;
use crate::world::{CoverBox, Force, Kind, Point, Volume};

/// The rift ledges' shelves: on the Crown's wall, and in the outer plains'.
pub(crate) const LEDGE_CCW_Z: f64 = 250.0;
pub(crate) const LEDGE_CW_Z: f64 = 165.0;
/// The citadel's mesa, the slot hold's.
const CITADEL_R: f64 = 78.0;
const CITADEL_TOP: f64 = 380.0;
const SLOT_R: f64 = 88.0;
const SLOT_TOP: f64 = 295.0;
/// The slot hold's three slots, degrees in its sector's frame.
const SLOTS: [f64; 3] = [-38.0, 42.0, 190.0];
/// The citadel's lower yard, degrees in its sector's frame, and how far out.
const YARD_DEG: f64 = 120.0;
const YARD_OUT: f64 = 153.0;
/// A cenote's pit.
const PIT_R: f64 = 55.0;
const PIT_DEPTH: f64 = 85.0;

/// Where a lane from `from` meets a site: its anchor, and a point to pass on
/// the way in (a slot's or a corridor's mouth), if it has one.
pub(crate) fn entry(site: &Plan, from: [f64; 2]) -> ([f64; 2], Option<[f64; 2]>) {
    let c = site.center;
    let f = Frame::new(site.sector.min(2) as usize);
    let axis = Frame::axis_deg(f.k);
    let dir = scale(sub(from, c), 1.0 / dist(from, c));
    let toward = libm::atan2(dir[1], dir[0]) * 180.0 / std::f64::consts::PI;
    let nearest = |angles: &[f64]| {
        angles.iter().copied().fold(angles[0], |b, a| if angle_diff(a + axis, toward) < angle_diff(b + axis, toward) { a } else { b }) + axis
    };
    match site.kind {
        SiteKind::MesaCitadel => (add(c, scale(unit(YARD_DEG + axis), YARD_OUT)), None),
        SiteKind::SlotHold => (c, Some(add(c, scale(unit(nearest(&SLOTS)), SLOT_R + 26.0)))),
        SiteKind::ReefWarren => (c, Some(add(c, scale(unit(nearest(&[0.0, 90.0, 180.0, 270.0])), 72.0)))),
        SiteKind::Cenote => (add(c, scale(dir, PIT_R + 30.0)), None),
        _ => (c, None),
    }
}

fn angle_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// The direction (degrees) from a site's center furthest from all its
/// lanes: the middle of the widest gap between them, where its landmarks
/// and stairs stand out of the lanes' way.
pub(crate) fn free_dir(p: &Plan) -> f64 {
    let mut d: Vec<f64> = p.dirs.iter().map(|a| a.rem_euclid(360.0)).collect();
    if d.is_empty() {
        return Frame::axis_deg(p.sector.min(2) as usize) + 180.0;
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut best = (d[0] + 360.0 - d[d.len() - 1], d[d.len() - 1]);
    for w in d.windows(2) {
        if w[1] - w[0] > best.0 {
            best = (w[1] - w[0], w[0]);
        }
    }
    best.1 + best.0 / 2.0
}

/// The direction (degrees) nearest a site's free one (`free_dir`) in which
/// everything `at(deg)` lays out (footprints: center, radius) keeps 4 m
/// clear of every lane: where its landmarks and stairs go.
fn clear_dir(k: &Kiln, p: &Plan, at: &dyn Fn(f64) -> Vec<([f64; 2], f64)>) -> f64 {
    let fd = free_dir(p);
    for step in 0..36 {
        let deg = fd + if step % 2 == 0 { 1.0 } else { -1.0 } * ((step + 1) / 2) as f64 * 10.0;
        if at(deg).iter().all(|&(q, r)| land::clearance(k, q).0 > r + 4.0) {
            return deg;
        }
    }
    fd
}

/// The axis-aligned unit vector nearest `deg`.
fn axis_near(deg: f64) -> [f64; 2] {
    let u = unit(deg);
    if u[0].abs() >= u[1].abs() {
        [u[0].signum(), 0.0]
    } else {
        [0.0, u[1].signum()]
    }
}

/// A box: a world rectangle from `bottom` to `top`.
fn bx(min: [f64; 2], max: [f64; 2], bottom: f64, top: f64, kind: Kind, facing: u8) -> CoverBox {
    CoverBox::new([min[0] as f32, min[1] as f32], [max[0] as f32, max[1] as f32], bottom as f32, top as f32, kind, facing)
}

/// A box centered at `c`, `half` m along `u` (a unit vector) and across it,
/// from `bottom` to `top`: turned to `u` (C1), unless that's along an axis.
fn oriented(c: [f64; 2], half: [f64; 2], u: [f64; 2], bottom: f64, top: f64, kind: Kind) -> CoverBox {
    if u[1] == 0.0 {
        bx(sub(c, half), add(c, half), bottom, top, kind, 0)
    } else if u[0] == 0.0 {
        let h = [half[1], half[0]];
        bx(sub(c, h), add(c, h), bottom, top, kind, 1)
    } else {
        CoverBox::turned([c[0] as f32, c[1] as f32], [half[0] as f32, half[1] as f32], [u[0] as f32, u[1] as f32], bottom as f32, top as f32, kind, 0)
    }
}

/// A box along the segment `a`-`b`, `half` m to either side of it.
fn strip(a: [f64; 2], b: [f64; 2], half: f64, bottom: f64, top: f64, kind: Kind) -> CoverBox {
    let l = dist(a, b);
    oriented(scale(add(a, b), 0.5), [l / 2.0, half], scale(sub(b, a), 1.0 / l), bottom, top, kind)
}

/// A box centered at `c`, `size` across, standing on the ground (its
/// bottom at the lowest corner, its top `height` above the highest).
fn stand(k: &Kiln, c: [f64; 2], size: [f64; 2], height: f64, kind: Kind, facing: u8) -> CoverBox {
    let (min, max) = (sub(c, scale(size, 0.5)), add(c, scale(size, 0.5)));
    let g = [k.height(min), k.height([max[0], min[1]]), k.height([min[0], max[1]]), k.height(max), k.height(c)];
    let (lo, hi) = g.iter().fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    bx(min, max, lo - 0.3, hi + height, kind, facing)
}

/// A wall from `a` to `b` (axis-aligned: they share an x or a y).
fn wall(k: &Kiln, a: [f64; 2], b: [f64; 2], thick: f64, height: f64, kind: Kind) -> CoverBox {
    let (min, max) = ([a[0].min(b[0]) - thick / 2.0, a[1].min(b[1]) - thick / 2.0], [a[0].max(b[0]) + thick / 2.0, a[1].max(b[1]) + thick / 2.0]);
    let c = scale(add(min, max), 0.5);
    let facing = if (b[0] - a[0]).abs() > (b[1] - a[1]).abs() { 1 } else { 0 };
    stand(k, c, sub(max, min), height, kind, facing)
}

/// Walls round a square of half-side `half` at `c`, in pieces (lanes cut gates).
fn walls_round(k: &Kiln, c: [f64; 2], half: f64, height: f64, out: &mut Vec<CoverBox>) {
    let piece = 12.0;
    let n = (2.0 * half / piece).round() as usize;
    for i in 0..n {
        let (t0, t1) = (-half + i as f64 * piece, -half + (i + 1) as f64 * piece);
        for (a, b) in [
            ([c[0] + t0, c[1] - half], [c[0] + t1, c[1] - half]),
            ([c[0] + t0, c[1] + half], [c[0] + t1, c[1] + half]),
            ([c[0] - half, c[1] + t0], [c[0] - half, c[1] + t1]),
            ([c[0] + half, c[1] + t0], [c[0] + half, c[1] + t1]),
        ] {
            out.push(wall(k, a, b, 1.2, height, Kind::Block));
        }
    }
}

/// A square spiral stair of boxes in the `side`-m square at `min`, from `z0`
/// up to `z1` or a little past it (whole flights), around a core: risers of
/// 0.4 m (`STEP_UP` is 0.45), treads of 0.6, four flights a turn. Its boxes
/// (fixed), and the path up it with its heights.
fn stairs(min: [f64; 2], side: f64, z0: f64, z1: f64, kind: Kind, out: &mut Vec<CoverBox>) -> (Vec<[f64; 2]>, Vec<f64>) {
    let w = 2.4;
    let steps = ((side - 2.0 * w) / 0.6).floor() as usize;
    let tread = (side - 2.0 * w) / steps as f64;
    let mut z = z0;
    let mut path = vec![[min[0] - 1.5, min[1] + w / 2.0]];
    let mut zs = vec![z0];
    let mut turn = 0;
    // Each flight's start corner and direction, counterclockwise from the south-west.
    let corners = [([min[0], min[1]], [1.0, 0.0]), ([min[0] + side - w, min[1]], [0.0, 1.0]), ([min[0] + side - w, min[1] + side - w], [-1.0, 0.0]), ([min[0], min[1] + side - w], [0.0, -1.0])];
    let mut first = true;
    while z < z1 - 0.2 {
        let (corner, d) = corners[turn % 4];
        // The landing in this corner (none under the first flight's start: it starts at the ground).
        if !first {
            out.push(bx(corner, add(corner, [w, w]), z - 0.4, z, kind, 0));
        }
        first = false;
        let start = add(corner, [w / 2.0, w / 2.0]);
        path.push(start);
        zs.push(z);
        // The flight runs from the corner square's far edge along `d`.
        let edge = if d[0] + d[1] > 0.0 { add(corner, scale(d, w)) } else { corner };
        for s in 0..steps {
            z += 0.4;
            let a = add(edge, scale(d, s as f64 * tread));
            let b = add(a, scale(d, tread));
            // The tread spans the flight's width, across its direction.
            let across = if d[0] != 0.0 { [0.0, w] } else { [w, 0.0] };
            let (p, q) = (a, add(b, across));
            out.push(bx([p[0].min(q[0]), p[1].min(q[1])], [p[0].max(q[0]), p[1].max(q[1])], z - 0.4, z, kind, 0));
        }
        turn += 1;
    }
    let (corner, _) = corners[turn % 4];
    out.push(bx(corner, add(corner, [w, w]), z - 0.4, z, kind, 0));
    path.push(add(corner, [w / 2.0, w / 2.0]));
    zs.push(z);
    // The core.
    out.push(bx(add(min, [w, w]), sub(add(min, [side, side]), [w, w]), z0 - 0.5, z, kind, 0));
    (path, zs)
}

#[cfg(test)]
pub(crate) fn test_stairs(min: [f64; 2], side: f64, z0: f64, z1: f64, out: &mut Vec<CoverBox>) -> (Vec<[f64; 2]>, Vec<f64>) {
    stairs(min, side, z0, z1, Kind::Block, out)
}

/// `stairs` in the `side` square centered at `c`, their entry on the side
/// facing `toward` (turned by quarter turns), on ground flattened to their
/// foot's height. Their path starts at the entry.
#[allow(clippy::too_many_arguments)]
fn stairs_at(k: &mut Kiln, c: [f64; 2], side: f64, z0: f64, z1: f64, toward: [f64; 2], kind: Kind, out: &mut Vec<CoverBox>) -> (Vec<[f64; 2]>, Vec<f64>) {
    pad(k, c, side * 0.75, z0, 8.0);
    let mut boxes = Vec::new();
    let (path, zs) = stairs(sub(c, [side / 2.0, side / 2.0]), side, z0, z1, kind, &mut boxes);
    // Quarter turns counterclockwise that take west (the entry) toward `toward`.
    let e = axis_near(libm::atan2(toward[1] - c[1], toward[0] - c[0]) * 180.0 / std::f64::consts::PI);
    let turns = if e[0] < 0.0 { 0 } else if e[1] < 0.0 { 1 } else if e[0] > 0.0 { 2 } else { 3 };
    let turn = |p: [f64; 2]| {
        let mut v = sub(p, c);
        for _ in 0..turns {
            v = [-v[1], v[0]];
        }
        add(c, v)
    };
    for b in boxes {
        let (p, q) = (turn([b.min[0] as f64, b.min[1] as f64]), turn([b.max[0] as f64, b.max[1] as f64]));
        out.push(bx([p[0].min(q[0]), p[1].min(q[1])], [p[0].max(q[0]), p[1].max(q[1])], b.bottom as f64, b.top as f64, b.kind, b.facing));
    }
    (path.into_iter().map(turn).collect(), zs)
}

/// A lift's speed up (m/s), and a short one's or its top's: slow enough
/// that a rider leaves it bobbing just over where it goes.
const LIFT_FAST: f64 = 20.0;
const LIFT_TALL: f64 = 30.0;
const LIFT_SLOW: f64 = 4.0;
/// A drop shaft's fall (m/s).
const DROP_SPEED: f64 = 6.0;

/// A lift (K1): an updraft in the square column of half-side `half` at `c`,
/// from its foot at `z0` to `z1` (the deck or ledge it serves): fast, then
/// slowing over its last stretch so a rider bobs just over `z1` and steps
/// off; and a one-way route for `site` from `from` (on the ground by its
/// foot) up it to `to` (on top). Tall lifts (over 200 m) run faster.
#[allow(clippy::too_many_arguments)]
fn lift_up(k: &mut Kiln, site: usize, c: [f64; 2], half: f64, z0: f64, z1: f64, from: [f64; 2], to: [f64; 2]) {
    let fast = if z1 - z0 > 200.0 { LIFT_TALL } else { LIFT_FAST };
    // Room to slow from `fast` to `LIFT_SLOW` at gravity's rate, and a little.
    let slow = (fast * fast - LIFT_SLOW * LIFT_SLOW) / (2.0 * 15.0) + 3.0;
    let column = |lo: f64, hi: f64, v: f64| Volume {
        min: [(c[0] - half) as f32, (c[1] - half) as f32, lo as f32],
        max: [(c[0] + half) as f32, (c[1] + half) as f32, hi as f32],
        force: Force::Lift,
        vel: [0.0, 0.0, v as f32],
    };
    if z1 - z0 > slow + 4.0 {
        k.volumes.push(column(z0 - 1.0, z1 - slow, fast));
        k.volumes.push(column(z1 - slow, z1 + 1.5, LIFT_SLOW));
    } else {
        k.volumes.push(column(z0 - 1.0, z1 + 1.5, LIFT_SLOW.max(6.0)));
    }
    // Walked: to just outside its foot, into it (and held there, carried),
    // then off at the top.
    let out = add(c, scale(sub(from, c), (half + 1.5) / dist(from, c)));
    let legs = vec![
        Leg { kind: LegKind::Road, path: vec![from, out], z: None },
        Leg { kind: LegKind::Road, path: vec![out, c, to], z: Some(vec![z0, z1 + 1.0, z1]) },
    ];
    k.internal.push(Route { a: site, b: site, legs, one_way: true });
}

/// A drop shaft (K1): a column of half-side `half` at `c` that slows a fall
/// to `DROP_SPEED`, from over `z0` (where it's stepped into) down to `z1`;
/// and a one-way route for `site` from `from` (up top) down it to `to`.
/// `edge` is where the top's floor ends beside it, stepped off from.
#[allow(clippy::too_many_arguments)]
fn drop_down(k: &mut Kiln, site: usize, c: [f64; 2], half: f64, z0: f64, z1: f64, from: [f64; 2], edge: [f64; 2], to: [f64; 2]) {
    k.volumes.push(Volume {
        min: [(c[0] - half) as f32, (c[1] - half) as f32, (z1 - 1.0) as f32],
        max: [(c[0] + half) as f32, (c[1] + half) as f32, (z0 + 3.0) as f32],
        force: Force::Drop,
        vel: [0.0, 0.0, -DROP_SPEED as f32],
    });
    let legs = vec![Leg { kind: LegKind::Road, path: vec![from, edge, c, to], z: Some(vec![z0, z0, z1, z1]) }];
    k.internal.push(Route { a: site, b: site, legs, one_way: true });
}

/// A Tunnel mouth's way under the ground (K1): its trench's head on the
/// surface, its axis, the chamber its ramp comes down into.
#[derive(Debug, Clone)]
pub(crate) struct Chamber {
    pub site: usize,
    /// The trench's head (on a cell's edge) and its axis (along a world axis).
    pub top: [f64; 2],
    pub e: [f64; 2],
    /// The ramp's foot, the chamber's center (a 16 m square past it).
    pub foot: [f64; 2],
    pub center: [f64; 2],
    /// The ground's level round the mouth, and the chamber's floor.
    pub surface: f64,
    pub floor: f64,
}

/// The trench's length (10 cells: a 12 m drop at 0.3), the chamber's half-side
/// and how far its floor is under the ground; how far under it a tube's floor
/// stays (its ceiling's top 2.5 m under it, at the least).
const TRENCH: f64 = 40.0;
const CHAMBER: f64 = 8.0;
const DEPTH: f64 = 12.0;
const TUBE_DEPTH: f64 = 8.0;
/// A tube's half-width inside its walls, its headroom, its steepest grade.
const TUBE: f64 = 3.0;
const TUBE_HIGH: f64 = 4.5;
const TUBE_GRADE: f64 = 0.25;

/// Digs site `i`'s Tunnel mouth: the ground round it leveled, holes in it
/// over the trench, the ramp down and its walls, the chamber's floor and
/// ceiling (its walls wait for the tube, `tunnels`).
fn mouth(k: &mut Kiln, i: usize) -> Chamber {
    let (top, e) = (k.sites[i].anchors[0], sub(k.sites[i].anchors[1], k.sites[i].anchors[0]));
    let n = [-e[1], e[0]];
    let foot = add(top, scale(e, TRENCH));
    let center = add(foot, scale(e, CHAMBER));
    let surface = k.mean(add(top, scale(e, TRENCH / 2.0)), 26.0);
    let corners = [sub(sub(top, scale(e, 4.0)), scale(n, 12.0)), add(add(center, scale(e, CHAMBER + 4.0)), scale(n, 12.0))];
    let (min, max) = ([corners[0][0].min(corners[1][0]), corners[0][1].min(corners[1][1])], [corners[0][0].max(corners[1][0]), corners[0][1].max(corners[1][1])]);
    land::pad_rect(k, min, max, surface, 24.0);
    let floor = surface - DEPTH;
    // The holes: the trench's cells (two across, ten along).
    if k.holes.is_empty() {
        k.holes = vec![0; (CELLS * CELLS).div_ceil(64)];
    }
    let res = TERRAIN_RES as f64;
    for a in 0..(TRENCH / res) as usize {
        for b in [-1.0, 0.0] {
            let q = add(add(top, scale(e, (a as f64 + 0.5) * res)), scale(n, (b + 0.5) * res));
            let (cx, cy) = ((q[0] / res).floor() as usize, (q[1] / res).floor() as usize);
            let j = cy * CELLS + cx;
            k.holes[j / 64] |= 1 << (j % 64);
        }
    }
    let mid = scale(add(top, foot), 0.5);
    let ramp = CoverBox::turned([mid[0] as f32, mid[1] as f32], [(TRENCH / 2.0) as f32, 4.0], [-e[0] as f32, -e[1] as f32], (floor - 1.0) as f32, surface as f32, Kind::Block, 0);
    k.fixed.push(ramp.wedge(floor as f32));
    for side in [-1.0, 1.0] {
        k.fixed.push(strip(add(top, scale(n, side * 4.3)), add(foot, scale(n, side * 4.3)), 0.3, floor - 1.0, surface, Kind::Block));
    }
    k.fixed.push(oriented(center, [CHAMBER, CHAMBER], e, floor - 1.0, floor, Kind::Block));
    k.fixed.push(oriented(center, [CHAMBER + 0.6, CHAMBER + 0.6], e, floor + 5.0, floor + 5.6, Kind::Block));
    Chamber { site: i, top, e, foot, center, surface, floor }
}

/// The tube between each sector's two Tunnel mouths (K1, the Undervein's
/// first): 6 m wide, its floor following the land at least `TUBE_DEPTH` under it
/// (no steeper than `TUBE_GRADE`), in 8 m segments of turned boxes (floor,
/// walls, ceiling); the chambers' walls, open to the trench and the tube;
/// and its lane, from one outpost to the other.
pub(crate) fn tunnels(k: &mut Kiln) {
    for s in 0..3 {
        let find = |site: usize| k.chambers.iter().find(|c| c.site == site).cloned().expect("a Tunnel mouth's chamber");
        let (a, b) = (find(super::site(s, s::O + 5)), find(super::site(s, s::O + 11)));
        tube(k, &a, &b);
    }
}

fn tube(k: &mut Kiln, a: &Chamber, b: &Chamber) {
    let l = dist(a.center, b.center);
    let u = scale(sub(b.center, a.center), 1.0 / l);
    let v = [-u[1], u[0]];
    // From a meter inside one chamber's wall to a meter inside the other's.
    let exit = |d: [f64; 2]| CHAMBER / d[0].abs().max(d[1].abs());
    let (s0, s1) = (exit(u) - 1.0, l - exit(u) + 1.0);
    let n = ((s1 - s0) / 8.0).ceil() as usize;
    let at = |j: usize| add(a.center, scale(u, s0 + (s1 - s0) * j as f64 / n as f64));
    // The ground over each point (its lowest across the tube's width), and
    // the floor: level from one chamber's floor to the other's, but dipped
    // where the ground does (keeping TUBE_DEPTH under it), no steeper than
    // TUBE_GRADE.
    let ground: Vec<f64> = (0..=n)
        .map(|j| {
            let p = at(j);
            [-4.0, 0.0, 4.0].iter().flat_map(|&x| [-4.0, 0.0, 4.0].map(|y| k.height(add(add(p, scale(v, x)), scale(u, y))))).fold(f64::MAX, f64::min)
        })
        .collect();
    let step = (s1 - s0) / n as f64;
    let floor: Vec<f64> = (0..=n)
        .map(|j| {
            let cone = (0..=n).map(|i| ground[i] - TUBE_DEPTH + TUBE_GRADE * step * (i as f64 - j as f64).abs()).fold(f64::MAX, f64::min);
            cone.min(lerp(a.floor, b.floor, j as f64 / n as f64))
        })
        .collect();
    for j in 0..n {
        let (p, q) = (at(j), at(j + 1));
        let (z0, z1) = (floor[j], floor[j + 1]);
        let (lo, hi) = (z0.min(z1), z0.max(z1));
        let mid = scale(add(p, q), 0.5);
        if hi - lo < 1e-6 {
            k.fixed.push(strip(p, q, TUBE, lo - 1.0, lo, Kind::Block));
        } else {
            // A wedge rising toward its higher end.
            let up = if z1 > z0 { u } else { scale(u, -1.0) };
            let w = CoverBox::turned([mid[0] as f32, mid[1] as f32], [(step / 2.0 + 0.05) as f32, TUBE as f32], [up[0] as f32, up[1] as f32], (lo - 1.0) as f32, hi as f32, Kind::Block, 0);
            k.fixed.push(w.wedge(lo as f32));
        }
        for side in [-1.0, 1.0] {
            let off = scale(v, side * (TUBE + 0.3));
            k.fixed.push(strip(sub(add(p, off), scale(u, 0.3)), add(add(q, off), scale(u, 0.3)), 0.3, lo - 1.0, hi + TUBE_HIGH + 0.6, Kind::Block));
        }
        k.fixed.push(strip(sub(p, scale(u, 0.3)), add(q, scale(u, 0.3)), TUBE + 0.6, hi + TUBE_HIGH, hi + TUBE_HIGH + 0.6, Kind::Block));
    }
    for (c, out) in [(a, u), (b, scale(u, -1.0))] {
        chamber_walls(k, c, out);
    }
    // The lane: the outposts' centers, down one trench, along the tube, up the other.
    let (ca, cb) = (k.sites[a.site].center, k.sites[b.site].center);
    let mut path = vec![a.top, a.foot, a.center];
    let mut z = vec![a.surface, a.floor, a.floor];
    for (j, &f) in floor.iter().enumerate() {
        path.push(at(j));
        z.push(f);
    }
    path.extend([b.center, b.foot, b.top]);
    z.extend([b.floor, b.floor, b.surface]);
    let legs = vec![
        Leg { kind: LegKind::Road, path: vec![ca, a.top], z: None },
        Leg { kind: LegKind::Road, path, z: Some(z) },
        Leg { kind: LegKind::Road, path: vec![b.top, cb], z: None },
    ];
    k.tunnels.push(Route { a: a.site, b: b.site, legs, one_way: false });
}

/// A chamber's walls, open to its trench and to its tube (leaving along `out`).
fn chamber_walls(k: &mut Kiln, c: &Chamber, out: [f64; 2]) {
    let n = [-c.e[1], c.e[0]];
    for normal in [c.e, scale(c.e, -1.0), n, scale(n, -1.0)] {
        let along = [-normal[1], normal[0]];
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        if dot(normal, c.e) < -0.5 {
            gaps.push((-4.0, 4.0)); // the trench
        }
        let o = dot(out, normal);
        if o >= out[0].abs().max(out[1].abs()) - 1e-9 && o > 0.0 {
            // Where the tube crosses this side: its walls' outer faces, and a little.
            let x = dot(scale(out, CHAMBER / o), along);
            let w = (TUBE + 0.6) / o + 0.3;
            gaps.push((x - w, x + w));
        }
        gaps.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());
        let mut from = -CHAMBER - 0.6;
        let wall = |k: &mut Kiln, f: f64, t: f64| {
            if t - f > 0.2 {
                let side = add(c.center, scale(normal, CHAMBER + 0.3));
                k.fixed.push(strip(add(side, scale(along, f)), add(side, scale(along, t)), 0.3, c.floor - 1.0, c.floor + 5.6, Kind::Block));
            }
        };
        for (g0, g1) in gaps {
            wall(k, from, g0);
            from = from.max(g1);
        }
        wall(k, from, CHAMBER + 0.6);
    }
}

/// Keeps scattered cover off the ground within `r` of `c` (a lift's foot).
fn keep_clear(k: &mut Kiln, c: [f64; 2], r: f64) {
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [r, r]), add(c, [r, r]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            if dist(Kiln::pos(ix, iy), c) <= r {
                k.mask[iy * TERRAIN_N + ix] |= MASK_CLEAR;
            }
        }
    }
}

/// Sets the land along `path` to the heights `z` at its points (a road's
/// width, narrow shoulders): ramps with level runs at their ends.
fn stamp_levels(k: &mut Kiln, path: &[[f64; 2]], z: &[f64]) {
    let mut pts = vec![path[0]];
    let mut zs = vec![z[0]];
    for j in 0..path.len() - 1 {
        let n = (dist(path[j], path[j + 1]) / 2.0).ceil().max(1.0) as usize;
        for i in 1..=n {
            let t = i as f64 / n as f64;
            pts.push(add(path[j], scale(sub(path[j + 1], path[j]), t)));
            zs.push(lerp(z[j], z[j + 1], t));
        }
    }
    super::lanes::stamp_path(k, &pts, &zs, ROAD_HALF, super::lanes::SHOULDER);
}

/// Pushes an internal route's legs (already stamped) for walking.
fn internal(k: &mut Kiln, site: usize, legs: Vec<Vec<[f64; 2]>>) {
    let legs = legs.into_iter().map(|path| Leg { kind: LegKind::Road, path, z: None }).collect();
    k.internal.push(Route { a: site, b: site, legs, one_way: false });
}

/// An internal route that ends up a stair (its heights its steps').
fn internal_up(k: &mut Kiln, site: usize, legs: Vec<Vec<[f64; 2]>>, stair: (Vec<[f64; 2]>, Vec<f64>)) {
    let mut legs: Vec<Leg> = legs.into_iter().map(|path| Leg { kind: LegKind::Road, path, z: None }).collect();
    legs.push(Leg { kind: LegKind::Road, path: stair.0, z: Some(stair.1) });
    k.internal.push(Route { a: site, b: site, legs, one_way: false });
}

/// A ramp hugging a round face around `c`: from angle `a0` to `a1` (degrees)
/// and height `z0` to `z1`, at the face's height (`face_offset`) outside
/// radius `r`. Stamped; returns its path.
#[allow(clippy::too_many_arguments)]
fn spiral(k: &mut Kiln, c: [f64; 2], r: f64, top: f64, ground: f64, a0: f64, a1: f64, z0: f64, z1: f64) -> Vec<[f64; 2]> {
    let n = ((a1 - a0).abs() / 3.0).ceil().max(2.0) as usize;
    let path: Vec<[f64; 2]> = (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let z = lerp(z0, z1, t);
            add(c, scale(unit(lerp(a0, a1, t)), r + face_offset(z, top, ground) + 3.0))
        })
        .collect();
    stamp_ramp(k, &path, z0, z1);
    path
}

/// Shapes site `i`'s ground (and stamps the ramps inside it); returns its level.
pub(crate) fn shape_site(k: &mut Kiln, i: usize) -> f64 {
    let p = k.sites[i].clone();
    let c = p.center;
    let f = Frame::new(p.sector.min(2) as usize);
    let axis = Frame::axis_deg(f.k);
    match p.kind {
        SiteKind::Span => {
            // Both bridgeheads raised to the deck: 90 m squares at the rift's edge.
            for head in [p.anchors[0], p.anchors[1]] {
                // Sheer on every side: its ramps cut down into it.
                land::pad_rect(k, sub(head, [45.0, 45.0]), add(head, [45.0, 45.0]), DECK, 0.0);
            }
            DECK
        }
        SiteKind::Cliffhold => cliffhold(k, i),
        SiteKind::RiftLedge => {
            let ccw = p.sector != NO_SECTOR && dist(c, f.ccw_rift().at(LEDGE_CCW_ALONG, 0.0)) < 400.0;
            let (rift, side, along, z) = if ccw { (f.ccw_rift(), -1.0, LEDGE_CCW_ALONG, LEDGE_CCW_Z) } else { (f.cw_rift(), 1.0, LEDGE_CW_ALONG, LEDGE_CW_Z) };
            let w = Rift::half_width(along);
            let (x0, x1, y0, y1) = Kiln::samples(sub(c, [140.0, 140.0]), add(c, [140.0, 140.0]));
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    let (a, x) = rift.coords(Kiln::pos(ix, iy));
                    let d = side * x;
                    if (a - along).abs() <= LEDGE_HALF && d >= w - 40.0 && d <= w + 45.0 {
                        let j = iy * TERRAIN_N + ix;
                        k.h[j] = z;
                        k.mask[j] |= MASK_PAD | MASK_CLEAR;
                    }
                }
            }
            z
        }
        SiteKind::Geode => geode(k, i),
        SiteKind::MesaCitadel => {
            let g = k.mean(c, CITADEL_R + 120.0);
            let radius = |a: f64| CITADEL_R * (1.0 + 0.05 * libm::sin(3.0 * a + 0.7));
            mesa(k, c, &radius, CITADEL_TOP, CITADEL_R * 1.1, 0.3);
            let yard = add(c, scale(unit(YARD_DEG + axis), YARD_OUT));
            pad(k, yard, 40.0, g, 20.0);
            pad(k, c, CITADEL_R - 14.0, CITADEL_TOP, 6.0);
            // The switchback road up its side: from beside the yard round to the top.
            let span = (CITADEL_TOP - g) / RAMP_GRADE / (CITADEL_R + 20.0) * 180.0 / std::f64::consts::PI * 1.05;
            let a0 = YARD_DEG + axis + 22.0;
            let path = spiral(k, c, CITADEL_R * 1.05, CITADEL_TOP, g, a0, a0 + span, g, CITADEL_TOP);
            let top_in = add(c, scale(unit(a0 + span), CITADEL_R - 20.0));
            stamp_levels(k, &[*path.last().unwrap(), top_in], &[CITADEL_TOP, CITADEL_TOP]);
            internal(k, i, vec![vec![yard, path[0]], path.clone(), vec![*path.last().unwrap(), top_in]]);
            k.sites[i].anchors = vec![yard, path[path.len() / 2]];
            CITADEL_TOP
        }
        SiteKind::SlotHold => {
            let g = k.mean(c, SLOT_R + 110.0);
            let radius = |a: f64| SLOT_R * (1.0 + 0.06 * libm::sin(4.0 * a + 1.9));
            mesa(k, c, &radius, SLOT_TOP, SLOT_R * 1.1, 0.4);
            // The slots: cut to the flats, from the knot at the center out through the mesa.
            for &deg in &SLOTS {
                let mouth = add(c, scale(unit(deg + axis), SLOT_R + 30.0));
                cut(k, c, mouth, 8.0, g);
            }
            pad(k, c, 14.0, g, 0.0);
            // A ramp up its face in the widest gap between slots (42 to 190 degrees).
            let rise = SLOT_TOP - g;
            let span = rise / RAMP_GRADE / (SLOT_R + 14.0) * 180.0 / std::f64::consts::PI * 1.05;
            let a0 = 42.0 + axis + 22.0;
            let path = spiral(k, c, SLOT_R * 1.05, SLOT_TOP, g, a0, a0 + span, g, SLOT_TOP);
            stamp_levels(k, &[*path.last().unwrap(), add(c, scale(unit(a0 + span), SLOT_R - 22.0))], &[SLOT_TOP, SLOT_TOP]);
            let mouth = add(c, scale(unit(42.0 + axis), SLOT_R + 26.0));
            let round = add(c, scale(unit((42.0 + axis + a0) / 2.0), SLOT_R + 50.0));
            internal(k, i, vec![vec![c, mouth, round, path[0]], path.clone(), vec![*path.last().unwrap(), add(c, scale(unit(a0 + span), SLOT_R - 22.0))]]);
            g
        }
        SiteKind::Cenote => {
            let g = k.mean(c, PIT_R + 60.0);
            pad(k, c, PIT_R + 45.0, g, 25.0);
            let floor = g - PIT_DEPTH;
            let (x0, x1, y0, y1) = Kiln::samples(sub(c, [PIT_R, PIT_R]), add(c, [PIT_R, PIT_R]));
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    if dist(Kiln::pos(ix, iy), c) < PIT_R {
                        k.h[iy * TERRAIN_N + ix] = floor + 0.6 * fbm(97, Kiln::pos(ix, iy), 12.0);
                    }
                }
            }
            // A ledge winding down its wall: ~310 degrees, beginning on the side facing the axis outward.
            let a0 = axis + 200.0;
            let span = PIT_DEPTH / RAMP_GRADE / (PIT_R - 7.0) * 180.0 / std::f64::consts::PI * 1.04;
            let n = (span / 3.0).ceil() as usize;
            let path: Vec<[f64; 2]> = (0..=n).map(|j| add(c, scale(unit(a0 + span * j as f64 / n as f64), PIT_R - 7.0))).collect();
            let entry = add(c, scale(unit(a0 - 8.0), PIT_R + 30.0));
            let mut full = vec![add(c, scale(unit(a0 - 10.0), PIT_R + 4.0))];
            full.extend(path.iter().copied());
            stamp_ramp(k, &full, g, floor);
            let bottom = add(c, scale(unit(a0 + span + 30.0), 18.0));
            internal(k, i, vec![vec![entry, full[0]], full.clone(), vec![*full.last().unwrap(), bottom]]);
            k.sites[i].anchors = vec![entry, path[n / 2], bottom];
            g
        }
        SiteKind::ReefWarren => {
            let g = SALT;
            pad(k, c, 30.0, g, 10.0);
            let (crest, half) = (g + 55.0, 7.0);
            // Four blocks of reef round a cross of corridors (along the sector's axes).
            let u = unit(axis);
            let v = [-u[1], u[0]];
            let (x0, x1, y0, y1) = Kiln::samples(sub(c, [90.0, 90.0]), add(c, [90.0, 90.0]));
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    let q = sub(Kiln::pos(ix, iy), c);
                    let (a, b) = (dot(q, u).abs(), dot(q, v).abs());
                    // Distance outside a block [half, 58] x [half, 32].
                    let dx = (half - a).max(a - 58.0).max(0.0);
                    let dy = (half - b).max(b - 32.0).max(0.0);
                    let d = len([dx, dy]);
                    let j = iy * TERRAIN_N + ix;
                    let top = crest + 2.0 * fbm(98, Kiln::pos(ix, iy), 15.0);
                    let hm = if d == 0.0 { top } else { top - (top - k.h[j]) * (d / 6.0).min(1.0) };
                    if a > half && b > half {
                        k.h[j] = k.h[j].max(hm);
                    } else if a <= half || b <= half {
                        // The corridors: down to the salt.
                        if a < 58.0 + 6.0 && b < 58.0 + 6.0 {
                            k.h[j] = k.h[j].min(g + 0.3 * fbm(99, Kiln::pos(ix, iy), 10.0)).max(g - 1.0);
                        }
                    }
                }
            }
            crest
        }
        SiteKind::Ossuary => {
            // The burial pit, and a ramp down into it.
            let g = k.sites[i].level.max(k.mean(c, 40.0));
            // Inside the site's flat ground, clear of its lanes.
            let axis = clear_dir(k, &p, &|d| vec![(add(c, scale(unit(d), 32.0)), 20.0), (add(add(c, scale(unit(d), 32.0)), scale(unit(d + 90.0), 34.0)), 8.0)]);
            let pit = add(c, scale(unit(axis), 32.0));
            let (x0, x1, y0, y1) = Kiln::samples(sub(pit, [20.0, 20.0]), add(pit, [20.0, 20.0]));
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    if dist(Kiln::pos(ix, iy), pit) < 18.0 {
                        k.h[iy * TERRAIN_N + ix] = g - 14.0;
                    }
                }
            }
            let (low, top) = (add(pit, scale(unit(axis + 90.0), 8.0)), add(pit, scale(unit(axis + 90.0), 52.0)));
            stamp_levels(k, &[add(pit, scale(unit(axis + 90.0), 2.0)), low, top, add(top, scale(unit(axis + 90.0), 6.0))], &[g - 14.0, g - 14.0, g, g]);
            internal(k, i, vec![vec![c, top, low, pit]]);
            g
        }
        SiteKind::GlassVault => {
            let g = k.sites[i].level.max(k.mean(c, p.half + 30.0));
            mound(k, c, p.half + 8.0, -22.0);
            g
        }
        SiteKind::WellShrine => {
            mound(k, c, 26.0, -7.0);
            k.mean(c, 30.0)
        }
        SiteKind::TunnelMouth => {
            // Its mouth (K1): a trench down through holes in the ground, along
            // whichever axis keeps it furthest from the lanes (axis-aligned,
            // so the holes are whole cells), its top 14 m out on a cell's edge.
            let mut best = ([1.0, 0.0], f64::MIN);
            for e in [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]] {
                let clear = (0..9).map(|j| land::clearance(k, add(c, scale(e, 14.0 + 8.0 * j as f64))).0).fold(f64::MAX, f64::min);
                if clear > best.1 + 1e-9 {
                    best = (e, clear);
                }
            }
            let e = best.0;
            let snap = |v: f64| (v / TERRAIN_RES as f64).round() * TERRAIN_RES as f64;
            let top = add([snap(c[0]), snap(c[1])], scale(e, 16.0));
            k.sites[i].dirs.push(libm::atan2(e[1], e[0]) * 180.0 / std::f64::consts::PI);
            k.sites[i].anchors = vec![top, add(top, e)];
            k.mean(c, 30.0)
        }
        _ => k.mean(c, p.half),
    }
}

/// Cuts a slot `half` m wide from `a` to `b` down to `level`.
fn cut(k: &mut Kiln, a: [f64; 2], b: [f64; 2], half: f64, level: f64) {
    let (min, max) = ([a[0].min(b[0]) - half - 4.0, a[1].min(b[1]) - half - 4.0], [a[0].max(b[0]) + half + 4.0, a[1].max(b[1]) + half + 4.0]);
    let (x0, x1, y0, y1) = Kiln::samples(min, max);
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let (d, _) = seg_dist(Kiln::pos(ix, iy), a, b);
            if d <= half {
                let j = iy * TERRAIN_N + ix;
                k.h[j] = k.h[j].min(level);
                k.mask[j] |= MASK_CLEAR;
            }
        }
    }
}

/// A Cliffhold: seven terraces climbing our wall of the counterclockwise
/// rift, joined by switchback ramps, from the floor gate to the summit.
fn cliffhold(k: &mut Kiln, i: usize) -> f64 {
    let f = Frame::new(k.sites[i].sector as usize);
    let rift = f.ccw_rift();
    let w = Rift::half_width(CLIFFHOLD_ALONG);
    let fl = Rift::floor(CLIFFHOLD_ALONG);
    let summit = k.height(rift.at(CLIFFHOLD_ALONG, -(w + 150.0)));
    let level = |t: usize| fl + (summit - fl) * t as f64 / 7.0;
    let band = 14.0;
    let (lo, hi) = (CLIFFHOLD_ALONG - 140.0, CLIFFHOLD_ALONG + 140.0);
    let c = k.sites[i].center;
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [260.0, 260.0]), add(c, [260.0, 260.0]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let (a, x) = rift.coords(Kiln::pos(ix, iy));
            let d = -x - w;
            if a < lo || a > hi || d < -2.0 || d > 7.0 * band + 30.0 {
                continue;
            }
            if d > 7.0 * band {
                // Behind the top terrace: the plain eased to its level.
                let j = iy * TERRAIN_N + ix;
                k.h[j] = lerp(summit, k.h[j], smoothstep(7.0 * band, 7.0 * band + 30.0, d));
                continue;
            }
            // Terrace t covers d in [band (t-1), band t]: flat but for 4 m steps.
            let t = ((d / band).floor() as usize + 1).min(7);
            let into = d - band * (t - 1) as f64;
            let z = if into < 4.0 && t > 1 { lerp(level(t - 1), level(t), into / 4.0) } else if d < 0.0 { fl } else { level(t) };
            let j = iy * TERRAIN_N + ix;
            k.h[j] = z;
            k.mask[j] |= MASK_PAD;
        }
    }
    // Switchbacks: terrace t to t+1 along terrace t+1, back and forth.
    let run = ramp_len_m(level(1) - level(0));
    let (a_lo, a_hi) = (CLIFFHOLD_ALONG - run / 2.0, CLIFFHOLD_ALONG + run / 2.0);
    let gate = rift.at(CLIFFHOLD_ALONG, -(w - 30.0));
    let mut legs = vec![vec![gate, rift.at(a_lo, -(w - 4.0))]];
    for t in 0..7 {
        let (from, to) = if t % 2 == 0 { (a_lo, a_hi) } else { (a_hi, a_lo) };
        // Along the inner part of terrace t + 1, from the edge of terrace t.
        let d = band * t as f64 + 5.0;
        let path = vec![rift.at(from, -(w + d)), rift.at(to, -(w + d))];
        stamp_ramp_narrow(k, &path, level(t), level(t + 1));
        if t > 0 {
            // Out from the last ramp's top to this one's foot, level.
            let last = *legs.last().unwrap().last().unwrap();
            stamp_levels(k, &[last, path[0], rift.at(from, -(w + d + 4.0))], &[level(t), level(t), level(t)]);
            legs.push(vec![last, path[0]]);
        }
        legs.push(path);
    }
    let keep_anchor = rift.at(a_hi + 30.0, -(w + 7.0 * band + 40.0));
    legs.push(vec![*legs.last().unwrap().last().unwrap(), rift.at(a_hi, -(w + 7.0 * band + 10.0)), keep_anchor]);
    internal(k, i, legs);
    k.sites[i].anchors = vec![keep_anchor, gate];
    summit
}

fn ramp_len_m(rise: f64) -> f64 {
    rise.abs() / RAMP_GRADE
}

/// A ramp the width of a terrace (no shoulders beyond it).
fn stamp_ramp_narrow(k: &mut Kiln, path: &[[f64; 2]], z0: f64, z1: f64) {
    let l = dist(path[0], path[1]);
    let n = (l / 2.0).ceil() as usize;
    let pts: Vec<[f64; 2]> = (0..=n).map(|j| add(path[0], scale(sub(path[1], path[0]), j as f64 / n as f64))).collect();
    let z: Vec<f64> = (0..=n).map(|j| lerp(z0, z1, j as f64 / n as f64)).collect();
    super::lanes::stamp_path(k, &pts, &z, 6.0, 1.5);
}

/// A Geode: a crystal shell 250 m across, its crest 150 m over the flats,
/// its floor sunk 100 m below them; cracks in the shell toward each lane;
/// inside, a terrace at the cracks' level, one halfway down, the floor.
fn geode(k: &mut Kiln, i: usize) -> f64 {
    let c = k.sites[i].center;
    let g = k.mean(c, 180.0);
    let cracks: Vec<f64> = k.sites[i].dirs.clone();
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [160.0, 160.0]), add(c, [160.0, 160.0]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let q = Kiln::pos(ix, iy);
            let v = sub(q, c);
            let r = len(v);
            if r > 156.0 {
                continue;
            }
            let a = libm::atan2(v[1], v[0]) * 180.0 / std::f64::consts::PI;
            let in_crack = cracks.iter().any(|&d| angle_diff(a, d) * std::f64::consts::PI / 180.0 * r < 7.0) && r > 96.0;
            let z = if r < 40.0 {
                g - 100.0
            } else if r < 44.0 {
                lerp(g - 100.0, g - 50.0, (r - 40.0) / 4.0)
            } else if r < 73.0 {
                g - 50.0
            } else if r < 77.0 {
                lerp(g - 50.0, g, (r - 73.0) / 4.0)
            } else if r < 104.0 || in_crack {
                g
            } else if r < 111.0 {
                lerp(g, g + 150.0, (r - 104.0) / 7.0)
            } else if r < 122.0 {
                g + 150.0 + 1.0 * fbm(101, q, 8.0)
            } else {
                let t = (r - 122.0) / 34.0;
                if t >= 1.0 {
                    continue;
                }
                lerp(g + 150.0, k.h[iy * TERRAIN_N + ix].min(g + 2.0), t)
            };
            let j = iy * TERRAIN_N + ix;
            k.h[j] = z;
            if r < 104.0 || in_crack {
                k.mask[j] |= MASK_PAD;
            }
        }
    }
    // Ramps down: the cracks' terrace to the middle one, the middle to the floor.
    let a0 = cracks.first().copied().unwrap_or(0.0) + 25.0;
    let span1 = 50.0 / RAMP_GRADE / 75.0 * 180.0 / std::f64::consts::PI * 1.04;
    let p1: Vec<[f64; 2]> = (0..=40).map(|j| add(c, scale(unit(a0 + span1 * j as f64 / 40.0), 75.0))).collect();
    stamp_ramp(k, &p1, g, g - 50.0);
    let span2 = 50.0 / RAMP_GRADE / 42.0 * 180.0 / std::f64::consts::PI * 1.04;
    let a1 = a0 + span1 + 12.0;
    let p2: Vec<[f64; 2]> = (0..=60).map(|j| add(c, scale(unit(a1 + span2 * j as f64 / 60.0), 42.0))).collect();
    stamp_ramp(k, &p2, g - 50.0, g - 100.0);
    let crack_in = add(c, scale(unit(cracks.first().copied().unwrap_or(0.0)), 92.0));
    let crack_out = add(c, scale(unit(cracks.first().copied().unwrap_or(0.0)), k.sites[i].half + 26.0));
    internal(
        k,
        i,
        vec![
            vec![crack_out, crack_in, add(c, scale(unit(a0 - 6.0), 86.0)), p1[0]],
            p1.clone(),
            vec![*p1.last().unwrap(), add(c, scale(unit(a1), 56.0)), p2[0]],
            p2.clone(),
            vec![*p2.last().unwrap(), add(c, scale(unit(a1 + span2 + 40.0), 16.0))],
        ],
    );
    k.sites[i].anchors = vec![crack_out, p1[20], c];
    g
}

/// The Hollow's routes down from each rim gate, by switchback ramps along
/// the terraces' cliffs, to the floor.
pub(crate) fn hollow(k: &mut Kiln) {
    for s in 0..3 {
        let f = Frame::new(s);
        let mut legs = vec![vec![f.polar(HOLLOW_R + 50.0, 0.0), f.polar(TERRACES[4].0 + 30.0, 0.0)]];
        let mut deg = 0.0;
        let mut dir = 1.0;
        let mut upper = TERRACES[4].1;
        for t in (0..5).rev() {
            let (start, _) = TERRACES[t];
            let lower = if t == 0 { HOLLOW_FLOOR } else { TERRACES[t - 1].1 };
            let r = start + TERRACE_CLIFF / 2.0;
            let span = (upper - lower) / RAMP_GRADE / r * 180.0 / std::f64::consts::PI * 1.04;
            let arc: Vec<[f64; 2]> = (0..=30).map(|j| f.polar(r, deg + dir * span * j as f64 / 30.0)).collect();
            // Level runs onto the ledge above at its start and the one below at its end.
            let mut path = vec![f.polar(start + TERRACE_CLIFF + 10.0, deg)];
            path.extend(arc.iter().copied());
            path.push(f.polar(start - 10.0, deg + dir * span));
            let mut z = vec![upper];
            z.extend((0..=30).map(|j| lerp(upper, lower, j as f64 / 30.0)));
            z.push(lower);
            stamp_levels(k, &path, &z);
            legs.push(vec![*legs.last().unwrap().last().unwrap(), path[0]]);
            legs.push(path);
            deg += dir * span;
            dir = -dir;
            upper = lower;
        }
        legs.push(vec![*legs.last().unwrap().last().unwrap(), f.polar(300.0, deg * 0.6), f.polar(90.0, deg * 0.3)]);
        internal(k, 0, legs);
    }
}

/// Boxes for a bridge from `a` to `b` (turned along it) at deck height `z`:
/// the deck and its rails.
pub(crate) fn bridge(k: &mut Kiln, a: [f64; 2], b: [f64; 2], z: f64) {
    let half = 7.0;
    k.fixed.push(strip(a, b, half, z - 2.5, z, Kind::Block));
    // Rails: a lip each side (falls are a choice), short of the ends.
    let u = scale(sub(b, a), 1.0 / dist(a, b));
    let v = [-u[1], u[0]];
    for side in [-1.0, 1.0] {
        let off = scale(v, side * half);
        k.fixed.push(strip(add(add(a, off), scale(u, 2.0)), sub(add(b, off), scale(u, 2.0)), 0.3, z, z + 1.1, Kind::Block));
    }
}

/// Every site's blockout boxes and control points.
pub(crate) fn sites(k: &mut Kiln) {
    let mut rng = Rng::new(0xB10C);
    for i in 0..k.sites.len() {
        let p = k.sites[i].clone();
        let c = p.center;
        let lvl = p.level;
        let mut out: Vec<CoverBox> = Vec::new();
        let mut fixed: Vec<CoverBox> = Vec::new();
        let mut points: Vec<[f64; 3]> = Vec::new();
        let f = Frame::new(p.sector.min(2) as usize);
        let axis = Frame::axis_deg(f.k);
        let on = |k: &Kiln, q: [f64; 2]| [q[0], q[1], k.height(q)];
        match p.kind {
            SiteKind::Hollow => {
                // The Heartwell: a spire of stacked blocks to 610 m.
                let tiers = [(30.0, 140.0), (24.0, 260.0), (18.0, 380.0), (13.0, 480.0), (9.0, 560.0), (6.0, 600.0), (3.0, 610.0)];
                let mut z = HOLLOW_FLOOR - 1.0;
                for &(h, top) in &tiers {
                    fixed.push(bx(sub(c, [h, h]), add(c, [h, h]), z, top, Kind::Block, 0));
                    z = top;
                }
                // Its gallery near the top, and on each sector's side a
                // walkway out from it to a lift up from the floor and a drop
                // shaft back down (K1).
                fixed.push(bx(sub(c, [16.0, 16.0]), add(c, [16.0, 16.0]), 548.0, 550.0, Kind::Block, 0));
                for s in 0..3 {
                    let u = unit(Frame::axis_deg(s));
                    let t = [-u[1], u[0]];
                    // Out until both shafts clear the spire's foot (30 m half-side).
                    let clear = |q: [f64; 2]| (q[0] - c[0]).abs().max((q[1] - c[1]).abs()) >= 30.0 + 2.5 + 1.0;
                    let at = |d: f64, side: f64| add(add(c, scale(u, d)), scale(t, side));
                    let mut d = 20.0;
                    while !(clear(at(d + 3.0, -4.0)) && clear(at(d + 3.0, 4.0))) {
                        d += 0.5;
                    }
                    fixed.push(strip(at(10.0, 0.0), at(d, 0.0), 7.0, 548.0, 550.0, Kind::Block));
                    let (up, down) = (at(d + 3.0, -4.0), at(d + 3.0, 4.0));
                    let (z_up, z_down) = (k.height(up), k.height(down));
                    lift_up(k, 0, up, 2.5, z_up, 550.0, at(d + 20.0, -4.0), at(d - 5.0, -4.0));
                    drop_down(k, 0, down, 2.5, 550.0, z_down, at(d - 5.0, 4.0), at(d - 0.8, 4.0), at(d + 20.0, 4.0));
                    keep_clear(k, at(d + 10.0, 0.0), 16.0);
                }
                // The spawn room at its foot, and cover on the floor.
                for d in 0..3 {
                    let rift = Rift::new(d);
                    let plaza = rift.at(560.0, 0.0);
                    points.push(on(k, plaza));
                    for j in 0..6 {
                        let q = add(plaza, [rng.range(-50.0, 50.0), rng.range(-50.0, 50.0)]);
                        out.push(stand(k, q, [rng.range(3.0, 8.0), rng.range(1.0, 3.0)], rng.range(1.0, 2.4), Kind::Block, (j % 4) as u8));
                    }
                }
                points.push([c[0] + 40.0, c[1], HOLLOW_FLOOR]);
                points.push([c[0] + 12.5, c[1] + 12.5, 550.0]);
                for s in 0..3 {
                    let fr = Frame::new(s);
                    // A gatehouse each side of each rim gate.
                    for side in [-1.0, 1.0] {
                        let q = fr.polar(HOLLOW_R + 12.0, side * 2.6);
                        out.push(stand(k, q, [10.0, 10.0], 9.0, Kind::Block, 0));
                    }
                }
            }
            SiteKind::Landing => {
                walls_round(k, c, 120.0, 6.0, &mut out);
                let well = add(c, scale(unit(clear_dir(k, &p, &|d| vec![(add(c, scale(unit(d), 50.0)), 19.0)])), 50.0));
                fixed.push(stand(k, well, [26.0, 26.0], 52.0, Kind::Block, 0));
                fixed.push(stand(k, well, [8.0, 8.0], 78.0, Kind::Crystal, 0));
                // The spawn room (C1): walls, a roof, and a door at each end
                // shielded for the faction whose Landing this is (sector k's
                // is faction k's), in the first direction round from the
                // well's far side that keeps clear of lanes and the well.
                let room_dir = (0..24)
                    .map(|j| Frame::axis_deg(p.sector as usize) + 180.0 + if j % 2 == 0 { 1.0 } else { -1.0 } * (j / 2) as f64 * 15.0)
                    .find(|&d| {
                        let q = add(c, scale(unit(d), 36.0));
                        land::clearance(k, q).0 > 24.0 && dist(q, well) > 36.0
                    })
                    .unwrap_or(Frame::axis_deg(p.sector as usize));
                let u = unit(room_dir);
                let v = [-u[1], u[0]];
                let q = add(c, scale(u, 36.0));
                let floor = k.mean(q, 12.0);
                pad(k, q, 14.0, floor, 8.0);
                let (hu, hv, high) = (7.0, 10.0, 5.0);
                let at = |a: f64, b: f64| add(add(q, scale(u, a)), scale(v, b));
                for s in [-1.0, 1.0] {
                    // The ends: a wall each side of the door, the door shielded.
                    fixed.push(strip(at(s * hu, -hv), at(s * hu, -2.0), 0.4, floor - 0.3, floor + high, Kind::Block));
                    fixed.push(strip(at(s * hu, 2.0), at(s * hu, hv), 0.4, floor - 0.3, floor + high, Kind::Block));
                    fixed.push(strip(at(s * hu, -2.0), at(s * hu, 2.0), 0.2, floor - 0.3, floor + high, Kind::Shield).shield(p.sector));
                    // The sides.
                    fixed.push(strip(at(-hu, s * hv), at(hu, s * hv), 0.4, floor - 0.3, floor + high, Kind::Block));
                }
                fixed.push(oriented(q, [hu + 0.4, hv + 0.4], u, floor + high, floor + high + 0.6, Kind::Block));
                keep_clear(k, q, 16.0);
                for j in 0..6 {
                    let q = add(c, scale(unit(axis + 60.0 * j as f64 + 30.0), 70.0));
                    out.push(stand(k, q, [16.0, 16.0], 0.3, Kind::Block, 0));
                    out.push(stand(k, add(q, [11.0, 0.0]), [3.0, 10.0], 4.0, Kind::Block, 0));
                }
            }
            SiteKind::Span => {
                let rift = f.ccw_rift();
                let (cross, _) = rift.crossing();
                // Across the deck (along the rift, outward).
                let v = [-cross[1], cross[0]];
                let (near, far) = rift.crossing_ends(SPAN_ALONG, 6.0);
                let half = 12.0;
                // The deck, the gallery under it (floor, walls with windows).
                fixed.push(strip(near, far, half, DECK - 2.0, DECK, Kind::Block));
                fixed.push(strip(near, far, half - 1.0, DECK - 9.5, DECK - 8.0, Kind::Block));
                let l = dist(near, far);
                let n = (l / 16.0) as usize;
                for j in 0..n {
                    let t0 = j as f64 / n as f64;
                    let t1 = (j as f64 + 0.6) / n as f64;
                    let (a, b) = (add(near, scale(sub(far, near), t0)), add(near, scale(sub(far, near), t1)));
                    for side in [-1.0, 1.0] {
                        let off = scale(v, side * (half - 1.5));
                        fixed.push(strip(add(a, off), add(b, off), 0.5, DECK - 8.0, DECK - 2.0, Kind::Block));
                    }
                }
                // Rails on the deck, with a gap over each pylon's lift.
                let mid = rift.at(SPAN_ALONG, 0.0);
                for side in [-1.0, 1.0] {
                    let off = scale(v, side * half);
                    let (a, b) = (add(near, off), add(far, off));
                    if side < 0.0 {
                        fixed.push(strip(a, b, 0.3, DECK, DECK + 1.1, Kind::Block));
                        continue;
                    }
                    let t = |d: f64| 0.5 + d / l;
                    let mut from = 0.0;
                    for gap in [-70.0, 70.0] {
                        let (g0, g1) = (t(gap - 5.0), t(gap + 12.0));
                        fixed.push(strip(add(a, scale(sub(b, a), from)), add(a, scale(sub(b, a), g0)), 0.3, DECK, DECK + 1.1, Kind::Block));
                        from = g1;
                    }
                    fixed.push(strip(add(a, scale(sub(b, a), from)), b, 0.3, DECK, DECK + 1.1, Kind::Block));
                }
                // Pylons from the floor to the gallery, a lift up each one's
                // outer side to the deck, and a drop shaft beside it down.
                let floor = Rift::floor(SPAN_ALONG);
                for side in [-1.0, 1.0] {
                    let q = add(mid, scale(cross, side * 70.0));
                    fixed.push(oriented(q, [8.0, 8.0], cross, floor - 1.0, DECK - 9.5, Kind::Block));
                    let shaft = add(q, scale(v, half + 3.5));
                    let foot = add(q, scale(v, half + 16.0));
                    let deck = add(q, scale(v, half - 6.0));
                    // (From and to the ground under each, the rift's floor.)
                    let z_up = k.height(shaft);
                    lift_up(k, i, shaft, 2.5, z_up, DECK, foot, deck);
                    let drop = add(add(q, scale(v, half + 3.5)), scale(cross, 7.0));
                    let edge = add(add(q, scale(v, half - 0.8)), scale(cross, 7.0));
                    let z_down = k.height(drop);
                    drop_down(k, i, drop, 2.5, DECK, z_down, add(deck, scale(cross, 7.0)), edge, add(foot, scale(cross, 7.0)));
                    points.push(on(k, add(q, scale(v, -(half + 8.0)))));
                }
                // Two arches over the deck, 150 m: stepped blocks from each end to a crown.
                for (shift, rise) in [(-0.12f64, 150.0f64), (0.12, 135.0)] {
                    let steps = 9;
                    for j in 0..steps {
                        for end in [0.0, 1.0] {
                            let t = (j as f64 + 0.5) / steps as f64 * 0.5;
                            let tt = if end == 0.0 { t + shift.max(0.0) } else { 1.0 - t + shift.min(0.0) };
                            let q = add(near, scale(sub(far, near), tt));
                            let z = DECK + rise * (1.0 - (1.0 - 2.0 * t) * (1.0 - 2.0 * t)).sqrt();
                            for side in [-1.0, 1.0] {
                                let qq = add(q, scale(v, side * (half + 5.0)));
                                fixed.push(oriented(qq, [l / steps as f64 / 4.0 + 1.0, 3.0], cross, z - 8.0, z, Kind::Block));
                            }
                        }
                    }
                    let q = add(near, scale(sub(far, near), 0.5 + shift));
                    fixed.push(oriented(q, [12.0, half + 8.0], cross, DECK + rise - 6.0, DECK + rise, Kind::Block));
                }
                // Gatehouses on both bridgeheads.
                for head in [p.anchors[0], p.anchors[1]] {
                    for side in [-1.0, 1.0] {
                        out.push(stand(k, add(head, scale(v, side * (half + 8.0))), [12.0, 12.0], 10.0, Kind::Block, 0));
                    }
                    points.push([head[0], head[1], DECK]);
                }
                points.push([mid[0], mid[1], DECK - 8.0]);
            }
            SiteKind::Cliffhold => {
                let rift = f.ccw_rift();
                let w = Rift::half_width(CLIFFHOLD_ALONG);
                // The summit keep: a tower 80 m over the rim.
                let keep = rift.at(CLIFFHOLD_ALONG - 90.0, -(w + 98.0 + 22.0));
                fixed.push(stand(k, keep, [22.0, 22.0], 80.0, Kind::Block, 0));
                // The floor gate: a gatehouse and walls at the wall's foot.
                let gate = rift.at(CLIFFHOLD_ALONG, -(w - 30.0));
                for side in [-1.0, 1.0] {
                    out.push(stand(k, rift.at(CLIFFHOLD_ALONG + side * 30.0, -(w - 30.0)), [14.0, 14.0], 9.0, Kind::Block, 0));
                }
                // Parapets along the terraces' edges.
                for t in 1..7 {
                    for j in 0..5 {
                        let a = CLIFFHOLD_ALONG - 120.0 + 60.0 * j as f64;
                        if (a - CLIFFHOLD_ALONG).abs() < 50.0 {
                            continue;
                        }
                        out.push(stand(k, rift.at(a, -(w + 14.0 * t as f64 - 12.5)), [5.0, 5.0], 1.1, Kind::Block, 0));
                    }
                }
                points.push(on(k, gate));
                points.push(on(k, rift.at(CLIFFHOLD_ALONG - 80.0, -(w + 14.0 * 3.5))));
                points.push(on(k, add(keep, [0.0, 16.0])));
                points.push(on(k, rift.at(CLIFFHOLD_ALONG + 90.0, -(w + 14.0 * 1.5))));
            }
            SiteKind::Geode => {
                // Crystals on the terraces, and in each crack's mouth (infantry only).
                for &d in &p.dirs {
                    let u = unit(d);
                    let v = [-u[1], u[0]];
                    let m = add(c, scale(u, 128.0));
                    for side in [-1.0, 1.0] {
                        let q = add(m, scale(v, side * 4.6));
                        let g = k.height(q);
                        fixed.push(bx(sub(q, [2.4, 2.4]), add(q, [2.4, 2.4]), g - 0.5, g + 12.0, Kind::Crystal, 0));
                    }
                }
                // A lift up the shell's inner face from the upper terrace, a
                // walkway over to the crest, a drop shaft beside it (K1): on
                // the side furthest from the lanes.
                let u = unit(free_dir(&p));
                let t = [-u[1], u[0]];
                let at = |r: f64, side: f64| add(add(c, scale(u, r)), scale(t, side));
                let crest = lvl + 151.0;
                fixed.push(strip(at(102.5, 3.5), at(118.0, 3.5), 6.0, crest - 2.0, crest, Kind::Crystal));
                lift_up(k, i, at(98.0, 0.0), 2.5, lvl, crest, at(90.0, 0.0), at(110.0, 0.0));
                drop_down(k, i, at(98.0, 7.0), 2.5, crest, lvl, at(110.0, 7.0), at(103.3, 7.0), at(90.0, 7.0));
                for j in 0..14 {
                    let r = if j % 2 == 0 { rng.range(80.0, 100.0) } else { rng.range(48.0, 68.0) };
                    let q = add(c, scale(unit(rng.range(0.0, 360.0)), r));
                    // (Not in the shafts' way.)
                    if dist(q, at(96.0, 3.5)) > 12.0 {
                        out.push(stand(k, q, [rng.range(2.0, 4.0), rng.range(2.0, 4.0)], rng.range(3.0, 9.0), Kind::Crystal, 0));
                    }
                }
                points.push(on(k, c));
                points.push(on(k, p.anchors[1]));
                let top = at(114.0, 3.5);
                points.push([top[0], top[1], crest]);
                points.push(on(k, add(c, scale(unit(p.dirs.first().copied().unwrap_or(0.0)), 100.0))));
            }
            SiteKind::RiftLedge => {
                // Buildings on the shelf, a tower at its back.
                let big = p.half > 60.0;
                let rift = if dist(c, f.ccw_rift().at(LEDGE_CCW_ALONG, 0.0)) < 400.0 { (f.ccw_rift(), -1.0, LEDGE_CCW_ALONG) } else { (f.cw_rift(), 1.0, LEDGE_CW_ALONG) };
                let (r, side, along) = rift;
                let w = Rift::half_width(along);
                for (da, dd, sx, sy, h) in [(-35.0, 20.0, 18.0, 12.0, 6.0), (30.0, 25.0, 14.0, 14.0, 5.0), (0.0, -25.0, 10.0, 8.0, 4.0)] {
                    out.push(stand(k, r.at(along + da, side * (w + dd)), [sx, sy], h, Kind::Block, 0));
                }
                fixed.push(stand(k, r.at(along, side * (w + 38.0)), [9.0, 9.0], 28.0, Kind::Block, 0));
                // A lift up the wall behind the shelf to the rim, a walkway
                // over the face to it, a drop shaft beside it (K1), between
                // the buildings and the tower: where the shelf's flat ends
                // (the wall's foot) and where the rim's begins, found.
                let (lift_a, drop_a, mid_a) = (along - 13.0, along - 21.0, along - 17.0);
                let at = |a: f64, d: f64| r.at(a, side * (w + d));
                let mut foot = 0.0;
                while foot < 80.0 && [lift_a, mid_a, drop_a].iter().all(|&a| k.height(at(a, foot + 1.0)) < lvl + 0.3) {
                    foot += 1.0;
                }
                let mut rim_d = foot + 4.0;
                while rim_d < foot + 150.0 && !(k.height(at(mid_a, rim_d)) > lvl + 20.0 && (k.height(at(mid_a, rim_d + 4.0)) - k.height(at(mid_a, rim_d))).abs() < 1.5) {
                    rim_d += 1.0;
                }
                rim_d += 6.0;
                let rim = [-6.0, 0.0, 6.0].iter().map(|&o| k.height(at(mid_a + o, rim_d))).fold(f64::MIN, f64::max);
                let col = foot - 4.0;
                fixed.push(strip(at(mid_a, col + 4.8), at(mid_a, rim_d), 7.0, rim - 2.0, rim, Kind::Block));
                lift_up(k, i, at(lift_a, col), 2.5, lvl, rim, at(lift_a, col - 18.0), at(lift_a, col + 12.0));
                drop_down(k, i, at(drop_a, col), 2.5, rim, lvl, at(drop_a, col + 12.0), at(drop_a, col + 5.2), at(drop_a, col - 18.0));
                keep_clear(k, at(mid_a, rim_d), 12.0);
                points.push(on(k, r.at(along - 45.0, side * (w + 5.0))));
                points.push(on(k, r.at(along + 45.0, side * (w + 5.0))));
                if big {
                    points.push(on(k, r.at(along, side * (w - 25.0))));
                }
            }
            SiteKind::MesaCitadel => {
                // Walls on top, the keep, a sniper tower; the lower yard walled.
                walls_round(k, c, 48.0, 4.0, &mut out);
                fixed.push(stand(k, c, [18.0, 18.0], 14.0, Kind::Block, 0));
                // The sniper tower beside where the road arrives on top.
                let top_in = k.internal.iter().rev().find(|r| r.a == i).map(|r| *r.legs.last().unwrap().path.last().unwrap()).unwrap_or(c);
                let a_top = libm::atan2(top_in[1] - c[1], top_in[0] - c[0]) * 180.0 / std::f64::consts::PI;
                let tower = add(c, scale(unit(a_top - 25.0), CITADEL_R - 26.0));
                let stair = stairs_at(k, tower, 9.0, CITADEL_TOP, CITADEL_TOP + 26.0, top_in, Kind::Block, &mut fixed);
                internal_up(k, i, vec![vec![top_in, stair.0[0]]], stair);
                let yard = p.anchors[0];
                walls_round(k, yard, 34.0, 3.0, &mut out);
                points.push(on(k, add(c, [10.0, 10.0])));
                points.push(on(k, yard));
                points.push(on(k, p.anchors[1]));
            }
            SiteKind::SlotHold => {
                // Walkways across the slots on top, rooms on top.
                let top = SLOT_TOP;
                for &deg in &SLOTS {
                    let u = unit(deg + axis);
                    for r in [35.0, 62.0] {
                        let q = add(c, scale(u, r));
                        fixed.push(bx(sub(q, [8.0, 8.0]), add(q, [8.0, 8.0]), top - 1.0, top, Kind::Block, 0));
                    }
                    let room = add(c, scale(unit(deg + axis + 60.0), 55.0));
                    out.push(stand(k, room, [16.0, 12.0], 5.0, Kind::Block, 0));
                }
                points.push([c[0], c[1], lvl]);
                points.push(on(k, add(c, scale(unit(axis + 116.0), 50.0))));
                points.push(on(k, add(c, scale(unit(axis - 100.0), 50.0))));
            }
            SiteKind::Cenote => {
                // Rings of buildings round the mouth.
                for j in 0..10 {
                    let a = axis + 36.0 * j as f64 + 18.0;
                    let q = add(c, scale(unit(a), PIT_R + 40.0));
                    out.push(stand(k, q, [14.0, 9.0], 5.0 + (j % 3) as f64 * 2.0, Kind::Block, 0));
                }
                points.push(on(k, add(c, scale(unit(axis + 90.0), PIT_R + 20.0))));
                points.push(on(k, p.anchors[1]));
                points.push(on(k, p.anchors[2]));
            }
            SiteKind::ReefWarren => {
                // A tower on the reef's crest, reached by stairs from the corridor.
                let u = unit(axis);
                let v = [-u[1], u[0]];
                let mouth = add(c, scale(u, 70.0));
                let foot = add(mouth, scale(v, 20.0));
                let stair = stairs_at(k, foot, 9.0, SALT, SALT + 55.0, mouth, Kind::Block, &mut fixed);
                internal_up(k, i, vec![vec![c, mouth, stair.0[0]]], stair);
                let tower = add(c, add(scale(u, 40.0), scale(v, 22.0)));
                fixed.push(bx(sub(tower, [5.0, 5.0]), add(tower, [5.0, 5.0]), SALT + 54.0, SALT + 70.0, Kind::Block, 0));
                for j in 0..6 {
                    let q = add(c, scale(if j % 2 == 0 { u } else { v }, if j < 3 { 40.0 } else { -40.0 }));
                    out.push(stand(k, q, [5.0, 5.0], 2.5, Kind::Block, 0));
                }
                points.push([c[0], c[1], SALT]);
                points.push([tower[0], tower[1] - 8.0, SALT + 55.0]);
                points.push(on(k, add(c, scale(v, -45.0))));
            }
            SiteKind::Ossuary => {
                // A colossal skeleton across the site: spine, ribs, skull.
                let u = unit(axis + 90.0);
                let v = [-u[1], u[0]];
                let along_x = u[0].abs() > u[1].abs();
                let (ua, va) = if along_x { ([u[0].signum(), 0.0], [0.0, 1.0]) } else { ([0.0, u[1].signum()], [1.0, 0.0]) };
                let _ = v;
                let spine_a = sub(c, scale(ua, 70.0));
                let spine_b = add(c, scale(ua, 60.0));
                let (smin, smax) = rect(spine_a, spine_b, 3.0, along_x);
                fixed.push(bx(smin, smax, lvl + 26.0, lvl + 32.0, Kind::Bone, 0));
                for j in 0..8 {
                    let t = -55.0 + 15.0 * j as f64;
                    let base = add(c, scale(ua, t));
                    let tall = 28.0 - (j as f64 - 3.5).abs() * 2.0;
                    for side in [-1.0, 1.0] {
                        let q = add(base, scale(va, side * 24.0));
                        out.push(bx(sub(q, [1.6, 1.6]), add(q, [1.6, 1.6]), lvl - 1.0, lvl + tall * 0.7, Kind::Bone, 0));
                        let q2 = add(base, scale(va, side * 15.0));
                        fixed.push(bx(sub(q2, [1.6, 1.6]), add(q2, [1.6, 1.6]), lvl + tall * 0.7, lvl + tall, Kind::Bone, 0));
                    }
                }
                // The skull: a fort of bone at the spine's head.
                let skull = add(spine_b, scale(ua, 18.0));
                walls_round(k, skull, 14.0, 9.0, &mut out);
                for b in out.iter_mut().rev().take(12) {
                    b.kind = Kind::Bone;
                }
                fixed.push(bx(sub(skull, [15.0, 15.0]), add(skull, [15.0, 15.0]), lvl + 9.0, lvl + 10.5, Kind::Bone, 0));
                points.push(on(k, add(c, scale(va, 8.0))));
                points.push(on(k, skull));
                points.push(on(k, add(c, scale(unit(axis), 45.0))));
            }
            SiteKind::GlassVault => {
                // A great hall sunk in the crater, a gatehouse on the glass, a fulgurite spire.
                let hall = c;
                let (hx, hy) = (24.0, 15.0);
                for (a, b) in [([-hx, -hy], [hx, -hy]), ([-hx, hy], [hx, hy]), ([-hx, -hy], [-hx, hy]), ([hx, -hy], [hx, hy])] {
                    let (a, b) = (add(hall, a), add(hall, b));
                    // Each wall in two, a door between.
                    let m = scale(add(a, b), 0.5);
                    let d = scale(sub(b, a), 1.0 / dist(a, b));
                    out.push(wall(k, a, sub(m, scale(d, 3.0)), 1.0, 10.0, Kind::Block));
                    out.push(wall(k, add(m, scale(d, 3.0)), b, 1.0, 10.0, Kind::Block));
                }
                let g = k.height(hall);
                fixed.push(bx(sub(hall, [hx, hy]), add(hall, [hx, hy]), g + 10.0, g + 11.0, Kind::Block, 0));
                let gh = add(c, scale(unit(axis + 180.0), p.half + 20.0));
                out.push(stand(k, gh, [14.0, 10.0], 7.0, Kind::Block, 0));
                // The fulgurite spire, its stairs in front of it (toward the hall).
                let fd = clear_dir(k, &p, &|d| vec![(add(c, scale(unit(d), p.half + 18.0)), 4.5), (add(c, scale(unit(d), p.half + 4.0)), 6.5)]);
                let spire = add(c, scale(unit(fd), p.half + 18.0));
                let base_z = k.height(spire);
                fixed.push(bx(sub(spire, [3.0, 3.0]), add(spire, [3.0, 3.0]), base_z - 1.0, base_z + 44.0, Kind::Crystal, 0));
                let foot = add(c, scale(unit(fd), p.half + 4.0));
                let stair = stairs_at(k, foot, 9.0, k.height(foot), k.height(foot) + 30.0, c, Kind::Crystal, &mut fixed);
                internal_up(k, i, vec![vec![c, stair.0[0]]], stair);
                points.push(on(k, hall));
                points.push(on(k, gh));
                if p.half > 60.0 {
                    points.push([spire[0] + 8.5, spire[1], base_z + 30.0]);
                }
            }
            SiteKind::Arch => {
                // A natural arch: two pillars and a span; the point under it, a perch on top.
                let fd = clear_dir(k, &p, &|d| {
                    let (ac, ax) = (add(c, scale(unit(d), 26.0)), axis_near(d + 90.0));
                    vec![(add(ac, scale(ax, 22.0)), 6.5), (sub(ac, scale(ax, 22.0)), 6.5), (add(ac, scale(ax, 33.0)), 6.5)]
                });
                let ac = add(c, scale(unit(fd), 26.0));
                let ax = axis_near(fd + 90.0);
                for side in [-1.0, 1.0] {
                    fixed.push(stand(k, add(ac, scale(ax, side * 22.0)), [9.0, 9.0], 26.0, Kind::Rock, 0));
                }
                let g = k.height(ac);
                let s = add(scale(ax, 53.0), scale([ax[1].abs(), ax[0].abs()], 9.0));
                let s = [s[0].abs(), s[1].abs()];
                fixed.push(bx(sub(ac, scale(s, 0.5)), add(ac, scale(s, 0.5)), g + 26.0, g + 32.0, Kind::Rock, 0));
                // Stairs beside a pillar, outside the arch, facing the way we came.
                let foot = add(ac, scale(ax, 22.0 + 4.5 + 2.0 + 4.5));
                let front = sub(foot, scale(unit(fd), 12.0));
                let stair = stairs_at(k, foot, 9.0, g, g + 31.6, front, Kind::Rock, &mut fixed);
                internal_up(k, i, vec![vec![c, front, stair.0[0]]], stair);
                points.push(on(k, ac));
            }
            SiteKind::Vent => {
                // The vent (a ring of crystal round its mouth): a vent lift up
                // to a walkway over to a ledge on a pillar, and a drop shaft
                // off the pillar's far side (K1).
                let dir = clear_dir(k, &p, &|d| vec![(add(c, scale(unit(d), 12.0)), 6.0), (add(c, scale(unit(d), 34.0)), 8.0), (add(c, scale(unit(d), 44.0)), 4.0)]);
                let u = unit(dir);
                let vent = add(c, scale(u, 12.0));
                let shaft = add(c, scale(u, 34.0 + 5.0 + 3.5));
                // Level ground at the lift's foot and the shaft's (dunes aren't).
                for q in [vent, shaft] {
                    let level = k.mean(q, 5.0);
                    pad(k, q, 6.0, level, 8.0);
                }
                let g = k.height(vent);
                for (dx, dy) in [(-5.0, 0.0), (5.0, 0.0), (0.0, -5.0), (0.0, 5.0)] {
                    out.push(bx(add(vent, [dx - 1.5, dy - 1.5]), add(vent, [dx + 1.5, dy + 1.5]), g - 0.5, g + 1.2, Kind::Crystal, 0));
                }
                let tower = add(c, scale(u, 34.0));
                let pillar = stand(k, tower, [10.0, 10.0], 20.0, Kind::Rock, 0);
                let top = pillar.top as f64;
                fixed.push(pillar);
                fixed.push(strip(add(vent, scale(u, 3.2)), add(vent, scale(u, 18.0)), 2.0, top - 1.5, top, Kind::Rock));
                lift_up(k, i, vent, 2.0, g, top, c, add(vent, scale(u, 10.0)));
                drop_down(k, i, shaft, 2.0, top, k.height(shaft), tower, add(tower, scale(u, 4.2)), add(tower, scale(u, 15.0)));
                keep_clear(k, add(c, scale(u, 25.0)), 25.0);
                points.push(on(k, vent));
            }
            SiteKind::TunnelMouth => {
                let ch = mouth(k, i);
                let (top, e) = (ch.top, ch.e);
                let n = [-e[1], e[0]];
                // A canopy over the trench's head on posts, a barrier by the outpost.
                fixed.push(oriented(add(top, scale(e, 8.0)), [8.5, 6.0], e, ch.surface + 5.0, ch.surface + 6.5, Kind::Block));
                for side in [-1.0, 1.0] {
                    fixed.push(oriented(add(top, scale(n, side * 6.0)), [1.0, 1.0], e, ch.surface - 0.3, ch.surface + 5.0, Kind::Block));
                }
                out.push(stand(k, add(c, scale(unit(axis - 90.0), 16.0)), [10.0, 1.2], 1.1, Kind::Block, 0));
                points.push([top[0], top[1], ch.surface]);
                k.chambers.push(ch);
            }
            SiteKind::SpirePerch => {
                // A hoodoo with a fortified foot; stairs to its top.
                let spire = add(c, scale(unit(clear_dir(k, &p, &|d| vec![(add(c, scale(unit(d), 22.0)), 6.5)])), 22.0));
                let g = k.height(spire);
                let stair = stairs_at(k, spire, 9.0, g, g + 34.0, c, Kind::Rock, &mut fixed);
                internal_up(k, i, vec![vec![c, stair.0[0]]], stair);
                for j in 0..4 {
                    let q = add(c, scale(unit(axis + 90.0 * j as f64 + 45.0), 22.0));
                    out.push(stand(k, q, [6.0, 1.2], 1.1, Kind::Block, 0));
                }
                points.push([c[0], c[1], g + 34.0]);
            }
            SiteKind::WellShrine => {
                let g = k.height(c);
                let well = add(c, scale(unit(clear_dir(k, &p, &|d| vec![(add(c, scale(unit(d), 10.0)), 3.6)])), 10.0));
                fixed.push(bx(sub(well, [2.5, 2.5]), add(well, [2.5, 2.5]), g - 0.5, g + 6.0, Kind::Crystal, 0));
                for j in 0..4 {
                    let q = add(c, scale(unit(axis + 90.0 * j as f64), 12.0));
                    out.push(stand(k, q, [4.0, 1.2], 1.1, Kind::Block, 0));
                }
                points.push(on(k, add(c, [4.0, 4.0])));
            }
            SiteKind::Wreck => {
                // A lattice ship on its side: a long hull, its decks stacked like floors.
                let u = unit(axis + 90.0);
                let along_x = u[0].abs() > u[1].abs();
                let ua = if along_x { [1.0, 0.0] } else { [0.0, 1.0] };
                let va = [ua[1], ua[0]];
                let g = k.height(c);
                let (hl, hw) = (30.0, 7.0);
                for side in [-1.0, 1.0] {
                    // Hull sides with gaps (breaches).
                    for (t0, t1) in [(-hl, -8.0), (-3.0, 12.0), (17.0, hl)] {
                        let (a, b) = (add(add(c, scale(ua, t0)), scale(va, side * hw)), add(add(c, scale(ua, t1)), scale(va, side * hw)));
                        out.push(wall(k, a, b, 1.0, 15.0, Kind::Block));
                    }
                }
                // Its decks clear the highest ground under them by a story (the
                // ground under a wreck isn't level): a head never meets them.
                let (min, max) = (sub(c, add(scale(ua, hl - 4.0), scale(va, hw - 0.5))), add(c, add(scale(ua, hl - 4.0), scale(va, hw - 0.5))));
                let (min, max) = ([min[0].min(max[0]), min[1].min(max[1])], [min[0].max(max[0]), min[1].max(max[1])]);
                let mut hi = g;
                for j in 0..=8 {
                    for i in 0..=8 {
                        hi = hi.max(k.height([lerp(min[0], max[0], i as f64 / 8.0), lerp(min[1], max[1], j as f64 / 8.0)]));
                    }
                }
                for z in [5.0, 10.0] {
                    fixed.push(bx(min, max, hi + z - 0.6, hi + z, Kind::Block, 0));
                }
                points.push(on(k, c));
            }
            _ => {}
        }
        let site = i as u16;
        k.points.extend(points.into_iter().map(|q| Point { pos: [q[0] as f32, q[1] as f32, q[2] as f32], site }));
        k.boxes.extend(out);
        k.fixed.extend(fixed);
    }
}

/// The axis-aligned rectangle of half-width `half` around the segment
/// `a`-`b` (which runs along x if `along_x`, else along y).
fn rect(a: [f64; 2], b: [f64; 2], half: f64, along_x: bool) -> ([f64; 2], [f64; 2]) {
    if along_x {
        ([a[0].min(b[0]), a[1] - half], [a[0].max(b[0]), a[1] + half])
    } else {
        ([a[0] - half, a[1].min(b[1])], [a[0] + half, a[1].max(b[1])])
    }
}

/// Cover between sites: boulders in the mesa country and on the rift floors,
/// crags on the Crown, fulgurite forests in the Glasslands, bones and reef
/// heads in the Dry Sea, a few rocks among the dunes.
pub(crate) fn scatter(k: &mut Kiln) {
    let mut rng = Rng::new(0x5CA7);
    let free = |k: &Kiln, q: [f64; 2], r: f64| {
        let (x0, x1, y0, y1) = Kiln::samples(sub(q, [r, r]), add(q, [r, r]));
        for iy in y0..=y1 {
            for ix in x0..=x1 {
                if k.mask[iy * TERRAIN_N + ix] & MASK_CLEAR != 0 {
                    return false;
                }
            }
        }
        let gr = [k.height(add(q, [r, 0.0])) - k.height(sub(q, [r, 0.0])), k.height(add(q, [0.0, r])) - k.height(sub(q, [0.0, r]))];
        len(gr) / (2.0 * r) < 0.45
    };
    // The same in every sector: generic cover by ring.
    let mut placed: Vec<([f64; 2], [f64; 2], f64, Kind)> = Vec::new();
    for _ in 0..2600 {
        let (r, deg) = (rng.range(820.0, 3650.0), rng.range(-60.0, 60.0));
        let l = scale(unit(deg), r);
        let crown = r < CROWN_R;
        let (size, tall, kind) = if crown && rng.next() < 0.12 {
            let s = rng.range(4.0, 9.0);
            ([s, s * rng.range(0.7, 1.3)], rng.range(18.0, 60.0), Kind::Rock)
        } else {
            let s = rng.range(1.4, 4.5);
            ([s, s * rng.range(0.6, 1.4)], rng.range(0.9, 3.2), Kind::Rock)
        };
        placed.push((l, size, tall, kind));
    }
    for s in 0..3 {
        let f = Frame::new(s);
        for &(l, size, tall, kind) in &placed {
            let q = f.world(l);
            if !free(k, q, size[0].max(size[1]) + 1.0) {
                continue;
            }
            // Thinner on the outer plains: they're the biomes'.
            if len(l) > 2700.0 && rng.next() < 0.75 {
                continue;
            }
            k.boxes.push(stand(k, q, size, tall, kind, 0));
        }
    }
    // Rift floors: scree boulders.
    for j in 0..3 {
        let rift = Rift::new(j);
        for _ in 0..260 {
            let along = rng.range(HOLLOW_R, RIFT_END - 60.0);
            let across = rng.range(-1.0, 1.0) * (Rift::half_width(along) - 12.0);
            let q = rift.at(along, across);
            let s = rng.range(2.0, 5.5);
            if free(k, q, s + 1.0) {
                k.boxes.push(stand(k, q, [s, s * rng.range(0.6, 1.4)], rng.range(1.2, 3.5), Kind::Rock, 0));
            }
        }
    }
    // The Hollow's floor.
    for _ in 0..120 {
        let q = add(CENTER, scale(unit(rng.range(0.0, 360.0)), rng.range(60.0, 420.0)));
        let s = rng.range(2.0, 6.0);
        if free(k, q, s + 1.0) {
            k.boxes.push(stand(k, q, [s, s * rng.range(0.6, 1.4)], rng.range(1.0, 3.0), Kind::Rock, 0));
        }
    }
    // The Glasslands' fulgurite forests: clusters of glass spikes.
    let f = Frame::new(0);
    for _ in 0..16 {
        let center = f.polar(rng.range(2750.0, 3700.0), rng.range(-55.0, 55.0));
        let radius = rng.range(45.0, 110.0);
        for _ in 0..(radius * 0.45) as usize {
            let q = add(center, scale(unit(rng.range(0.0, 360.0)), radius * rng.next().sqrt()));
            let s = rng.range(1.2, 2.8);
            if free(k, q, s + 1.0) {
                k.boxes.push(stand(k, q, [s, s], rng.range(8.0, 36.0), Kind::Crystal, 0));
            }
        }
    }
    // The Dry Sea: ribs and reef heads on the salt.
    let f = Frame::new(2);
    for _ in 0..70 {
        let q = f.polar(rng.range(2550.0, 3150.0), rng.range(-34.0, 34.0));
        if !free(k, q, 8.0) {
            continue;
        }
        if rng.next() < 0.5 {
            // A rib: two posts and a lintel, 6-12 m.
            let tall = rng.range(6.0, 12.0);
            let ax = if rng.next() < 0.5 { [1.0, 0.0] } else { [0.0, 1.0] };
            let g = k.height(q);
            for side in [-1.0, 1.0] {
                let p = add(q, scale(ax, side * 4.0));
                k.boxes.push(bx(sub(p, [0.9, 0.9]), add(p, [0.9, 0.9]), g - 0.5, g + tall, Kind::Bone, 0));
            }
            let s = add(scale(ax, 5.0), scale([ax[1], ax[0]], 0.9));
            k.boxes.push(bx(sub(q, s), add(q, s), g + tall, g + tall + 1.2, Kind::Bone, 0));
        } else {
            let s = rng.range(3.0, 8.0);
            k.boxes.push(stand(k, q, [s, s * rng.range(0.6, 1.4)], rng.range(2.0, 6.0), Kind::Rock, 0));
        }
    }
    // The Dune sea: the odd rock.
    let f = Frame::new(1);
    for _ in 0..90 {
        let q = f.polar(rng.range(2750.0, 3700.0), rng.range(-55.0, 55.0));
        let s = rng.range(2.0, 5.0);
        if free(k, q, s + 1.0) {
            k.boxes.push(stand(k, q, [s, s * rng.range(0.6, 1.4)], rng.range(1.0, 3.0), Kind::Rock, 0));
        }
    }
    let _ = (land::CROWN, ROAD_HALF);
}

/// Removes boxes (not the fixed ones: decks, stairs, crack crystals) that
/// stand in a route's way: within 3 m of its path and between its feet and
/// head height.
pub(crate) fn clear_lanes(k: &mut Kiln, paths: &[Vec<[f64; 2]>]) {
    // Path segments by 32 m cell.
    let cell = 32.0;
    let dim = (crate::movement::WORLD_SIZE as f64 / cell) as usize;
    let mut cells: Vec<Vec<([f64; 2], [f64; 2])>> = vec![Vec::new(); dim * dim];
    for path in paths {
        for w in path.windows(2) {
            let (min, max) = ([w[0][0].min(w[1][0]) - 4.0, w[0][1].min(w[1][1]) - 4.0], [w[0][0].max(w[1][0]) + 4.0, w[0][1].max(w[1][1]) + 4.0]);
            let c = |v: f64| ((v / cell).floor().max(0.0) as usize).min(dim - 1);
            for y in c(min[1])..=c(max[1]) {
                for x in c(min[0])..=c(max[0]) {
                    cells[y * dim + x].push((w[0], w[1]));
                }
            }
        }
    }
    let h = &k.h;
    let height = |p: [f64; 2]| {
        let res = TERRAIN_RES as f64;
        let i = |v: f64| ((v / res).round().clamp(0.0, (TERRAIN_N - 1) as f64)) as usize;
        h[i(p[1]) * TERRAIN_N + i(p[0])]
    };
    k.boxes.retain(|b| {
        let (min, max) = ([b.min[0] as f64, b.min[1] as f64], [b.max[0] as f64, b.max[1] as f64]);
        let c = |v: f64| ((v / cell).floor().max(0.0) as usize).min(dim - 1);
        let mid = scale(add(min, max), 0.5);
        let half = scale(sub(max, min), 0.5);
        for y in c(min[1] - 4.0)..=c(max[1] + 4.0) {
            for x in c(min[0] - 4.0)..=c(max[0] + 4.0) {
                for &(a, bb) in &cells[y * dim + x] {
                    // Distance from the segment to the rectangle, approximately:
                    // from points along the segment.
                    let l = dist(a, bb);
                    let n = (l / 1.0).ceil().max(1.0) as usize;
                    for j in 0..=n {
                        let p = add(a, scale(sub(bb, a), j as f64 / n as f64));
                        let dx = ((p[0] - mid[0]).abs() - half[0]).max(0.0);
                        let dy = ((p[1] - mid[1]).abs() - half[1]).max(0.0);
                        if len([dx, dy]) < 3.0 {
                            let z = height(p);
                            if (b.bottom as f64) < z + 2.4 && (b.top as f64) > z + 0.45 {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        true
    });
}
