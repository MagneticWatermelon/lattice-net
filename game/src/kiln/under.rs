//! Kiln's tunnels (K3, docs/kiln.md "Tunnels"): every base's own and the
//! Undervein between sites.
//!
//! - **A base's tunnels.** Its undercroft, a hall in the rock under the
//!   point nearest its spawn room's level, a shaft (a lift up, a drop shaft
//!   down) to the ground beside the point; the spawn room's covered exit, a
//!   shaft down through the middle of the spawn room to a chamber under it
//!   whose tunnel to the undercroft is shielded for the site's holder (the
//!   spawn room's shield, at the tunnel's inner end); a chamber under a
//!   second point with its own shaft; an approach tunnel from the ground
//!   150-300 m out, down a trench. A cenote's undercroft is its cavern, down
//!   a trench from its pit's floor.
//! - **The Undervein.** Tubes under the lanes: each sector's two Tunnel
//!   mouths (K1), from the one to the citadel's undercroft and from the
//!   other to its first biome base's, and two under each rift, down a shaft
//!   on one side to a junction under its floor (a trench up to it), up a
//!   shaft on the other.
//!
//! A tunnel that climbs more than a walk can (`GRADE`) takes a shaft between
//! two chambers stacked in the rock (`deep::shaft_pair`). Long tubes have a
//! chamber every 150 m or so, and where one runs more than 400 m between
//! stops (junctions and exits: `stretches`), an exit off one of them: a
//! chamber beside it, level with it, a shaft up to open ground (`exits`).
//! Built in two passes: first every chamber, shaft and mouth (each may level
//! the ground round it), then the tubes between them, which keep under
//! whatever ground there is by then.

use std::collections::HashMap;

use super::deep::{self, axis_of, rev, snap, trench_leg, Chamber};
use super::build::free_dir;
use super::lanes::{Leg, LegKind, Route};
use super::*;
use crate::world::{SiteKind, CELLS};

/// A base's undercroft: half its side, its headroom, how far its shaft is
/// off its center.
const U_HALF: f64 = 12.0;
const U_HIGH: f64 = 6.0;
const U_OFF: f64 = 7.0;
/// The other chambers with a shaft up (a second point's, a spawn room's foot).
const HALF: f64 = 10.0;
const HIGH: f64 = 5.0;
const OFF: f64 = 5.0;
/// How far under the ground at its shaft a chamber's floor is.
const DEPTH: f64 = 16.0;
/// A chamber along a long tube.
const MID_HALF: f64 = 10.0;
const MID_HIGH: f64 = 6.0;
const MID_EVERY: f64 = 150.0;
/// The steepest a tube climbs between two chambers: more, and a shaft takes it.
const GRADE: f64 = 0.3;
/// Rock kept over a chamber's ceiling.
const ROCK: f64 = 2.5;
const SHAFT: f64 = 4.0;
/// A tube's half-width to its walls' outer faces.
const TUBE_OUT: f64 = 3.6;
/// The farthest a tunnel runs between stops: a junction or an exit.
pub(crate) const STRETCH: f64 = 400.0;

/// A tunnel to dig: its chambers in order and how each joins the next, the
/// sites it runs between, and where on the ground it starts and ends
/// (beyond a mouth's trench).
struct Net {
    a: usize,
    b: usize,
    from: Option<[f64; 2]>,
    to: Option<[f64; 2]>,
    hops: Vec<usize>,
    links: Vec<Link>,
}

enum Link {
    Bore,
    /// A shaft between stacked chambers: the walks down it and up it.
    Shaft { down: Vec<Leg>, up: Vec<Leg> },
}

/// A box of rock something takes: its footprint and heights.
#[derive(Clone, Copy)]
struct Rect {
    min: [f64; 2],
    max: [f64; 2],
    z0: f64,
    z1: f64,
}

impl Rect {
    fn chamber(c: [f64; 2], half: f64, floor: f64, high: f64) -> Self {
        let h = half + 0.6;
        Self { min: sub(c, [h, h]), max: add(c, [h, h]), z0: floor - 1.0, z1: floor + high + 0.6 }
    }

    fn near(&self, p: [f64; 2], m: f64) -> bool {
        p[0] > self.min[0] - m && p[0] < self.max[0] + m && p[1] > self.min[1] - m && p[1] < self.max[1] + m
    }

    fn overlaps(&self, o: &Rect, m: f64, mz: f64) -> bool {
        self.min[0] < o.max[0] + m && o.min[0] < self.max[0] + m && self.min[1] < o.max[1] + m && o.min[1] < self.max[1] + m && self.z0 < o.z1 + mz && o.z0 < self.z1 + mz
    }
}

/// A tube's run: from, to, its heights, the chambers at its ends.
#[derive(Clone, Copy)]
struct Seg {
    a: [f64; 2],
    b: [f64; 2],
    z0: f64,
    z1: f64,
    ends: (usize, usize),
}

/// What's in the rock: chambers and tubes, dug and planned.
fn taken(k: &Kiln, planned: &[(usize, usize)]) -> (Vec<(usize, Rect)>, Vec<Seg>) {
    let rects = k.chambers.iter().enumerate().map(|(i, c)| (i, Rect::chamber(c.center, c.half, c.floor, c.high))).collect();
    let mut segs: Vec<Seg> = k.bores.iter().map(|b| Seg { a: b.0, b: b.1, z0: b.2, z1: b.3, ends: b.4 }).collect();
    for &(x, y) in planned {
        let (cx, cy) = (&k.chambers[x], &k.chambers[y]);
        segs.push(Seg { a: cx.center, b: cy.center, z0: cx.floor.min(cy.floor) - 1.0, z1: cx.floor.max(cy.floor) + 5.1, ends: (x, y) });
    }
    (rects, segs)
}

fn seg_near_rect(s: &Seg, r: &Rect, m: f64) -> bool {
    if s.z0 >= r.z1 + 3.0 || r.z0 >= s.z1 + 3.0 {
        return false;
    }
    let n = (dist(s.a, s.b) / 2.0).ceil().max(1.0) as usize;
    (0..=n).any(|i| r.near(add(s.a, scale(sub(s.b, s.a), i as f64 / n as f64)), m))
}

fn seg_near_seg(s: &Seg, o: &Seg, m: f64) -> bool {
    if s.z0 >= o.z1 + 3.0 || o.z0 >= s.z1 + 3.0 {
        return false;
    }
    let n = (dist(s.a, s.b) / 2.0).ceil().max(1.0) as usize;
    (0..=n).any(|i| seg_dist(add(s.a, scale(sub(s.b, s.a), i as f64 / n as f64)), o.a, o.b).0 < m)
}

fn shares(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 == b.0 || a.0 == b.1 || a.1 == b.0 || a.1 == b.1
}

/// Whether new chambers `rects` and tubes `segs` keep clear of what's in
/// the rock (and of `reserved`): a tube may meet only its own ends.
fn free(k: &Kiln, planned: &[(usize, usize)], rects: &[Rect], segs: &[Seg], reserved: &[Rect]) -> bool {
    let (have, tubes) = taken(k, planned);
    for r in rects {
        if have.iter().any(|(_, o)| r.overlaps(o, 3.0, 3.0)) || reserved.iter().any(|o| r.overlaps(o, 3.0, 3.0)) {
            return false;
        }
        if tubes.iter().any(|t| seg_near_rect(t, r, TUBE_OUT + 2.0)) {
            return false;
        }
    }
    for s in segs {
        // (Its own ends' rects are new or among `have`, by index.)
        if have.iter().any(|(i, o)| *i != s.ends.0 && *i != s.ends.1 && seg_near_rect(s, o, TUBE_OUT + 2.0)) {
            return false;
        }
        if reserved.iter().any(|o| seg_near_rect(s, o, TUBE_OUT + 2.0)) {
            return false;
        }
        if tubes.iter().any(|t| !shares(t.ends, s.ends) && seg_near_seg(s, t, 2.0 * TUBE_OUT + 3.0)) {
            return false;
        }
    }
    true
}

/// Whether there's at least `top` of ground all over the square of half-side `h` at `c`.
fn rock_over(k: &Kiln, c: [f64; 2], h: f64, top: f64) -> bool {
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [h, h]), add(c, [h, h]));
    (y0..=y1).all(|iy| (x0..=x1).all(|ix| k.h[iy * TERRAIN_N + ix] >= top))
}

/// The lowest ground over the square of half-side `h` at `c`.
fn lowest(k: &Kiln, c: [f64; 2], h: f64) -> f64 {
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [h, h]), add(c, [h, h]));
    (y0..=y1).flat_map(|iy| (x0..=x1).map(move |ix| (ix, iy))).map(|(ix, iy)| k.h[iy * TERRAIN_N + ix]).fold(f64::MAX, f64::min)
}

const CELL: f64 = 64.0;

fn cell(p: [f64; 2]) -> (i32, i32) {
    ((p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32)
}

type Segs = HashMap<(i32, i32), Vec<([f64; 2], [f64; 2])>>;
/// Tubes dug, by their chambers: their floors' points and heights.
type Dug = HashMap<(usize, usize), (Vec<[f64; 2]>, Vec<f64>)>;

/// The ways on the ground (lanes, the routes inside sites where they're on
/// or over it, tunnels' legs to their mouths) and the boxes on it, by cell,
/// for how near something is to them.
struct Ground {
    ways: Segs,
    boxes: HashMap<(i32, i32), Vec<Rect>>,
}

impl Ground {
    fn new(k: &Kiln) -> Self {
        let mut ways: Segs = HashMap::new();
        let mut add_seg = |a: [f64; 2], b: [f64; 2]| {
            let (c0, c1) = (cell([a[0].min(b[0]), a[1].min(b[1])]), cell([a[0].max(b[0]), a[1].max(b[1])]));
            for cy in c0.1..=c1.1 {
                for cx in c0.0..=c1.0 {
                    ways.entry((cx, cy)).or_default().push((a, b));
                }
            }
        };
        for c in &k.corridors {
            for w in c.windows(2) {
                add_seg(w[0], w[1]);
            }
        }
        for r in k.internal.iter().chain(&k.tunnels) {
            for l in &r.legs {
                for (j, w) in l.path.windows(2).enumerate() {
                    let up = match &l.z {
                        None => true,
                        Some(z) => z[j].max(z[j + 1]) > k.height(w[0]).min(k.height(w[1])) - 3.0,
                    };
                    if up {
                        add_seg(w[0], w[1]);
                    }
                }
            }
        }
        let mut boxes: HashMap<(i32, i32), Vec<Rect>> = HashMap::new();
        for b in k.boxes.iter().chain(&k.fixed) {
            let r = Rect { min: [b.min[0] as f64, b.min[1] as f64], max: [b.max[0] as f64, b.max[1] as f64], z0: b.bottom as f64, z1: b.top as f64 };
            let (c0, c1) = (cell(r.min), cell(r.max));
            for cy in c0.1..=c1.1 {
                for cx in c0.0..=c1.0 {
                    boxes.entry((cx, cy)).or_default().push(r);
                }
            }
        }
        Self { ways, boxes }
    }

    /// How far `p` is from the nearest way (up to a cell's width; beyond, MAX).
    fn way(&self, p: [f64; 2]) -> f64 {
        let c = cell(p);
        let mut d = f64::MAX;
        for dy in -1..=1 {
            for dx in -1..=1 {
                for &(a, b) in self.ways.get(&(c.0 + dx, c.1 + dy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                    d = d.min(seg_dist(p, a, b).0);
                }
            }
        }
        d
    }

    /// Whether a box stands within `r` of `p` between heights `z0` and `z1`.
    fn boxed(&self, p: [f64; 2], r: f64, z0: f64, z1: f64) -> bool {
        let c = cell(p);
        (-1..=1).any(|dy| (-1..=1).any(|dx| self.boxes.get(&(c.0 + dx, c.1 + dy)).is_some_and(|v| v.iter().any(|b| b.near(p, r) && b.z1 > z0 && b.z0 < z1))))
    }
}

fn hole_near(k: &Kiln, p: [f64; 2], r: f64) -> bool {
    if k.holes.is_empty() {
        return false;
    }
    let res = TERRAIN_RES as f64;
    let (x0, x1) = (((p[0] - r) / res).floor().max(0.0) as usize, (((p[0] + r) / res).floor() as usize).min(CELLS - 1));
    let (y0, y1) = (((p[1] - r) / res).floor().max(0.0) as usize, (((p[1] + r) / res).floor() as usize).min(CELLS - 1));
    (y0..=y1).any(|cy| (x0..=x1).any(|cx| {
        let j = cy * CELLS + cx;
        k.holes[j / 64] & (1 << (j % 64)) != 0
    }))
}

/// Where a shaft's mouth is looked for and how it takes the ground: the
/// rings round what it's near, how far the ground may rise and fall over
/// the mouth's 22 m, the pad leveled round it (radius, blend), and how far
/// that may lower the ground over its chamber.
struct Lay {
    rings: &'static [f64],
    rough: f64,
    pad: (f64, f64),
    sink: f64,
}

/// In a site, where the ground's level already.
const LEVEL: Lay = Lay { rings: &[12.0, 16.0, 20.0, 25.0, 30.0, 36.0, 42.0], rough: 1.0, pad: (SHAFT + 1.0, 2.0), sink: 0.0 };
/// Out in the open (an exit along a long tunnel, on a hillside maybe): a
/// level pad cut and filled round it.
const ROUGH: Lay = Lay { rings: &[25.0, 30.0, 36.0, 42.0, 50.0, 60.0, 72.0, 86.0, 100.0], rough: 10.0, pad: (SHAFT + 6.0, 12.0), sink: 5.0 };

/// Whether the ground at `c` takes a shaft's mouth: about `z` high (any,
/// if `z` is NaN), level enough (`lay`), open (no ways, roads, boxes,
/// lifts, holes or points near).
fn open_ground(k: &Kiln, g: &Ground, c: [f64; 2], z: f64, lay: &Lay) -> bool {
    let surface = k.height(c);
    if (surface - z).abs() > 2.5 {
        return false;
    }
    let r = SHAFT + 7.0;
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [r, r]), add(c, [r, r]));
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            if dist(Kiln::pos(ix, iy), c) > r {
                continue;
            }
            let j = iy * TERRAIN_N + ix;
            if k.mask[j] & MASK_ROAD != 0 {
                return false;
            }
            lo = lo.min(k.h[j]);
            hi = hi.max(k.h[j]);
        }
    }
    // (Nothing on the ground its pad would move.)
    let reach = (SHAFT + 7.0).max(lay.pad.0 + lay.pad.1 + 2.0);
    hi - lo <= lay.rough
        && g.way(c) > SHAFT + 8.0
        && !g.boxed(c, reach, surface - 3.0 - lay.rough, surface + 10.0)
        && !hole_near(k, c, SHAFT + 8.0)
        && !k.points.iter().any(|p| dist([p.pos[0] as f64, p.pos[1] as f64], c) < SHAFT + 6.0)
        && !k.volumes.iter().any(|v| dist([(v.min[0] + v.max[0]) as f64 / 2.0, (v.min[1] + v.max[1]) as f64 / 2.0], c) < SHAFT + 10.0)
}

/// Where a chamber with a shaft up to the ground goes.
struct Spot {
    corner: [f64; 2],
    /// The axis the shaft stands off the chamber's center along (the
    /// chamber behind it), and the one its lift's half is along.
    back: [f64; 2],
    side: [f64; 2],
    center: [f64; 2],
    surface: f64,
    floor: f64,
}

/// A chamber that isn't dug: for asking whether a tube to it would walk.
fn ghost(center: [f64; 2], half: f64, floor: f64, high: f64) -> Chamber {
    Chamber { site: 0, top: center, e: [1.0, 0.0], foot: center, trench: false, center, half, high, surface: floor, floor, outs: Vec::new(), stand: center, shield: None, blocked: Vec::new(), shafts: 0, ground: false, pair: None }
}

/// Whether a straight tube between two chambers walks: its floor meets
/// both of theirs (the ground doesn't dip it under either near its end) and
/// it climbs no steeper than `GRADE` between them.
fn walkable(k: &Kiln, ca: &Chamber, cb: &Chamber) -> bool {
    let f = deep::tube_floor(k, ca, cb);
    let n = f.floor.len() - 1;
    (f.floor[0] - ca.floor).abs() < 0.5 && (f.floor[n] - cb.floor).abs() < 0.5 && (ca.floor - cb.floor).abs() <= GRADE * (f.step * n as f64).max(1.0)
}

/// The directions tubes leave chamber `ch` in, dug and planned.
fn outs(k: &Kiln, planned: &[(usize, usize)], ch: usize) -> Vec<[f64; 2]> {
    let c = k.chambers[ch].center;
    let mut v = k.chambers[ch].outs.clone();
    for &(x, y) in planned {
        let other = if x == ch { y } else if y == ch { x } else { continue };
        let d = sub(k.chambers[other].center, c);
        v.push(scale(d, 1.0 / len(d)));
    }
    v
}

/// Whether a tube leaving chamber `ch` along `dir` would crowd one already
/// leaving it (their walls would cross outside it) or its trench.
fn crowded(k: &Kiln, planned: &[(usize, usize)], ch: usize, dir: [f64; 2]) -> bool {
    let c = &k.chambers[ch];
    // Two tubes 7.2 m wide part outside a chamber's walls if they leave it
    // far enough apart: 50 degrees for the smallest.
    let apart = if c.half >= 12.0 { 0.75 } else { 0.64 };
    (c.trench && dot(dir, scale(c.e, -1.0)) > 0.5) || c.blocked.iter().any(|&(b, cos)| dot(b, dir) > cos) || outs(k, planned, ch).iter().any(|&o| dot(o, dir) > apart)
}

/// A chamber of half-side `half` (its shaft `off` m off its center) with a
/// shaft up to open ground on `lay`'s rings round `near` (a point) at about
/// its height, its floor `DEPTH` under that ground (or at `level`, lower
/// where the rock's thinner), under rock all over, clear of what's in the
/// rock and of `reserved`; its shaft not on the side toward `away` (where
/// its tubes go); its mouth laid as `lay` says. The first that `prefer`
/// likes, else the first.
#[allow(clippy::too_many_arguments)]
fn spot(k: &Kiln, g: &Ground, planned: &[(usize, usize)], near: [f64; 3], half: f64, off: f64, high: f64, level: Option<f64>, lay: &Lay, reserved: &[Rect], away: Option<[f64; 2]>, prefer: &dyn Fn(&Kiln, &Spot) -> bool) -> Option<Spot> {
    let mut fallback = None;
    for &r in lay.rings {
        for j in 0..24 {
            let corner = snap(add([near[0], near[1]], scale(unit(j as f64 * 15.0), r)));
            if !open_ground(k, g, corner, near[2], lay) {
                continue;
            }
            let surface = k.height(corner);
            let mut backs = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]];
            if let Some(o) = away {
                let to = sub(corner, o);
                backs.sort_by(|a, b| dot(*b, to).total_cmp(&dot(*a, to)));
            }
            for back in backs {
                let center = sub(corner, scale(back, off));
                if let Some(o) = away {
                    let d = sub(o, center);
                    if dot(d, back) >= 0.5 * len(d) {
                        continue;
                    }
                }
                let floor = match level {
                    Some(f) => f.min(lowest(k, center, half + 1.0) - high - 0.6 - ROCK - lay.sink),
                    None => surface - DEPTH,
                };
                if !rock_over(k, center, half + 1.0, floor + high + 0.6 + ROCK + lay.sink) {
                    continue;
                }
                let col = Rect { min: sub(corner, [SHAFT + 1.0, SHAFT + 1.0]), max: add(corner, [SHAFT + 1.0, SHAFT + 1.0]), z0: floor + high, z1: surface };
                if !free(k, planned, &[Rect::chamber(center, half, floor, high), col], &[], reserved) {
                    continue;
                }
                let s = Spot { corner, back, side: [-back[1], back[0]], center, surface, floor };
                if prefer(k, &s) {
                    return Some(s);
                }
                if fallback.is_none() {
                    fallback = Some(s);
                }
            }
        }
    }
    fallback
}

/// Digs a spot's chamber and its shaft (the ground's level there already).
fn dig_spot(k: &mut Kiln, site: usize, s: &Spot, half: f64, high: f64, lay: &Lay) -> usize {
    let c = deep::chamber_open(k, site, s.center, half, high, s.floor, s.surface, &[s.corner], &[]);
    k.chambers[c].stand = sub(s.center, scale(s.back, 3.0));
    k.chambers[c].blocked.push((s.back, 0.74));
    let above = add(s.corner, scale(s.side, SHAFT + 3.0));
    let from = sub(s.corner, scale(s.side, SHAFT + 5.8));
    let stand = k.chambers[c].stand;
    deep::shaft_with(k, site, s.corner, c, s.side, stand, above, Some(from), lay.pad);
    c
}

/// A mouth's trench and chamber for a tunnel to the chamber `to`, on open
/// ground about `r` m from `around` (the first ring that has room): clear
/// of ways, sites, rifts, boxes and holes, its trench heading toward `to`,
/// the tube from it leaving `to` clear of its others. One a straight tube
/// walks to first.
#[allow(clippy::too_many_arguments)]
fn find_mouth(k: &Kiln, g: &Ground, planned: &[(usize, usize)], around: [f64; 2], rings: &[f64], to: usize, start_deg: f64) -> Option<([f64; 2], [f64; 2])> {
    let target = k.chambers[to].clone();
    let mut fallback = None;
    for &r in rings {
        for j in 0..36 {
            let deg = start_deg + if j % 2 == 0 { 1.0 } else { -1.0 } * ((j + 1) / 2) as f64 * 10.0;
            let p = add(around, scale(unit(deg), r));
            let e = axis_of(sub(target.center, p));
            let n = [-e[1], e[0]];
            let top = snap(p);
            let center = add(top, scale(e, 48.0));
            let back = sub(center, target.center);
            if crowded(k, planned, to, scale(back, 1.0 / len(back))) {
                continue;
            }
            // The leveled rectangle (as `deep::mouth_sized`), and room round it.
            let corners = [sub(sub(top, scale(e, 10.0)), scale(n, 18.0)), add(add(center, scale(e, 18.0)), scale(n, 18.0))];
            let (min, max) = ([corners[0][0].min(corners[1][0]), corners[0][1].min(corners[1][1])], [corners[0][0].max(corners[1][0]), corners[0][1].max(corners[1][1])]);
            let mut ok = true;
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            let mut y = min[1];
            'scan: while y <= max[1] {
                let mut x = min[0];
                while x <= max[0] {
                    let q = [x, y];
                    let j = {
                        let (ix, _, iy, _) = Kiln::samples(q, q);
                        iy * TERRAIN_N + ix
                    };
                    let site_gap = k.sites.iter().map(|s| dist(q, s.center) - s.half).fold(f64::MAX, f64::min);
                    // (On a rift's floor or well back from its walls.)
                    let rift = land::rift_clearance(q);
                    if k.mask[j] & (MASK_ROAD | MASK_PAD) != 0 || g.way(q) < 14.0 || site_gap < 20.0 || (rift < 50.0 && rift > -30.0) {
                        ok = false;
                        break 'scan;
                    }
                    lo = lo.min(k.h[j]);
                    hi = hi.max(k.h[j]);
                    x += 4.0;
                }
                y += 4.0;
            }
            if !ok || hi - lo > 12.0 {
                continue;
            }
            let mid = scale(add(min, max), 0.5);
            let reach = len(sub(max, min)) / 2.0 + 4.0;
            if g.boxed(mid, reach, f64::MIN, f64::MAX) || hole_near(k, mid, reach + 10.0) {
                continue;
            }
            if k.volumes.iter().any(|v| dist([(v.min[0] + v.max[0]) as f64 / 2.0, (v.min[1] + v.max[1]) as f64 / 2.0], mid) < reach + 20.0) {
                continue;
            }
            let surface = k.mean(add(top, scale(e, 20.0)), 26.0);
            let floor = surface - 12.0;
            let ch = Rect::chamber(center, 8.0, floor, 5.0);
            let tube = Seg { a: center, b: target.center, z0: floor.min(target.floor) - 1.0, z1: floor.max(target.floor) + 5.1, ends: (usize::MAX, to) };
            if !free(k, planned, &[ch], &[tube], &[]) {
                continue;
            }
            if walkable(k, &ghost(center, 8.0, floor, 5.0), &target) {
                return Some((top, e));
            }
            if fallback.is_none() {
                fallback = Some((top, e));
            }
        }
    }
    fallback
}

/// How chambers `a` and `b` join: the chambers after `a` (ending with `b`)
/// and the links between them. A straight tube if it walks (`walkable`);
/// else a shaft pair by the end that's too high, down to where the tube can
/// run (under whatever dips it, or the other end's level), and on from
/// there (which may want a pair by the other end too). Long tubes get
/// chambers along them where they fit.
fn connect(k: &mut Kiln, site: usize, planned: &mut Vec<(usize, usize)>, a: usize, b: usize) -> (Vec<usize>, Vec<Link>) {
    connect_at(k, site, planned, a, b, 0)
}

fn connect_at(k: &mut Kiln, site: usize, planned: &mut Vec<(usize, usize)>, a: usize, b: usize, depth: usize) -> (Vec<usize>, Vec<Link>) {
    let (ca, cb) = (k.chambers[a].clone(), k.chambers[b].clone());
    if walkable(k, &ca, &cb) {
        return bore_run(k, site, planned, a, b);
    }
    if depth >= 2 {
        k.notes.push(format!("site {site}: chambers {a} and {b} joined by a tube that doesn't walk"));
        return bore_run(k, site, planned, a, b);
    }
    let f = deep::tube_floor(k, &ca, &cb);
    let n = f.floor.len() - 1;
    let (a_binds, b_binds) = ((f.floor[0] - ca.floor).abs() >= 0.5, (f.floor[n] - cb.floor).abs() >= 0.5);
    // The end to put the shaft by: the one a dip's too near to walk down
    // from, else the higher.
    let at_a = if a_binds != b_binds { a_binds } else { ca.floor >= cb.floor };
    let (hi_i, far_i) = if at_a { (a, b) } else { (b, a) };
    let (hi, far) = (k.chambers[hi_i].clone(), k.chambers[far_i].clone());
    let dip = f.cone.iter().copied().fold(f64::MAX, f64::min) - 0.5;
    let low = if a_binds || b_binds { dip.min(far.floor) } else { far.floor };
    let toward = scale(sub(far.center, hi.center), 1.0 / dist(far.center, hi.center));
    let turn = |v: [f64; 2], deg: f64| {
        let (s, c) = libm::sincos(rad(deg));
        [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
    };
    let mut why = [0; 6];
    for (half, d) in [10.0, 7.0].into_iter().flat_map(|h| [12.0, 18.0, 24.0, 34.0, 46.0, 60.0].map(|d| (h, d))) {
        let (off, _) = deep::pair_offsets(half);
        if hi.half + d - off - half < 3.0 {
            continue;
        }
        for rot in [0.0, 40.0, -40.0, 80.0, -80.0, 130.0, -130.0, 180.0] {
            let dir = turn(toward, rot);
            let s = add(hi.center, scale(dir, hi.half + d));
            if dist(s, far.center) < far.half + 24.0 {
                why[0] += 1;
                continue;
            }
            if crowded(k, planned, hi_i, dir) {
                why[1] += 1;
                continue;
            }
            let dh = scale(dir, -1.0);
            let dx = scale(sub(far.center, s), 1.0 / dist(far.center, s));
            let (uc, xc) = (add(s, scale(dh, off)), add(s, scale(dx, off)));
            if !rock_over(k, uc, half + 1.0, hi.floor + 5.6 + ROCK) || !rock_over(k, xc, half + 1.0, low + 5.6 + ROCK) {
                why[2] += 1;
                continue;
            }
            let upper = ghost(uc, half, hi.floor, 5.0);
            if !walkable(k, &hi, &upper) {
                why[3] += 1;
                continue;
            }
            let rects = [Rect::chamber(uc, half, hi.floor, 5.0), Rect::chamber(xc, half, low, 5.0), Rect { min: sub(s, [5.0, 5.0]), max: add(s, [5.0, 5.0]), z0: low, z1: hi.floor }];
            let to_hi = Seg { a: hi.center, b: uc, z0: hi.floor - 1.0, z1: hi.floor + 5.1, ends: (hi_i, usize::MAX) };
            let to_far = Seg { a: xc, b: far.center, z0: low.min(far.floor) - 1.0, z1: low.max(far.floor) + 5.1, ends: (far_i, usize::MAX) };
            if !free(k, planned, &rects, &[to_hi, to_far], &[]) {
                why[4] += 1;
                continue;
            }
            let (upper, lower, down, up) = deep::shaft_pair(k, site, s, dh, dx, hi.floor, low, half);
            let mut hops = Vec::new();
            let mut links = Vec::new();
            if at_a {
                let (h1, l1) = bore_run(k, site, planned, a, upper);
                hops.extend(h1);
                links.extend(l1);
                links.push(Link::Shaft { down, up });
                hops.push(lower);
                let (h2, l2) = connect_at(k, site, planned, lower, b, depth + 1);
                hops.extend(h2);
                links.extend(l2);
            } else {
                let (h1, l1) = connect_at(k, site, planned, a, lower, depth + 1);
                hops.extend(h1);
                links.extend(l1);
                links.push(Link::Shaft { down, up });
                hops.push(upper);
                let (h2, l2) = bore_run(k, site, planned, upper, b);
                hops.extend(h2);
                links.extend(l2);
            }
            return (hops, links);
        }
    }
    k.notes.push(format!("site {site}: no room for a shaft between chambers {a} and {b} (too near the far end, crowded, no rock, no walk, not free: {why:?}; floors {:.1} {:.1} down to {low:.1})", ca.floor, cb.floor));
    bore_run(k, site, planned, a, b)
}

/// A straight run of tube from `a` to `b`, with chambers along it every
/// `MID_EVERY` m or so where they fit (under rock, the tubes to and from
/// them walking, clear of the rest).
fn bore_run(k: &mut Kiln, site: usize, planned: &mut Vec<(usize, usize)>, a: usize, b: usize) -> (Vec<usize>, Vec<Link>) {
    let (ca, cb) = (k.chambers[a].clone(), k.chambers[b].clone());
    let l = dist(ca.center, cb.center);
    let count = ((l / MID_EVERY).round() as usize).saturating_sub(1);
    let mut hops = Vec::new();
    let mut links = Vec::new();
    let mut last = a;
    let whole = deep::tube_floor(k, &ca, &cb);
    for i in 1..=count {
        let t = i as f64 / (count + 1) as f64;
        let c = add(ca.center, scale(sub(cb.center, ca.center), t));
        // On the tube's floor there, under rock.
        let j = ((whole.floor.len() - 1) as f64 * t).round() as usize;
        let floor = whole.floor[j].min(lowest(k, c, MID_HALF + 1.0) - MID_HIGH - 0.6 - ROCK);
        let mid = ghost(c, MID_HALF, floor, MID_HIGH);
        let prev = k.chambers[last].clone();
        if !walkable(k, &prev, &mid) || !walkable(k, &mid, &cb) {
            continue;
        }
        let rect = Rect::chamber(c, MID_HALF, floor, MID_HIGH);
        let seg = Seg { a: prev.center, b: c, z0: prev.floor.min(floor) - 1.0, z1: prev.floor.max(floor) + 5.1, ends: (last, usize::MAX) };
        if !free(k, planned, &[rect], &[seg], &[]) {
            continue;
        }
        let m = deep::chamber(k, site, c, MID_HALF, MID_HIGH, floor, k.height(c), &[]);
        planned.push((last, m));
        hops.push(m);
        links.push(Link::Bore);
        last = m;
    }
    planned.push((last, b));
    hops.push(b);
    links.push(Link::Bore);
    (hops, links)
}

/// Digs a net's tubes (each once: nets may share them) and lays its
/// routes: one both ways, or one each way where it has a shaft (a lift up,
/// a drop shaft down).
fn dig(k: &mut Kiln, net: &Net, dug: &mut Dug) {
    // Each piece: (forward legs, backward legs).
    let mut pieces: Vec<(Vec<Leg>, Vec<Leg>)> = Vec::new();
    let ground = |a: [f64; 2], b: [f64; 2]| Leg { kind: LegKind::Road, path: vec![a, b], z: None };
    let first = k.chambers[net.hops[0]].clone();
    if let Some(f) = net.from {
        let down = trench_leg(&first);
        pieces.push((vec![ground(f, first.top), down.clone()], vec![rev(&down), ground(first.top, f)]));
    }
    let mut one_way = false;
    for (i, link) in net.links.iter().enumerate() {
        let (x, y) = (net.hops[i], net.hops[i + 1]);
        match link {
            Link::Bore => {
                let (pts, floor) = if let Some((p, f)) = dug.get(&(x, y)) {
                    (p.clone(), f.clone())
                } else if let Some((p, f)) = dug.get(&(y, x)) {
                    (p.iter().rev().copied().collect(), f.iter().rev().copied().collect())
                } else {
                    let r = deep::bore(k, x, y);
                    dug.insert((x, y), r.clone());
                    r
                };
                let (cx, cy) = (&k.chambers[x], &k.chambers[y]);
                let mut path = vec![cx.stand];
                let mut z = vec![cx.floor];
                path.extend(pts);
                z.extend(floor);
                path.push(cy.stand);
                z.push(cy.floor);
                let leg = Leg { kind: LegKind::Road, path, z: Some(z) };
                pieces.push((vec![leg.clone()], vec![rev(&leg)]));
            }
            Link::Shaft { down, up } => {
                one_way = true;
                if k.chambers[x].floor > k.chambers[y].floor {
                    pieces.push((down.clone(), up.clone()));
                } else {
                    pieces.push((up.clone(), down.clone()));
                }
            }
        }
    }
    let last = k.chambers[*net.hops.last().unwrap()].clone();
    if let Some(t) = net.to {
        let down = trench_leg(&last);
        pieces.push((vec![rev(&down), ground(last.top, t)], vec![ground(t, last.top), down]));
    }
    let fwd: Vec<Leg> = pieces.iter().flat_map(|p| p.0.iter().cloned()).collect();
    if one_way {
        let back: Vec<Leg> = pieces.iter().rev().flat_map(|p| p.1.iter().cloned()).collect();
        k.tunnels.push(Route { a: net.a, b: net.b, legs: fwd, one_way: true });
        k.tunnels.push(Route { a: net.b, b: net.a, legs: back, one_way: true });
    } else {
        k.tunnels.push(Route { a: net.a, b: net.b, legs: fwd, one_way: false });
    }
}

/// Base `i`'s tunnels (as the module says): the nets to dig, and its undercroft.
fn base(k: &mut Kiln, g: &Ground, planned: &mut Vec<(usize, usize)>, i: usize) -> (Vec<Net>, Option<usize>) {
    let p = k.sites[i].clone();
    let Some(&(_, q, u, floor)) = k.rooms.iter().find(|r| r.0 == i) else {
        k.notes.push(format!("base {i} has no spawn room"));
        return (Vec::new(), None);
    };
    let mut nets = Vec::new();
    let mut pts: Vec<[f64; 3]> = k.points.iter().filter(|pt| pt.site as usize == i).map(|pt| [pt.pos[0] as f64, pt.pos[1] as f64, pt.pos[2] as f64]).collect();
    let rank = |pt: &[f64; 3]| (pt[2] - floor).abs() + 0.05 * dist([pt[0], pt[1]], q);
    pts.sort_by(|a, b| rank(a).total_cmp(&rank(b)));
    // The spawn room's foot, under it: reserved while the undercroft's placed.
    let foot = Rect::chamber(q, HALF, floor - DEPTH, HIGH);
    let col = Rect { min: sub(q, [SHAFT + 1.0, SHAFT + 1.0]), max: add(q, [SHAFT + 1.0, SHAFT + 1.0]), z0: floor - DEPTH, z1: floor };
    let foot_ghost = ghost(q, HALF, floor - DEPTH, HIGH);
    let (uc, upt) = if p.kind == SiteKind::Cenote {
        let Some(c) = cavern(k, i) else {
            k.notes.push(format!("cenote {i} has no room for its cavern"));
            return (Vec::new(), None);
        };
        (c, p.anchors[2])
    } else {
        let mut found = None;
        // An undercroft a straight tube from the spawn room's foot walks to.
        let prefer = |k: &Kiln, s: &Spot| walkable(k, &foot_ghost, &ghost(s.center, U_HALF, s.floor, U_HIGH));
        for pt in &pts {
            if let Some(s) = spot(k, g, planned, *pt, U_HALF, U_OFF, U_HIGH, None, &LEVEL, &[foot, col], Some(q), &prefer) {
                found = Some((s, [pt[0], pt[1]]));
                break;
            }
        }
        let Some((s, pt)) = found else {
            k.notes.push(format!("base {i} has no room for its undercroft"));
            return (Vec::new(), None);
        };
        (dig_spot(k, i, &s, U_HALF, U_HIGH, &LEVEL), pt)
    };
    // The covered exit: a shaft down the spawn room's middle (a lift in the
    // half toward one door, a drop shaft in the other's), to its foot.
    let to_u = sub(k.chambers[uc].center, q);
    let t = axis_of(to_u);
    let side = if dot(t, u).abs() > 0.5 { scale(t, -1.0) } else { u };
    if !free(k, planned, &[foot], &[], &[]) {
        k.notes.push(format!("base {i}: its spawn room's foot is in the way of a tunnel"));
    }
    let fc = deep::chamber_open(k, i, q, HALF, HIGH, floor - DEPTH, floor, &[q], &[]);
    k.chambers[fc].stand = add(q, scale(t, 7.0));
    k.chambers[fc].blocked.push((scale(t, -1.0), 0.57));
    k.chambers[fc].shield = Some((i, p.sector));
    let stand = k.chambers[fc].stand;
    deep::shaft_with(k, i, q, fc, side, stand, add(q, scale(side, 7.5)), Some(sub(q, scale(side, 8.0))), (0.0, 0.0));
    let (hops, links) = connect(k, i, planned, fc, uc);
    nets.push(Net { a: i, b: i, from: None, to: None, hops: [vec![fc], hops].concat(), links });
    // A second point's chamber, a straight tube from the undercroft if it can.
    for pt in pts.iter().filter(|pt| dist([pt[0], pt[1]], upt) > 35.0) {
        let under = k.chambers[uc].clone();
        let prefer = |k: &Kiln, s: &Spot| {
            let d = sub(s.center, under.center);
            !crowded(k, planned, uc, scale(d, 1.0 / len(d))) && walkable(k, &under, &ghost(s.center, HALF, s.floor, HIGH))
        };
        if let Some(s) = spot(k, g, planned, *pt, HALF, OFF, HIGH, None, &LEVEL, &[], Some(under.center), &prefer) {
            if !prefer(k, &s) {
                continue;
            }
            let vc = dig_spot(k, i, &s, HALF, HIGH, &LEVEL);
            let (hops, links) = connect(k, i, planned, uc, vc);
            nets.push(Net { a: i, b: i, from: None, to: None, hops: [vec![uc], hops].concat(), links });
            break;
        }
    }
    // The approach tunnel, from the ground outside (away from the spawn room).
    let start = {
        let v = sub(k.chambers[uc].center, q);
        libm::atan2(v[1], v[0]) * 180.0 / std::f64::consts::PI
    };
    match find_mouth(k, g, planned, k.chambers[uc].center, &[200.0, 170.0, 230.0, 150.0, 260.0, 290.0], uc, start) {
        Some((top, e)) => {
            let mc = deep::mouth_at(k, i, top, e);
            let (hops, links) = connect(k, i, planned, mc, uc);
            nets.push(Net { a: i, b: i, from: Some(sub(top, scale(e, 12.0))), to: None, hops: [vec![mc], hops].concat(), links });
        }
        None => k.notes.push(format!("base {i} has no room for an approach tunnel")),
    }
    (nets, Some(uc))
}

/// A Geode's approach tunnel: from the flats outside it down to its well
/// chamber (K2's, which had only its shaft from the Geode's floor).
fn geode(k: &mut Kiln, g: &Ground, planned: &mut Vec<(usize, usize)>, i: usize) -> Option<Net> {
    let well = k.chambers.iter().position(|c| c.site == i && !c.trench && c.half >= 14.0)?;
    let c = k.chambers[well].center;
    let start = free_dir(&k.sites[i]);
    let rings: Vec<f64> = [70.0, 100.0, 130.0, 160.0].iter().map(|r| k.sites[i].half + r).collect();
    let Some((top, e)) = find_mouth(k, g, planned, c, &rings, well, start) else {
        k.notes.push(format!("Geode {i} has no room for an approach tunnel"));
        return None;
    };
    let mc = deep::mouth_at(k, i, top, e);
    let (hops, links) = connect(k, i, planned, mc, well);
    Some(Net { a: i, b: i, from: Some(sub(top, scale(e, 12.0))), to: None, hops: [vec![mc], hops].concat(), links })
}

/// A cenote's cavern: down a trench across its pit's floor, under the floor
/// and the wall beyond, clear of the ledge's foot and the bottom's point.
fn cavern(k: &mut Kiln, i: usize) -> Option<usize> {
    let p = k.sites[i].clone();
    let c = p.center;
    let (bottom, ledge_end) = (p.anchors[2], p.anchors[3]);
    for e in [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]] {
        // (Leveled no further out than the ledge winding down its wall.)
        let top = snap(sub(c, scale(e, 34.0)));
        let n = [-e[1], e[0]];
        let (a, b) = (sub(top, scale(e, 4.0)), add(top, scale(e, 40.0 + 2.0 * 14.0 + 4.0)));
        // Off the trench and its leveled ground.
        let off = |q: [f64; 2]| {
            let v = sub(q, top);
            let (x, y) = (dot(v, e), dot(v, n).abs());
            x < dot(sub(a, top), e) - 4.0 || x > dot(sub(b, top), e) + 4.0 || y > 16.0
        };
        if !off(bottom) || seg_dist(ledge_end, top, add(top, scale(e, 40.0))).0 < 16.0 {
            continue;
        }
        return Some(deep::mouth_sized(k, i, top, e, 14.0, 8.0, 14.0, (7.0, 3.0)));
    }
    None
}

/// Where along a chain of chambers a branch toward `target` leaves from:
/// the chain up to it (from its first chamber) and the links along it. The
/// nearest chamber it doesn't crowd; else a chamber beside the nearest, out
/// of the others' way, off which it leaves.
fn branch(k: &mut Kiln, site: usize, planned: &mut Vec<(usize, usize)>, chain: &[usize], target: [f64; 2]) -> (Vec<usize>, Vec<Link>) {
    let along = |n: usize| (chain[..=n].to_vec(), (0..n).map(|_| Link::Bore).collect::<Vec<_>>());
    let mut order: Vec<usize> = (0..chain.len()).collect();
    order.sort_by(|&a, &b| dist(k.chambers[chain[a]].center, target).total_cmp(&dist(k.chambers[chain[b]].center, target)));
    for &i in &order {
        let c = k.chambers[chain[i]].center;
        let d = dist(c, target);
        if !crowded(k, planned, chain[i], scale(sub(target, c), 1.0 / d)) {
            return along(i);
        }
    }
    for &i in &order {
        let c = k.chambers[chain[i]].clone();
        let toward = scale(sub(target, c.center), 1.0 / dist(target, c.center));
        for rot in [45.0, -45.0, 70.0, -70.0, 95.0, -95.0] {
            let (s, co) = libm::sincos(rad(rot));
            let dir = [toward[0] * co - toward[1] * s, toward[0] * s + toward[1] * co];
            if crowded(k, planned, chain[i], dir) {
                continue;
            }
            let at = add(c.center, scale(dir, c.half + MID_HALF + 30.0));
            let floor = c.floor.min(lowest(k, at, MID_HALF + 1.0) - MID_HIGH - 0.6 - ROCK);
            let side = ghost(at, MID_HALF, floor, MID_HIGH);
            if !rock_over(k, at, MID_HALF + 1.0, floor + MID_HIGH + 0.6 + ROCK) || !walkable(k, &c, &side) {
                continue;
            }
            let seg = Seg { a: c.center, b: at, z0: floor.min(c.floor) - 1.0, z1: floor.max(c.floor) + 5.1, ends: (chain[i], usize::MAX) };
            if !free(k, planned, &[Rect::chamber(at, MID_HALF, floor, MID_HIGH)], &[seg], &[]) {
                continue;
            }
            let m = deep::chamber(k, site, at, MID_HALF, MID_HIGH, floor, k.height(at), &[]);
            planned.push((chain[i], m));
            let (mut hops, mut links) = along(i);
            hops.push(m);
            links.push(Link::Bore);
            return (hops, links);
        }
    }
    k.notes.push(format!("site {site}: no room to branch toward ({:.0}, {:.0})", target[0], target[1]));
    along(order[0])
}

/// The Undervein: the nets under the lanes (as the module says).
fn undervein(k: &mut Kiln, g: &Ground, planned: &mut Vec<(usize, usize)>, crofts: &[(usize, usize)]) -> Vec<Net> {
    let mut nets = Vec::new();
    let mouth = |k: &Kiln, site: usize| k.chambers.iter().position(|c| c.site == site && c.trench).expect("a Tunnel mouth's chamber");
    let mut chains = Vec::new();
    for s in 0..3 {
        let (o5, o11) = (site(s, s::O + 5), site(s, s::O + 11));
        let (m5, m11) = (mouth(k, o5), mouth(k, o11));
        let (hops, links) = bore_run(k, o5, planned, m5, m11);
        let chain = [vec![m5], hops].concat();
        nets.push(Net { a: o5, b: o11, from: Some(k.sites[o5].center), to: Some(k.sites[o11].center), hops: chain.clone(), links });
        // Off it, under their lanes: to the citadel's undercroft (from the
        // O5 end) and the first biome base's (from the O11 end).
        let back: Vec<usize> = chain.iter().rev().copied().collect();
        for (o, along, b) in [(o5, chain.clone(), site(s, s::CITADEL)), (o11, back.clone(), site(s, s::BIOME_A))] {
            let Some(&(_, u)) = crofts.iter().find(|c| c.0 == b) else { continue };
            let (prefix, prefix_links) = branch(k, o, planned, &along[..along.len() / 2 + 1], k.chambers[u].center);
            let (hops, links) = connect(k, o, planned, *prefix.last().unwrap(), u);
            nets.push(Net { a: o, b, from: Some(k.sites[o].center), to: None, hops: [prefix, hops].concat(), links: prefix_links.into_iter().chain(links).collect() });
        }
        chains.push(back);
    }
    // Two under each rift: from the next sector's O11 to this one's O12
    // (under the small bridge between them), and from this one's Cliffhold's
    // deep gallery to the next's O7 (under the one from its keep).
    for j in 0..3 {
        let rift = Rift::new(j);
        let from_o11 = chains[(j + 1) % 3].clone();
        let hall = k.chambers.iter().position(|c| c.site == site(j, s::CLIFFHOLD) && !c.trench && c.half > 12.0).expect("the Cliffhold's deep gallery");
        for (along, chain, near) in [(2300.0, from_o11, site(j, s::O + 12)), (3250.0, vec![hall], site(j + 1, s::O + 7))] {
            if let Some(n) = rift_tube(k, g, planned, &rift, along, &chain, near) {
                nets.push(n);
            }
        }
    }
    nets
}

/// A tube under rift `rift` at `along`: from a chamber along `chain` (on
/// one side) over to the rift's edge, down a shaft to under its floor,
/// across under it by a junction (a trench up to the floor), up a shaft on
/// the far side and over to a mouth by site `near`.
fn rift_tube(k: &mut Kiln, g: &Ground, planned: &mut Vec<(usize, usize)>, rift: &Rift, along: f64, chain: &[usize], near: usize) -> Option<Net> {
    let start = chain[0];
    let a_site = k.chambers[start].site;
    let side_a = rift.coords(k.chambers[start].center).1.signum();
    let w = Rift::half_width(along);
    let floor_z = Rift::floor(along);
    // The junction: a mouth on the floor, off the roads, its trench along the rift.
    let e = axis_of(rift.dir);
    let mut junction = None;
    'j: for da in [0.0, 30.0, -30.0, 60.0, -60.0] {
        for across in [-0.45, 0.45, -0.25, 0.25] {
            let top = snap(sub(rift.at(along + da, across * w), scale(e, 28.0)));
            let (a, b) = (top, add(top, scale(e, 56.0)));
            let clear = (0..=8).all(|i| {
                let q = add(a, scale(sub(b, a), i as f64 / 8.0));
                g.way(q) > 24.0 && !g.boxed(q, 14.0, floor_z - 5.0, floor_z + 30.0) && !hole_near(k, q, 20.0)
            });
            if clear {
                junction = Some(top);
                break 'j;
            }
        }
    }
    let Some(top) = junction else {
        k.notes.push(format!("rift {} at {along}: no room on the floor for the junction", rift.j));
        return None;
    };
    let jcenter = add(top, scale(e, 48.0));
    let low = k.mean(add(top, scale(e, 20.0)), 26.0) - 12.0;
    // The shafts: under each side's ground beyond its wall's top.
    let mut shafts = Vec::new();
    for side in [side_a, -side_a] {
        let mut found = None;
        for out in [80.0, 95.0, 110.0, 130.0] {
            let s = rift.at(along, side * (w + out));
            let dx = scale(sub(jcenter, s), 1.0 / dist(jcenter, s));
            let outward = scale(rift.normal, side);
            let hi = lowest(k, add(s, scale(outward, 5.0)), 11.0) - 5.6 - ROCK - 6.0;
            let (uc, xc) = (add(s, scale(outward, 5.0)), add(s, scale(dx, 5.0)));
            if !rock_over(k, xc, 11.0, low + 5.6 + ROCK) {
                continue;
            }
            let rects = [Rect::chamber(uc, 10.0, hi, 5.0), Rect::chamber(xc, 10.0, low, 5.0)];
            if free(k, planned, &rects, &[], &[]) {
                found = Some((s, dx, hi));
                break;
            }
        }
        let Some(f) = found else {
            k.notes.push(format!("rift {} at {along}: no room for the shaft on side {side}", rift.j));
            return None;
        };
        shafts.push(f);
    }
    // The far end: a mouth near the far site, its trench toward the far shaft.
    let around = k.sites[near].center;
    let far_s = shafts[1].0;
    let start_deg = {
        let v = sub(far_s, around);
        libm::atan2(v[1], v[0]) * 180.0 / std::f64::consts::PI
    };
    // (A ghost of the far shaft's upper chamber, for the mouth's search.)
    let far_ghost = ghost(add(far_s, scale(rift.normal, -side_a * 5.0)), 10.0, shafts[1].2, 5.0);
    k.chambers.push(far_ghost);
    let found = find_mouth(k, g, planned, around, &[50.0, 65.0, 80.0, 100.0, 120.0, 140.0], k.chambers.len() - 1, start_deg);
    k.chambers.pop();
    let Some((mtop, me)) = found else {
        k.notes.push(format!("rift {} at {along}: no room for a mouth by site {near}", rift.j));
        return None;
    };
    let jc = deep::mouth_at(k, a_site, top, e);
    let mc = deep::mouth_at(k, near, mtop, me);
    // The near side's upper chamber faces where the chain's branch is; the far's, the mouth.
    let (prefix, prefix_links) = branch(k, a_site, planned, &chain[..chain.len() / 2 + 1], shafts[0].0);
    let branch = *prefix.last().unwrap();
    let mut pairs = Vec::new();
    for (i, &(s, dx, hi)) in shafts.iter().enumerate() {
        let toward = if i == 0 { k.chambers[branch].center } else { k.chambers[mc].center };
        let dh = scale(sub(toward, s), 1.0 / dist(toward, s));
        pairs.push(deep::shaft_pair(k, a_site, s, dh, dx, hi, low, 10.0));
    }
    // The chain: along it to the branch, over to the near shaft, down,
    // across by the junction, up, over to the mouth.
    let mut hops: Vec<usize> = prefix[1..].to_vec();
    let mut links: Vec<Link> = prefix_links;
    let mut join = |k: &mut Kiln, planned: &mut Vec<(usize, usize)>, from: usize, to: usize| {
        let (h, l) = connect(k, a_site, planned, from, to);
        hops.extend(h);
        links.extend(l);
    };
    let (u0, l0, down0, up0) = pairs.remove(0);
    let (u1, l1, down1, up1) = pairs.remove(0);
    join(k, planned, branch, u0);
    join(k, planned, l0, jc);
    join(k, planned, jc, l1);
    join(k, planned, u1, mc);
    // (The shafts' links, in their places: after the tube to each upper chamber, and to each lower.)
    let mut out_hops = Vec::new();
    let mut out_links = Vec::new();
    for (h, l) in hops.into_iter().zip(links) {
        out_hops.push(h);
        out_links.push(l);
        if h == u0 {
            out_hops.push(l0);
            out_links.push(Link::Shaft { down: down0.clone(), up: up0.clone() });
        }
        if h == l1 {
            out_hops.push(u1);
            out_links.push(Link::Shaft { down: down1.clone(), up: up1.clone() });
        }
    }
    let start_site = k.chambers[start].site;
    let trench = k.chambers[start].trench;
    // The junction's trench: a way down to the tube from the floor.
    let jtop = k.chambers[jc].top;
    let down = trench_leg(&k.chambers[jc].clone());
    k.tunnels.push(Route { a: near, b: near, legs: vec![Leg { kind: LegKind::Road, path: vec![sub(jtop, scale(e, 12.0)), jtop], z: None }, down], one_way: false });
    Some(Net { a: start_site, b: near, from: trench.then(|| k.sites[start_site].center), to: Some(k.sites[near].center), hops: [vec![start], out_hops].concat(), links: out_links })
}

/// A stretch of tunnel between stops: its chambers end to end, and how far
/// along it each is (along the tubes, up and down the shafts between them).
pub(crate) struct Stretch {
    pub chambers: Vec<usize>,
    pub along: Vec<f64>,
}

impl Stretch {
    pub fn length(&self) -> f64 {
        *self.along.last().unwrap()
    }
}

/// The tunnels' stretches between stops (docs/kiln.md "Tunnels": "no more
/// than 400 m between junctions or exits"), given the tubes between
/// chambers: a stop is a junction (three ways or more, or a dead end) or
/// an exit (a trench, a shaft up to the ground).
pub(crate) fn stretches(k: &Kiln, tubes: &[(usize, usize)]) -> Vec<Stretch> {
    let mut tubes: Vec<(usize, usize)> = tubes.iter().map(|&(a, b)| (a.min(b), a.max(b))).collect();
    tubes.sort_unstable();
    tubes.dedup();
    let far = |a: &Chamber, b: &Chamber| libm::hypot(dist(a.center, b.center), a.floor - b.floor);
    let mut edges: Vec<(usize, usize, f64)> = tubes.iter().map(|&(a, b)| (a, b, far(&k.chambers[a], &k.chambers[b]))).collect();
    for (i, c) in k.chambers.iter().enumerate() {
        if let Some(p) = c.pair.filter(|&p| p > i) {
            edges.push((i, p, (c.floor - k.chambers[p].floor).abs()));
        }
    }
    let mut at: Vec<Vec<usize>> = vec![Vec::new(); k.chambers.len()];
    for (e, &(a, b, _)) in edges.iter().enumerate() {
        at[a].push(e);
        at[b].push(e);
    }
    let stop = |i: usize| k.chambers[i].trench || k.chambers[i].ground || at[i].len() != 2;
    let mut seen = vec![false; edges.len()];
    let mut out = Vec::new();
    for s in (0..k.chambers.len()).filter(|&i| stop(i)) {
        for &first in &at[s] {
            if seen[first] {
                continue;
            }
            let (mut here, mut e) = (s, first);
            let mut st = Stretch { chambers: vec![s], along: vec![0.0] };
            loop {
                seen[e] = true;
                let (a, b, l) = edges[e];
                here = if a == here { b } else { a };
                st.chambers.push(here);
                st.along.push(st.length() + l);
                if stop(here) {
                    break;
                }
                match at[here].iter().find(|&&x| x != e) {
                    Some(&next) if !seen[next] => e = next,
                    _ => break,
                }
            }
            out.push(st);
        }
    }
    out
}

/// The tubes dug and planned, by their chambers.
fn all_tubes(k: &Kiln, planned: &[(usize, usize)]) -> Vec<(usize, usize)> {
    k.bores.iter().map(|b| b.4).chain(planned.iter().copied()).collect()
}

/// Exits where the tunnels run too far between stops: off a chamber along
/// such a stretch (one that leaves both sides short enough first, then one
/// near the ground, then the nearest its middle), a chamber beside it with
/// a shaft up to open ground.
fn exits(k: &mut Kiln, g: &Ground, planned: &mut Vec<(usize, usize)>) -> Vec<Net> {
    let mut nets = Vec::new();
    let mut tried = Vec::new();
    loop {
        let mut long: Vec<Stretch> = stretches(k, &all_tubes(k, planned)).into_iter().filter(|s| s.length() > STRETCH).collect();
        long.sort_by(|a, b| b.length().total_cmp(&a.length()));
        let mut added = false;
        'each: for st in &long {
            let total = st.length();
            let both = |j: usize| st.along[j] <= STRETCH && total - st.along[j] <= STRETCH;
            let mut order: Vec<usize> = (1..st.chambers.len() - 1).filter(|&j| !tried.contains(&st.chambers[j])).collect();
            let deep = |j: usize| {
                let c = &k.chambers[st.chambers[j]];
                k.height(c.center) - c.floor > 40.0
            };
            let key = |j: usize| (!both(j), deep(j));
            order.sort_by(|&a, &b| key(a).cmp(&key(b)).then((st.along[a] - total / 2.0).abs().total_cmp(&(st.along[b] - total / 2.0).abs())));
            for j in order {
                tried.push(st.chambers[j]);
                if let Some(n) = exit(k, g, planned, st.chambers[j]) {
                    nets.push(n);
                    added = true;
                    break 'each;
                }
            }
        }
        if !added {
            for st in long {
                let (a, b) = (&k.chambers[st.chambers[0]], &k.chambers[*st.chambers.last().unwrap()]);
                k.notes.push(format!("{:.0} m of tunnel between stops, from ({:.0}, {:.0}, {:.0}) to ({:.0}, {:.0}, {:.0})", st.length(), a.center[0], a.center[1], a.floor, b.center[0], b.center[1], b.floor));
            }
            return nets;
        }
    }
}

/// An exit off chamber `ch`: a chamber beside it, level with it, with a
/// shaft up to open ground (as tall as the rock is deep there), a tube to
/// it that walks, leaving `ch` clear of its others.
fn exit(k: &mut Kiln, g: &Ground, planned: &mut Vec<(usize, usize)>, ch: usize) -> Option<Net> {
    let c = k.chambers[ch].clone();
    let prefer = |k: &Kiln, s: &Spot| {
        let d = sub(s.center, c.center);
        !crowded(k, planned, ch, scale(d, 1.0 / len(d))) && walkable(k, &c, &ghost(s.center, HALF, s.floor, HIGH))
    };
    let s = spot(k, g, planned, [c.center[0], c.center[1], f64::NAN], HALF, OFF, HIGH, Some(c.floor), &ROUGH, &[], Some(c.center), &prefer)?;
    if !prefer(k, &s) {
        return None;
    }
    let vc = dig_spot(k, c.site, &s, HALF, HIGH, &ROUGH);
    let (hops, links) = connect(k, c.site, planned, ch, vc);
    Some(Net { a: c.site, b: c.site, from: None, to: None, hops: [vec![ch], hops].concat(), links })
}

/// Every base's tunnels and the Undervein (then every chamber's walls).
pub(crate) fn build(k: &mut Kiln) {
    // Points on the ground stay on it, wherever it's leveled below.
    let on_ground: Vec<bool> = k.points.iter().map(|p| (p.pos[2] as f64 - k.height([p.pos[0] as f64, p.pos[1] as f64])).abs() < 0.05).collect();
    let g = Ground::new(k);
    let mut planned: Vec<(usize, usize)> = Vec::new();
    let mut nets = Vec::new();
    let mut crofts = Vec::new();
    for i in 0..k.sites.len() {
        if k.sites[i].kind.base() {
            let (n, u) = base(k, &g, &mut planned, i);
            nets.extend(n);
            if let Some(u) = u {
                crofts.push((i, u));
            }
        } else if k.sites[i].kind == SiteKind::Geode {
            nets.extend(geode(k, &g, &mut planned, i));
        }
    }
    nets.extend(undervein(k, &g, &mut planned, &crofts));
    nets.extend(exits(k, &g, &mut planned));
    let mut dug = HashMap::new();
    for n in &nets {
        dig(k, n, &mut dug);
    }
    for i in 0..k.chambers.len() {
        deep::walls(k, i);
    }
    let heights: Vec<Option<f64>> = k.points.iter().zip(&on_ground).map(|(p, &on)| on.then(|| k.height([p.pos[0] as f64, p.pos[1] as f64]))).collect();
    for (p, h) in k.points.iter_mut().zip(heights) {
        if let Some(h) = h {
            p.pos[2] = h as f32;
        }
    }
}
