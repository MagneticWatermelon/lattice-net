//! Kiln's connectors and its Deep (K1, K2): lifts and drop shafts, lattice
//! springs, rooms, and what's under the ground: chambers, the tubes between
//! them, trenches down to them through holes in the terrain, and shafts.
//!
//! Everything under the ground is boxes inside the terrain's shell (`World`'s
//! holes): a chamber is a floor, a ceiling and walls with openings where
//! trenches, tubes and shafts come in; a tube is turned boxes in 8 m
//! segments. Chambers are axis-aligned, so the holes over trenches and
//! shafts are whole height cells and their walls line them exactly.

use super::build::{oriented, strip};
use super::land;
use super::lanes::{Leg, LegKind, Route};
use super::*;
use crate::world::{CoverBox, Force, Kind, Volume, CELLS};

/// A lift's speed up (m/s), a tall one's, and its top's: slow enough that a
/// rider leaves it bobbing just over where it goes.
const LIFT_FAST: f64 = 20.0;
const LIFT_TALL: f64 = 30.0;
const LIFT_SLOW: f64 = 4.0;
/// A drop shaft's fall (m/s).
const DROP_SPEED: f64 = 6.0;
/// Gravity (movement's).
const G: f64 = 15.0;

/// A trench's length (10 cells: a 12 m drop at 0.3), a Tunnel mouth's
/// chamber's half-side and how far its floor is under the ground; how far
/// under the ground a tube's floor stays (its ceiling's top 2.5 m under it,
/// at the least).
const TRENCH: f64 = 40.0;
const CHAMBER: f64 = 8.0;
const DEPTH: f64 = 12.0;
const TUBE_DEPTH: f64 = 8.0;
/// A tube's half-width inside its walls, its headroom, its steepest grade.
const TUBE: f64 = 3.0;
const TUBE_HIGH: f64 = 4.5;
const TUBE_GRADE: f64 = 0.25;
/// A shaft's half-side: two height cells across.
const SHAFT: f64 = 4.0;

/// A lift (K1): an updraft in the square column of half-side `half` at `c`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lift_up(k: &mut Kiln, site: usize, c: [f64; 2], half: f64, z0: f64, z1: f64, from: [f64; 2], to: [f64; 2]) {
    lift_rect(k, site, c, [half, half], z0, z1, from, to, true);
}

/// A lift: an updraft in the column of half-sizes `half` at `c`, from its
/// foot at `z0` to `z1` (the deck or ledge it serves): fast, then slowing
/// over its last stretch so a rider bobs just over `z1` and steps off; and a
/// one-way route for `site` from `from` (by its foot: on the ground if
/// `on_ground`, else at `z0`, a deck or a chamber's floor) up it to `to` (on
/// top, at `z1`). Tall lifts (over 200 m) run faster.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lift_rect(k: &mut Kiln, site: usize, c: [f64; 2], half: [f64; 2], z0: f64, z1: f64, from: [f64; 2], to: [f64; 2], on_ground: bool) {
    lift_volume(k, c, half, z0, z1);
    let legs = lift_legs(c, half, z0, z1, from, to, on_ground);
    k.internal.push(Route { a: site, b: site, legs, one_way: true });
}

/// A lift's updraft (as `lift_rect`), without its route.
pub(crate) fn lift_volume(k: &mut Kiln, c: [f64; 2], half: [f64; 2], z0: f64, z1: f64) {
    let fast = if z1 - z0 > 200.0 { LIFT_TALL } else { LIFT_FAST };
    // Room to slow from `fast` to `LIFT_SLOW` at gravity's rate, and a little.
    let slow = (fast * fast - LIFT_SLOW * LIFT_SLOW) / (2.0 * G) + 3.0;
    let column = |lo: f64, hi: f64, v: f64| Volume {
        min: [(c[0] - half[0]) as f32, (c[1] - half[1]) as f32, lo as f32],
        max: [(c[0] + half[0]) as f32, (c[1] + half[1]) as f32, hi as f32],
        force: Force::Lift,
        vel: [0.0, 0.0, v as f32],
    };
    if z1 - z0 > slow + 4.0 {
        k.volumes.push(column(z0 - 1.0, z1 - slow, fast));
        k.volumes.push(column(z1 - slow, z1 + 1.5, LIFT_SLOW));
    } else {
        k.volumes.push(column(z0 - 1.0, z1 + 1.5, LIFT_SLOW.max(6.0)));
    }
}

/// A lift's walk (as `lift_rect`): to just outside its foot, into it (and
/// held there, carried), then off at the top.
pub(crate) fn lift_legs(c: [f64; 2], half: [f64; 2], z0: f64, z1: f64, from: [f64; 2], to: [f64; 2], on_ground: bool) -> Vec<Leg> {
    let reach = half[0].min(half[1]) + 1.5;
    let out = add(c, scale(sub(from, c), reach / dist(from, c)));
    vec![
        Leg { kind: LegKind::Road, path: vec![from, out], z: (!on_ground).then(|| vec![z0, z0]) },
        Leg { kind: LegKind::Road, path: vec![out, c, to], z: Some(vec![z0, z1 + 1.0, z1]) },
    ]
}

/// A drop shaft (K1): a square column of half-side `half` at `c`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drop_down(k: &mut Kiln, site: usize, c: [f64; 2], half: f64, z0: f64, z1: f64, from: [f64; 2], edge: [f64; 2], to: [f64; 2]) {
    drop_rect(k, site, c, [half, half], z0, z1, from, edge, to, (false, true));
}

/// A drop shaft: a column of half-sizes `half` at `c` that slows a fall to
/// `DROP_SPEED`, from over `z0` (where it's stepped into) down to `z1`; and
/// a one-way route for `site` from `from` (up top) down it to `to`. `edge`
/// is where the top's floor ends beside it, stepped off from. `on_ground`:
/// whether the top and the bottom are the ground (else at `z0`, `z1`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn drop_rect(k: &mut Kiln, site: usize, c: [f64; 2], half: [f64; 2], z0: f64, z1: f64, from: [f64; 2], edge: [f64; 2], to: [f64; 2], on_ground: (bool, bool)) {
    drop_volume(k, c, half, z0, z1);
    let legs = drop_legs(c, z0, z1, from, edge, to, on_ground);
    k.internal.push(Route { a: site, b: site, legs, one_way: true });
}

/// A drop shaft's slowed fall (as `drop_rect`), without its route.
pub(crate) fn drop_volume(k: &mut Kiln, c: [f64; 2], half: [f64; 2], z0: f64, z1: f64) {
    k.volumes.push(Volume {
        min: [(c[0] - half[0]) as f32, (c[1] - half[1]) as f32, (z1 - 1.0) as f32],
        max: [(c[0] + half[0]) as f32, (c[1] + half[1]) as f32, (z0 + 3.0) as f32],
        force: Force::Drop,
        vel: [0.0, 0.0, -DROP_SPEED as f32],
    });
}

/// A drop shaft's walk (as `drop_rect`): off the edge, down, and on.
pub(crate) fn drop_legs(c: [f64; 2], z0: f64, z1: f64, from: [f64; 2], edge: [f64; 2], to: [f64; 2], on_ground: (bool, bool)) -> Vec<Leg> {
    vec![
        Leg { kind: LegKind::Road, path: vec![from, edge], z: (!on_ground.0).then(|| vec![z0, z0]) },
        Leg { kind: LegKind::Road, path: vec![edge, c], z: Some(vec![z0, z1]) },
        Leg { kind: LegKind::Road, path: vec![c, to], z: (!on_ground.1).then(|| vec![z1, z1]) },
    ]
}

/// A lattice spring (K2): a pad at `pad` (its floor at `z0`) that throws
/// whoever stands on it straight up, to 5 m over `z1`, beside a ledge at
/// `z1` whose nearest edge is a couple of meters off; and a one-way route
/// for `site` from `from` onto the pad, up, and over onto `to` (on the ledge).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spring(k: &mut Kiln, site: usize, pad: [f64; 2], z0: f64, z1: f64, from: [f64; 2], to: [f64; 2]) {
    // Under 8 m, the landing is one waypoint: a midpoint (lanes are walked
    // in 8 m steps) would hold a walker still over it, braking in the air.
    debug_assert!(dist(pad, to) < 8.0);
    let vz = libm::sqrt(2.0 * G * (z1 + 5.0 - z0));
    let h = 1.5;
    k.volumes.push(Volume {
        min: [(pad[0] - h) as f32, (pad[1] - h) as f32, (z0 - 0.5) as f32],
        max: [(pad[0] + h) as f32, (pad[1] + h) as f32, (z0 + 0.5) as f32],
        force: Force::Spring,
        vel: [0.0, 0.0, vz as f32],
    });
    // Onto the pad, held over it while thrown up, then across near the top.
    let legs = vec![Leg { kind: LegKind::Road, path: vec![from, pad, pad, to], z: Some(vec![z0, z0, z1 + 2.0, z1]) }];
    k.internal.push(Route { a: site, b: site, legs, one_way: true });
}

/// A lift up a cliff with a drop shaft beside it (K1, K2), found from the
/// ground along a line out from a lower flat (`at(side, d)`: the point
/// `side` m to the side of it and `d` m out): the column on the lower flat
/// at the cliff's foot (searched from `d0`, the flat at `lo`), a walkway
/// over the face to the first flat at least `rise` m higher. The lift is
/// at side +4, the drop shaft at -4.
pub(crate) fn cliff_lifts(k: &mut Kiln, site: usize, at: &dyn Fn(f64, f64) -> [f64; 2], d0: f64, lo: f64, rise: f64) {
    let mut foot = d0;
    while foot < d0 + 120.0 && [-4.0, 0.0, 4.0].iter().all(|&s| k.height(at(s, foot + 1.0)) < lo + 0.3) {
        foot += 1.0;
    }
    let mut top_d = foot + 4.0;
    while top_d < foot + 200.0 && !(k.height(at(0.0, top_d)) > lo + rise && (k.height(at(0.0, top_d + 4.0)) - k.height(at(0.0, top_d))).abs() < 1.5) {
        top_d += 1.0;
    }
    top_d += 6.0;
    let top = [-6.0, 0.0, 6.0].iter().map(|&s| k.height(at(s, top_d))).fold(f64::MIN, f64::max);
    let col = foot - 4.0;
    k.fixed.push(strip(at(0.0, col + 4.8), at(0.0, top_d), 7.0, top - 2.0, top, Kind::Block));
    lift_up(k, site, at(4.0, col), 2.5, lo, top, at(4.0, col - 18.0), at(4.0, col + 12.0));
    drop_down(k, site, at(-4.0, col), 2.5, top, lo, at(-4.0, col + 12.0), at(-4.0, col + 5.2), at(-4.0, col - 18.0));
    keep_clear(k, at(0.0, top_d), 12.0);
}

/// Keeps scattered cover off the ground within `r` of `c` (a lift's foot).
pub(crate) fn keep_clear(k: &mut Kiln, c: [f64; 2], r: f64) {
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [r, r]), add(c, [r, r]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            if dist(Kiln::pos(ix, iy), c) <= r {
                k.mask[iy * TERRAIN_N + ix] |= MASK_CLEAR;
            }
        }
    }
}

/// A room (C1): walls round a `2 hu` × `2 hv` footprint at `q` (its ends
/// across `u`), a door in the middle of each end, a roof `high` over `floor`;
/// the doors shielded for site `shield.0`, held by faction `shield.1` at the
/// start (they let its holder through), if any.
#[allow(clippy::too_many_arguments)]
pub(crate) fn room(k: &mut Kiln, q: [f64; 2], u: [f64; 2], hu: f64, hv: f64, high: f64, floor: f64, shield: Option<(usize, u8)>) {
    let v = [-u[1], u[0]];
    let at = |a: f64, b: f64| add(add(q, scale(u, a)), scale(v, b));
    for s in [-1.0, 1.0] {
        k.fixed.push(strip(at(s * hu, -hv), at(s * hu, -2.0), 0.4, floor - 0.3, floor + high, Kind::Block));
        k.fixed.push(strip(at(s * hu, 2.0), at(s * hu, hv), 0.4, floor - 0.3, floor + high, Kind::Block));
        if let Some((site, f)) = shield {
            k.fixed.push(strip(at(s * hu, -2.0), at(s * hu, 2.0), 0.2, floor - 0.3, floor + high, Kind::Shield).site_shield(site as u16, f));
        }
        k.fixed.push(strip(at(-hu, s * hv), at(hu, s * hv), 0.4, floor - 0.3, floor + high, Kind::Block));
    }
    k.fixed.push(oriented(q, [hu + 0.4, hv + 0.4], u, floor + high, floor + high + 0.6, Kind::Block));
}

/// A spawn room (K3, GDD §3.4): a room (as `room`) whose doors are shielded
/// for site `site`, held by `faction` at the start (`territory::NONE`: no one,
/// sealed till someone takes it), with a baffle wall `BAFFLE` m out from each
/// door: you leave it sideways, and nothing outside is in sight from inside.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_room(k: &mut Kiln, q: [f64; 2], u: [f64; 2], hu: f64, hv: f64, floor: f64, site: usize, faction: u8) {
    room(k, q, u, hu, hv, 4.5, floor, Some((site, faction)));
    let v = [-u[1], u[0]];
    for s in [-1.0, 1.0] {
        let at = |b: f64| add(add(q, scale(u, s * (hu + BAFFLE))), scale(v, b));
        // Wider than the room: from a step inside the door, nothing past it shows.
        k.fixed.push(strip(at(-hv - 2.5), at(hv + 2.5), 0.4, floor - 0.3, floor + 3.6, Kind::Block));
    }
}

/// How far a spawn room's baffles stand out from its doors.
pub(crate) const BAFFLE: f64 = 3.0;

/// A chamber under the ground (K1, K2): an axis-aligned room, its walls
/// left open where a trench, tubes and shafts come in (`walls`, once they're
/// all known).
#[derive(Debug, Clone)]
pub(crate) struct Chamber {
    pub site: usize,
    /// A Tunnel mouth's: its trench's head (on a cell's edge), its axis
    /// (along a world axis) and its ramp's foot. Else the center, (1, 0), the center.
    pub top: [f64; 2],
    pub e: [f64; 2],
    pub foot: [f64; 2],
    pub trench: bool,
    pub center: [f64; 2],
    /// Its half-side and its headroom.
    pub half: f64,
    pub high: f64,
    /// The ground's level over it, and its floor.
    pub surface: f64,
    pub floor: f64,
    /// The directions tubes leave it in.
    pub outs: Vec<[f64; 2]>,
    /// Where routes through it stand: its center, unless a shaft's there.
    pub stand: [f64; 2],
    /// Its tubes' openings shielded for (site, its holder at the start): a
    /// spawn room's foot (K3), so the spawn room's shield stops attackers
    /// at the tunnel's inner end.
    pub shield: Option<(usize, u8)>,
    /// Directions no tube may leave it in, within an angle (the cosine):
    /// toward a shaft off its center (one would cross the shaft's column
    /// from where routes stand).
    pub blocked: Vec<([f64; 2], f64)>,
    /// The shafts into it (to the ground, or to a chamber over or under it).
    pub shafts: usize,
    /// Whether one of them goes up to the ground (an exit, as a trench is).
    pub ground: bool,
    /// The chamber over or under it, down or up a shaft between them.
    pub pair: Option<usize>,
}

fn set_hole(k: &mut Kiln, cx: usize, cy: usize) {
    if k.holes.is_empty() {
        k.holes = vec![0; (CELLS * CELLS).div_ceil(64)];
    }
    let j = cy * CELLS + cx;
    k.holes[j / 64] |= 1 << (j % 64);
}

/// An axis-aligned slab over [min, max] from `z0` to `z1`, with the squares
/// (center, half-side) in `openings` left out (a ceiling under shafts).
fn slab(k: &mut Kiln, min: [f64; 2], max: [f64; 2], z0: f64, z1: f64, openings: &[([f64; 2], f64)]) {
    let mut xs = vec![min[0], max[0]];
    let mut ys = vec![min[1], max[1]];
    for &(c, h) in openings {
        xs.extend([c[0] - h, c[0] + h]);
        ys.extend([c[1] - h, c[1] + h]);
    }
    let clip = |v: &mut Vec<f64>, lo: f64, hi: f64| {
        v.retain(|&x| x >= lo && x <= hi);
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v.dedup();
    };
    clip(&mut xs, min[0], max[0]);
    clip(&mut ys, min[1], max[1]);
    for j in 0..ys.len() - 1 {
        // Runs of open cells along x, merged.
        let mut run: Option<f64> = None;
        for i in 0..xs.len() {
            let solid = i + 1 < xs.len() && {
                let m = [(xs[i] + xs[i + 1]) / 2.0, (ys[j] + ys[j + 1]) / 2.0];
                !openings.iter().any(|&(c, h)| (m[0] - c[0]).abs() < h && (m[1] - c[1]).abs() < h)
            };
            match (solid, run) {
                (true, None) => run = Some(xs[i]),
                (false, Some(x0)) => {
                    k.fixed.push(super::build::bx([x0, ys[j]], [xs[i], ys[j + 1]], z0, z1, Kind::Block, 0));
                    run = None;
                }
                _ => {}
            }
        }
    }
}

/// A chamber of half-side `half` and headroom `high` at `center` (snapped to
/// cells' corners by the caller where shafts open into it), its floor at
/// `floor` under ground at `surface`; its ceiling left open over the shafts
/// at `shafts` (cells' corners). Returns its index in `k.chambers`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chamber(k: &mut Kiln, site: usize, center: [f64; 2], half: f64, high: f64, floor: f64, surface: f64, shafts: &[[f64; 2]]) -> usize {
    chamber_open(k, site, center, half, high, floor, surface, shafts, &[])
}

/// A chamber (as `chamber`) with its floor open too, over the shafts at
/// `below` (K3: a shaft down to a chamber under it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn chamber_open(k: &mut Kiln, site: usize, center: [f64; 2], half: f64, high: f64, floor: f64, surface: f64, shafts: &[[f64; 2]], below: &[[f64; 2]]) -> usize {
    let open = |at: &[[f64; 2]]| at.iter().map(|&c| (c, SHAFT)).collect::<Vec<([f64; 2], f64)>>();
    slab(k, sub(center, [half, half]), add(center, [half, half]), floor - 1.0, floor, &open(below));
    let h = half + 0.6;
    slab(k, sub(center, [h, h]), add(center, [h, h]), floor + high, floor + high + 0.6, &open(shafts));
    k.chambers.push(Chamber { site, top: center, e: [1.0, 0.0], foot: center, trench: false, center, half, high, surface, floor, outs: Vec::new(), stand: center, shield: None, blocked: Vec::new(), shafts: 0, ground: false, pair: None });
    k.chambers.len() - 1
}

/// Digs site `i`'s Tunnel mouth (its anchors: the trench's head, and a step
/// along its axis). Returns the chamber's index.
pub(crate) fn mouth(k: &mut Kiln, i: usize) -> usize {
    let (top, e) = (k.sites[i].anchors[0], sub(k.sites[i].anchors[1], k.sites[i].anchors[0]));
    mouth_at(k, i, top, e)
}

/// A mouth for `site` (K1): the ground round it leveled, holes in it over a
/// trench from `top` (a cell's edge) along `e` (a world axis), the ramp down
/// and its walls, and the chamber it comes down into. Returns its index.
pub(crate) fn mouth_at(k: &mut Kiln, site: usize, top: [f64; 2], e: [f64; 2]) -> usize {
    mouth_sized(k, site, top, e, CHAMBER, 5.0, DEPTH, (12.0, 24.0))
}

/// A mouth (as `mouth_at`) whose chamber is `half` m to a side and `high`
/// high, its floor `depth` under the ground, the ground round it leveled
/// `pad.0` m to either side of the trench and blended back over `pad.1` m
/// (K3: a cenote's cavern, off its pit's floor).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mouth_sized(k: &mut Kiln, site: usize, top: [f64; 2], e: [f64; 2], half: f64, high: f64, depth: f64, pad: (f64, f64)) -> usize {
    let n = [-e[1], e[0]];
    let foot = add(top, scale(e, TRENCH));
    let center = add(foot, scale(e, half));
    let surface = k.mean(add(top, scale(e, TRENCH / 2.0)), 26.0);
    let corners = [sub(sub(top, scale(e, 4.0)), scale(n, pad.0)), add(add(center, scale(e, half + 4.0)), scale(n, pad.0))];
    let (min, max) = ([corners[0][0].min(corners[1][0]), corners[0][1].min(corners[1][1])], [corners[0][0].max(corners[1][0]), corners[0][1].max(corners[1][1])]);
    land::pad_rect(k, min, max, surface, pad.1);
    let floor = surface - depth;
    // The holes: the trench's cells (two across, ten along).
    let res = TERRAIN_RES as f64;
    for a in 0..(TRENCH / res) as usize {
        for b in [-1.0, 0.0] {
            let q = add(add(top, scale(e, (a as f64 + 0.5) * res)), scale(n, (b + 0.5) * res));
            set_hole(k, (q[0] / res).floor() as usize, (q[1] / res).floor() as usize);
        }
    }
    let mid = scale(add(top, foot), 0.5);
    let ramp = CoverBox::turned([mid[0] as f32, mid[1] as f32], [(TRENCH / 2.0) as f32, 4.0], [-e[0] as f32, -e[1] as f32], (floor - 1.0) as f32, surface as f32, Kind::Block, 0);
    k.fixed.push(ramp.wedge(floor as f32));
    for side in [-1.0, 1.0] {
        k.fixed.push(strip(add(top, scale(n, side * 4.3)), add(foot, scale(n, side * 4.3)), 0.3, floor - 1.0, surface, Kind::Block));
    }
    k.fixed.push(oriented(center, [half, half], e, floor - 1.0, floor, Kind::Block));
    k.fixed.push(oriented(center, [half + 0.6, half + 0.6], e, floor + high, floor + high + 0.6, Kind::Block));
    k.chambers.push(Chamber { site, top, e, foot, trench: true, center, half, high, surface, floor, outs: Vec::new(), stand: center, shield: None, blocked: Vec::new(), shafts: 0, ground: false, pair: None });
    k.chambers.len() - 1
}

/// A shaft (K2) from the ground down into chamber `ch`: the 2 × 2 height
/// cells round `corner` (a cells' corner, over the chamber, its ceiling open
/// there) cut out and walled from the chamber's ceiling up to the ground; a
/// lift up one half, a drop shaft down the other (`side`: the lift's half,
/// a unit vector along a world axis); one-way routes up and down between
/// `below` (on the chamber's floor) and `above` (on the ground by the hole).
pub(crate) fn shaft(k: &mut Kiln, site: usize, corner: [f64; 2], ch: usize, side: [f64; 2], below: [f64; 2], above: [f64; 2]) {
    shaft_with(k, site, corner, ch, side, below, above, None, (10.0, 10.0));
}

/// A shaft (as `shaft`), its drop shaft's route coming from `from` on the
/// ground (else 6 m beyond the edge it's stepped off), the ground round its
/// mouth leveled over `pad` (radius, blend; nothing if the radius is 0: the
/// caller has).
#[allow(clippy::too_many_arguments)]
pub(crate) fn shaft_with(k: &mut Kiln, site: usize, corner: [f64; 2], ch: usize, side: [f64; 2], below: [f64; 2], above: [f64; 2], from: Option<[f64; 2]>, pad: (f64, f64)) {
    // Level ground round its mouth, so its walls are flush all round.
    if pad.0 > 0.0 {
        let level = k.mean(corner, 8.0);
        land::pad(k, corner, pad.0, level, pad.1);
    }
    let res = TERRAIN_RES as f64;
    let (cx, cy) = ((corner[0] / res).round() as usize, (corner[1] / res).round() as usize);
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        set_hole(k, cx + dx - 1, cy + dy - 1);
    }
    k.chambers[ch].shafts += 1;
    k.chambers[ch].ground = true;
    let c = k.chambers[ch].clone();
    let surface = [[-SHAFT, -SHAFT], [SHAFT, -SHAFT], [-SHAFT, SHAFT], [SHAFT, SHAFT], [0.0, 0.0]].iter().map(|&o| k.height(add(corner, o))).fold(f64::MIN, f64::max);
    let ceiling = c.floor + c.high;
    for (a, b) in [([-1.0, -1.0], [1.0, -1.0]), ([1.0, -1.0], [1.0, 1.0]), ([1.0, 1.0], [-1.0, 1.0]), ([-1.0, 1.0], [-1.0, -1.0])] {
        let (p, q) = (add(corner, scale(a, SHAFT + 0.3)), add(corner, scale(b, SHAFT + 0.3)));
        k.fixed.push(strip(p, q, 0.3, ceiling, surface, Kind::Block));
    }
    // The halves: the lift's toward `side`, the drop shaft's away.
    let half = if side[0] != 0.0 { [SHAFT / 2.0 - 0.1, SHAFT - 0.1] } else { [SHAFT - 0.1, SHAFT / 2.0 - 0.1] };
    let (up, down) = (add(corner, scale(side, SHAFT / 2.0)), sub(corner, scale(side, SHAFT / 2.0)));
    lift_rect(k, site, up, half, c.floor, surface, below, above, false);
    let edge = sub(corner, scale(side, SHAFT + 0.8));
    drop_rect(k, site, down, half, surface, c.floor, from.unwrap_or(sub(edge, scale(side, 6.0))), edge, below, (true, false));
    keep_clear(k, corner, 14.0);
}

/// A shaft between two chambers stacked in the rock (K3: where a tunnel
/// climbs more than a walk can): the 8 m column at `s` through the floor of
/// the upper chamber (its floor at `hi`) and the ceiling of the lower (at
/// `lo`), each `half` m to a side (10, or 7 where there's little room) and
/// off-center: the upper toward `dh` (where its tube goes), the lower toward
/// `dx`; walled between them, a lift up one half and a drop shaft down the
/// other. Returns the chambers (upper, lower) and the walks down and up
/// between where each stands (`Chamber::stand`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn shaft_pair(k: &mut Kiln, site: usize, s: [f64; 2], dh: [f64; 2], dx: [f64; 2], hi: f64, lo: f64, half: f64) -> (usize, usize, Vec<Leg>, Vec<Leg>) {
    const HIGH: f64 = 5.0;
    let (off, stand) = pair_offsets(half);
    let surface = k.height(s);
    let upper = chamber_open(k, site, add(s, scale(dh, off)), half, HIGH, hi, surface, &[], &[s]);
    let lower = chamber_open(k, site, add(s, scale(dx, off)), half, HIGH, lo, surface, &[s], &[]);
    k.chambers[upper].stand = add(s, scale(dh, stand));
    k.chambers[lower].stand = add(s, scale(dx, stand));
    k.chambers[upper].blocked.push((scale(dh, -1.0), 0.57));
    k.chambers[lower].blocked.push((scale(dx, -1.0), 0.57));
    k.chambers[upper].shafts += 1;
    k.chambers[lower].shafts += 1;
    k.chambers[upper].pair = Some(lower);
    k.chambers[lower].pair = Some(upper);
    for (a, b) in [([-1.0, -1.0], [1.0, -1.0]), ([1.0, -1.0], [1.0, 1.0]), ([1.0, 1.0], [-1.0, 1.0]), ([-1.0, 1.0], [-1.0, -1.0])] {
        let (p, q) = (add(s, scale(a, SHAFT + 0.3)), add(s, scale(b, SHAFT + 0.3)));
        k.fixed.push(strip(p, q, 0.3, lo + HIGH, hi - 1.0, Kind::Block));
    }
    // The halves: the lift's on the upper chamber's side (stepped off it
    // straight onto that floor, never over the drop shaft's opening), the
    // drop shaft's on the lower's (landed in, never walked out over the lift).
    let side = pair_side(dh, dx);
    let half2 = if side[0] != 0.0 { [SHAFT / 2.0 - 0.1, SHAFT - 0.1] } else { [SHAFT - 0.1, SHAFT / 2.0 - 0.1] };
    let (up, down) = (add(s, scale(side, SHAFT / 2.0)), sub(s, scale(side, SHAFT / 2.0)));
    lift_volume(k, up, half2, lo, hi);
    drop_volume(k, down, half2, hi, lo);
    let (top, bottom) = (k.chambers[upper].stand, k.chambers[lower].stand);
    let edge = sub(s, scale(side, SHAFT + 0.8));
    let down_legs = drop_legs(down, hi, lo, top, edge, bottom, (false, false));
    let up_legs = lift_legs(up, half2, lo, hi, bottom, top, false);
    (upper, lower, down_legs, up_legs)
}

/// How far a shaft pair's chambers of half-side `half` stand off their
/// shaft, and where routes through them stand.
pub(crate) fn pair_offsets(half: f64) -> (f64, f64) {
    if half >= 10.0 {
        (5.0, 8.0)
    } else {
        (2.0, 6.5)
    }
}

/// The axis a shaft pair's lift half is on: toward its upper chamber's side
/// and away from its lower's, as nearly as an axis can.
pub(crate) fn pair_side(dh: [f64; 2], dx: [f64; 2]) -> [f64; 2] {
    let axes = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]];
    let score = |a: &[f64; 2]| dot(*a, dh).min(-dot(*a, dx));
    axes.into_iter().fold(axes[0], |b, a| if score(&a) > score(&b) + 1e-9 { a } else { b })
}

/// A leg walked the other way.
pub(crate) fn rev(l: &Leg) -> Leg {
    let mut path = l.path.clone();
    path.reverse();
    let z = l.z.as_ref().map(|z| z.iter().rev().copied().collect());
    Leg { kind: l.kind.clone(), path, z }
}

/// Down a mouth's trench into its chamber: from its head to where it stands.
pub(crate) fn trench_leg(c: &Chamber) -> Leg {
    Leg { kind: LegKind::Road, path: vec![c.top, c.foot, c.center], z: Some(vec![c.surface, c.floor, c.floor]) }
}

/// Where a great site's tube routes start and end (K2; the rest are `under`'s).
pub(crate) enum Ends {
    /// A route inside `site`, from `from` on the ground (down chamber `a`'s
    /// trench) to chamber `b` (or its trench's head, if it has one), or to
    /// `to` in it (beside a shaft at its center).
    Inside { from: Option<[f64; 2]>, to: Option<[f64; 2]> },
}

/// The tube between chambers `a` and `b` (K1, K2): 6 m wide, its floor level
/// from one chamber's floor to the other's but dipped where the ground does
/// (keeping `TUBE_DEPTH` under it, no steeper than `TUBE_GRADE`), in 8 m
/// segments of turned boxes (floor, walls, ceiling); and its route.
pub(crate) fn tube(k: &mut Kiln, a: usize, b: usize, ends: Ends) {
    let (ca, cb) = (k.chambers[a].clone(), k.chambers[b].clone());
    let (pts, floor) = bore(k, a, b);
    // The route: down a's trench (if it has one), along the tube, up b's.
    let (mut path, mut z) = if ca.trench { (vec![ca.top, ca.foot, ca.center], vec![ca.surface, ca.floor, ca.floor]) } else { (vec![ca.stand], vec![ca.floor]) };
    path.extend(pts);
    z.extend(floor);
    let to_inside = match ends {
        Ends::Inside { to, .. } => to,
    };
    path.push(to_inside.unwrap_or(cb.stand));
    z.push(cb.floor);
    if cb.trench && to_inside.is_none() {
        path.extend([cb.foot, cb.top]);
        z.extend([cb.floor, cb.surface]);
    }
    let Ends::Inside { from, .. } = ends;
    let from = from.filter(|_| ca.trench);
    let (to, ra, rb): (Option<[f64; 2]>, usize, usize) = (None, ca.site, ca.site);
    let mut legs = Vec::new();
    if let Some(f) = from {
        legs.push(Leg { kind: LegKind::Road, path: vec![f, path[0]], z: None });
    }
    let last = *path.last().unwrap();
    legs.push(Leg { kind: LegKind::Road, path, z: Some(z) });
    if let Some(t) = to {
        legs.push(Leg { kind: LegKind::Road, path: vec![last, t], z: None });
    }
    k.tunnels.push(Route { a: ra, b: rb, legs, one_way: false });
}

/// The tube between chambers `a` and `b` (as `tube`), without a route:
/// its floor's points from inside `a`'s wall to inside `b`'s, and their heights.
pub(crate) fn bore(k: &mut Kiln, a: usize, b: usize) -> (Vec<[f64; 2]>, Vec<f64>) {
    let (ca, cb) = (k.chambers[a].clone(), k.chambers[b].clone());
    let TubeFloor { pts, floor, step, u, .. } = tube_floor(k, &ca, &cb);
    let v = [-u[1], u[0]];
    let n = pts.len() - 1;
    let l = dist(ca.center, cb.center);
    // Its walls and ceiling start at the chambers' walls, never inside them
    // (where they'd stand in the way of another tube's): along each wall's
    // line, where it leaves the one chamber and meets the other.
    let leave = |c: &Chamber, off: f64, dir: [f64; 2]| {
        let w = [-dir[1], dir[0]];
        (0..2).filter(|&i| dir[i].abs() > 1e-9).map(|i| (c.half * dir[i].signum() - w[i] * off) / dir[i]).fold(f64::MAX, f64::min)
    };
    let wall_span = |off: f64| (leave(&ca, off, u), l - leave(&cb, -off, scale(u, -1.0)));
    let spans = [wall_span(-(TUBE + 0.3)), wall_span(TUBE + 0.3)];
    let roof = (spans[0].0.min(spans[1].0), spans[0].1.max(spans[1].1));
    let s_at = |p: [f64; 2]| dot(sub(p, ca.center), u);
    let clip = |p: [f64; 2], q: [f64; 2], span: (f64, f64)| {
        let (sp, sq) = (s_at(p) - 0.3, s_at(q) + 0.3);
        let (s0, s1) = (sp.max(span.0), sq.min(span.1));
        (s1 - s0 > 0.1).then(|| (add(ca.center, scale(u, s0)), add(ca.center, scale(u, s1))))
    };
    for j in 0..n {
        let (p, q) = (pts[j], pts[j + 1]);
        let (z0, z1) = (floor[j], floor[j + 1]);
        let (lo, hi) = (z0.min(z1), z0.max(z1));
        let mid = scale(add(p, q), 0.5);
        if hi - lo < 1e-6 {
            // (Overlapping the next by a little: abutting exactly, a point on
            // the seam could round to neither.)
            k.fixed.push(strip(sub(p, scale(u, 0.05)), add(q, scale(u, 0.05)), TUBE, lo - 1.0, lo, Kind::Block));
        } else {
            // A wedge rising toward its higher end.
            let up = if z1 > z0 { u } else { scale(u, -1.0) };
            let w = CoverBox::turned([mid[0] as f32, mid[1] as f32], [(step / 2.0 + 0.05) as f32, TUBE as f32], [up[0] as f32, up[1] as f32], (lo - 1.0) as f32, hi as f32, Kind::Block, 0);
            k.fixed.push(w.wedge(lo as f32));
        }
        for (side, span) in [-1.0, 1.0].into_iter().zip(spans) {
            let off = scale(v, side * (TUBE + 0.3));
            if let Some((p, q)) = clip(p, q, span) {
                k.fixed.push(strip(add(p, off), add(q, off), 0.3, lo - 1.0, hi + TUBE_HIGH + 0.6, Kind::Block));
            }
        }
        if let Some((p, q)) = clip(p, q, roof) {
            k.fixed.push(strip(p, q, TUBE + 0.6, hi + TUBE_HIGH, hi + TUBE_HIGH + 0.6, Kind::Block));
        }
    }
    k.chambers[a].outs.push(u);
    k.chambers[b].outs.push(scale(u, -1.0));
    let (lo, hi) = floor.iter().fold((f64::MAX, f64::MIN), |(l, h), &f| (l.min(f), h.max(f)));
    k.bores.push((pts[0], pts[n], lo - 1.0, hi + TUBE_HIGH + 0.6, (a, b)));
    (pts, floor)
}

/// A tube dug: from, to, the heights it takes, its chambers.
pub(crate) type Bored = ([f64; 2], [f64; 2], f64, f64, (usize, usize));

/// A tube's floor (as `bore` digs it).
pub(crate) struct TubeFloor {
    /// From a meter inside one chamber's wall to a meter inside the other's.
    pub pts: Vec<[f64; 2]>,
    pub floor: Vec<f64>,
    /// How low the ground lets it be at each point: `TUBE_DEPTH` under the
    /// ground there, or rising from wherever that's lower no steeper than
    /// `TUBE_GRADE`. The floor is this or the line between the chambers'
    /// floors, whichever's lower: it meets them only where this is no lower.
    pub cone: Vec<f64>,
    pub step: f64,
    pub u: [f64; 2],
}

/// The floor of the tube from chamber `ca` to `cb`: level from one floor to
/// the other but dipped where the ground does (keeping `TUBE_DEPTH` under
/// it, no steeper than `TUBE_GRADE`), in 8 m steps.
pub(crate) fn tube_floor(k: &Kiln, ca: &Chamber, cb: &Chamber) -> TubeFloor {
    let l = dist(ca.center, cb.center);
    let u = scale(sub(cb.center, ca.center), 1.0 / l);
    let v = [-u[1], u[0]];
    let exit = |half: f64| half / u[0].abs().max(u[1].abs());
    let (s0, s1) = (exit(ca.half) - 1.0, l - exit(cb.half) + 1.0);
    let n = ((s1 - s0) / 8.0).ceil().max(1.0) as usize;
    let at = |j: usize| add(ca.center, scale(u, s0 + (s1 - s0) * j as f64 / n as f64));
    let ground: Vec<f64> = (0..=n)
        .map(|j| {
            let p = at(j);
            [-4.0, 0.0, 4.0].iter().flat_map(|&x| [-4.0, 0.0, 4.0].map(|y| k.height(add(add(p, scale(v, x)), scale(u, y))))).fold(f64::MAX, f64::min)
        })
        .collect();
    let step = (s1 - s0) / n as f64;
    let cone: Vec<f64> = (0..=n).map(|j| (0..=n).map(|i| ground[i] - TUBE_DEPTH + TUBE_GRADE * step * (i as f64 - j as f64).abs()).fold(f64::MAX, f64::min)).collect();
    let floor = (0..=n).map(|j| cone[j].min(lerp(ca.floor, cb.floor, j as f64 / n as f64))).collect();
    TubeFloor { pts: (0..=n).map(at).collect(), floor, cone, step, u }
}

/// A chamber's walls, open to its trench and its tubes (shielded, at a
/// spawn room's foot).
pub(crate) fn walls(k: &mut Kiln, i: usize) {
    let c = k.chambers[i].clone();
    let n = [-c.e[1], c.e[0]];
    for normal in [c.e, scale(c.e, -1.0), n, scale(n, -1.0)] {
        let along = [-normal[1], normal[0]];
        // (from, to, a tube's: not the trench's)
        let mut gaps: Vec<(f64, f64, bool)> = Vec::new();
        if c.trench && dot(normal, c.e) < -0.5 {
            gaps.push((-4.0, 4.0, false)); // the trench
        }
        let end = c.half + 0.6;
        for &out in &c.outs {
            let o = dot(out, normal);
            if o > 0.05 {
                // Where the tube crosses this side's line: its walls' outer
                // faces, and a little; any of it that's on this side (a tube
                // that leaves by a corner crosses two).
                let x = dot(scale(out, (c.half + 0.3) / o), along);
                let w = (TUBE + 0.6) / o + 0.3;
                if x + w > -end && x - w < end {
                    gaps.push(((x - w).max(-end), (x + w).min(end), true));
                }
            }
        }
        gaps.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());
        let mut from = -c.half - 0.6;
        let wall = |k: &mut Kiln, f: f64, t: f64| {
            if t - f > 0.2 {
                let side = add(c.center, scale(normal, c.half + 0.3));
                k.fixed.push(strip(add(side, scale(along, f)), add(side, scale(along, t)), 0.3, c.floor - 1.0, c.floor + c.high + 0.6, Kind::Block));
            }
        };
        for &(g0, g1, _) in &gaps {
            wall(k, from, g0);
            from = from.max(g1);
        }
        wall(k, from, c.half + 0.6);
        if let Some((site, f)) = c.shield {
            let side = add(c.center, scale(normal, c.half + 0.3));
            for &(g0, g1, _) in gaps.iter().filter(|g| g.2) {
                k.fixed.push(strip(add(side, scale(along, g0)), add(side, scale(along, g1)), 0.2, c.floor - 1.0, c.floor + c.high + 0.6, Kind::Shield).site_shield(site as u16, f));
            }
        }
    }
}

/// The world axis nearest the direction `d`.
pub(crate) fn axis_of(d: [f64; 2]) -> [f64; 2] {
    if d[0].abs() >= d[1].abs() {
        [d[0].signum(), 0.0]
    } else {
        [0.0, d[1].signum()]
    }
}

/// `p` snapped to the nearest height cells' corner.
pub(crate) fn snap(p: [f64; 2]) -> [f64; 2] {
    let r = TERRAIN_RES as f64;
    [(p[0] / r).round() * r, (p[1] / r).round() * r]
}
