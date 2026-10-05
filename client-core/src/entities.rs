//! What a client knows about other entities, and where it draws them.
//!
//! Every entity has one timeline of samples, whatever tier they came from:
//! near states (30 Hz, with velocity) and mid/far blobs (10 and 2 Hz, without),
//! each stamped with its game step.
//!
//! **Each tier has its own render delay.** The render clock runs at the near
//! delay (~67 ms: two 30 Hz updates). Mid and far entities are drawn a fixed
//! `mid_lag` later (at ~200 ms in all: two 10 Hz updates, so a lost one is
//! bridged). 200 ms in the past is invisible at 150 m and beyond, and lag
//! compensation rewinds each target by its own tier's delay. When an entity
//! changes tier its lag glides to the new one at `LAG_SLEW` (133 ms over
//! ~0.5 s): it plays 25% fast or slow for a moment instead of skipping.
//!
//! Drawing an entity at its render step `t` (render step minus its lag):
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
/// Samples kept per entity. A mid entity is drawn ~6 steps behind its
/// newest sample, and while it changes tier its near samples come one a
/// step: 16 leaves room for late ones.
const SAMPLES: usize = 16;
/// How fast an entity's lag follows its tier's, in steps of lag per step.
pub const LAG_SLEW: f64 = 0.25;
/// The default lag of mid and far entities behind near ones, in steps
/// (200 ms - 67 ms).
pub const MID_LAG: f64 = 4.0;

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
    /// Exact for near samples, to 1/15th for mid and far.
    pub health: u8,
    pub dead: bool,
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
            health: q.health,
            dead: q.flags & delta::FLAG_DEAD != 0,
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
            health: b.health,
            dead: b.dead,
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
    pub health: u8,
    /// Dead, waiting to respawn (draw it lying down).
    pub dead: bool,
    /// The tier of its newest sample.
    pub tier: Tier,
    pub how: How,
    /// The game step it's drawn at: the render step minus its tier's lag.
    pub at: f64,
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
    /// Visual offset, as of render step `visual_at`.
    visual: [f32; 3],
    visual_at: f64,
    /// How far behind the render step it's drawn, in steps, as of render
    /// step `lag_at` (it slews toward its tier's lag).
    lag: f64,
    lag_at: f64,
    /// Where it was drawn last frame, the render step then, and dead then.
    drawn: Option<([f32; 3], f64, bool)>,
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
    fn new(known: Known, s: Sample, lag: f64, at: f64) -> Self {
        Self { known, samples: [s; SAMPLES], len: 1, visual: [0.0; 3], visual_at: at, lag, lag_at: at, drawn: None }
    }

    /// The lag its tier asks for: near none, mid and far `mid_lag`.
    fn target(&self, mid_lag: f64) -> f64 {
        if self.newest().tier == Tier::Near {
            0.0
        } else {
            mid_lag
        }
    }

    /// Its lag at render step `r`.
    fn lag(&self, r: f64, mid_lag: f64) -> f64 {
        let (target, d) = (self.target(mid_lag), LAG_SLEW * (r - self.lag_at).max(0.0));
        if self.lag < target {
            (self.lag + d).min(target)
        } else {
            (self.lag - d).max(target)
        }
    }

    /// `lag`, kept as the new starting point (the target can change after).
    fn settle_lag(&mut self, r: f64, mid_lag: f64) -> f64 {
        if r > self.lag_at {
            (self.lag, self.lag_at) = (self.lag(r, mid_lag), r);
        }
        self.lag
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
            health: x.health,
            dead: x.dead,
            tier: self.newest().tier,
            how,
            at: r,
        };
        if r < s[0].step {
            return state(&s[0], s[0].pos, How::New);
        }
        let i = s.partition_point(|x| x.step <= r);
        if i < n {
            // s[i - 1].step <= r < s[i].step
            let (a, b) = (&s[i - 1], &s[i]);
            if a.dead && !b.dead {
                // It respawned in between: the corpse stays until the new
                // life's first sample, then it's there. Never a streak
                // across the map.
                return state(a, a.pos, How::Interpolated);
            }
            let t = ((r - a.step) / (b.step - a.step)) as f32;
            return RenderState {
                pos: [lerp(a.pos[0], b.pos[0], t), lerp(a.pos[1], b.pos[1], t), lerp(a.pos[2], b.pos[2], t)],
                yaw: lerp_angle(a.yaw, b.yaw, t),
                pitch: lerp(a.pitch, b.pitch, t),
                airborne: if t < 0.5 { a.airborne } else { b.airborne },
                health: if t < 0.5 { a.health } else { b.health },
                dead: if t < 0.5 { a.dead } else { b.dead },
                tier: self.newest().tier,
                how: How::Interpolated,
                at: r,
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
        // Never across a death or respawn (a corpse to a new spawn point).
        let derived = (interval <= MAX_INTERVAL && s[n - 2].dead == last.dead).then(|| {
            let (p, dt) = (&s[n - 2], interval as f32);
            [(last.pos[0] - p.pos[0]) / dt, (last.pos[1] - p.pos[1]) / dt, (last.pos[2] - p.pos[2]) / dt]
        });
        let limit = if interval <= MAX_INTERVAL { interval.max(MAX_EXTRAPOLATION) } else { MAX_EXTRAPOLATION };
        let per_step = 1.0 / TICK_HZ as f32;
        let v = match (last.vel, derived) {
            _ if last.dead => [0.0; 3], // the dead lie still
            (Some(v), d) => [v[0] * per_step, v[1] * per_step, d.map_or(0.0, |d| d[2])],
            (None, Some(d)) => d,
            (None, None) => [0.0; 3],
        };
        let dt = ahead.min(limit) as f32;
        let pos = [last.pos[0] + v[0] * dt, last.pos[1] + v[1] * dt, last.pos[2] + v[2] * dt];
        state(last, pos, if ahead <= limit { How::Extrapolated } else { How::Held })
    }

    /// Whether it's overdue enough to forget at its own render step `t`.
    fn gone(&self, t: f64) -> bool {
        let s = self.samples();
        let n = s.len();
        let interval = if n >= 2 { s[n - 1].step - s[n - 2].step } else { FORGET };
        t - s[n - 1].step > (2.0 * interval).clamp(FORGET_MIN, FORGET)
    }

    /// What's left of the visual offset at render step `r`.
    fn visual(&self, r: f64) -> [f32; 3] {
        let k = (-(r - self.visual_at).max(0.0) / SMOOTH).exp() as f32;
        self.visual.map(|o| o * k)
    }

    /// Where it's drawn at render step `r`, lagging `lag` steps.
    fn render(&self, r: f64, lag: f64) -> RenderState {
        let mut s = self.raw(r - lag);
        let o = self.visual(r);
        for (p, o) in s.pos.iter_mut().zip(o) {
            *p += o;
        }
        s
    }

    /// Adds a sample. With the render step it arrived at, the change it makes
    /// on screen is absorbed into the visual offset; returns that change, in
    /// meters.
    fn add(&mut self, s: Sample, at: Option<f64>, mid_lag: f64) -> Option<f32> {
        let Some(r) = at else {
            self.insert(s);
            return None;
        };
        let t = r - self.settle_lag(r, mid_lag);
        let before = self.raw(t).pos;
        self.insert(s);
        let after = self.raw(t).pos;
        let cur = self.visual(r);
        let d = [before[0] - after[0], before[1] - after[1], before[2] - after[2]];
        let o = [cur[0] + d[0], cur[1] + d[1], cur[2] + d[2]];
        let len = (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt();
        self.visual = if len > TELEPORT { [0.0; 3] } else { o };
        self.visual_at = r;
        Some((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
    }
}

/// Quarter-step bins of `NearNeed`, up to 16 steps.
const NEED_BINS: usize = 64;
/// `NearNeed` forgets with this time constant, in steps (3 s).
const NEED_MEMORY: f64 = 90.0;

/// What near delay would have kept near entities interpolated. When a near
/// update arrives, the entity's previous update was the newest it had until
/// then: a render delay at least as long as that update was behind the
/// newest step (when its successor arrived) never ran past it. A decaying
/// histogram of those, in quarter steps.
///
/// It reflects every way near updates come late: loss, jitter, and the near
/// tier's per-tick cap (in a crowd, ~100 candidates share 64 sends a tick,
/// so a near entity updates every 1-3 ticks, not every tick).
#[derive(Debug, Clone)]
pub struct NearNeed {
    hist: [f32; NEED_BINS],
    at: Option<f64>,
}

impl Default for NearNeed {
    fn default() -> Self {
        Self { hist: [0.0; NEED_BINS], at: None }
    }
}

impl NearNeed {
    /// An update arrived when its predecessor was `behind` steps behind the newest.
    pub fn record(&mut self, behind: f64) {
        let b = ((behind * 4.0).ceil().max(0.0) as usize).min(NEED_BINS - 1);
        self.hist[b] += 1.0;
    }

    /// Ages the histogram to render step `r`.
    pub fn decay_to(&mut self, r: f64) {
        match self.at {
            Some(at) if r > at => {
                let k = (-(r - at) / NEED_MEMORY).exp() as f32;
                self.hist.iter_mut().for_each(|h| *h *= k);
                self.at = Some(r);
            }
            None => self.at = Some(r),
            _ => {}
        }
    }

    /// The delay that would have covered a `q` share of recent near updates,
    /// in steps; `None` with too few to tell.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let total: f32 = self.hist.iter().sum();
        if total < 30.0 {
            return None;
        }
        let mut seen = 0.0;
        for (b, &h) in self.hist.iter().enumerate() {
            seen += h;
            if seen as f64 >= q * total as f64 {
                return Some(b as f64 / 4.0);
            }
        }
        Some((NEED_BINS - 1) as f64 / 4.0)
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
    /// Entity-frames drawn moving faster than `STREAK_SPEED` (no player
    /// runs that fast: a jump or smear on screen), by tier.
    pub streaks: [u64; 3],
}

/// Drawn speeds above this are streaks: sprint is 9 m/s, a fall ~15.
pub const STREAK_SPEED: f32 = 20.0;

/// Every entity this client has heard about.
#[derive(Debug)]
pub struct Entities {
    tracks: HashMap<u16, Track>,
    /// Mid and far entities are drawn this many steps behind near ones.
    mid_lag: f64,
    /// Near states by entity and tick: the baselines near deltas refer to.
    near: NearHistory,
    /// Update intervals per tier in server ticks, until drained.
    pub intervals: [Vec<u16>; 3],
    pub bad_blobs: u64,
    pub smooth: SmoothStats,
    /// What near delay recent near updates needed (see `NearNeed`).
    pub need: NearNeed,
}

impl Default for Entities {
    fn default() -> Self {
        Self::new(MID_LAG)
    }
}

impl Entities {
    /// Mid and far entities drawn `mid_lag` steps behind near ones.
    pub fn new(mid_lag: f64) -> Self {
        Self {
            tracks: HashMap::new(),
            mid_lag: mid_lag.max(0.0),
            near: NearHistory::default(),
            intervals: Default::default(),
            bad_blobs: 0,
            smooth: SmoothStats::default(),
            need: NearNeed::default(),
        }
    }

    pub fn mid_lag(&self) -> f64 {
        self.mid_lag
    }

    /// Mid and far entities glide to the new lag (`LAG_SLEW`).
    pub fn set_mid_lag(&mut self, lag: f64) {
        self.mid_lag = lag.max(0.0);
    }

    /// A near message: `step` maps its server tick to a game step (`None` if
    /// unknown yet), `render` is the render step it arrived at and `newest`
    /// the newest step the server had sent by then.
    pub(crate) fn on_near(
        &mut self,
        data: &[u8],
        step: impl Fn(u32) -> Option<f64>,
        render: Option<f64>,
        newest: Option<f64>,
    ) -> Result<(), lattice_net::wire::DecodeError> {
        let mut got = Vec::new();
        let tick = delta::decode_near(data, &mut self.near, |e, q| got.push((e, q)))?;
        let at = step(tick);
        for (e, q) in got {
            let known = Known { tick, tier: Tier::Near, pos: q.pos() };
            let sample = at.map(|s| Sample::from_near(s, &q));
            // What delay its previous near update needed (see NearNeed).
            if let (Some(s), Some(newest), Some(t)) = (sample, newest, self.tracks.get(&e)) {
                let prev = t.newest();
                if prev.tier == Tier::Near && s.step > prev.step && s.step - prev.step <= 8.0 {
                    self.need.record(newest - prev.step);
                }
            }
            self.record(e, known, sample, render);
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
                    if let Some(pop) = t.add(s, render, self.mid_lag) {
                        self.smooth.pops[tier as usize].push((pop * 1000.0).round().min(u16::MAX as f32) as u16);
                    }
                }
            }
            None => {
                if let Some(s) = sample {
                    let lag = if s.tier == Tier::Near { 0.0 } else { self.mid_lag };
                    self.tracks.insert(entity, Track::new(now, s, lag, render.unwrap_or(0.0)));
                }
            }
        }
    }

    /// Draws every entity at render step `r` (each behind it by its tier's
    /// lag): calls `f(entity, state)`, counts how each was drawn, and forgets
    /// entities that stopped coming.
    pub fn render(&mut self, r: f64, mut f: impl FnMut(u16, &RenderState)) {
        let (smooth, mid_lag) = (&mut self.smooth, self.mid_lag);
        self.tracks.retain(|&e, t| {
            let lag = t.settle_lag(r, mid_lag);
            if t.gone(r - lag) {
                return false;
            }
            let s = t.render(r, lag);
            smooth.frames[s.tier as usize][s.how as usize] += 1;
            if let Some((p, at, was_dead)) = t.drawn {
                let dt = (r - at) as f32 / TICK_HZ as f32;
                let d = ((s.pos[0] - p[0]).powi(2) + (s.pos[1] - p[1]).powi(2) + (s.pos[2] - p[2]).powi(2)).sqrt();
                // A respawn is a cut (dead last frame, alive now), not a streak.
                if dt > 0.0 && d > 0.3 && d / dt > STREAK_SPEED && !(was_dead && !s.dead) {
                    smooth.streaks[s.tier as usize] += 1;
                }
            }
            t.drawn = Some((s.pos, r, s.dead));
            f(e, &s);
            true
        });
    }

    /// Where entity `entity` is drawn at render step `r`, without counting it.
    pub fn render_one(&self, entity: u16, r: f64) -> Option<RenderState> {
        self.tracks.get(&entity).map(|t| t.render(r, t.lag(r, self.mid_lag)))
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

    impl Track {
        /// A near-timeline track (no lag) for the timeline tests.
        fn new_test(k: Known, s: Sample) -> Self {
            Track::new(k, s, 0.0, 0.0)
        }

        /// `add` with no mid lag: every tier on the near timeline.
        fn add_test(&mut self, s: Sample, at: Option<f64>) -> Option<f32> {
            self.add(s, at, 0.0)
        }
    }

    fn s(step: f64, x: f32, vel: Option<[f32; 2]>) -> Sample {
        Sample { step, tier: if vel.is_some() { Tier::Near } else { Tier::Far }, pos: [x, 0.0, 10.0], vel, yaw: 0.0, pitch: 0.0, airborne: false, health: 100, dead: false }
    }

    #[test]
    fn interpolates_between_samples_and_keeps_them_in_order() {
        let k = Known { tick: 0, tier: Tier::Near, pos: [0.0; 2] };
        let mut t = Track::new_test(k, s(10.0, 0.0, Some([0.0; 2])));
        t.add_test(s(12.0, 2.0, Some([0.0; 2])), None);
        t.add_test(s(11.0, 0.5, Some([0.0; 2])), None); // late, out of order
        assert_eq!(t.samples().iter().map(|x| x.step).collect::<Vec<_>>(), [10.0, 11.0, 12.0]);
        let r = t.raw(11.5);
        assert_eq!((r.pos[0], r.how), (1.25, How::Interpolated));
        assert_eq!(t.raw(9.0).how, How::New, "before the first sample");
        for i in 13..30 {
            t.add_test(s(i as f64, i as f32, None), None);
        }
        assert_eq!(t.len, SAMPLES);
        assert_eq!(t.samples()[0].step, 14.0, "the oldest are dropped");
    }

    #[test]
    fn extrapolates_with_velocity_for_a_while_then_holds() {
        let k = Known { tick: 0, tier: Tier::Near, pos: [0.0; 2] };
        // Near: 6 m/s, so 0.2 m per step.
        let t = Track::new_test(k, s(10.0, 0.0, Some([6.0, 0.0])));
        let r = t.raw(12.0);
        assert!((r.pos[0] - 0.4).abs() < 1e-5 && r.how == How::Extrapolated);
        let r = t.raw(30.0);
        assert!((r.pos[0] - 0.2 * MAX_EXTRAPOLATION as f32).abs() < 1e-5 && r.how == How::Held);
        // Far: velocity derived from the two newest samples (3 m over 15 steps).
        let mut t = Track::new_test(k, s(0.0, 0.0, None));
        t.add_test(s(15.0, 3.0, None), None);
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
        let mut t = Track::new_test(k, s(10.0, 0.0, Some([6.0, 0.0])));
        // Drawn at 12 while extrapolating: 0.4 m. Then the sample for 11
        // arrives: the entity had stopped at 0.1.
        let before = t.render(12.0, 0.0).pos[0];
        let pop = t.add_test(s(11.0, 0.1, Some([0.0; 2])), Some(12.0)).unwrap();
        assert!((pop - 0.3).abs() < 1e-5, "pop {pop}");
        assert!((t.render(12.0, 0.0).pos[0] - before).abs() < 1e-6, "no jump on screen");
        let later = t.render(12.0 + 4.0 * SMOOTH, 0.0).pos[0];
        assert!((later - 0.1).abs() < 0.3 * 0.02, "the offset decays: {later}");
        // A sample beyond the render step doesn't change what's drawn: no pop.
        let mut t = Track::new_test(k, s(10.0, 0.0, Some([6.0, 0.0])));
        t.add_test(s(11.0, 0.2, Some([6.0, 0.0])), None);
        assert_eq!(t.add_test(s(12.0, 0.4, Some([6.0, 0.0])), Some(10.5)), Some(0.0));
        // A teleport isn't smoothed.
        t.add_test(s(13.0, 150.0, Some([6.0, 0.0])), Some(12.9));
        assert_eq!(t.visual, [0.0; 3]);
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
        let mut e = Entities::new(0.0);
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

    #[test]
    fn tiers_draw_at_their_own_delay_and_glide_between_them() {
        // An entity running east at 6 m/s (0.2 m a step), one sample a step.
        let mut e = Entities::new(MID_LAG);
        let k = |tier| Known { tick: 0, tier, pos: [0.0; 2] };
        let at = |step: f64, tier| Sample { step, tier, pos: [step as f32 * 0.2, 0.0, 0.0], vel: Some([6.0, 0.0]), yaw: 0.0, pitch: 0.0, airborne: false, health: 100, dead: false };
        let draw = |e: &mut Entities, r: f64| {
            let mut got = None;
            e.render(r, |_, s| got = Some(*s));
            got.unwrap()
        };
        // Mid: drawn MID_LAG behind the render step.
        for step in 0..40 {
            e.record(1, k(Tier::Mid), Some(at(step as f64, Tier::Mid)), Some(step as f64 - 2.0));
        }
        let s = draw(&mut e, 37.0);
        assert_eq!((s.at, s.how), (37.0 - MID_LAG, How::Interpolated));
        // It becomes near: the lag glides to 0 at LAG_SLEW, never jumping.
        // One sample a step, two frames a step.
        let mut last = s;
        let mut r = 37.0;
        for step in 40..80 {
            e.record(1, k(Tier::Near), Some(at(step as f64, Tier::Near)), Some(r));
            for _ in 0..2 {
                r += 0.5;
                let s = draw(&mut e, r);
                let moved = (s.pos[0] - last.pos[0]) as f64;
                assert!((0.0..=0.2 * 0.5 * (1.0 + LAG_SLEW) + 1e-4).contains(&moved), "at most 25% fast: {moved} m in half a step");
                assert_eq!(s.how, How::Interpolated);
                last = s;
            }
        }
        assert_eq!(last.at, r, "settled on the near timeline");
        // And back to mid: slower, never backwards.
        for step in 80..120 {
            e.record(1, k(Tier::Mid), Some(at(step as f64, Tier::Mid)), Some(r));
            for _ in 0..2 {
                r += 0.5;
                let s = draw(&mut e, r);
                let moved = (s.pos[0] - last.pos[0]) as f64;
                assert!(moved >= 0.2 * 0.5 * (1.0 - LAG_SLEW) - 1e-4, "at most 25% slow: {moved}");
                assert_eq!(s.how, How::Interpolated);
                last = s;
            }
        }
        assert!((last.at - (r - MID_LAG)).abs() < 1e-9);
    }

    #[test]
    fn near_need_tracks_how_late_near_updates_come() {
        let mut n = NearNeed::default();
        assert_eq!(n.quantile(0.99), None, "too few to tell");
        // Updates every step, each arriving as the newest: 1 step behind.
        for _ in 0..100 {
            n.record(1.0);
        }
        assert_eq!(n.quantile(0.99), Some(1.0));
        // In a crowd, every 1-3 steps: the 99th percentile is 3.
        for i in 0..300 {
            n.record([1.0, 2.0, 3.0][i % 3]);
        }
        assert_eq!(n.quantile(0.99), Some(3.0));
        assert_eq!((n.quantile(0.5), n.quantile(0.6)), (Some(1.0), Some(2.0)), "400 updates: 200 at 1, 100 at 2, 100 at 3");
        // It forgets (3 s time constant): 20 s later, new data dominates.
        n.decay_to(0.0);
        n.decay_to(600.0);
        for _ in 0..100 {
            n.record(1.25);
        }
        assert_eq!(n.quantile(0.99), Some(1.25));
    }

    #[test]
    fn a_respawn_is_a_cut_not_a_streak() {
        let k = Known { tick: 0, tier: Tier::Near, pos: [0.0; 2] };
        let at = |step: f64, x: f32, dead: bool| Sample { step, tier: Tier::Near, pos: [x, 0.0, 0.0], vel: Some([0.0; 2]), yaw: 0.0, pitch: 0.0, airborne: false, health: if dead { 0 } else { 100 }, dead };
        let mut t = Track::new_test(k, at(10.0, 5.0, true));
        t.add_test(at(11.0, 5.0, true), None);
        // Respawned 40 m away.
        t.add_test(at(12.0, 45.0, false), Some(11.2));
        for (r, x) in [(11.2, 5.0), (11.5, 5.0), (11.99, 5.0), (12.0, 45.0)] {
            assert_eq!(t.render(r, 0.0).pos[0], x, "at {r}: the corpse, then the new life");
        }
        assert_eq!(t.visual, [0.0; 3], "nothing to smooth");
        // And no velocity from the corpse to the spawn point.
        t.add_test(at(13.0, 45.2, false), None);
        let ahead = t.raw(14.0).pos[0];
        assert!((ahead - 45.2).abs() < 0.3, "{ahead}");
    }
}
