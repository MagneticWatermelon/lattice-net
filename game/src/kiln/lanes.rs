//! Kiln's lanes: about 110 of them, 2-4 per site, each a route a player can
//! walk from one site to the next (docs/kiln.md, "Sites and fronts").
//!
//! A route is legs: roads graded to the land (cut and filled to at most
//! `ROAD_GRADE`), ramps hugging a face from one height to another (a rift
//! wall, the Crown's escarpment), and bridges (an axis-aligned deck across a
//! rift). Every sector's lanes are the same, turned into place.
//!
//! The lanes a sector has (its sites as in `s`; `n.` is the next sector's):
//! - the Landing to three outposts (O5, O6, O10);
//! - from the Landing, the shortest way to the Hollow is three lanes and
//!   about 3 km: an outpost, the Geode, then over the Crown;
//! - the Hollow's six: from each Span up its rift's floor, and from each
//!   Geode over the Crown;
//! - each rift crossed by its Span's deck, two smaller bridges, and a pair
//!   of ramps down to its floor and up the far wall (the Undervein's two
//!   tubes come with tunnels, K1).

use super::land::{self, DECK};
use super::*;
use crate::world::Lane;

/// A road's half-width, and the shoulder over which it blends into the land.
pub(crate) const ROAD_HALF: f64 = 6.0;
pub(crate) const SHOULDER: f64 = 6.0;
/// How steep a graded road may get (20 degrees: a vehicle road's too).
pub(crate) const ROAD_GRADE: f64 = 0.36;
/// A road's banks where it's cut or filled: no steeper than this (rise over
/// run), so a road is never a wall across the land.
const BANK_SLOPE: f64 = 0.8;
const BANK_MAX: f64 = 60.0;
/// Paths are graded and stamped at points this far apart.
const DENSE: f64 = 2.0;
/// How far beyond a rift's floor a small bridge lands: past the top of its
/// wall (a wall of up to ~240 m: its talus, its 6:1 face, the edge's wobble).
const BANK_OUT: f64 = 46.0;

#[derive(Debug, Clone)]
pub(crate) enum LegKind {
    /// Graded to the land.
    Road,
    /// From one height to another, evenly: from the land's height at its
    /// ends unless given.
    Ramp(Option<f64>, Option<f64>),
    /// A level deck at this height (or the banks' mean), banks flattened to it.
    Bridge(Option<f64>),
}

#[derive(Debug, Clone)]
pub(crate) struct Leg {
    pub kind: LegKind,
    pub path: Vec<[f64; 2]>,
    /// Its heights at its points, where they aren't the land's (a stair's).
    pub z: Option<Vec<f64>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub a: usize,
    pub b: usize,
    pub legs: Vec<Leg>,
    /// Only from `a` to `b` (up a lift, down a drop shaft).
    pub one_way: bool,
}

/// Builds a route leg by leg.
pub(crate) struct R {
    route: Route,
    at: [f64; 2],
}

impl R {
    pub fn from(a: usize, at: [f64; 2]) -> Self {
        Self { route: Route { a, b: a, legs: Vec::new(), one_way: false }, at }
    }

    pub fn road(mut self, to: [f64; 2]) -> Self {
        self.route.legs.push(Leg { kind: LegKind::Road, path: vec![self.at, to], z: None });
        self.at = to;
        self
    }

    /// A ramp along `path` (from where we are), its heights the land's at its ends unless given.
    pub fn ramp(mut self, path: Vec<[f64; 2]>, z0: Option<f64>, z1: Option<f64>) -> Self {
        let mut p = vec![self.at];
        p.extend(path);
        self.at = *p.last().unwrap();
        self.route.legs.push(Leg { kind: LegKind::Ramp(z0, z1), path: p, z: None });
        self
    }

    pub fn bridge(mut self, to: [f64; 2], z: Option<f64>) -> Self {
        self.route.legs.push(Leg { kind: LegKind::Bridge(z), path: vec![self.at, to], z: None });
        self.at = to;
        self
    }

    /// Ends at site `b`, at `to`.
    pub fn to(self, b: usize, to: [f64; 2]) -> Route {
        let mut r = self.road(to).route;
        r.b = b;
        r
    }
}

/// A ramp along a rift wall, hugging its face: from `along0` at height `z0`
/// to `along1` at `z1`, on the side `side` (-1: across < 0). Points every
/// ~10 m; the face at height z is `8 + (z - floor - 9.6) / 6` m beyond the
/// floor's edge (`land::rift_carve`), and the ramp runs 3 m into it.
pub(crate) fn wall_ramp(rift: &Rift, side: f64, along0: f64, along1: f64, z0: f64, z1: f64) -> Vec<[f64; 2]> {
    let n = ((along1 - along0).abs() / 10.0).ceil().max(2.0) as usize;
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let along = lerp(along0, along1, t);
            let z = lerp(z0, z1, t);
            // On the face; its lowest 20 m swing out onto the floor, past the
            // wall's wobble (+-9 m), so it ends on the floor wherever that is.
            let face = |z: f64| 8.0 + (z - Rift::floor(along) - 9.6) / 6.0 + 3.0;
            let above = z - Rift::floor(along);
            let off = if above < 20.0 { lerp(-15.0, face(Rift::floor(along) + 20.0), above.max(0.0) / 20.0) } else { face(z) };
            rift.at(along, side * (Rift::half_width(along) + off))
        })
        .collect()
}

/// The length a ramp needs to climb `rise` m at `RAMP_GRADE`.
pub(crate) fn ramp_len(rise: f64) -> f64 {
    rise.abs() / RAMP_GRADE
}

/// The length a ramp in a rift wall needs (`WALL_GRADE`: infantry's ways
/// to the ledges are steeper than the roads).
pub(crate) fn wall_len(rise: f64) -> f64 {
    rise.abs() / WALL_GRADE
}

/// A ramp up the Crown's escarpment: from its foot at `deg0` (degrees from
/// sector `f`'s axis) along it to its top, toward `dir` (+1 counterclockwise).
pub(crate) fn escarpment(f: &Frame, deg0: f64, dir: f64) -> (Vec<[f64; 2]>, [f64; 2], [f64; 2]) {
    let rise = land::CROWN - land::MESA_FLATS;
    let r_mid = CROWN_R + 20.0;
    let span = ramp_len(rise) * 1.05 / r_mid * 180.0 / std::f64::consts::PI;
    let n = 40;
    let path: Vec<[f64; 2]> = (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            // From the talus's foot to the plateau's edge, across the face.
            let r = lerp(CROWN_R + 52.0, CROWN_R - 10.0, smoothstep(0.0, 1.0, t));
            f.polar(r, deg0 + dir * span * t)
        })
        .collect();
    let (foot, top) = (path[0], *path.last().unwrap());
    (path, foot, top)
}

/// Every lane, and the routes inside the Hollow, in plan.
pub(crate) fn plan(sites: &[Plan]) -> Vec<Route> {
    let mut out = Vec::new();
    let c = |i: usize| sites[i].center;
    for k in 0..3 {
        let f = Frame::new(k);
        let n = f.next();
        let (ccw, cw) = (f.ccw_rift(), f.cw_rift());
        let me = |i: usize| site(k, i);
        let next = |i: usize| site(k + 1, i);
        let o = |n: usize| me(s::O + n);
        let span = &sites[me(s::SPAN)];
        let (our_head, far_head, floor_mid) = (span.anchors[0], span.anchors[1], span.anchors[2]);
        let ch = &sites[me(s::CLIFFHOLD)];
        let (keep, gate_ch) = (ch.anchors[0], ch.anchors[1]);
        let geode_foot = |toward: [f64; 2]| {
            let g = c(me(s::GEODE));
            add(g, scale(sub(toward, g), (sites[me(s::GEODE)].half + 26.0) / dist(toward, g)))
        };
        let entry = |site: usize, from: [f64; 2]| super::build::entry(&sites[site], from);
        // A plain road between two sites (through their entries).
        let road = |a: usize, b: usize| {
            let (ea, eb) = (entry(a, c(b)), entry(b, c(a)));
            let mut r = R::from(a, ea.0);
            if let Some(m) = ea.1 {
                r = r.road(m);
            }
            match eb.1 {
                Some(m) => r.road(m).to(b, eb.0),
                None => r.to(b, eb.0),
            }
        };

        // The outer plains and the mesa country.
        for (a, b) in [
            (me(s::LANDING), o(5)),
            (me(s::LANDING), o(6)),
            (me(s::LANDING), o(10)),
            (o(5), o(9)),
            (o(9), me(s::BIOME_A)),
            (me(s::BIOME_A), o(7)),
            (o(11), me(s::BIOME_A)),
            (o(11), o(3)),
            (o(3), me(s::CITADEL)),
            (me(s::CITADEL), o(5)),
            (o(6), me(s::SLOT_HOLD)),
            (me(s::SLOT_HOLD), o(4)),
            (me(s::SLOT_HOLD), me(s::BIOME_B)),
            (me(s::BIOME_B), o(10)),
            (me(s::BIOME_B), o(12)),
            (me(s::BIOME_B), o(8)),
        ] {
            out.push(road(a, b));
        }
        // To the Geode: to the foot of the crack facing each.
        for a in [o(5), o(6), me(s::CITADEL), me(s::SLOT_HOLD)] {
            let e = entry(a, c(me(s::GEODE)));
            let mut r = R::from(a, e.0);
            if let Some(m) = e.1 {
                r = r.road(m);
            }
            out.push(r.to(me(s::GEODE), geode_foot(c(a))));
        }
        // The clockwise rift's ledge, from the plains: O11 down a ramp in our
        // wall to its inner end. (Its outer end is the floor's way up.)
        let ledge_cw = c(me(s::LEDGE_CW));
        let lcz = super::build::LEDGE_CW_Z;
        {
            let end_along = LEDGE_CW_ALONG - LEDGE_HALF;
            let top_along = end_along - wall_len(228.0 - lcz);
            let path = wall_ramp(&cw, 1.0, top_along, end_along, 228.0, lcz);
            let top = path[0];
            let above = add(top, scale(sub(top, cw.at(top_along, 0.0)), 14.0 / dist(top, cw.at(top_along, 0.0))));
            out.push(R::from(o(11), c(o(11))).road(above).ramp(path, None, Some(lcz)).to(me(s::LEDGE_CW), ledge_cw));
        }
        // Up the Crown's escarpment: west of the axis (O3 and the citadel to
        // O1), at the axis (the Geode to the Hollow), east (O4 and the slot
        // hold to O2). Each climbs away from the lanes that come to its foot,
        // so road and ramp never run side by side at two heights.
        let (west, west_foot, _) = escarpment(&f, -44.0, 1.0);
        let (east, east_foot, _) = escarpment(&f, 44.0, -1.0);
        let (mid, mid_foot, _) = escarpment(&f, 10.6, -1.0);
        for a in [o(3), me(s::CITADEL)] {
            let e = entry(a, west_foot);
            let mut r = R::from(a, e.0);
            if let Some(m) = e.1 {
                r = r.road(m);
            }
            out.push(r.road(west_foot).ramp(west[1..].to_vec(), None, None).to(o(1), c(o(1))));
        }
        for a in [o(4), me(s::SLOT_HOLD)] {
            let e = entry(a, east_foot);
            let mut r = R::from(a, e.0);
            if let Some(m) = e.1 {
                r = r.road(m);
            }
            out.push(r.road(east_foot).ramp(east[1..].to_vec(), None, None).to(o(2), c(o(2))));
        }
        // The Crown lane: the Geode's inner crack, the escarpment, the rim's gate.
        let gate = f.polar(HOLLOW_R + 50.0, 0.0);
        out.push(R::from(me(s::GEODE), geode_foot(mid_foot)).road(mid_foot).ramp(mid[1..].to_vec(), None, None).road(f.polar(1100.0, -3.0)).to(0, gate));
        // The counterclockwise rift's ledge, halfway down the Crown's wall:
        // one ramp from the Crown above its inner end, another from its outer
        // end to the floor, then out along the floor to the Span.
        let ledge = c(me(s::LEDGE_CCW));
        let lz = super::build::LEDGE_CCW_Z;
        let inner = LEDGE_CCW_ALONG - LEDGE_HALF;
        let top_along = inner - wall_len(land::CROWN - lz);
        let path = wall_ramp(&ccw, -1.0, top_along, inner, land::CROWN, lz);
        let top = path[0];
        let above = add(top, scale(sub(top, ccw.at(top_along, 0.0)), 14.0 / dist(top, ccw.at(top_along, 0.0))));
        out.push(R::from(o(2), c(o(2))).road(above).ramp(path, None, Some(lz)).to(me(s::LEDGE_CCW), ledge));
        let outer = LEDGE_CCW_ALONG + LEDGE_HALF;
        let floor_along = outer + wall_len(lz - Rift::floor(outer + 400.0));
        let path = wall_ramp(&ccw, -1.0, outer, floor_along, lz, Rift::floor(floor_along) + 0.5);
        let off_floor = ccw.at(floor_along + 25.0, -(Rift::half_width(floor_along) - 30.0));
        out.push(R::from(me(s::LEDGE_CCW), ledge).ramp(path, Some(lz), Some(Rift::floor(floor_along) + 0.3)).road(off_floor).to(me(s::SPAN), floor_mid));
        // The Span: up its rift's floor to the Hollow (to the plaza at the mouth).
        out.push(R::from(me(s::SPAN), floor_mid).road(ccw.at(1300.0, 0.0)).to(0, ccw.at(560.0, 0.0)));
        // Off the Span's bridgeheads: ramps down to the flats, then roads.
        for (b, head, toward) in [(o(4), our_head, c(o(4))), (o(12), our_head, c(o(12))), (next(s::O + 3), far_head, c(next(s::O + 3)))] {
            let dir = scale(sub(toward, head), 1.0 / dist(toward, head));
            let foot = add(head, scale(dir, 40.0 + ramp_len(DECK - land::MESA_FLATS + 6.0)));
            let start = add(head, scale(dir, 40.0));
            out.push(R::from(me(s::SPAN), head).road(start).ramp(vec![foot], Some(DECK), None).to(b, c(b)));
        }
        // The next sector's clockwise ledge, from the floor: a ramp up its
        // wall (the far side of ours) to its outer end. Two lanes climb it:
        // from the Span's floor, and from the Cliffhold's gate across the
        // floor (the rift's pair of ramps down and up: the Cliffhold's
        // terraces down, this one up).
        let nl = next(s::LEDGE_CW);
        let n_outer = LEDGE_CW_ALONG + LEDGE_HALF;
        let up_from = n_outer + wall_len(lcz - Rift::floor(n_outer));
        let up = wall_ramp(&ccw, 1.0, up_from, n_outer, Rift::floor(up_from) + 0.5, lcz);
        let foot = ccw.at(up_from + 25.0, Rift::half_width(up_from) - 30.0);
        out.push(R::from(me(s::SPAN), floor_mid).road(ccw.at(up_from - 200.0, 30.0)).road(foot).road(up[0]).ramp(up[1..].to_vec(), None, Some(lcz)).to(nl, c(nl)));
        out.push(R::from(me(s::CLIFFHOLD), gate_ch).road(foot).road(up[0]).ramp(up[1..].to_vec(), None, Some(lcz)).to(nl, c(nl)));
        // The Cliffhold's keep: to O8 and O12, and across a small bridge to
        // the next sector's O7.
        out.push(R::from(me(s::CLIFFHOLD), keep).to(o(8), c(o(8))));
        out.push(R::from(me(s::CLIFFHOLD), keep).to(o(12), c(o(12))));
        let (cross, _) = ccw.crossing();
        let (near, far) = ccw.crossing_ends(3330.0, BANK_OUT);
        let round = ccw.at(3330.0, -(Rift::half_width(3330.0) + 70.0));
        out.push(
            R::from(me(s::CLIFFHOLD), keep)
                .road(round)
                .road(sub(near, scale(cross, 14.0)))
                .road(near)
                .bridge(far, None)
                .road(add(far, scale(cross, 14.0)))
                .to(next(s::O + 7), c(next(s::O + 7))),
        );
        // O12 across a small bridge to the next sector's O11.
        let (near, far) = ccw.crossing_ends(2240.0, BANK_OUT);
        out.push(
            R::from(o(12), c(o(12)))
                .road(sub(near, scale(cross, 14.0)))
                .road(near)
                .bridge(far, None)
                .road(add(far, scale(cross, 14.0)))
                .to(next(s::O + 11), c(next(s::O + 11))),
        );
        let _ = n;
    }
    out
}

/// Heights along a path every `DENSE` m: (points, distances).
fn densify(path: &[[f64; 2]]) -> (Vec<[f64; 2]>, Vec<f64>) {
    let mut pts = vec![path[0]];
    let mut s = vec![0.0];
    for w in path.windows(2) {
        let l = dist(w[0], w[1]);
        let n = (l / DENSE).ceil().max(1.0) as usize;
        for i in 1..=n {
            pts.push(add(w[0], scale(sub(w[1], w[0]), i as f64 / n as f64)));
            s.push(s.last().unwrap() + l / n as f64);
        }
    }
    (pts, s)
}

/// A graded profile of the land `t` along distances `s`: the midpoint of its
/// fill (the lowest line over it no steeper than `g`) and its cut (the
/// highest under it), held to the land at both ends: kept inside the cones
/// of slope `g` from each end (where those meet, the land's too steep for
/// `g` end to end, and it's a straight line).
fn grade(t: &[f64], s: &[f64], g: f64) -> Vec<f64> {
    let n = t.len();
    let mut up = t.to_vec();
    let mut lo = t.to_vec();
    for i in 1..n {
        let d = s[i] - s[i - 1];
        up[i] = up[i].max(up[i - 1] - g * d);
        lo[i] = lo[i].min(lo[i - 1] + g * d);
    }
    for i in (0..n - 1).rev() {
        let d = s[i + 1] - s[i];
        up[i] = up[i].max(up[i + 1] - g * d);
        lo[i] = lo[i].min(lo[i + 1] + g * d);
    }
    let (t0, t1, l) = (t[0], t[n - 1], s[n - 1]);
    (0..n)
        .map(|i| {
            let mid = 0.5 * (up[i] + lo[i]);
            let band_lo = (t0 - g * s[i]).max(t1 - g * (l - s[i]));
            let band_hi = (t0 + g * s[i]).min(t1 + g * (l - s[i]));
            if band_lo <= band_hi {
                mid.clamp(band_lo, band_hi)
            } else {
                lerp(t0, t1, s[i] / l)
            }
        })
        .collect()
}

/// Sets the land along a dense path to its heights `z`: flat across
/// `half`, blending back over `shoulder` (`banked`: or wider, so its banks
/// are no steeper than `BANK_SLOPE`).
pub(crate) fn stamp_path(k: &mut Kiln, pts: &[[f64; 2]], z: &[f64], half: f64, shoulder: f64) {
    stamp_path_banked(k, pts, z, half, shoulder, false)
}

pub(crate) fn stamp_path_banked(k: &mut Kiln, pts: &[[f64; 2]], z: &[f64], half: f64, shoulder: f64, banked: bool) {
    let reach = half + if banked { BANK_MAX } else { shoulder };
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in pts {
        min = [min[0].min(p[0]), min[1].min(p[1])];
        max = [max[0].max(p[0]), max[1].max(p[1])];
    }
    let (x0, x1, y0, y1) = Kiln::samples(sub(min, [reach, reach]), add(max, [reach, reach]));
    // Each sample's nearest point on the path (the first segment's, on a
    // tie), visiting only the samples within reach of each segment.
    let wide = x1 - x0 + 1;
    let mut best = vec![(f64::MAX, 0.0); wide * (y1 - y0 + 1)];
    for (j, w) in pts.windows(2).enumerate() {
        let lo = [w[0][0].min(w[1][0]) - reach, w[0][1].min(w[1][1]) - reach];
        let hi = [w[0][0].max(w[1][0]) + reach, w[0][1].max(w[1][1]) + reach];
        let (sx0, sx1, sy0, sy1) = Kiln::samples(lo, hi);
        for iy in sy0.max(y0)..=sy1.min(y1) {
            for ix in sx0.max(x0)..=sx1.min(x1) {
                let (d, t) = seg_dist(Kiln::pos(ix, iy), w[0], w[1]);
                let b = &mut best[(iy - y0) * wide + ix - x0];
                if d < b.0 {
                    *b = (d, lerp(z[j], z[j + 1], t));
                }
            }
        }
    }
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let (d, target) = best[(iy - y0) * wide + ix - x0];
            if d >= reach {
                continue;
            }
            let i = iy * TERRAIN_N + ix;
            // Beside the road itself, leave other roads, ramps and sites be.
            if d > half && k.mask[i] & (MASK_ROAD | MASK_PAD) != 0 {
                continue;
            }
            // Where it's filled, banks no steeper than `BANK_SLOPE` (a
            // smoothstep's steepest is 1.5x its mean); where it's cut, the
            // cut's walls (a road at a cliff's foot leaves the cliff be).
            let fill = target - k.h[i];
            let bank = if banked && fill > 0.0 { shoulder.max(1.5 * fill / BANK_SLOPE).min(BANK_MAX) } else { shoulder };
            if d >= half + bank {
                continue;
            }
            let w = 1.0 - smoothstep(half, half + bank, d);
            k.h[i] = lerp(k.h[i], target, w);
            if d <= half - 1.0 {
                k.mask[i] |= MASK_ROAD;
            }
            if d <= half + 3.0 {
                k.mask[i] |= MASK_CLEAR;
            }
        }
    }
}

/// A ramp from `z0` to `z1` along `path`, evenly by length.
pub(crate) fn stamp_ramp(k: &mut Kiln, path: &[[f64; 2]], z0: f64, z1: f64) -> f64 {
    let (pts, s) = densify(path);
    let total = *s.last().unwrap();
    let z: Vec<f64> = s.iter().map(|&d| lerp(z0, z1, if total > 0.0 { d / total } else { 1.0 })).collect();
    stamp_path(k, &pts, &z, ROAD_HALF, SHOULDER);
    (z1 - z0).abs() / total.max(1e-9)
}

/// Cuts the ground under a deck from `a` to `b` down to its underside (`z`
/// its top) where it rises above that (a bank's rim higher than the deck):
/// on a deck, the ground is never at the feet.
fn trim_under(k: &mut Kiln, a: [f64; 2], b: [f64; 2], z: f64) {
    let z = z - 2.6;
    // Its length but its ends (the banks at its ends stay level with it),
    // and a little wider than it.
    let (half, ends) = (9.0, 6.0);
    let l = dist(a, b);
    let u = scale(sub(b, a), 1.0 / l);
    let (min, max) = ([a[0].min(b[0]) - half, a[1].min(b[1]) - half], [a[0].max(b[0]) + half, a[1].max(b[1]) + half]);
    let (x0, x1, y0, y1) = Kiln::samples(min, max);
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let q = sub(Kiln::pos(ix, iy), a);
            let (s, t) = (dot(q, u), (q[0] * u[1] - q[1] * u[0]).abs());
            let i = iy * TERRAIN_N + ix;
            if s >= ends && s <= l - ends && t <= half && k.h[i] > z {
                k.h[i] = z;
            }
        }
    }
}

/// Stamps every route's legs, in order (bridges' banks first). Returns each
/// route's decks' heights by leg.
pub(crate) fn stamp(k: &mut Kiln, routes: &[Route]) -> Vec<Vec<Option<f64>>> {
    let mut steepest: f64 = 0.0;
    let mut heights: Vec<Vec<Option<f64>>> = routes.iter().map(|r| vec![None; r.legs.len()]).collect();
    let mut banks = Vec::new();
    let mut decks = Vec::new();
    for (ri, r) in routes.iter().enumerate() {
        for (li, leg) in r.legs.iter().enumerate() {
            if let LegKind::Bridge(z) = leg.kind {
                let (a, b) = (leg.path[0], leg.path[1]);
                let z = z.unwrap_or_else(|| 0.5 * (k.height(a) + k.height(b)));
                heights[ri][li] = Some(z);
                // The banks flattened to the deck.
                let back = scale(sub(a, b), 1.0 / dist(a, b));
                for bank in [add(a, scale(back, 6.0)), sub(b, scale(back, 6.0))] {
                    land::pad(k, bank, 12.0, z, 18.0);
                    banks.push((bank, z));
                }
                decks.push((a, b, z));
                trim_under(k, a, b, z);
                super::build::bridge(k, a, b, z);
            }
        }
        for leg in &r.legs {
            match leg.kind {
                LegKind::Road => {
                    let (pts, s) = densify(&leg.path);
                    let t: Vec<f64> = pts.iter().map(|&p| k.height(p)).collect();
                    let z = grade(&t, &s, ROAD_GRADE);
                    stamp_path_banked(k, &pts, &z, ROAD_HALF, SHOULDER, true);
                }
                LegKind::Ramp(z0, z1) => {
                    let z0 = z0.unwrap_or_else(|| k.height(leg.path[0]));
                    let z1 = z1.unwrap_or_else(|| k.height(*leg.path.last().unwrap()));
                    steepest = steepest.max(stamp_ramp(k, &leg.path, z0, z1));
                }
                LegKind::Bridge(_) => {}
            }
        }
    }
    // Again, in case a later road crossed one.
    for (bank, z) in banks {
        land::pad(k, bank, 12.0, z, 18.0);
    }
    for (a, b, z) in decks {
        trim_under(k, a, b, z);
    }
    let _ = steepest;
    heights
}

/// The routes as the world's lanes: their paths every ~8 m, at the land's
/// final height (a bridge's deck over its rift: `decks`, from `stamp`).
pub(crate) fn finish(k: &Kiln, routes: &[Route], decks: &[Vec<Option<f64>>]) -> Vec<Lane> {
    routes
        .iter()
        .enumerate()
        .map(|(ri, r)| {
            let mut path: Vec<[f32; 3]> = Vec::new();
            for (li, leg) in r.legs.iter().enumerate() {
                let deck = decks.get(ri).and_then(|d| d[li]);
                for (j, w) in leg.path.windows(2).enumerate() {
                    let l = dist(w[0], w[1]);
                    let n = (l / 8.0).ceil().max(1.0) as usize;
                    for i in 0..=n {
                        if i == 0 && !path.is_empty() {
                            continue;
                        }
                        let t = i as f64 / n as f64;
                        let p = add(w[0], scale(sub(w[1], w[0]), t));
                        let z = match &leg.z {
                            Some(z) => lerp(z[j], z[j + 1], t),
                            None => deck.unwrap_or_else(|| k.height(p)),
                        };
                        path.push([p[0] as f32, p[1] as f32, z as f32]);
                    }
                }
            }
            Lane { a: r.a as u16, b: r.b as u16, path, one_way: r.one_way }
        })
        .collect()
}
