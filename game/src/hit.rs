//! Segment tests for projectiles: against players' hitboxes, cover boxes and
//! the terrain. Each returns the fraction `t` in [0, 1] along the segment
//! where it first hits, if it does.

use crate::world::World;

/// Player hitboxes, from the feet: a body capsule and a head sphere. (Only
/// for shots; moving uses the movement collider.)
pub const BODY_RADIUS: f32 = 0.4;
pub const BODY_LOW: f32 = 0.4;
pub const BODY_HIGH: f32 = 1.1;
pub const HEAD_RADIUS: f32 = 0.2;
pub const HEAD_AT: f32 = 1.6;

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

/// Where a segment hits a player standing with feet at `feet`: (t, head).
/// The head counts when it's first along the segment.
pub fn player(p0: V, p1: V, feet: V) -> Option<(f32, bool)> {
    // Cheap reject: the segment's box against the player's.
    let lo = [p0[0].min(p1[0]), p0[1].min(p1[1]), p0[2].min(p1[2])];
    let hi = [p0[0].max(p1[0]), p0[1].max(p1[1]), p0[2].max(p1[2])];
    let r = BODY_RADIUS;
    if hi[0] < feet[0] - r || lo[0] > feet[0] + r || hi[1] < feet[1] - r || lo[1] > feet[1] + r || hi[2] < feet[2] || lo[2] > feet[2] + HEAD_AT + HEAD_RADIUS {
        return None;
    }
    let head = sphere(p0, p1, [feet[0], feet[1], feet[2] + HEAD_AT], HEAD_RADIUS);
    let body = capsule(p0, p1, [feet[0], feet[1], feet[2] + BODY_LOW], [feet[0], feet[1], feet[2] + BODY_HIGH], BODY_RADIUS);
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

/// Whether nothing (terrain or cover) stands between `p0` and `p1`: what a
/// player at `p0` can see of `p1`. The server's "after cover" count and the
/// bots' targeting use the same test.
pub fn line_clear(world: &World, p0: V, p1: V) -> bool {
    if terrain(world, p0, p1).is_some() {
        return false;
    }
    let mid = [(p0[0] + p1[0]) / 2.0, (p0[1] + p1[1]) / 2.0];
    let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
    let half = (dx * dx + dy * dy).sqrt() / 2.0;
    let mut clear = true;
    world.boxes_near(mid[0], mid[1], half + 1.0, |c| {
        clear &= aabb(p0, p1, [c.min[0], c.min[1], c.bottom], [c.max[0], c.max[1], c.top]).is_none();
    });
    clear
}

/// The segment p0 -> p1 against the terrain: sampled at most every 2 m,
/// then the crossing refined by bisection.
pub fn terrain(world: &World, p0: V, p1: V) -> Option<f32> {
    let below = |t: f32| {
        let p = [p0[0] + (p1[0] - p0[0]) * t, p0[1] + (p1[1] - p0[1]) * t, p0[2] + (p1[2] - p0[2]) * t];
        p[2] < world.terrain(p[0], p[1])
    };
    if below(0.0) {
        return Some(0.0);
    }
    let len = dot(sub(p1, p0), sub(p1, p0)).sqrt();
    let n = (len / 2.0).ceil().max(1.0) as u32;
    let mut prev = 0.0;
    for k in 1..=n {
        let t = k as f32 / n as f32;
        if below(t) {
            let (mut lo, mut hi) = (prev, t);
            for _ in 0..8 {
                let mid = (lo + hi) / 2.0;
                if below(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return Some(hi);
        }
        prev = t;
    }
    None
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
        let (t, head) = player([0.0, 0.0, 1.6], [20.0, 0.0, 1.6], feet).unwrap();
        assert!(head && (t - 0.49).abs() < 1e-4, "eye level: head ({t})");
        let (_, head) = player([0.0, 0.0, 0.8], [20.0, 0.0, 0.8], feet).unwrap();
        assert!(!head, "chest");
        assert!(player([0.0, 0.0, 2.0], [20.0, 0.0, 2.0], feet).is_none(), "over the head");
        assert!(player([0.0, 0.6, 0.8], [20.0, 0.6, 0.8], feet).is_none(), "beside");
        // From above (a hill), the head comes first.
        let (_, head) = player([10.0, 0.0, 10.0], [10.0, 0.0, 0.0], feet).unwrap();
        assert!(head);
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
