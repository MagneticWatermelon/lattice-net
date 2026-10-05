//! What a client knows about other entities, and where it draws them.
//!
//! Every entity has one timeline of samples, whatever tier they came from:
//! near states (30 Hz, with velocity) and mid/far blobs (10 and 2 Hz, without),
//! each stamped with its game step. Drawing at render step `r`:
//!
//! - between two samples: interpolate;
//! - past the newest: extrapolate, with the near tier's velocity or one derived
//!   from the two newest samples, for as long as the entity's last update
//!   interval (its tier's rate: 33 ms near, 0.5 s far, longer when the server
//!   degrades), but at least `MAX_EXTRAPOLATION` and at most `MAX_INTERVAL`;
//!   then hold;
//! - before the oldest (an entity that just appeared): show the oldest.
//!
//! A sample that changes what's on screen (it ends an extrapolation, or came
//! late) doesn't snap the entity: the difference becomes a visual offset that
//! decays over `SMOOTH`. That difference, before smoothing, is the entity's
//! *pop*, which the bots measure per tier.
//!
//! An entity that stops coming (it left this client's interest, or despawned:
//! neither is sent) is dropped once it's twice its update interval overdue,
//! but no sooner than `FORGET_MIN` and no later than `FORGET`.

use std::collections::HashMap;

use lattice_game::delta::{self, NearHistory, NearQ};
use lattice_game::movement::TICK_HZ;
use lattice_game::msg::{self, BlobState};
use lattice_game::tier::Tier;

/// An entity is extrapolated for at least this long past its newest sample,
/// even when its updates come faster, in steps (250 ms).
pub const MAX_EXTRAPOLATION: f64 = 7.5;
/// Longest update interval extrapolation and derived velocities span, in
/// steps (2 s; far updates at the bottom of the ladder are 1.5 s apart).
pub const MAX_INTERVAL: f64 = 60.0;
/// An entity whose newest sample is this far behind the render step is
/// forgotten, in steps (2 s)...
pub const FORGET: f64 = 60.0;
/// ...or sooner, at twice its update interval, but not before this (0.5 s).
pub const FORGET_MIN: f64 = 15.0;
/// Visual offsets decay with this time constant, in steps (100 ms).
pub const SMOOTH: f64 = 3.0;
/// An offset longer than this is a teleport, not an error: dropped, in meters.
/// (A far entity extrapolated for 1.5 s can be off by several meters.)
pub const TELEPORT: f32 = 10.0;
/// Samples kept per entity.
const SAMPLES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Game step the state is at.
    pub step: f64,
    pub tier: Tier,
    /// x, y and the height of the feet.
    pub pos: [f32; 3],
    /// Horizontal velocity, if the tier sends it (near does).
    pub vel: Option<[f32; 2]>,
    /// Radians.
    pub yaw: f32,
    pub pitch: f32,
    pub airborne: bool,
}

impl Sample {
    pub fn from_near(step: f64, q: &NearQ) -> Self {
        let p = q.pos();
        Self {
            step,
            tier: Tier::Near,
            pos: [p[0], p[1], q.z()],
            vel: Some(q.vel()),
            yaw: q.yaw(),
            pitch: (q.pitch as f32 * 256.0 + 128.0 - 32768.0) / 32768.0 * std::f32::consts::FRAC_PI_2,
            airborne: q.flags & delta::FLAG_AIRBORNE != 0,
        }
    }

    pub fn from_blob(step: f64, tier: Tier, b: &BlobState) -> Self {
        Self {
            step,
            tier,
            pos: [b.pos[0], b.pos[1], b.z],
            vel: None,
            yaw: b.yaw,
            pitch: b.pitch,
            airborne: b.airborne,
        }
    }
}

/// How an entity was drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Interpolated = 0,
    Extrapolated = 1,
    /// Past the extrapolation limit: its updates stopped coming.
    Held = 2,
    /// Before its first sample: it just appeared (drawn there).
    New = 3,
}

/// Where to draw an entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderState {
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub airborne: bool,
    /// The tier of its newest sample.
    pub tier: Tier,
    pub how: How,
}

/// An entity's latest update, by server tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Known {
    /// Server tick of the last update.
    pub tick: u32,
    pub tier: Tier,
    pub pos: [f32; 2],
}

#[derive(Debug, Clone)]
struct Track {
    known: Known,
    /// Ascending by step.
    samples: [Sample; SAMPLES],
    len: usize,
    /// Visual offset, as of render step `offset_at`.
    offset: [f32; 3],
    offset_at: f64,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let d = (b - a + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
    (a + d * t).rem_euclid(tau)
}

impl Track {
    fn new(known: Known, s: Sample) -> Self {
        Self { known, samples: [s; SAMPLES], len: 1, offset: [0.0; 3], offset_at: 0.0 }
    }

    fn samples(&self) -> &[Sample] {
        &self.samples[..self.len]
    }

    fn newest(&self) -> &Sample {
        &self.samples[self.len - 1]
    }

    fn insert(&mut self, s: Sample) {
        let at = self.samples().partition_point(|o| o.step < s.step);
        if at < self.len && self.samples[at].step == s.step {
            self.samples[at] = s;
            return;
        }
        if self.len == SAMPLES {
            if at == 0 {
                return; // older than everything we keep
            }
            self.samples.copy_within(1..at, 0);
            self.samples[at - 1] = s;
        } else {
            self.samples.copy_within(at..self.len, at + 1);
            self.samples[at] = s;
            self.len += 1;
        }
    }

    /// Where the samples alone put the entity at render step `r`.
    fn raw(&self, r: f64) -> RenderState {
        let s = self.samples();
        let n = s.len();
        let state = |x: &Sample, pos: [f32; 3], how| RenderState {
            pos,
            yaw: x.yaw,
            pitch: x.pitch,
            airborne: x.airborne,
            tier: self.newest().tier,
            how,
        };
        if r < s[0].step {
            return state(&s[0], s[0].pos, How::New);
        }
        let i = s.partition_point(|x| x.step <= r);
        if i < n {
            // s[i - 1].step <= r < s[i].step
            let (a, b) = (&s[i - 1], &s[i]);
            let t = ((r - a.step) / (b.step - a.step)) as f32;
            return RenderState {
                pos: [lerp(a.pos[0], b.pos[0], t), lerp(a.pos[1], b.pos[1], t), lerp(a.pos[2], b.pos[2], t)],
                yaw: lerp_angle(a.yaw, b.yaw, t),
                pitch: lerp(a.pitch, b.pitch, t),
                airborne: if t < 0.5 { a.airborne } else { b.airborne },
                tier: self.newest().tier,
                how: How::Interpolated,
            };
        }
        let last = &s[n - 1];
        let ahead = r - last.step;
        if ahead == 0.0 {
            return state(last, last.pos, How::Interpolated);
        }
        // Velocity in m per step: the tier's own, or from the two newest
        // samples; extrapolated for as long as updates have been apart.
        let interval = if n >= 2 { last.step - s[n - 2].step } else { f64::INFINITY };
        let derived = (interval <= MAX_INTERVAL).then(|| {
            let (p, dt) = (&s[n - 2], interval as f32);
            [(last.pos[0] - p.pos[0]) / dt, (last.pos[1] - p.pos[1]) / dt, (last.pos[2] - p.pos[2]) / dt]
        });
        let limit = if interval <= MAX_INTERVAL { interval.max(MAX_EXTRAPOLATION) } else { MAX_EXTRAPOLATION };
        let per_step = 1.0 / TICK_HZ as f32;
        let v = match (last.vel, derived) {
            (Some(v), d) => [v[0] * per_step, v[1] * per_step, d.map_or(0.0, |d| d[2])],
            (None, Some(d)) => d,
            (None, None) => [0.0; 3],
        };
        let dt = ahead.min(limit) as f32;
        let pos = [last.pos[0] + v[0] * dt, last.pos[1] + v[1] * dt, last.pos[2] + v[2] * dt];
        state(last, pos, if ahead <= limit { How::Extrapolated } else { How::Held })
    }

    /// Whether it's overdue enough to forget at render step `r`.
    fn gone(&self, r: f64) -> bool {
        let s = self.samples();
        let n = s.len();
        let interval = if n >= 2 { s[n - 1].step - s[n - 2].step } else { FORGET };
        r - s[n - 1].step > (2.0 * interval).clamp(FORGET_MIN, FORGET)
    }

    fn offset(&self, r: f64) -> [f32; 3] {
        let k = (-(r - self.offset_at).max(0.0) / SMOOTH).exp() as f32;
        self.offset.map(|o| o * k)
    }

    fn render(&self, r: f64) -> RenderState {
        let mut s = self.raw(r);
        let o = self.offset(r);
        for (p, o) in s.pos.iter_mut().zip(o) {
            *p += o;
        }
        s
    }

    /// Adds a sample. With the render step it arrived at, the change it makes
    /// on screen is absorbed into the offset; returns that change, in meters.
    fn add(&mut self, s: Sample, at: Option<f64>) -> Option<f32> {
        let Some(r) = at else {
            self.insert(s);
            return None;
        };
        let before = self.raw(r).pos;
        self.insert(s);
        let after = self.raw(r).pos;
        let cur = self.offset(r);
        let d = [before[0] - after[0], before[1] - after[1], before[2] - after[2]];
        let o = [cur[0] + d[0], cur[1] + d[1], cur[2] + d[2]];
        let len = (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt();
        self.offset = if len > TELEPORT { [0.0; 3] } else { o };
        self.offset_at = r;
        Some((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
    }
}

/// Smoothness, per tier (near, mid, far).
#[derive(Debug, Clone, Default)]
pub struct SmoothStats {
    /// Entity-frames drawn, by tier and `How`.
    pub frames: [[u64; 4]; 3],
    /// Pops: what each sample arriving changed on screen before smoothing,
    /// in mm (capped at 65 m), until drained.
    pub pops: [Vec<u16>; 3],
}

/// Every entity this client has heard about.
#[derive(Debug, Default)]
pub struct Entities {
    tracks: HashMap<u16, Track>,
    /// Near states by entity and tick: the baselines near deltas refer to.
    near: NearHistory,
    /// Update intervals per tier in server ticks, until drained.
    pub intervals: [Vec<u16>; 3],
    pub bad_blobs: u64,
    pub smooth: SmoothStats,
}

impl Entities {
    /// A near message: `step` maps its server tick to a game step (`None` if
    /// unknown yet), `render` is the render step it arrived at.
    pub(crate) fn on_near(
        &mut self,
        data: &[u8],
        step: impl Fn(u32) -> Option<f64>,
        render: Option<f64>,
    ) -> Result<(), lattice_net::wire::DecodeError> {
        let mut got = Vec::new();
        let tick = delta::decode_near(data, &mut self.near, |e, q| got.push((e, q)))?;
        let at = step(tick);
        for (e, q) in got {
            let known = Known { tick, tier: Tier::Near, pos: q.pos() };
            self.record(e, known, at.map(|s| Sample::from_near(s, &q)), render);
        }
        Ok(())
    }

    pub(crate) fn on_blob(&mut self, tick: u32, step: Option<f64>, tier: Tier, blob: &[u8], render: Option<f64>) {
        let Ok(b) = msg::decode_blob(blob) else {
            self.bad_blobs += 1;
            return;
        };
        let known = Known { tick, tier, pos: b.pos };
        self.record(b.entity, known, step.map(|s| Sample::from_blob(s, tier, &b)), render);
    }

    fn record(&mut self, entity: u16, now: Known, sample: Option<Sample>, render: Option<f64>) {
        let tier = now.tier;
        match self.tracks.get_mut(&entity) {
            Some(t) => {
                let gap = now.tick.wrapping_sub(t.known.tick);
                if gap > 0 && gap < u16::MAX as u32 {
                    self.intervals[tier as usize].push(gap as u16);
                }
                if gap as i32 >= 0 {
                    t.known = now;
                }
                if let Some(s) = sample {
                    if let Some(pop) = t.add(s, render) {
                        self.smooth.pops[tier as usize].push((pop * 1000.0).round().min(u16::MAX as f32) as u16);
                    }
                }
            }
            None => {
                if let Some(s) = sample {
                    self.tracks.insert(entity, Track::new(now, s));
                }
            }
        }
    }

    /// Draws every entity at render step `r`: calls `f(entity, state)`,
    /// counts how each was drawn, and forgets entities that stopped coming.
    pub fn render(&mut self, r: f64, mut f: impl FnMut(u16, &RenderState)) {
        let frames = &mut self.smooth.frames;
        self.tracks.retain(|&e, t| {
            if t.gone(r) {
                return false;
            }
            let s = t.render(r);
            frames[s.tier as usize][s.how as usize] += 1;
            f(e, &s);
            true
        });
    }

    /// Where entity `entity` is drawn at render step `r`, without counting it.
    pub fn render_one(&self, entity: u16, r: f64) -> Option<RenderState> {
        self.tracks.get(&entity).map(|t| t.render(r))
    }

    /// The newest sample of `entity`: where the server last said it was,
    /// with no delay and no smoothing (the client's server ghost).
    pub fn newest(&self, entity: u16) -> Option<Sample> {
        self.tracks.get(&entity).map(|t| *t.newest())
    }

    /// The near state received for `entity` at `tick`, if still in history.
    pub fn near_state(&self, entity: u16, tick: u32) -> Option<NearQ> {
        self.near.get(entity, tick)
    }

    pub fn get(&self, entity: u16) -> Option<Known> {
        self.tracks.get(&entity).map(|t| t.known)
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(step: f64, x: f32, vel: Option<[f32; 2]>) -> Sample {
        Sample { step, tier: if vel.is_some() { Tier::Near } else { Tier::Far }, pos: [x, 0.0, 10.0], vel, yaw: 0.0, pitch: 0.0, airborne: false }
    }

    #[test]
    fn interpolates_between_samples_and_keeps_them_in_order() {
        let k = Known { tick: 0, tier: Tier::Near, pos: [0.0; 2] };
        let mut t = Track::new(k, s(10.0, 0.0, Some([0.0; 2])));
        t.add(s(12.0, 2.0, Some([0.0; 2])), None);
        t.add(s(11.0, 0.5, Some([0.0; 2])), None); // late, out of order
        assert_eq!(t.samples().iter().map(|x| x.step).collect::<Vec<_>>(), [10.0, 11.0, 12.0]);
        let r = t.raw(11.5);
        assert_eq!((r.pos[0], r.how), (1.25, How::Interpolated));
        assert_eq!(t.raw(9.0).how, How::New, "before the first sample");
        for i in 13..30 {
            t.add(s(i as f64, i as f32, None), None);
        }
        assert_eq!(t.len, SAMPLES);
        assert_eq!(t.samples()[0].step, 22.0, "the oldest are dropped");
    }

    #[test]
    fn extrapolates_with_velocity_for_a_while_then_holds() {
        let k = Known { tick: 0, tier: Tier::Near, pos: [0.0; 2] };
        // Near: 6 m/s, so 0.2 m per step.
        let t = Track::new(k, s(10.0, 0.0, Some([6.0, 0.0])));
        let r = t.raw(12.0);
        assert!((r.pos[0] - 0.4).abs() < 1e-5 && r.how == How::Extrapolated);
        let r = t.raw(30.0);
        assert!((r.pos[0] - 0.2 * MAX_EXTRAPOLATION as f32).abs() < 1e-5 && r.how == How::Held);
        // Far: velocity derived from the two newest samples (3 m over 15 steps).
        let mut t = Track::new(k, s(0.0, 0.0, None));
        t.add(s(15.0, 3.0, None), None);
        let r = t.raw(20.0);
        assert!((r.pos[0] - 4.0).abs() < 1e-5 && r.how == How::Extrapolated, "{r:?}");
        assert!((r.pos[2] - 10.0).abs() < 1e-5, "flat: height stays");
        // ...for as long as its updates are apart (15 steps), then held.
        let r = t.raw(15.0 + 15.0);
        assert!((r.pos[0] - 6.0).abs() < 1e-5 && r.how == How::Extrapolated, "{r:?}");
        assert_eq!(t.raw(15.0 + 15.5).how, How::Held);
    }

    #[test]
    fn a_correction_is_smoothed_not_popped() {
        let k = Known { tick: 0, tier: Tier::Near, pos: [0.0; 2] };
        let mut t = Track::new(k, s(10.0, 0.0, Some([6.0, 0.0])));
        // Drawn at 12 while extrapolating: 0.4 m. Then the sample for 11
        // arrives: the entity had stopped at 0.1.
        let before = t.render(12.0).pos[0];
        let pop = t.add(s(11.0, 0.1, Some([0.0; 2])), Some(12.0)).unwrap();
        assert!((pop - 0.3).abs() < 1e-5, "pop {pop}");
        assert!((t.render(12.0).pos[0] - before).abs() < 1e-6, "no jump on screen");
        let later = t.render(12.0 + 4.0 * SMOOTH).pos[0];
        assert!((later - 0.1).abs() < 0.3 * 0.02, "the offset decays: {later}");
        // A sample beyond the render step doesn't change what's drawn: no pop.
        let mut t = Track::new(k, s(10.0, 0.0, Some([6.0, 0.0])));
        t.add(s(11.0, 0.2, Some([6.0, 0.0])), None);
        assert_eq!(t.add(s(12.0, 0.4, Some([6.0, 0.0])), Some(10.5)), Some(0.0));
        // A teleport isn't smoothed.
        t.add(s(13.0, 150.0, Some([6.0, 0.0])), Some(12.9));
        assert_eq!(t.offset, [0.0; 3]);
    }

    #[test]
    fn angles_interpolate_the_short_way() {
        let a = 6.2;
        let b = 0.1;
        let m = lerp_angle(a, b, 0.5);
        assert!(!(0.5..6.0).contains(&m), "crosses 0, not pi: {m}");
    }

    #[test]
    fn entities_that_stop_coming_are_forgotten() {
        let mut e = Entities::default();
        let k = Known { tick: 0, tier: Tier::Far, pos: [0.0; 2] };
        // Far, every 15 steps: forgotten 30 steps after its last.
        e.record(1, k, Some(s(0.0, 0.0, None)), None);
        e.record(1, k, Some(s(15.0, 0.0, None)), None);
        // Near, every step: forgotten after FORGET_MIN.
        let near = Known { tier: Tier::Near, ..k };
        e.record(2, near, Some(s(44.0, 0.0, Some([0.0; 2]))), None);
        e.record(2, near, Some(s(45.0, 0.0, Some([0.0; 2]))), None);
        // One sample: no interval yet, so FORGET.
        e.record(3, k, Some(s(10.0, 0.0, None)), None);
        let count = |e: &mut Entities, r: f64| {
            let mut n = 0;
            e.render(r, |_, _| n += 1);
            n
        };
        assert_eq!(count(&mut e, 45.0), 3);
        assert_eq!(count(&mut e, 45.1), 2, "the far one is 2 intervals overdue");
        assert_eq!(count(&mut e, 45.0 + FORGET_MIN + 0.1), 1);
        assert_eq!(count(&mut e, 10.0 + FORGET + 0.1), 0);
        assert!(e.is_empty());
    }
}
