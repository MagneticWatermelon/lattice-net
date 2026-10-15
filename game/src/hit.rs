//! Segment tests for projectiles: against players' hitboxes, cover (boxes,
//! turned boxes, wedges, shields) and the terrain (a shell, open at its
//! holes). Each returns the fraction `t` in [0, 1] along the segment where
//! it first hits, if it does.

use crate::world::{CoverBox, World};

/// Player hitboxes, from the feet: a body capsule and a head sphere. (Only
/// for shots; moving uses the movement collider.) The head is centered on
/// the eyes, standing (1.6 m) and crouched (1.15 m: a kneel, torso upright),
/// so no stance sees over cover with less of its head than standing does.
/// Crouched, the body covers 0-1.1 m and the head 0.95-1.35 m: its top
/// shows over the crouched collider's 1.2 m, as cover rules expect (GDD
/// §6.2: low cover hides most of a crouching player).
pub const BODY_RADIUS: f32 = 0.4;
pub const BODY_LOW: f32 = 0.4;
pub const BODY_HIGH: f32 = 1.1;
pub const HEAD_RADIUS: f32 = 0.2;
pub const HEAD_AT: f32 = 1.6;
pub const CROUCH_BODY_HIGH: f32 = 0.7;
pub const CROUCH_HEAD_AT: f32 = 1.15;

/// Where to check a line of sight to a player, from its feet: its chest.
pub const fn chest(crouched: bool) -> f32 {
    if crouched {
        0.75
    } else {
        1.0
    }
}

type V = [f32; 3];

fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V, b: V) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The segment p0 -> p1 against a sphere.
pub fn sphere(p0: V, p1: V, c: V, r: f32) -> Option<f32> {
    let d = sub(p1, p0);
    let m = sub(p0, c);
    let (a, b, cc) = (dot(d, d), dot(m, d), dot(m, m) - r * r);
    if cc <= 0.0 {
        return Some(0.0); // starts inside
    }
    if a == 0.0 || b > 0.0 {
        return None;
    }
    let disc = b * b - a * cc;
    if disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    (t <= 1.0).then_some(t.max(0.0))
}

/// The segment p0 -> p1 against a capsule: points within `r` of a..b.
pub fn capsule(p0: V, p1: V, a: V, b: V, r: f32) -> Option<f32> {
    // Inigo Quilez's capsule intersection, on the normalized ray.
    let seg = sub(p1, p0);
    let len = dot(seg, seg).sqrt();
    if len == 0.0 {
        return None;
    }
    let rd = seg.map(|v| v / len);
    let (ba, oa) = (sub(b, a), sub(p0, a));
    let (baba, bard, baoa, rdoa, oaoa) = (dot(ba, ba), dot(ba, rd), dot(ba, oa), dot(rd, oa), dot(oa, oa));
    let qa = baba - bard * bard;
    let mut best: Option<f32> = None;
    if qa > 1e-9 {
        let qb = baba * rdoa - baoa * bard;
        let qc = baba * oaoa - baoa * baoa - r * r * baba;
        let h = qb * qb - qa * qc;
        if h >= 0.0 {
            let t = (-qb - h.sqrt()) / qa;
            let y = baoa + t * bard;
            if y > 0.0 && y < baba {
                best = Some(t);
            }
        }
    }
    // The end caps (also the whole answer for a ray along the axis).
    for c in [a, b] {
        if let Some(t) = sphere(p0, p1, c, r) {
            let t = t * len;
            if best.is_none_or(|b| t < b) {
                best = Some(t);
            }
        }
    }
    // Starting inside the cylinder part.
    let inside = {
        let y = baoa / baba;
        (0.0..=1.0).contains(&y) && dot(sub(oa, ba.map(|v| v * y)), sub(oa, ba.map(|v| v * y))) <= r * r
    };
    if inside {
        return Some(0.0);
    }
    best.filter(|&t| (0.0..=len).contains(&t)).map(|t| t / len)
}

/// Where a segment hits a player with feet at `feet`, standing or
/// `crouched`: (t, head). The head counts when it's first along the segment.
pub fn player(p0: V, p1: V, feet: V, crouched: bool) -> Option<(f32, bool)> {
    player_sized(p0, p1, feet, crouched, false)
}

/// An Exo's hitboxes: its armor wider (`EXO_BODY_RADIUS`), its helmet bigger.
pub const EXO_BODY_RADIUS: f32 = 0.55;
pub const EXO_HEAD_RADIUS: f32 = 0.24;

/// `player`, for an Exo if `wide`.
pub fn player_sized(p0: V, p1: V, feet: V, crouched: bool, wide: bool) -> Option<(f32, bool)> {
    let (body_high, head_at) = if crouched { (CROUCH_BODY_HIGH, CROUCH_HEAD_AT) } else { (BODY_HIGH, HEAD_AT) };
    let (body_r, head_r) = if wide { (EXO_BODY_RADIUS, EXO_HEAD_RADIUS) } else { (BODY_RADIUS, HEAD_RADIUS) };
    // Cheap reject: the segment's box against the player's.
    let lo = [p0[0].min(p1[0]), p0[1].min(p1[1]), p0[2].min(p1[2])];
    let hi = [p0[0].max(p1[0]), p0[1].max(p1[1]), p0[2].max(p1[2])];
    let r = body_r;
    if hi[0] < feet[0] - r || lo[0] > feet[0] + r || hi[1] < feet[1] - r || lo[1] > feet[1] + r || hi[2] < feet[2] || lo[2] > feet[2] + head_at + head_r {
        return None;
    }
    let head = sphere(p0, p1, [feet[0], feet[1], feet[2] + head_at], head_r);
    let body = capsule(p0, p1, [feet[0], feet[1], feet[2] + BODY_LOW], [feet[0], feet[1], feet[2] + body_high], body_r);
    match (head, body) {
        (Some(h), Some(b)) if h <= b => Some((h, true)),
        (_, Some(b)) => Some((b, false)),
        (Some(h), None) => Some((h, true)),
        (None, None) => None,
    }
}

/// The segment p0 -> p1 against an axis-aligned box (slab test).
pub fn aabb(p0: V, p1: V, min: V, max: V) -> Option<f32> {
    let d = sub(p1, p0);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for k in 0..3 {
        if d[k].abs() < 1e-9 {
            if p0[k] < min[k] || p0[k] > max[k] {
                return None;
            }
        } else {
            let (mut a, mut b) = ((min[k] - p0[k]) / d[k], (max[k] - p0[k]) / d[k]);
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

/// The segment p0 -> p1 against a cover box of any shape (C1): in its frame
/// if it's turned, the slab test, and a wedge's sloped top as one more
/// plane. (Shields: the caller decides whom they stop, `CoverBox::blocks`.)
pub fn solid(p0: V, p1: V, b: &CoverBox) -> Option<f32> {
    if !b.is_turned() && !b.is_wedge() {
        return aabb(p0, p1, [b.min[0], b.min[1], b.bottom], [b.max[0], b.max[1], b.top]);
    }
    let (a, c) = (b.local(p0[0], p0[1]), b.local(p1[0], p1[1]));
    let (q0, q1) = ([a[0], a[1], p0[2]], [c[0], c[1], p1[2]]);
    let mut t = aabb_span(q0, q1, [-b.half[0], -b.half[1], b.bottom], [b.half[0], b.half[1], b.top])?;
    if b.is_wedge() {
        // Under the top: z <= low + k (along + half), k its rise per meter.
        let k = (b.top - b.low) / (2.0 * b.half[0]);
        let f = |q: V| q[2] - b.low - k * (q[0] + b.half[0]);
        let (f0, f1) = (f(q0), f(q1));
        if f0 > 0.0 && f1 > 0.0 {
            return None;
        }
        if f0 > 0.0 {
            t.0 = t.0.max(f0 / (f0 - f1));
        } else if f1 > 0.0 {
            t.1 = t.1.min(f0 / (f0 - f1));
        }
        if t.0 > t.1 {
            return None;
        }
    }
    Some(t.0)
}

/// Where segment p0 -> p1 enters and leaves an axis-aligned box, as
/// fractions clipped to [0, 1].
fn aabb_span(p0: V, p1: V, min: V, max: V) -> Option<(f32, f32)> {
    let d = sub(p1, p0);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for k in 0..3 {
        if d[k].abs() < 1e-9 {
            if p0[k] < min[k] || p0[k] > max[k] {
                return None;
            }
        } else {
            let (mut a, mut b) = ((min[k] - p0[k]) / d[k], (max[k] - p0[k]) / d[k]);
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((t0, t1))
}

/// A box's outward unit normal where segment p0 -> p1 hits it at `t`
/// (`solid`'s answer): the face nearest that point, in its frame (a
/// wedge's slope, where it's nearest); back along the segment from inside.
pub fn solid_normal(p0: V, p1: V, t: f32, b: &CoverBox) -> V {
    let p = [p0[0] + (p1[0] - p0[0]) * t, p0[1] + (p1[1] - p0[1]) * t, p0[2] + (p1[2] - p0[2]) * t];
    let l = b.local(p[0], p[1]);
    let d = sub(p1, p0);
    let len = dot(d, d).sqrt().max(1e-6);
    let mut best = (f32::MAX, [-d[0] / len, -d[1] / len, -d[2] / len]);
    let mut face = |gap: f32, n: V| {
        if gap < best.0 {
            best = (gap, n);
        }
    };
    let (x, y) = (b.world_dir([1.0, 0.0]), b.world_dir([0.0, 1.0]));
    face((l[0] + b.half[0]).abs(), [-x[0], -x[1], 0.0]);
    face((b.half[0] - l[0]).abs(), [x[0], x[1], 0.0]);
    face((l[1] + b.half[1]).abs(), [-y[0], -y[1], 0.0]);
    face((b.half[1] - l[1]).abs(), [y[0], y[1], 0.0]);
    face((p[2] - b.bottom).abs(), [0.0, 0.0, -1.0]);
    if b.is_wedge() {
        let k = (b.top - b.low) / (2.0 * b.half[0]);
        let s = (1.0 + k * k).sqrt();
        face((p[2] - b.top_along(l[0])).abs() / s, [-k * x[0] / s, -k * x[1] / s, 1.0 / s]);
    } else {
        face((p[2] - b.top).abs(), [0.0, 0.0, 1.0]);
    }
    best.1
}

/// Whether nothing (terrain or cover) stands between `p0` and `p1`: what a
/// player (`who`: a faction, and who holds each site) at `p0` can see of
/// `p1` (their own shields don't hide it). The server's "after cover" count
/// and the bots' targeting use the same test.
pub fn line_clear<'a>(world: &World, p0: V, p1: V, who: impl Into<crate::movement::Mover<'a>>) -> bool {
    let who = who.into();
    if terrain(world, p0, p1).is_some() {
        return false;
    }
    let mid = [(p0[0] + p1[0]) / 2.0, (p0[1] + p1[1]) / 2.0];
    let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
    let half = (dx * dx + dy * dy).sqrt() / 2.0;
    let mut clear = true;
    world.boxes_near(mid[0], mid[1], half + 1.0, |c| {
        clear &= !c.stops(&who) || solid(p0, p1, c).is_none();
    });
    clear
}

/// The segment p0 -> p1 against the terrain: sampled at most every 2 m,
/// then the crossing refined by bisection. Where it has holes, the terrain
/// is a shell: a segment hits it where it crosses the surface (from above,
/// or from under it in a tunnel) anywhere but through a hole.
pub fn terrain(world: &World, p0: V, p1: V) -> Option<f32> {
    let at = |t: f32| [p0[0] + (p1[0] - p0[0]) * t, p0[1] + (p1[1] - p0[1]) * t, p0[2] + (p1[2] - p0[2]) * t];
    let below = |t: f32| {
        let p = at(t);
        p[2] < world.terrain(p[0], p[1])
    };
    let shell = world.has_holes();
    let mut side = below(0.0);
    if side && !shell {
        return Some(0.0);
    }
    let len = dot(sub(p1, p0), sub(p1, p0)).sqrt();
    let n = (len / 2.0).ceil().max(1.0) as u32;
    let mut prev = 0.0;
    for k in 1..=n {
        let t = k as f32 / n as f32;
        if below(t) != side {
            let (mut lo, mut hi) = (prev, t);
            for _ in 0..8 {
                let mid = (lo + hi) / 2.0;
                if below(mid) != side {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let p = at(hi);
            if !shell || !world.hole(p[0], p[1]) {
                return Some(hi);
            }
            side = !side; // through a hole
        }
        prev = t;
    }
    None
}

/// What a segment hit (a tracer, a thrown gadget: `segment_hit`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impact {
    pub pos: [f32; 3],
    /// Unit normal of the surface hit.
    pub normal: [f32; 3],
    /// The flight's direction at impact (unit).
    pub dir: [f32; 3],
    /// `None` for the terrain, else the cover box's kind
    /// (`world::Kind as u8`).
    pub cover: Option<u8>,
}

/// Where a segment from `a` to `b`, fired by `faction`, first hits the
/// ground or cover (its own faction's shields let it through).
pub fn segment_hit(world: &World, a: [f32; 3], b: [f32; 3], faction: u8) -> Option<Impact> {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let dir = if len > 1e-6 { d.map(|v| v / len) } else { [0.0, 0.0, -1.0] };
    let at = |t: f32| [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t];
    let mut best: Option<(f32, [f32; 3], Option<u8>)> = terrain(world, a, b).map(|t| {
        let p = at(t);
        (t, terrain_normal(world, p[0], p[1]), None)
    });
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    world.boxes_near(mid[0], mid[1], len / 2.0 + 1.0, |c| {
        if !c.blocks(faction) {
            return;
        }
        if let Some(t) = solid(a, b, c) {
            if best.is_none_or(|(bt, _, _)| t < bt) {
                let normal = if c.is_turned() || c.is_wedge() {
                    solid_normal(a, b, t, c)
                } else {
                    box_normal(at(t), [c.min[0], c.min[1], c.bottom], [c.max[0], c.max[1], c.top], dir)
                };
                best = Some((t, normal, Some(c.kind as u8)));
            }
        }
    });
    best.map(|(t, normal, cover)| Impact { pos: at(t), normal, dir, cover })
}

/// The terrain's unit normal at (x, y), from its slope across a sample.
pub fn terrain_normal(world: &World, x: f32, y: f32) -> [f32; 3] {
    let e = crate::world::TERRAIN_RES * 0.5;
    let dx = world.terrain(x + e, y) - world.terrain(x - e, y);
    let dy = world.terrain(x, y + e) - world.terrain(x, y - e);
    let n = [-dx, -dy, 2.0 * e];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    n.map(|v| v / len)
}

/// The face of a box a point on its surface lies on, as an outward normal;
/// a point inside (a segment starting in it) faces back along `dir`.
fn box_normal(p: [f32; 3], min: [f32; 3], max: [f32; 3], dir: [f32; 3]) -> [f32; 3] {
    let mut best = (f32::MAX, [-dir[0], -dir[1], -dir[2]]);
    for k in 0..3 {
        for (face, sign) in [(min[k], -1.0), (max[k], 1.0)] {
            let gap = (p[k] - face).abs();
            if gap < best.0 {
                let mut n = [0.0; 3];
                n[k] = sign;
                best = (gap, n);
            }
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spheres_and_capsules() {
        // Straight through a sphere of radius 1 at x = 5.
        assert_eq!(sphere([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [5.0, 0.0, 0.0], 1.0), Some(0.4));
        assert_eq!(sphere([0.0, 2.0, 0.0], [10.0, 2.0, 0.0], [5.0, 0.0, 0.0], 1.0), None, "passes by");
        assert_eq!(sphere([0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [5.0, 0.0, 0.0], 1.0), None, "stops short");
        // A vertical capsule 0..2 m, r 0.5, hit across.
        let t = capsule([-5.0, 0.0, 1.0], [5.0, 0.0, 1.0], [0.0, 0.0, 0.0], [0.0, 0.0, 2.0], 0.5).unwrap();
        assert!((t - 0.45).abs() < 1e-5, "{t}");
        // Over its top cap: within r of the top point.
        assert!(capsule([-5.0, 0.0, 2.4], [5.0, 0.0, 2.4], [0.0, 0.0, 0.0], [0.0, 0.0, 2.0], 0.5).is_some());
        assert!(capsule([-5.0, 0.0, 2.6], [5.0, 0.0, 2.6], [0.0, 0.0, 0.0], [0.0, 0.0, 2.0], 0.5).is_none());
        // Straight down its axis: the top cap first.
        let t = capsule([0.0, 0.0, 5.0], [0.0, 0.0, -5.0], [0.0, 0.0, 0.0], [0.0, 0.0, 2.0], 0.5).unwrap();
        assert!((t - 0.25).abs() < 1e-5, "{t}");
    }

    #[test]
    fn players_are_hit_in_the_head_or_the_body() {
        let feet = [10.0, 0.0, 0.0];
        let (t, head) = player([0.0, 0.0, 1.6], [20.0, 0.0, 1.6], feet, false).unwrap();
        assert!(head && (t - 0.49).abs() < 1e-4, "eye level: head ({t})");
        let (_, head) = player([0.0, 0.0, 0.8], [20.0, 0.0, 0.8], feet, false).unwrap();
        assert!(!head, "chest");
        assert!(player([0.0, 0.0, 2.0], [20.0, 0.0, 2.0], feet, false).is_none(), "over the head");
        assert!(player([0.0, 0.6, 0.8], [20.0, 0.6, 0.8], feet, false).is_none(), "beside");
        // From above (a hill), the head comes first.
        let (_, head) = player([10.0, 0.0, 10.0], [10.0, 0.0, 0.0], feet, false).unwrap();
        assert!(head);
    }

    #[test]
    fn crouched_players_are_lower() {
        let feet = [10.0, 0.0, 0.0];
        // A standing head shot passes over a crouched player.
        assert!(player([0.0, 0.0, 1.6], [20.0, 0.0, 1.6], feet, true).is_none());
        assert!(player([0.0, 0.0, 1.4], [20.0, 0.0, 1.4], feet, true).is_none(), "over the crouched head");
        let (_, head) = player([0.0, 0.0, 1.2], [20.0, 0.0, 1.2], feet, true).unwrap();
        assert!(head, "its head at the crouched eyes' height");
        let (_, head) = player([0.0, 0.0, 0.5], [20.0, 0.0, 0.5], feet, true).unwrap();
        assert!(!head, "its body");
        // Over a 1.1 m wall, a crouched player's eyes see; so does a shooter
        // its head, as much of it as a standing head over a 1.5 m wall.
        let shown = |at: f32, wall: f32| at + HEAD_RADIUS - wall;
        assert!(crate::weapon::CROUCH_EYE_HEIGHT > 1.1 && (shown(CROUCH_HEAD_AT, 1.1) - shown(HEAD_AT, 1.5)).abs() < 0.06);
    }

    #[test]
    fn boxes_block() {
        let (min, max) = ([4.0, -1.0, 0.0], [5.0, 1.0, 3.0]);
        assert_eq!(aabb([0.0, 0.0, 1.0], [10.0, 0.0, 1.0], min, max), Some(0.4));
        assert_eq!(aabb([0.0, 0.0, 4.0], [10.0, 0.0, 4.0], min, max), None, "over it");
        assert_eq!(aabb([4.5, 0.0, 1.0], [10.0, 0.0, 1.0], min, max), Some(0.0), "from inside");
    }

    #[test]
    fn the_terrain_stops_shots() {
        let w = World::shared(1);
        let (x, y) = (2000.0, 3000.0);
        let g = w.terrain(x, y);
        // Down into the ground from 10 m up.
        let t = terrain(&w, [x, y, g + 10.0], [x, y, g - 10.0]).unwrap();
        assert!((t - 0.5).abs() < 0.01, "{t}");
        // Level, well above it: nothing.
        assert!(terrain(&w, [x, y, g + 200.0], [x + 10.0, y, g + 200.0]).is_none());
    }
}
