//! Kiln's ground: the heights, and what they're made of.
//!
//! `base` makes the continent's shape sample by sample: the land rising in
//! rings toward the center (outer plains, mesa country, the Crown's
//! escarpment and plateau), the three biomes of the outer plains, the rifts
//! and the Hollow carved out of it, and the Rim around it all. `features`
//! adds mesas, buttes, outcrops and reefs where nothing else goes; `sites`
//! shapes each site's ground. Walls are steeper than `MAX_SLOPE` (a wall,
//! even to a jump); everything meant to be walked is well under it.

use super::*;
use crate::world::{hash, Ground, SiteKind};

/// Heights (m) of the layers' grounds.
pub const HOLLOW_FLOOR: f64 = 80.0;
pub const CROWN: f64 = 420.0;
/// The mesa country's flats: at the Crown's foot, and at its outer edge.
pub const MESA_FLATS: f64 = 250.0;
pub const MESA_FLATS_OUT: f64 = 236.0;
/// The outer plains beyond 3.6 km.
pub const PLAIN: f64 = 192.0;
pub const RIM_TOP: f64 = 460.0;
/// The Span's deck and its bridgeheads.
pub const DECK: f64 = 315.0;
/// The Dry Sea's salt flats.
pub const SALT: f64 = 112.0;

/// The Hollow's terraces: (where its cliff starts, the level above it). The
/// floor is `HOLLOW_FLOOR` inside the first; each cliff is 22 m wide.
pub const TERRACES: [(f64, f64); 5] = [(440.0, 148.0), (520.0, 216.0), (600.0, 284.0), (680.0, 352.0), (760.0, 420.0)];
pub const TERRACE_CLIFF: f64 = 22.0;
/// The crater's rim: a lip 12 m above the Crown from 790 to 832 m, cut by a
/// gate on each sector's axis.
pub const LIP: (f64, f64, f64) = (790.0, 832.0, 12.0);
pub const GATE_HALF: f64 = 9.0;

/// Value noise in [-1, 1] at (x, y) in lattice units.
pub(crate) fn noise(seed: u64, x: f64, y: f64) -> f64 {
    let (fx, fy) = (x.floor(), y.floor());
    let (ix, iy) = (fx as i64, fy as i64);
    let (tx, ty) = (x - fx, y - fy);
    let v = |dx: i64, dy: i64| (hash(seed, 77, ix + dx, iy + dy) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(tx), s(ty));
    let a = lerp(v(0, 0), v(1, 0), sx);
    let b = lerp(v(0, 1), v(1, 1), sx);
    lerp(a, b, sy)
}

/// Two octaves of `noise` at wavelength `w` meters.
pub(crate) fn fbm(seed: u64, p: [f64; 2], w: f64) -> f64 {
    (noise(seed, p[0] / w, p[1] / w) + 0.5 * noise(seed ^ 0x55, p[0] / (0.45 * w), p[1] / (0.45 * w))) / 1.5
}

/// Noise on a circle: a smooth function of the angle `a` (radians) with the
/// given period count, for wobbling round edges.
pub(crate) fn wobble(seed: u64, a: f64, waves: f64) -> f64 {
    noise(seed, libm::cos(a) * waves, libm::sin(a) * waves)
}

/// A point's sector, and its angle from that sector's axis (degrees, -60..60).
pub(crate) fn sector_of(p: [f64; 2]) -> (usize, f64) {
    let v = sub(p, CENTER);
    let deg = libm::atan2(v[1], v[0]) * 180.0 / std::f64::consts::PI;
    let rel = (deg - 30.0 + 60.0).rem_euclid(360.0);
    let k = ((rel / 120.0) as usize).min(2);
    (k, rel - 120.0 * k as f64 - 60.0)
}

/// The radial profile: the height of the land at `r` m from the center,
/// before anything is carved, at angle `aphi` (|degrees from a sector's
/// axis|, so the wobbles are the same in every sector and meet at their
/// borders).
fn radial(r: f64, q: [f64; 2], aphi: f64) -> f64 {
    let crown_edge = CROWN_R + 22.0 * wobble(11, rad(aphi) * 2.0, 2.0);
    let flats = if r < MESA_R {
        lerp(MESA_FLATS, MESA_FLATS_OUT, ((r - CROWN_R - 40.0) / (MESA_R - CROWN_R - 40.0)).clamp(0.0, 1.0))
    } else if r < 3000.0 {
        lerp(MESA_FLATS_OUT, 232.0, (r - MESA_R) / 400.0)
    } else if r < 3600.0 {
        lerp(232.0, PLAIN, (r - 3000.0) / 600.0)
    } else {
        PLAIN
    } + 3.0 * fbm(21, q, 180.0);
    let crown = CROWN + 5.0 * fbm(23, q, 140.0);
    let d = r - crown_edge;
    if d <= 0.0 {
        crown
    } else if d >= 40.0 {
        flats
    } else {
        // A face over 28 m, then a talus apron.
        let t = d / 40.0;
        let f = if t < 0.7 { 0.85 * t / 0.7 } else { 0.85 + 0.15 * (t - 0.7) / 0.3 };
        lerp(crown, flats, f)
    }
}

/// The height a biome adds at local point `l` of sector `k` (0 on the
/// mesa country and in, growing over the outer plains).
fn biome(k: usize, l: [f64; 2], h: f64) -> f64 {
    let r = len(l);
    let w = smoothstep(2450.0, 2750.0, r);
    if w <= 0.0 {
        return h;
    }
    match BIOMES[k] {
        // Fused glass: nearly flat.
        Biome::Glasslands => h + w * (2.5 * fbm(31, l, 130.0) + 1.0 * fbm(32, l, 37.0)),
        // Dunes up to 55 m: ridges across the sector's axis, gentle on the
        // windward side and steep (40 degrees) on the lee, their crests rising
        // and falling along them.
        Biome::DuneSea => {
            let u = (l[0] + 45.0 * fbm(41, l, 340.0)) / 240.0;
            let t = u - u.floor();
            let p = if t < 0.72 { t / 0.72 } else { (1.0 - t) / 0.28 };
            let amp = 0.3 + 0.7 * smoothstep(-0.35, 0.45, fbm(42, l, 520.0));
            h + w * (55.0 * amp * p * p * (3.0 - 2.0 * p) + 2.0 * fbm(43, l, 45.0))
        }
        // The salt basin: a long hollow across the sector, down to the Floor
        // layer, with shelving shores.
        Biome::DrySea => {
            let (cx, a, b) = (2850.0, 380.0, 2000.0);
            let f = ((l[0] - cx) / a) * ((l[0] - cx) / a) + (l[1] / b) * (l[1] / b) - 1.0;
            let g = [2.0 * (l[0] - cx) / (a * a), 2.0 * l[1] / (b * b)];
            let sd = f / len(g).max(1e-9);
            let basin = 1.0 - smoothstep(-40.0, 230.0, sd);
            let salt = SALT + 0.4 * fbm(51, l, 60.0);
            lerp(h + w * 1.5 * fbm(52, l, 90.0), salt, basin)
        }
    }
}

/// A rift's carve at (along, across): its floor, a talus, then a wall of
/// 6:1 to any height. None outside its reach.
fn rift_carve(rift: &Rift, p: [f64; 2]) -> Option<f64> {
    let (along, across) = rift.coords(p);
    if along < HOLLOW_FLOOR_R - 60.0 {
        return None;
    }
    let side = if across < 0.0 { 3 } else { 5 };
    let w = Rift::half_width(along.min(RIFT_END)) + 9.0 * noise(60 + rift.j as u64 * 7 + side, along / 70.0, 0.5);
    let d = if along <= RIFT_END { across.abs() - w } else { len([along - RIFT_END, across]) - w };
    if d > 80.0 {
        return None;
    }
    let floor = Rift::floor(along) + 1.2 * noise(61 + rift.j as u64, along / 23.0, across / 23.0);
    Some(if d <= 0.0 {
        floor
    } else if d <= 8.0 {
        floor + 1.2 * d
    } else {
        floor + 9.6 + 6.0 * (d - 8.0)
    })
}

/// The Hollow's crater at `r`: its floor, the terraces' ledges and cliffs,
/// up to the Crown. None outside it.
fn crater(r: f64, wob: f64) -> Option<f64> {
    if r >= TERRACES[4].0 + TERRACE_CLIFF + wob {
        return None;
    }
    let mut level = HOLLOW_FLOOR;
    for &(start, top) in &TERRACES {
        let s = start + wob;
        if r < s {
            return Some(level);
        }
        if r < s + TERRACE_CLIFF {
            return Some(lerp(level, top, (r - s) / TERRACE_CLIFF));
        }
        level = top;
    }
    Some(level)
}

/// The crater's rim lip at `r` (with its gates), as a height to raise to.
fn lip(r: f64, aphi: f64) -> Option<f64> {
    let (a, b, up) = LIP;
    if r <= a || r >= b {
        return None;
    }
    // A gate on each sector's axis: the lip cut down to the Crown.
    if aphi * std::f64::consts::PI / 180.0 * r < GATE_HALF {
        return None;
    }
    let t = ((r - a) / 8.0).min(1.0).min((b - r) / 10.0);
    Some(CROWN + up * t.clamp(0.0, 1.0))
}

/// The Rim at `e` m from the map's edge: an escarpment rising to 460 m.
fn rim(e: f64, along_edge: f64) -> Option<f64> {
    let e = e + 14.0 * noise(71, along_edge / 90.0, 0.3);
    if e >= RIM + 40.0 {
        return None;
    }
    Some(if e <= RIM - 60.0 {
        RIM_TOP + 6.0 * noise(72, along_edge / 50.0, e / 50.0)
    } else if e <= RIM {
        // The face: 4:1 up from the talus.
        lerp(RIM_TOP, PLAIN + 34.0, (e - (RIM - 60.0)) / 60.0)
    } else {
        lerp(PLAIN + 34.0, 0.0, (e - RIM) / 40.0)
    })
}

/// The height at world point `p`, before features and sites.
fn base_at(p: [f64; 2], rifts: &[Rift; 3]) -> f64 {
    let v = sub(p, CENTER);
    let r = len(v);
    let (k, phi) = sector_of(p);
    let aphi = phi.abs();
    // Mirrored local coordinates: the same noise either side of an axis, so
    // neighbors agree along their border.
    let q = scale(unit(aphi), r);
    let mut h = radial(r, q, aphi);
    let local = Frame::new(k).local(p);
    h = biome(k, local, h);
    if aphi > 48.0 {
        // Toward the border beyond the rifts' ends: meet the neighbor halfway.
        let n = if phi > 0.0 { (k + 1) % 3 } else { (k + 2) % 3 };
        let other = biome(n, Frame::new(n).local(p), radial(r, q, aphi));
        h = lerp(h, other, 0.5 * smoothstep(48.0, 60.0, aphi));
    }
    // The crater's lip, then the rifts cut through it.
    if let Some(l) = lip(r, aphi) {
        h = h.max(l);
    }
    for rift in rifts {
        if let Some(c) = rift_carve(rift, p) {
            h = h.min(c);
        }
    }
    let wob = 8.0 * wobble(81, rad(aphi) * 3.0, 3.0);
    if let Some(c) = crater(r, wob) {
        h = h.min(c);
    }
    let edges = [p[0], p[1], WORLD - p[0], WORLD - p[1]];
    let (i, e) = edges.iter().copied().enumerate().fold((0, f64::MAX), |b, (i, e)| if e < b.1 { (i, e) } else { b });
    if let Some(rh) = rim(e, if i % 2 == 0 { p[1] } else { p[0] }) {
        h = h.max(rh);
    }
    h
}

const WORLD: f64 = crate::movement::WORLD_SIZE as f64;

/// The continent's shape, every sample (rows in parallel: the result doesn't
/// depend on how they're split).
pub(crate) fn base() -> Vec<f64> {
    let n = TERRAIN_N;
    let mut h = vec![0.0; n * n];
    let rifts = [Rift::new(0), Rift::new(1), Rift::new(2)];
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(16);
    let rows_per = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (chunk, rows) in h.chunks_mut(rows_per * n).enumerate() {
            let rifts = &rifts;
            s.spawn(move || {
                for (i, v) in rows.iter_mut().enumerate() {
                    let (ix, iy) = (i % n, chunk * rows_per + i / n);
                    *v = base_at(Kiln::pos(ix, iy), rifts);
                }
            });
        }
    });
    h
}

/// Raises the ground to a flat-topped landform: `top` inside the radius
/// `radius(angle)` around `c`, then a face of about 5:1 and a talus down to
/// the ground. `rough` wobbles the top.
pub(crate) fn mesa(k: &mut Kiln, c: [f64; 2], radius: &dyn Fn(f64) -> f64, top: f64, rmax: f64, rough: f64) {
    let reach = rmax + (top - HOLLOW_FLOOR) / 4.5 + 8.0;
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [reach, reach]), add(c, [reach, reach]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let p = Kiln::pos(ix, iy);
            let v = sub(p, c);
            let d = len(v) - radius(libm::atan2(v[1], v[0]));
            let i = iy * TERRAIN_N + ix;
            let g = k.h[i];
            if g >= top {
                continue;
            }
            let hm = if d <= 0.0 {
                top + rough * fbm(91, p, 30.0)
            } else {
                let fw = (top - g) / 4.5 + 4.0;
                let t = d / fw;
                if t >= 1.0 {
                    continue;
                }
                let f = if t < 0.8 { 0.85 * t / 0.8 } else { 0.85 + 0.15 * (t - 0.8) / 0.2 };
                lerp(top, g, f)
            };
            k.h[i] = g.max(hm);
        }
    }
}

/// A smooth mound (or, with `height` < 0, a bowl) of `height` and radius `r`:
/// a raised cosine, its steepest slope `height * pi / 2r`.
pub(crate) fn mound(k: &mut Kiln, c: [f64; 2], r: f64, height: f64) {
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [r, r]), add(c, [r, r]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let d = dist(Kiln::pos(ix, iy), c);
            if d < r {
                k.h[iy * TERRAIN_N + ix] += height * 0.5 * (1.0 + libm::cos(std::f64::consts::PI * d / r));
            }
        }
    }
}

/// Flattens a disc to `level`, blending back to the ground over `blend` m.
pub(crate) fn pad(k: &mut Kiln, c: [f64; 2], r: f64, level: f64, blend: f64) {
    let reach = r + blend;
    let (x0, x1, y0, y1) = Kiln::samples(sub(c, [reach, reach]), add(c, [reach, reach]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let d = dist(Kiln::pos(ix, iy), c);
            if d < reach {
                let i = iy * TERRAIN_N + ix;
                let w = 1.0 - smoothstep(r, reach, d);
                k.h[i] = lerp(k.h[i], level, w);
                if d < r {
                    k.mask[i] |= MASK_PAD | MASK_CLEAR;
                }
            }
        }
    }
}

/// Sets a rectangle (world-axis-aligned) to `level`, blending over `blend`.
pub(crate) fn pad_rect(k: &mut Kiln, min: [f64; 2], max: [f64; 2], level: f64, blend: f64) {
    let (x0, x1, y0, y1) = Kiln::samples(sub(min, [blend, blend]), add(max, [blend, blend]));
    for iy in y0..=y1 {
        for ix in x0..=x1 {
            let p = Kiln::pos(ix, iy);
            let dx = (min[0] - p[0]).max(p[0] - max[0]).max(0.0);
            let dy = (min[1] - p[1]).max(p[1] - max[1]).max(0.0);
            let d = len([dx, dy]);
            if d < blend || blend == 0.0 && d == 0.0 {
                let i = iy * TERRAIN_N + ix;
                let w = if blend > 0.0 { 1.0 - smoothstep(0.0, blend, d) } else { 1.0 };
                k.h[i] = lerp(k.h[i], level, w);
                if d == 0.0 {
                    k.mask[i] |= MASK_PAD | MASK_CLEAR;
                }
            }
        }
    }
}

/// The nearest distance from `p` to any lane's path, and to any site (beyond
/// its half-size).
pub(crate) fn clearance(k: &Kiln, p: [f64; 2]) -> (f64, f64) {
    let mut lane = f64::MAX;
    for c in &k.corridors {
        for w in c.windows(2) {
            lane = lane.min(seg_dist(p, w[0], w[1]).0);
        }
    }
    let site = k.sites.iter().map(|s| dist(p, s.center) - s.half).fold(f64::MAX, f64::min);
    (lane, site)
}

/// How far `p` is from the nearest rift's floor edge (negative: inside a rift).
pub(crate) fn rift_clearance(p: [f64; 2]) -> f64 {
    (0..3)
        .map(|j| {
            let rift = Rift::new(j);
            let (along, across) = rift.coords(p);
            if along < HOLLOW_R {
                f64::MAX
            } else if along <= RIFT_END {
                across.abs() - Rift::half_width(along)
            } else {
                len([along - RIFT_END, across]) - Rift::half_width(RIFT_END)
            }
        })
        .fold(f64::MAX, f64::min)
}

/// A deterministic stream of numbers in [0, 1).
pub(crate) struct Rng(u64, u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed, 0)
    }
    pub fn next(&mut self) -> f64 {
        self.1 += 1;
        (hash(self.0, 1234, self.1 as i64, 0) >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// Mesas, buttes and outcrops in the mesa country and on the Crown, and the
/// Dry Sea's reefs: placed the same in every sector (each candidate kept
/// only where all three of its copies are clear of lanes, sites and rifts).
pub(crate) fn features(k: &mut Kiln) {
    let frames = [Frame::new(0), Frame::new(1), Frame::new(2)];
    let clear_everywhere = |k: &Kiln, l: [f64; 2], r: f64, lanes: f64, sites: f64, rifts: f64, others: &[([f64; 2], f64)]| {
        frames.iter().all(|f| {
            let p = f.world(l);
            let (dl, ds) = clearance(k, p);
            dl > r + lanes && ds > r + sites && rift_clearance(p) > r + rifts && others.iter().all(|&(c, rr)| dist(c, p) > r + rr + 30.0)
        })
    };
    // Mesas: tops higher toward the center (460 near the Crown, 320 at the edge).
    let mut rng = Rng::new(0x3E5A);
    let mut placed: Vec<([f64; 2], f64, f64)> = Vec::new();
    let mut tries = 0;
    while placed.len() < 16 && tries < 4000 {
        tries += 1;
        let (r, deg, size) = (rng.range(1520.0, 2560.0), rng.range(-56.0, 56.0), rng.range(45.0, 150.0));
        let l = scale(unit(deg), r);
        let others: Vec<_> = placed.iter().map(|&(c, rr, _)| (frames[0].world(c), rr)).collect();
        if !clear_everywhere(k, l, size * 1.25, 45.0, 60.0, 50.0, &others) {
            continue;
        }
        // As tall over the ground as a mesa of 318-462 m over the mesa
        // country's flats (where the Dry Sea's basin reaches in, over its salt).
        let top = lerp(462.0, 318.0, (r - 1520.0) / 1040.0) + rng.range(-22.0, 8.0);
        placed.push((l, size, top - MESA_FLATS_OUT));
    }
    // Buttes: small and tall, and hoodoo-like crags.
    let mut buttes: Vec<([f64; 2], f64, f64)> = Vec::new();
    tries = 0;
    while buttes.len() < 22 && tries < 4000 {
        tries += 1;
        let (r, deg, size) = (rng.range(1500.0, 3300.0), rng.range(-57.0, 57.0), rng.range(14.0, 34.0));
        let l = scale(unit(deg), r);
        let mut others: Vec<_> = placed.iter().map(|&(c, rr, _)| (frames[0].world(c), rr * 1.25)).collect();
        others.extend(buttes.iter().map(|&(c, rr, _)| (frames[0].world(c), rr)));
        if !clear_everywhere(k, l, size, 30.0, 45.0, 40.0, &others) {
            continue;
        }
        // In the Dry Sea's basin they'd stand in the salt: fine, but not in its dunes' troughs.
        // 35-100 m over the ground, wherever they stand.
        let tall = rng.range(35.0, 100.0) * lerp(1.0, 0.6, ((r - 1500.0) / 1800.0).clamp(0.0, 1.0));
        buttes.push((l, size, tall));
    }
    for (i, &(l, size, tall)) in placed.iter().chain(buttes.iter()).enumerate() {
        let harm: Vec<(f64, f64, f64)> = (1..5).map(|n| (n as f64, rng.range(0.0, std::f64::consts::TAU), rng.range(0.03, 0.16) / n as f64)).collect();
        for f in &frames {
            let c = f.world(l);
            let rot = rad(Frame::axis_deg(f.k));
            let radius = |a: f64| size * (1.0 + harm.iter().map(|&(n, ph, amp)| amp * libm::sin(n * (a - rot) + ph)).sum::<f64>());
            let top = k.height(c) + tall;
            mesa(k, c, &radius, top, size * 1.4, if i < placed.len() { 1.5 } else { 0.6 });
            k.mesas.push((c, size * 1.4));
        }
    }
    // Outcrops on the Crown: low rounded knobs to fight over.
    let mut knobs: Vec<([f64; 2], f64)> = Vec::new();
    tries = 0;
    while knobs.len() < 9 && tries < 3000 {
        tries += 1;
        let (r, deg, size) = (rng.range(870.0, 1350.0), rng.range(-50.0, 50.0), rng.range(25.0, 55.0));
        let l = scale(unit(deg), r);
        let others: Vec<_> = knobs.iter().map(|&(c, rr)| (frames[0].world(c), rr)).collect();
        if !clear_everywhere(k, l, size, 20.0, 30.0, 40.0, &others) {
            continue;
        }
        knobs.push((l, size));
        let height = size / 1.9;
        for f in &frames {
            mound(k, f.world(l), size, height);
        }
    }
    // The Glasslands' blast scars and the Dune sea's hollows: shallow bowls.
    for (sector, count, rmin, rmax) in [(0usize, 7, 30.0, 70.0), (1, 4, 30.0, 50.0)] {
        let f = frames[sector];
        let mut n = 0;
        tries = 0;
        while n < count && tries < 2000 {
            tries += 1;
            let (r, deg, size) = (rng.range(2750.0, 3650.0), rng.range(-55.0, 55.0), rng.range(rmin, rmax));
            let p = f.polar(r, deg);
            let (dl, ds) = clearance(k, p);
            if dl < size + 20.0 || ds < size + 40.0 || rift_clearance(p) < size + 40.0 {
                continue;
            }
            mound(k, p, size, -size / 4.5);
            n += 1;
        }
    }
    reefs(k, &mut rng);
}

/// The Dry Sea's petrified reefs: long ridges 30-80 m tall on the basin's
/// salt, steep-sided, flat along their crests.
fn reefs(k: &mut Kiln, rng: &mut Rng) {
    let f = Frame::new(2);
    let mut n = 0;
    let mut tries = 0;
    while n < 9 && tries < 3000 {
        tries += 1;
        let (r, deg) = (rng.range(2550.0, 3150.0), rng.range(-34.0, 34.0));
        let a = rng.range(0.0, 180.0);
        let (half_len, half_w, tall) = (rng.range(60.0, 170.0), rng.range(14.0, 26.0), rng.range(30.0, 80.0));
        let c = f.polar(r, deg);
        let d = scale(unit(a), half_len);
        let (p0, p1) = (sub(c, d), add(c, d));
        // Clear of lanes and sites along its whole length.
        let ok = (0..=8).all(|i| {
            let p = add(p0, scale(sub(p1, p0), i as f64 / 8.0));
            let (dl, ds) = clearance(k, p);
            dl > half_w + 28.0 && ds > half_w + 40.0 && rift_clearance(p) > 60.0 && k.mesas.iter().all(|&(m, mr)| dist(m, p) > mr + half_w + 30.0)
        });
        if !ok {
            continue;
        }
        n += 1;
        let reach = half_len + half_w + tall / 4.0 + 8.0;
        let (x0, x1, y0, y1) = Kiln::samples(sub(c, [reach, reach]), add(c, [reach, reach]));
        let crest = SALT + tall;
        for iy in y0..=y1 {
            for ix in x0..=x1 {
                let p = Kiln::pos(ix, iy);
                let (dd, t) = seg_dist(p, p0, p1);
                // Narrower toward its ends.
                let w = half_w * (0.55 + 0.45 * (1.0 - (2.0 * t - 1.0) * (2.0 * t - 1.0)).sqrt());
                let i = iy * TERRAIN_N + ix;
                let g = k.h[i];
                let top = crest + 3.0 * fbm(93, p, 25.0) - 12.0 * (2.0 * t - 1.0) * (2.0 * t - 1.0);
                let hm = if dd <= w {
                    top
                } else {
                    let fw = (top - g).max(0.0) / 5.0 + 3.0;
                    let s = (dd - w) / fw;
                    if s >= 1.0 {
                        continue;
                    }
                    lerp(top, g, s)
                };
                k.h[i] = g.max(hm);
            }
        }
        k.mesas.push((c, half_len + half_w));
    }
}

/// Where along a face at height `z` a ramp hugging it runs: the face falls
/// from `top` to `ground` over about (top - ground) / 4.5 m beyond its edge.
pub(crate) fn face_offset(z: f64, top: f64, ground: f64) -> f64 {
    let fw = (top - ground) / 4.5 + 4.0;
    let t = ((top - z) / (top - ground).max(1.0)).clamp(0.0, 1.0);
    // Inverse of the face's profile (85% of the drop over 80% of its width).
    let s = if t < 0.85 { t / 0.85 * 0.8 } else { 0.8 + (t - 0.85) / 0.15 * 0.2 };
    s * fw
}

/// Each site's ground.
pub(crate) fn sites(k: &mut Kiln) {
    for i in 0..k.sites.len() {
        let (kind, c, half) = (k.sites[i].kind, k.sites[i].center, k.sites[i].half);
        let level = match kind {
            SiteKind::Hollow => HOLLOW_FLOOR,
            SiteKind::Span => DECK,
            SiteKind::Cliffhold | SiteKind::RiftLedge | SiteKind::Geode | SiteKind::MesaCitadel | SiteKind::SlotHold | SiteKind::ReefWarren => {
                build::shape_site(k, i)
            }
            SiteKind::Cenote => build::shape_site(k, i),
            SiteKind::Ossuary => {
                let level = k.mean(c, half);
                pad(k, c, half, level, 40.0);
                build::shape_site(k, i);
                level
            }
            SiteKind::GlassVault => {
                let level = k.mean(c, half + 30.0);
                pad(k, c, half + 30.0, level, 40.0);
                build::shape_site(k, i);
                level
            }
            _ => {
                let level = k.mean(c, half);
                pad(k, c, half, level, if kind == SiteKind::Landing { 60.0 } else { 30.0 });
                if matches!(kind, SiteKind::WellShrine | SiteKind::TunnelMouth) {
                    build::shape_site(k, i);
                }
                level
            }
        };
        k.sites[i].level = level;
        if kind == SiteKind::Span {
            build::shape_site(k, i);
        }
    }
}

/// What each sample's ground is (`Ground`), from where it is, how high and
/// how steep, and the roads and pads stamped on it.
pub(crate) fn ground(k: &mut Kiln) {
    let n = TERRAIN_N;
    let mut out = vec![0u8; n * n];
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(16);
    let rows_per = n.div_ceil(threads);
    let k_ref: &Kiln = k;
    std::thread::scope(|scope| {
        for (chunk, rows) in out.chunks_mut(rows_per * n).enumerate() {
            scope.spawn(move || ground_rows(k_ref, chunk * rows_per, rows));
        }
    });
    k.ground = out;
}

/// `ground` for the rows from `first` (filling `out`, whole rows).
fn ground_rows(k: &Kiln, first: usize, out: &mut [u8]) {
    let n = TERRAIN_N;
    for (r, row) in out.chunks_mut(n).enumerate() {
        let iy = first + r;
        for (ix, cell) in row.iter_mut().enumerate() {
            let i = iy * n + ix;
            let p = Kiln::pos(ix, iy);
            let at = |dx: i64, dy: i64| k.h[((iy as i64 + dy).clamp(0, n as i64 - 1) as usize) * n + (ix as i64 + dx).clamp(0, n as i64 - 1) as usize];
            let slope = len([(at(1, 0) - at(-1, 0)) / 8.0, (at(0, 1) - at(0, -1)) / 8.0]);
            let z = k.h[i];
            let r = dist(p, CENTER);
            let (sector, _) = sector_of(p);
            let g = if k.mask[i] & MASK_ROAD != 0 {
                Ground::Road
            } else if k.mask[i] & MASK_PAD != 0 {
                Ground::Pad
            } else if slope > 1.4 {
                if z < 150.0 && (rift_clearance(p) < 120.0 || r < HOLLOW_R) {
                    Ground::Basalt
                } else if z > 455.0 {
                    Ground::Pale
                } else {
                    Ground::Ochre
                }
            } else if slope > 0.75 {
                Ground::Scree
            } else if z > 455.0 || (r < CROWN_R + 30.0 && r > HOLLOW_R - 20.0 && z > 400.0) {
                Ground::Pale
            } else if r < HOLLOW_R + 10.0 || rift_clearance(p) < 0.0 {
                if z < 130.0 {
                    Ground::Basalt
                } else {
                    Ground::Ochre
                }
            } else if z > 290.0 {
                Ground::Ochre
            } else {
                let outer = smoothstep(2350.0, 2700.0, r) + 0.35 * fbm(97, p, 70.0);
                if outer < 0.5 {
                    Ground::Sand
                } else {
                    match BIOMES[sector] {
                        Biome::Glasslands => Ground::Glass,
                        Biome::DuneSea => Ground::Sand,
                        Biome::DrySea => {
                            if z < SALT + 6.0 {
                                Ground::Salt
                            } else {
                                Ground::Sand
                            }
                        }
                    }
                }
            };
            *cell = g as u8;
        }
    }
}
