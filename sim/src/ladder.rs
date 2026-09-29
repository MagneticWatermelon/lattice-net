//! Degradation under load, in the order CLAUDE.md fixes: shrink tier radii,
//! then lower update rates, then lower the tick rate (30 -> 20 Hz), then a
//! mild time dilation (>= 0.8).
//!
//! The controller watches each tick's work time (simulation + egress) as a
//! fraction of its wall period. It steps down a level when the p90 of the last
//! `WINDOW` ticks exceeds `high`, then holds for `WINDOW` ticks so the window
//! only judges the new level. It steps back up after `CALM_TICKS` consecutive
//! ticks below `low`. The gap between the two thresholds keeps it from flapping.
//!
//! **Tick rate and movement.** A lower tick rate doesn't change what an input
//! means: each input is still one 1/30 s movement step, and a 20 Hz tick
//! consumes 1.5 of them on average. Prediction stays bit-exact and clients
//! keep sending 30 inputs per game-second; only dilation slows that down.
//!
//! **Pace.** Clients pace their inputs by one number: game-seconds per
//! wall-second. The server advertises the lower of its intended dilation and
//! the pace it actually achieved over the last `WINDOW` ticks, so an overrun it
//! didn't plan for slows clients down too, instead of overflowing input queues.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::interest::InterestConfig;
use crate::movement::TICK_HZ;

/// Ticks the controller judges at once, and holds after each step down.
pub const WINDOW: usize = 30;
/// Consecutive calm ticks before stepping back up.
pub const CALM_TICKS: u32 = 90;

/// What a level changes. Radii scale the base config; counts cap it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rung {
    pub mid_radius: f32,
    pub far_radius: f32,
    pub near_candidates: usize,
    pub near_per_tick: usize,
    pub mid_period: u32,
    pub far_period: u32,
    pub tick_hz: u32,
    pub dilation: f32,
}

const R0: Rung = Rung {
    mid_radius: 1.0,
    far_radius: 1.0,
    near_candidates: usize::MAX,
    near_per_tick: usize::MAX,
    mid_period: 3,
    far_period: 15,
    tick_hz: TICK_HZ,
    dilation: 1.0,
};

/// Level 0 is normal service; each level keeps the changes before it.
pub const RUNGS: [Rung; 9] = [
    R0,
    // 1-3: shrink tier radii
    Rung { mid_radius: 0.9, far_radius: 0.8, ..R0 },
    Rung { mid_radius: 0.8, far_radius: 0.6, ..R0 },
    Rung { mid_radius: 0.7, far_radius: 0.45, near_candidates: 80, ..R0 },
    // 4-5: lower update rates
    Rung { mid_radius: 0.7, far_radius: 0.45, near_candidates: 80, near_per_tick: 48, mid_period: 5, far_period: 30, ..R0 },
    Rung { mid_radius: 0.7, far_radius: 0.45, near_candidates: 64, near_per_tick: 40, mid_period: 5, far_period: 30, ..R0 },
    // 6: lower the tick rate
    Rung {
        mid_radius: 0.7,
        far_radius: 0.45,
        near_candidates: 64,
        near_per_tick: 40,
        mid_period: 5,
        far_period: 30,
        tick_hz: 20,
        dilation: 1.0,
    },
    // 7-8: time dilation
    Rung {
        mid_radius: 0.7,
        far_radius: 0.45,
        near_candidates: 64,
        near_per_tick: 40,
        mid_period: 5,
        far_period: 30,
        tick_hz: 20,
        dilation: 0.9,
    },
    Rung {
        mid_radius: 0.7,
        far_radius: 0.45,
        near_candidates: 64,
        near_per_tick: 40,
        mid_period: 5,
        far_period: 30,
        tick_hz: 20,
        dilation: 0.8,
    },
];

pub const MAX_LEVEL: u8 = (RUNGS.len() - 1) as u8;

impl Rung {
    /// The interest config at this rung, from the undegraded one.
    pub fn apply(&self, base: &InterestConfig) -> InterestConfig {
        InterestConfig {
            mid_radius: base.mid_radius * self.mid_radius,
            far_radius: base.far_radius * self.far_radius,
            near_candidates: base.near_candidates.min(self.near_candidates),
            near_per_tick: base.near_per_tick.min(self.near_per_tick),
            mid_period: self.mid_period,
            far_period: self.far_period,
            ..base.clone()
        }
    }

    /// Wall-clock time between ticks.
    pub fn period(&self) -> Duration {
        if self.dilation == 1.0 {
            Duration::from_secs(1) / self.tick_hz
        } else {
            Duration::from_micros((1e6 / (self.tick_hz as f64 * self.dilation as f64)).round() as u64)
        }
    }

    /// Game time one tick advances.
    pub fn game_dt(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.tick_hz as f64)
    }

    /// 1/30 s movement steps per tick (1.5 at 20 Hz).
    pub fn steps_per_tick(&self) -> f64 {
        TICK_HZ as f64 / self.tick_hz as f64
    }
}

#[derive(Debug, Clone)]
pub struct LadderConfig {
    pub enabled: bool,
    /// Step down when the p90 of work / period over the window exceeds this.
    pub high: f32,
    /// Step up after `CALM_TICKS` ticks with work / period below this.
    pub low: f32,
}

impl Default for LadderConfig {
    fn default() -> Self {
        Self { enabled: true, high: 0.85, low: 0.6 }
    }
}

#[derive(Debug)]
pub struct Ladder {
    cfg: LadderConfig,
    level: u8,
    recent: VecDeque<f32>,
    hold: usize,
    calm: u32,
}

impl Ladder {
    pub fn new(cfg: LadderConfig) -> Self {
        Self { cfg, level: 0, recent: VecDeque::with_capacity(WINDOW), hold: 0, calm: 0 }
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    pub fn rung(&self) -> &'static Rung {
        &RUNGS[self.level as usize]
    }

    /// Feeds one tick's work time. Returns the new level if it changed.
    pub fn observe(&mut self, work: Duration, period: Duration) -> Option<u8> {
        let load = work.as_secs_f32() / period.as_secs_f32();
        if self.recent.len() == WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(load);
        if !self.cfg.enabled {
            return None;
        }
        self.calm = if load < self.cfg.low { self.calm + 1 } else { 0 };
        if self.hold > 0 {
            self.hold -= 1;
            return None;
        }
        if self.recent.len() == WINDOW && self.p90() > self.cfg.high && self.level < MAX_LEVEL {
            self.level += 1;
            self.hold = WINDOW;
            self.calm = 0;
            return Some(self.level);
        }
        if self.calm >= CALM_TICKS && self.level > 0 {
            self.level -= 1;
            self.hold = WINDOW;
            self.calm = 0;
            return Some(self.level);
        }
        None
    }

    fn p90(&self) -> f32 {
        let mut v: Vec<f32> = self.recent.iter().copied().collect();
        let i = (v.len() * 9 / 10).min(v.len() - 1);
        *v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1
    }
}

/// Per-client bandwidth ladder: mid and far radius scales by level.
pub const CLIENT_MID: [f32; 4] = [1.0, 0.9, 0.8, 0.7];
pub const CLIENT_FAR: [f32; 4] = [1.0, 0.8, 0.6, 0.45];
/// Ticks between steps down, and the window a step up must survive.
const CLIENT_HOLD: u16 = 15;
/// Starve-free ticks before a step back up (a probe).
const CLIENT_CALM: u16 = 90;
/// A probe that starves again within `CLIENT_HOLD` ticks doubles the calm
/// stretch needed before the next one, up to this.
const CLIENT_CALM_MAX: u16 = 30 * 30;

/// One client's bandwidth ladder. A client whose carried far entities starve
/// (they didn't fit its byte budget two ticks running) steps down, shrinking
/// its own mid/far radii; after a calm stretch it probes one level back up.
/// Probes that fail at once back off exponentially, so a client that can't
/// afford more doesn't starve every few seconds. The near tier is never cut.
#[derive(Debug, Clone)]
pub struct ClientLadder {
    level: u8,
    hold: u16,
    calm: u16,
    calm_needed: u16,
    probing: bool,
}

impl Default for ClientLadder {
    fn default() -> Self {
        Self { level: 0, hold: 0, calm: 0, calm_needed: CLIENT_CALM, probing: false }
    }
}

impl ClientLadder {
    pub fn level(&self) -> u8 {
        self.level
    }

    pub fn mid_scale(&self) -> f32 {
        CLIENT_MID[self.level as usize]
    }

    pub fn far_scale(&self) -> f32 {
        CLIENT_FAR[self.level as usize]
    }

    /// Feed one tick: how many carried far entities starved.
    pub fn update(&mut self, starved: u64) {
        let max = CLIENT_FAR.len() as u8 - 1;
        let probe_failed = self.probing && self.hold > 0;
        self.hold = self.hold.saturating_sub(1);
        if starved > 0 {
            self.calm = 0;
            if probe_failed {
                self.calm_needed = self.calm_needed.saturating_mul(2).min(CLIENT_CALM_MAX);
            }
            self.probing = false;
            if (self.hold == 0 || probe_failed) && self.level < max {
                self.level += 1;
                self.hold = CLIENT_HOLD;
            }
        } else {
            self.calm = self.calm.saturating_add(1);
            if self.probing && self.hold == 0 {
                self.probing = false;
                self.calm_needed = CLIENT_CALM; // the probe held
            }
            if self.calm >= self.calm_needed && self.hold == 0 && self.level > 0 {
                self.level -= 1;
                self.calm = 0;
                self.hold = CLIENT_HOLD;
                self.probing = true;
            }
        }
    }
}

/// How far behind its own schedule the server runs: actual tick intervals
/// over the intended ones, across the last `WINDOW` ticks. Each interval is
/// judged against its own tick's period, so a planned change of tick rate or
/// dilation doesn't read as falling behind; only real lateness does.
#[derive(Debug, Default)]
pub struct PaceMeter {
    ticks: VecDeque<(Instant, Duration)>,
}

impl PaceMeter {
    /// Record a tick starting at `at` that was meant to last `period`.
    pub fn record(&mut self, at: Instant, period: Duration) {
        if self.ticks.len() == WINDOW {
            self.ticks.pop_front();
        }
        self.ticks.push_back((at, period));
    }

    /// Actual / intended wall time, at least 1. `None` until a few ticks are in.
    pub fn stretch(&self) -> Option<f32> {
        if self.ticks.len() < 5 {
            return None;
        }
        let (first, last) = (self.ticks.front()?.0, self.ticks.back()?.0);
        let actual = last.saturating_duration_since(first).as_secs_f32();
        let intended: f32 = self.ticks.iter().take(self.ticks.len() - 1).map(|t| t.1.as_secs_f32()).sum();
        (intended > 0.0).then(|| (actual / intended).max(1.0))
    }
}

/// The pace to advertise, in game-seconds per wall-second: the dilation the
/// server runs at, slowed further by however far it is behind schedule.
pub fn advertised_pace(dilation: f32, stretch: Option<f32>) -> f32 {
    // Up to 2% late is sleep jitter, not a server falling behind.
    let stretch = stretch.filter(|&s| s > 1.02).unwrap_or(1.0);
    (dilation / stretch).clamp(0.1, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Duration = Duration::from_millis(33);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn rungs_degrade_monotonically_in_the_documented_order() {
        for w in RUNGS.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(b.mid_radius <= a.mid_radius && b.far_radius <= a.far_radius);
            assert!(b.near_candidates <= a.near_candidates && b.near_per_tick <= a.near_per_tick);
            assert!(b.mid_period >= a.mid_period && b.far_period >= a.far_period);
            assert!(b.tick_hz <= a.tick_hz && b.dilation <= a.dilation);
        }
        // Radii go first, dilation last.
        assert!(RUNGS[1].far_radius < 1.0 && RUNGS[1].mid_period == 3);
        assert!(RUNGS[6].tick_hz == 20 && RUNGS[6].dilation == 1.0);
        assert_eq!(RUNGS[MAX_LEVEL as usize].dilation, 0.8);
        assert_eq!(RUNGS[6].steps_per_tick(), 1.5);
        assert_eq!(RUNGS[8].period(), Duration::from_micros(62_500));
        assert_eq!(RUNGS[0].period(), Duration::from_secs(1) / 30);
    }

    #[test]
    fn a_single_spike_does_not_degrade_but_sustained_load_does() {
        let mut l = Ladder::new(LadderConfig::default());
        for i in 0..WINDOW * 3 {
            let work = if i % 20 == 0 { ms(60) } else { ms(15) }; // 1 in 20 slow
            assert_eq!(l.observe(work, P), None);
        }
        let mut changes = Vec::new();
        for i in 0..WINDOW * 4 {
            if let Some(level) = l.observe(ms(31), P) {
                changes.push((i, level));
            }
        }
        assert!(changes[0].0 < 5, "reacts within a few ticks of sustained load: {changes:?}");
        let levels: Vec<u8> = changes.iter().map(|c| c.1).collect();
        assert_eq!(levels, vec![1, 2, 3, 4], "then one step per window, not one per tick");
    }

    #[test]
    fn recovers_one_level_per_calm_stretch_and_does_not_flap() {
        let mut l = Ladder::new(LadderConfig::default());
        for _ in 0..WINDOW * 10 {
            l.observe(ms(31), P);
        }
        assert_eq!(l.level(), MAX_LEVEL, "clamped at the bottom");
        // Between the thresholds: no change either way.
        for _ in 0..CALM_TICKS * 3 {
            assert_eq!(l.observe(ms(24), P), None);
        }
        let mut ups = 0;
        for _ in 0..(CALM_TICKS as usize + WINDOW) * MAX_LEVEL as usize {
            if l.observe(ms(10), P).is_some() {
                ups += 1;
            }
        }
        assert_eq!((l.level(), ups), (0, MAX_LEVEL as usize));
    }

    #[test]
    fn client_ladder_steps_down_on_starvation_and_backs_off_failed_probes() {
        let mut c = ClientLadder::default();
        // Starving every tick: one level per hold, down to the floor.
        let mut levels = Vec::new();
        for _ in 0..100 {
            c.update(1);
            levels.push(c.level());
        }
        assert_eq!(levels[0], 1);
        assert_eq!(levels[CLIENT_HOLD as usize - 1], 1, "held");
        assert_eq!(levels[CLIENT_HOLD as usize], 2);
        assert_eq!(c.level(), 3, "floor");
        assert!((c.far_scale() - 0.45).abs() < 1e-6);

        // Calm: probes back up after CLIENT_CALM ticks.
        let ticks_to_probe = |c: &mut ClientLadder, starve_after_probe: bool| {
            let before = c.level();
            for i in 1..=CLIENT_CALM_MAX as u32 + 1 {
                c.update(0);
                if c.level() < before {
                    if starve_after_probe {
                        c.update(1); // the probe fails at once
                    }
                    return i;
                }
            }
            panic!("never probed")
        };
        assert_eq!(ticks_to_probe(&mut c, true), CLIENT_CALM as u32);
        assert_eq!(c.level(), 3, "a failed probe steps straight back down");
        assert_eq!(ticks_to_probe(&mut c, true), 2 * CLIENT_CALM as u32, "and the next waits twice as long");
        assert_eq!(ticks_to_probe(&mut c, true), 4 * CLIENT_CALM as u32);
        // A probe that holds resets the wait.
        assert_eq!(ticks_to_probe(&mut c, false), 8 * CLIENT_CALM as u32);
        for _ in 0..CLIENT_HOLD {
            c.update(0);
        }
        assert_eq!(c.level(), 2);
        assert_eq!(ticks_to_probe(&mut c, false), CLIENT_CALM as u32 - CLIENT_HOLD as u32);
    }

    #[test]
    fn disabled_ladder_never_moves() {
        let mut l = Ladder::new(LadderConfig { enabled: false, ..Default::default() });
        for _ in 0..WINDOW * 5 {
            assert_eq!(l.observe(ms(100), P), None);
        }
        assert_eq!(l.level(), 0);
    }

    #[test]
    fn pace_follows_real_lateness_but_not_planned_changes() {
        let t0 = Instant::now();
        let mut m = PaceMeter::default();
        assert_eq!(m.stretch(), None);
        // A 30 Hz server that actually ticks every 50 ms: pace 2/3.
        for i in 0..WINDOW as u32 {
            m.record(t0 + ms(50) * i, RUNGS[0].period());
        }
        assert!((m.stretch().unwrap() - 1.5).abs() < 0.01);
        assert!((advertised_pace(1.0, m.stretch()) - 2.0 / 3.0).abs() < 0.01, "involuntary slowdown");

        // On schedule through a planned switch from 16 Hz (dilation 0.8) back
        // to 30 Hz: no stretch, so the new pace is advertised at once.
        let mut m = PaceMeter::default();
        let mut at = t0;
        for i in 0..WINDOW {
            let rung = if i < 20 { RUNGS[8] } else { RUNGS[0] };
            m.record(at, rung.period());
            at += rung.period();
        }
        assert!((m.stretch().unwrap() - 1.0).abs() < 0.001);
        assert_eq!(advertised_pace(RUNGS[0].dilation, m.stretch()), 1.0);
        assert_eq!(advertised_pace(0.8, None), 0.8);
    }
}
