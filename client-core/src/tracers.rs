//! Cosmetic tracers: every shot a client shows, flown with the server's
//! kinematics (`weapon::Flight`) until it hits the ground or cover, or runs
//! out of range. What to draw and when is decided here, so every client
//! shows the same thing; a client only draws the streaks and the impacts.
//!
//! - **Our shots** leave at once from the muzzle, along a pick of our own in
//!   the cone of fire (the server's pick is secret), converging on what the
//!   eye aims at. The server sends each of our shots back with where it
//!   really went, about a round trip later, and the tracer is re-aimed along
//!   it, from the same muzzle and as far along, so it lands where the shot
//!   did. One that already landed keeps its own path.
//! - **Others' shots** (`SeenShot`, from near-tier shooters) wait until their
//!   shooter is drawn at the moment it fired, then leave from its drawn
//!   muzzle: what you see firing is what fired.
//! - **Distant fights' ambient shots** (`AmbientShot`) leave at once.

use lattice_game::events::SeenShot;
use lattice_game::hit;
use lattice_game::weapon::{self, aim, Flight, EYE_HEIGHT, RANGE_STEPS, SUBSTEPS};
use lattice_game::world::World;

use crate::distant::AmbientShot;
use crate::entities::Entities;

/// Seconds per flight segment.
pub const SEG: f32 = 1.0 / (SUBSTEPS * 30) as f32;
/// How long a tracer flies at most (the projectile's range), in seconds.
pub const RANGE_SECS: f32 = RANGE_STEPS as f32 / 30.0;
/// How much of the path behind a tracer's head shows (a fading streak), m.
pub const TRAIL: f32 = 20.0;
/// Our tracers leave from a muzzle off the line of sight, aimed to converge
/// on it this far out (from the eye itself they'd be a dot under the
/// crosshair).
pub const CONVERGE: f32 = 120.0;
/// Others' shots wait this long (s) for their shooter to be drawn firing.
pub const PENDING_SECS: f32 = 1.0;
/// Tracers flying at once, at most: the oldest give way (a huge fight's
/// ambient shots are capped at 600/s, 2 s of flight each).
pub const MAX_FLYING: usize = 4096;

/// Whose shot a tracer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Own = 0,
    /// A near-tier player's, drawn exactly.
    Seen = 1,
    /// A distant fight's.
    Ambient = 2,
}

/// What a tracer hit.
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

/// One of our shots: when it fired (the server's step), the eye it left
/// from and its tracer's muzzle, until it's re-aimed along the real shot.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OwnShot {
    step: f64,
    eye: [f32; 3],
    muzzle: [f32; 3],
    real: bool,
    /// Its weapon's muzzle speed.
    speed: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tracer {
    pub flight: Flight,
    /// Where it left from.
    pub start: [f32; 3],
    /// Seconds flown, and not yet flown (less than a segment).
    pub age: f32,
    owed: f32,
    pub kind: Kind,
    /// The shooter's faction.
    pub faction: u8,
    own: Option<OwnShot>,
}

impl Tracer {
    /// The assault rifle's.
    pub fn new(origin: [f32; 3], dir: [f32; 3], kind: Kind, faction: u8) -> Self {
        Self::at(origin, dir, weapon::MUZZLE_SPEED, kind, faction)
    }

    /// At `speed` m/s (its weapon's, `weapon::Weapon::speed`).
    pub fn at(origin: [f32; 3], dir: [f32; 3], speed: f32, kind: Kind, faction: u8) -> Self {
        Self { flight: Flight::at(origin, dir, speed), start: origin, age: 0.0, owed: 0.0, kind, faction, own: None }
    }

    /// Ours: from `muzzle`, converging on what the eye at `eye` aims at along
    /// `aim_dir` (a direction in the cone of fire), `CONVERGE` out.
    fn ours(eye: [f32; 3], muzzle: [f32; 3], aim_dir: [f32; 3], faction: u8, own: Option<OwnShot>, speed: f32) -> Self {
        let target = [0, 1, 2].map(|k| eye[k] + aim_dir[k] * CONVERGE);
        let d = [0, 1, 2].map(|k| target[k] - muzzle[k]);
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
        Self { own, ..Self::at(muzzle, d.map(|v| v / len), speed, Kind::Own, faction) }
    }

    /// The streak to draw: from its tail (up to `TRAIL` behind the head, no
    /// longer than the path flown) to its head.
    pub fn streak(&self) -> ([f32; 3], [f32; 3]) {
        let f = &self.flight;
        let p = f.pos;
        let s = self.start;
        let flown = ((p[0] - s[0]).powi(2) + (p[1] - s[1]).powi(2) + (p[2] - s[2]).powi(2)).sqrt();
        let speed = (f.vel[0] * f.vel[0] + f.vel[1] * f.vel[1] + f.vel[2] * f.vel[2]).sqrt().max(1.0);
        let back = TRAIL.min(flown) / speed;
        ([p[0] - f.vel[0] * back, p[1] - f.vel[1] * back, p[2] - f.vel[2] * back], p)
    }
}

/// Where a segment from `a` to `b`, fired by `faction`, first hits the
/// ground or cover (its own faction's shields let it through).
pub fn segment_hit(world: &World, a: [f32; 3], b: [f32; 3], faction: u8) -> Option<Impact> {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let dir = if len > 1e-6 { d.map(|v| v / len) } else { [0.0, 0.0, -1.0] };
    let at = |t: f32| [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t];
    let mut best: Option<(f32, [f32; 3], Option<u8>)> = hit::terrain(world, a, b).map(|t| {
        let p = at(t);
        (t, terrain_normal(world, p[0], p[1]), None)
    });
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    world.boxes_near(mid[0], mid[1], len / 2.0 + 1.0, |c| {
        if !c.blocks(faction) {
            return;
        }
        if let Some(t) = hit::solid(a, b, c) {
            if best.is_none_or(|(bt, _, _)| t < bt) {
                let normal = if c.is_turned() || c.is_wedge() {
                    hit::solid_normal(a, b, t, c)
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
    let e = lattice_game::world::TERRAIN_RES * 0.5;
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

/// Every tracer a client shows, and the shots waiting for theirs.
#[derive(Debug, Default)]
pub struct Tracers {
    pub flying: Vec<Tracer>,
    /// Others' shots waiting for their shooter to be drawn firing: (shot,
    /// seconds when it arrived).
    pending: Vec<(SeenShot, f32)>,
    /// Our own shots as the server says they went, to re-aim their tracers.
    real: Vec<SeenShot>,
}

/// A shooter drawn firing: where a muzzle flash and a shot's sound go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fired {
    /// `None` for a distant fight's shot from no one drawn.
    pub shooter: Option<u16>,
    pub faction: u8,
    pub origin: [f32; 3],
    pub dir: [f32; 3],
    pub kind: Kind,
    /// The shooter's weapon's kit (a distant fight's: the assault rifle's).
    pub kit: u8,
}

/// A cosmetic pellet's spread: any well-spread bits will do (the server's
/// picks are secret).
fn pellet_pick(step: f64, k: u8) -> u64 {
    let mut h = step.to_bits() ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^ (h >> 33)
}

impl Tracers {
    /// Others' shots waiting for their shooter to be drawn firing.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn push(&mut self, t: Tracer) {
        if self.flying.len() >= MAX_FLYING {
            // The oldest is first in line (they're pushed in order).
            self.flying.remove(0);
        }
        self.flying.push(t);
    }

    /// One of our shots, fired from the eye at `eye` along `dir` (in the cone
    /// of fire) while looking at (`yaw`, `pitch`) with kit `kit`'s weapon:
    /// its tracer leaves from the muzzle `offset` from the eye (meters
    /// right, up and forward of the view); a shotgun's pellets too, each
    /// its own way (cosmetic: the server's picks are secret). `step`: when
    /// it fired, in the server's steps (`ClientCore::last_shot_step`), to
    /// re-aim it along the real shot.
    #[allow(clippy::too_many_arguments)]
    pub fn fire_own(&mut self, eye: [f32; 3], yaw: f32, pitch: f32, dir: [f32; 3], offset: [f32; 3], step: Option<f64>, faction: u8, kit: u8) -> Fired {
        let muzzle = muzzle(eye, yaw, pitch, offset);
        let w = weapon::of(kit);
        let own = step.map(|step| OwnShot { step, eye, muzzle, real: false, speed: w.speed });
        self.push(Tracer::ours(eye, muzzle, dir, faction, own, w.speed));
        for k in 1..w.pellets {
            let d = w.pellet(dir, pellet_pick(step.unwrap_or(0.0), k));
            self.push(Tracer::ours(eye, muzzle, d, faction, None, w.speed));
        }
        Fired { shooter: None, faction, origin: muzzle, dir, kind: Kind::Own, kit }
    }

    /// Shots the server says were fired (`ClientCore::drain_news`): ours
    /// re-aim their tracers; others wait for their shooter to be drawn
    /// firing. `secs`: the client's clock now, in seconds.
    pub fn on_shots(&mut self, own: Option<u16>, shots: impl IntoIterator<Item = SeenShot>, secs: f32) {
        for s in shots {
            if Some(s.shooter) == own {
                self.real.push(s);
            } else {
                self.pending.push((s, secs));
            }
        }
    }

    /// Distant fights' shots (`ClientCore::drain_ambient`), leaving now from
    /// their muzzles (`offset` from the eye, as for `fire_own`).
    pub fn on_ambient(&mut self, shots: &[AmbientShot], offset: [f32; 3], mut fired: impl FnMut(Fired)) {
        for a in shots {
            let (yaw, pitch) = (a.dir[1].atan2(a.dir[0]), a.dir[2].clamp(-1.0, 1.0).asin());
            let o = muzzle(a.origin, yaw, pitch, offset);
            self.push(Tracer::new(o, a.dir, Kind::Ambient, a.faction));
            fired(Fired { shooter: a.shooter, faction: a.faction, origin: o, dir: a.dir, kind: Kind::Ambient, kit: 0 });
        }
    }

    /// Starts the waiting shots whose shooters are drawn at the moment they
    /// fired, at render step `r`, from their drawn muzzles (`offset` from
    /// the eye); drops ones whose shooter isn't drawn within `PENDING_SECS`.
    /// Then re-aims our tracers the server's word has come back for.
    pub fn update(&mut self, ents: Option<&Entities>, r: Option<f64>, secs: f32, world: &World, offset: [f32; 3], mut fired: impl FnMut(Fired)) {
        let mut pending = std::mem::take(&mut self.pending);
        if let (Some(ents), Some(r)) = (ents, r) {
            pending.retain(|(shot, at)| match ents.render_one(shot.shooter, r) {
                Some(st) if st.at >= shot.step => {
                    let eye = [st.pos[0], st.pos[1], st.pos[2] + EYE_HEIGHT];
                    let dir = aim(shot.yaw, shot.pitch);
                    let (yaw, pitch) = (shot.yaw as f32 / 65536.0 * std::f32::consts::TAU, shot.pitch as f32 / 32767.0 * std::f32::consts::FRAC_PI_2);
                    let o = muzzle(eye, yaw, pitch, offset);
                    let faction = lattice_game::faction::faction(shot.shooter);
                    let w = weapon::of(st.kit);
                    self.push(Tracer::at(o, dir, w.speed, Kind::Seen, faction));
                    for k in 1..w.pellets {
                        self.push(Tracer::at(o, w.pellet(dir, pellet_pick(shot.step, k)), w.speed, Kind::Seen, faction));
                    }
                    fired(Fired { shooter: Some(shot.shooter), faction, origin: o, dir, kind: Kind::Seen, kit: st.kit });
                    false
                }
                _ => secs - *at < PENDING_SECS,
            });
        } else {
            pending.retain(|(_, at)| secs - *at < PENDING_SECS);
        }
        self.pending = pending;
        for shot in std::mem::take(&mut self.real) {
            self.correct(&shot, world);
        }
    }

    /// Our shot `shot` really went along its direction: re-aim its tracer,
    /// still in flight, from the same muzzle and as far along, so it lands
    /// where the shot did; if the real path already hit something by then,
    /// it ends. One that already landed keeps its own path.
    pub fn correct(&mut self, shot: &SeenShot, world: &World) {
        let close = |t: &&mut Tracer| t.own.is_some_and(|o| !o.real && (o.step - shot.step).abs() < 0.5);
        let Some((i, t)) = self.flying.iter_mut().enumerate().filter(|(_, t)| close(t)).min_by(|a, b| {
            let d = |t: &Tracer| t.own.map_or(f64::MAX, |o| (o.step - shot.step).abs());
            d(a.1).total_cmp(&d(b.1))
        }) else {
            return;
        };
        let o = t.own.expect("matched an own shot");
        let mut real = Tracer::ours(o.eye, o.muzzle, aim(shot.yaw, shot.pitch), t.faction, Some(OwnShot { real: true, ..o }), o.speed);
        let mut flown = 0.0;
        while flown + SEG <= t.age + 1e-6 {
            let next = real.flight.advance();
            if segment_hit(world, real.flight.pos, next.pos, real.faction).is_some() {
                self.flying.swap_remove(i);
                return;
            }
            real.flight = next;
            flown += SEG;
        }
        (real.age, real.owed) = (t.age, t.owed);
        *t = real;
    }

    /// Flies every tracer `dt` seconds on, segment by segment; one that hits
    /// the ground or cover ends there (`impact`), one out of range just ends.
    pub fn fly(&mut self, dt: f32, world: &World, mut impact: impl FnMut(&Tracer, Impact)) {
        self.flying.retain_mut(|t| {
            t.owed += dt;
            while t.owed >= SEG {
                let next = t.flight.advance();
                t.owed -= SEG;
                t.age += SEG;
                if let Some(hit) = segment_hit(world, t.flight.pos, next.pos, t.faction) {
                    impact(t, hit);
                    return false;
                }
                if t.age > RANGE_SECS {
                    return false;
                }
                t.flight = next;
            }
            true
        });
    }
}

/// A muzzle `offset` (right, up, forward, meters) from an eye looking at
/// (`yaw`, `pitch`).
pub fn muzzle(eye: [f32; 3], yaw: f32, pitch: f32, offset: [f32; 3]) -> [f32; 3] {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let fwd = [cp * cy, cp * sy, sp];
    let right = [sy, -cy, 0.0];
    let up = [-cy * sp, -sy * sp, cp];
    [0, 1, 2].map(|k| eye[k] + right[k] * offset[0] + up[k] * offset[1] + fwd[k] * offset[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_game::weapon::angles;

    const OFFSET: [f32; 3] = [0.25, -0.2, 0.6];

    /// One of our tracers 40 m above the ground at (2000, 2000), fired
    /// east at step 100 and flown `segments` segments.
    fn flying(world: &World, segments: u32) -> Tracers {
        let eye = [2000.0, 2000.0, world.terrain(2000.0, 2000.0) + 40.0];
        let mut tracers = Tracers::default();
        tracers.fire_own(eye, 0.0, 0.0, aim(0, 0), OFFSET, Some(100.0), 0, 0);
        let t = &mut tracers.flying[0];
        for _ in 0..segments {
            t.flight = t.flight.advance();
        }
        t.age = segments as f32 * SEG;
        tracers
    }

    fn shot(step: f64, dir: [f32; 3]) -> SeenShot {
        let (yaw, pitch) = angles(dir);
        SeenShot { shooter: 7, step, yaw, pitch }
    }

    #[test]
    fn our_tracer_follows_the_real_shot() {
        let world = World::shared(1);
        let real = aim(300, 120); // ~1.6 degrees right and ~0.3 up of the aim
        let mut tracers = flying(&world, 6);
        // Another shot's word (3 steps later) leaves it alone.
        tracers.correct(&shot(103.0, real), &world);
        assert!(tracers.flying[0].own.is_some_and(|o| !o.real));
        tracers.correct(&shot(100.0, real), &world);
        let t = &tracers.flying[0];
        let o = t.own.unwrap();
        assert!(o.real, "re-aimed");
        // Exactly where the real shot's tracer is after as many segments, from
        // the same muzzle.
        let s = shot(0.0, real);
        let mut want = Tracer::ours(o.eye, o.muzzle, aim(s.yaw, s.pitch), 0, None, weapon::MUZZLE_SPEED);
        for _ in 0..6 {
            want.flight = want.flight.advance();
        }
        assert_eq!(t.flight, want.flight);
        assert_eq!(t.age, 6.0 * SEG);
        // A second word for the same shot changes nothing.
        let before = t.flight;
        tracers.correct(&shot(100.0, aim(0, -200)), &world);
        assert_eq!(tracers.flying[0].flight, before);
    }

    #[test]
    fn a_real_shot_that_already_landed_ends_its_tracer() {
        let world = World::shared(1);
        // Flown 30 segments (300 m): straight down, the real shot hit the
        // ground 40 m below long before.
        let mut tracers = flying(&world, 30);
        tracers.correct(&shot(100.0, aim(0, -32000)), &world);
        assert!(tracers.flying.is_empty());
    }

    #[test]
    fn tracers_land_on_the_ground_with_its_normal() {
        let world = World::shared(1);
        let (x, y) = (3000.0, 3000.0);
        let g = world.terrain(x, y);
        let mut tracers = Tracers::default();
        tracers.push(Tracer::new([x, y, g + 30.0], [0.0, 0.0, -1.0], Kind::Seen, 1));
        let mut hits = Vec::new();
        for _ in 0..30 {
            tracers.fly(1.0 / 60.0, &world, |t, hit| hits.push((t.kind, hit)));
        }
        assert!(tracers.flying.is_empty(), "it landed");
        let (kind, hit) = hits[0];
        assert_eq!(kind, Kind::Seen);
        assert!((hit.pos[2] - g).abs() < 0.05, "on the ground: {} vs {g}", hit.pos[2]);
        assert!(hit.normal[2] > 0.5 && hit.cover.is_none(), "the terrain faces up: {:?}", hit.normal);
    }

    #[test]
    fn tracers_stop_at_cover_and_face_out_of_it() {
        let world = World::shared(1);
        // The first wall, hit square on from the west, a meter below its top.
        let b = world.boxes().iter().find(|b| b.kind == lattice_game::world::Kind::Wall && b.max[0] - b.min[0] < 1.0).unwrap();
        let (y, z) = ((b.min[1] + b.max[1]) / 2.0, b.top - 1.0);
        let hit = segment_hit(&world, [b.min[0] - 5.0, y, z], [b.max[0] + 5.0, y, z], 0).expect("it hits the wall");
        assert_eq!(hit.cover, Some(lattice_game::world::Kind::Wall as u8));
        assert!((hit.pos[0] - b.min[0]).abs() < 1e-3, "on its west face");
        assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn muzzles_sit_in_the_view_frame() {
        let eye = [10.0, 20.0, 30.0];
        // Looking east, level: right is south (-y), up is up.
        let m = muzzle(eye, 0.0, 0.0, [1.0, 2.0, 3.0]);
        let want = [13.0, 19.0, 32.0];
        assert!((0..3).all(|k| (m[k] - want[k]).abs() < 1e-5), "{m:?}");
        // Looking straight up, forward is up and "up" is back (west).
        let m = muzzle(eye, 0.0, std::f32::consts::FRAC_PI_2, [0.0, 1.0, 1.0]);
        let want = [9.0, 20.0, 31.0];
        assert!((0..3).all(|k| (m[k] - want[k]).abs() < 1e-5), "{m:?}");
    }
}
