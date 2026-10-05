//! The render timeline: which game step the client draws other entities at.
//!
//! Every entity is drawn at one render step, a fixed delay behind the newest
//! step the server has sent, so near entities (30 Hz) always have samples on
//! both sides to interpolate between, and every tier agrees on when "now" is.
//!
//! The newest step is an estimate, not just the last snapshot's: snapshots
//! arrive with jitter, and following each one would make render time stutter.
//! The estimate tracks the *earliest* arrivals: a snapshot that arrives
//! earlier than expected moves it up at once (it can't have arrived before it
//! was sent), and one that arrives later pulls it down by a small share, so a
//! lasting change in delay is adopted over a second or two while jitter is
//! not. The render step follows `newest - delay`, speeding up or slowing down
//! by at most `SLEW` to catch it, so it never stalls or runs backwards; only
//! an error past `SNAP` (a long hitch, a server restart) makes it jump.
//!
//! Steps are 1/30 s of game time. The server advertises its pace (game
//! seconds per wall second), and the clock runs at pace × 30 steps per second.

use std::time::{Duration, Instant};

use lattice_game::movement::TICK_HZ;

/// Steps of error past which the render clock jumps instead of slewing (0.5 s).
pub const SNAP: f64 = 15.0;
/// Most the render clock runs fast or slow to catch its target.
pub const SLEW: f64 = 0.1;
/// Share of a late arrival the newest-step estimate adopts, per snapshot.
const DRIFT_DOWN: f64 = 0.02;

/// Seconds from `b` to `a`, negative if `a` is earlier.
fn secs(a: Instant, b: Instant) -> f64 {
    if a >= b {
        (a - b).as_secs_f64()
    } else {
        -(b - a).as_secs_f64()
    }
}

#[derive(Debug, Clone)]
pub struct RenderClock {
    /// Render delay, in steps.
    delay: f64,
    /// Steps per wall second: 30 × the server's pace.
    rate: f64,
    /// The newest step estimated to have arrived by a time: (time, step).
    newest: Option<(Instant, f64)>,
    /// The last render step handed out, and when.
    render: Option<(Instant, f64)>,
    /// Times the render step jumped instead of slewing (backwards jumps included).
    pub snaps: u64,
    /// Times it jumped backwards.
    pub backwards: u64,
}

impl RenderClock {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay: delay.as_secs_f64() * TICK_HZ as f64,
            rate: TICK_HZ as f64,
            newest: None,
            render: None,
            snaps: 0,
            backwards: 0,
        }
    }

    /// The render delay, in steps.
    pub fn delay(&self) -> f64 {
        self.delay
    }

    /// The last render step handed out by `render_at`.
    pub fn last_render(&self) -> Option<f64> {
        self.render.map(|(_, r)| r)
    }

    /// The newest step estimated to have arrived by `t`.
    pub fn newest_at(&self, t: Instant) -> Option<f64> {
        self.newest.map(|(t0, s0)| s0 + secs(t, t0) * self.rate)
    }

    /// A snapshot of game step `step` arrived at `at`; the server's pace is
    /// `pace` game seconds per wall second.
    pub fn on_snapshot(&mut self, step: u32, pace: f32, at: Instant) {
        let s = step as f64;
        let newest = match self.newest_at(at) {
            None => s,
            Some(est) => {
                let e = s - est;
                if e > 0.0 || e < -SNAP {
                    s // earlier than expected (or a different timeline): adopt it
                } else {
                    est + e * DRIFT_DOWN
                }
            }
        };
        self.newest = Some((at, newest));
        self.rate = TICK_HZ as f64 * pace.clamp(0.1, 1.0) as f64;
    }

    /// The render step at `now`, advancing the clock; `None` before the first
    /// snapshot. Calls must not go back in time (one that does gets the last
    /// step again).
    pub fn render_at(&mut self, now: Instant) -> Option<f64> {
        let target = self.newest_at(now)? - self.delay;
        let r = match self.render {
            None => target,
            Some((t, r)) if now <= t => return Some(r),
            Some((t, r)) => {
                let run = secs(now, t) * self.rate;
                let free = r + run;
                let err = target - free;
                if err.abs() > SNAP {
                    self.snaps += 1;
                    self.backwards += (target < r) as u64;
                    target
                } else {
                    free + err.clamp(-run * SLEW, run * SLEW)
                }
            }
        };
        self.render = Some((now, r));
        Some(r)
    }
}

/// Maps server ticks to game steps. Snapshots carry both; entity messages
/// carry only the tick, so their step comes from here. At 30 Hz a tick is a
/// step; at 20 Hz it's 1 or 2. If a tick's snapshot was lost, its step is
/// extrapolated from the newest known tick at the recent steps per tick.
#[derive(Debug, Clone)]
pub struct TickSteps {
    ring: [(u32, u32); 64],
    last: Option<(u32, u32)>,
    per_tick: f64,
}

impl Default for TickSteps {
    fn default() -> Self {
        Self { ring: [(u32::MAX, 0); 64], last: None, per_tick: 1.0 }
    }
}

impl TickSteps {
    pub fn put(&mut self, tick: u32, step: u32) {
        if let Some((t0, s0)) = self.last {
            let dt = tick.wrapping_sub(t0) as i32;
            if dt <= 0 {
                return; // older than what we have
            }
            let inst = step.wrapping_sub(s0) as f64 / dt as f64;
            self.per_tick += (inst - self.per_tick) * 0.2;
        }
        self.ring[tick as usize % self.ring.len()] = (tick, step);
        self.last = Some((tick, step));
    }

    pub fn get(&self, tick: u32) -> Option<f64> {
        let (t, s) = self.ring[tick as usize % self.ring.len()];
        if t == tick {
            return Some(s as f64);
        }
        let (t0, s0) = self.last?;
        Some(s0 as f64 + tick.wrapping_sub(t0) as i32 as f64 * self.per_tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_nanos(33_333_333);

    #[test]
    fn steady_arrivals_render_a_fixed_delay_behind() {
        let t0 = Instant::now();
        let mut c = RenderClock::new(Duration::from_millis(100));
        assert_eq!(c.delay(), 3.0);
        assert_eq!(c.render_at(t0), None, "nothing before the first snapshot");
        let mut last = f64::MIN;
        for i in 0..300u32 {
            let at = t0 + TICK * i;
            c.on_snapshot(1000 + i, 1.0, at);
            // Frames at ~144 Hz between snapshots.
            for f in 0..4 {
                let now = at + TICK / 4 * f;
                let r = c.render_at(now).unwrap();
                assert!(r >= last, "never backwards");
                last = r;
                let want = 1000.0 + i as f64 + f as f64 / 4.0 - 3.0;
                assert!((r - want).abs() < 1e-3, "frame {i}.{f}: {r} vs {want}");
            }
        }
        assert_eq!((c.snaps, c.backwards), (0, 0));
    }

    #[test]
    fn jitter_is_ridden_out_and_the_clock_never_runs_backwards() {
        let t0 = Instant::now();
        let mut c = RenderClock::new(Duration::from_millis(100));
        let mut rng = 12345u64;
        let mut last = f64::MIN;
        let mut worst: f64 = 0.0;
        for i in 0..900u32 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let jitter = Duration::from_micros((rng >> 33) % 40_000); // 0-40 ms late
            c.on_snapshot(i, 1.0, t0 + TICK * i + jitter);
            let now = t0 + TICK * i + TICK / 2;
            let r = c.render_at(now).unwrap();
            assert!(r >= last);
            last = r;
            if i > 90 {
                // Ideal: the earliest-arrival timeline (no jitter) minus 3 steps.
                let ideal = i as f64 + 0.5 - 3.0;
                worst = worst.max((r - ideal).abs());
            }
        }
        assert!(worst < 0.75, "render step strays {worst} steps from the ideal under 40 ms of jitter");
        assert_eq!(c.snaps, 0);
    }

    #[test]
    fn a_longer_path_is_adopted_by_slewing_and_a_restart_snaps() {
        let t0 = Instant::now();
        let mut c = RenderClock::new(Duration::from_millis(100));
        for i in 0..60u32 {
            c.on_snapshot(i, 1.0, t0 + TICK * i);
            c.render_at(t0 + TICK * i);
        }
        // From now on every snapshot takes 2 steps longer to arrive.
        let (mut last, mut last_t) = (c.render_at(t0 + TICK * 59).unwrap(), 59);
        for i in 60..600u32 {
            c.on_snapshot(i, 1.0, t0 + TICK * (i + 2));
            let r = c.render_at(t0 + TICK * (i + 2)).unwrap();
            let speed = (r - last) / (i + 2 - last_t) as f64;
            assert!((0.9 - 1e-6..=1.1 + 1e-6).contains(&speed), "slews at most 10%: {speed}");
            (last, last_t) = (r, i + 2);
        }
        assert!((last - (599.0 - 3.0)).abs() < 0.1, "settled 3 steps behind the newest: {last}");
        assert_eq!(c.snaps, 0);
        // The server restarts its steps from 0.
        let t1 = t0 + TICK * 700;
        c.on_snapshot(5, 1.0, t1);
        assert_eq!(c.render_at(t1), Some(2.0));
        assert_eq!((c.snaps, c.backwards), (1, 1));
    }

    #[test]
    fn the_clock_runs_at_the_servers_pace() {
        let t0 = Instant::now();
        let mut c = RenderClock::new(Duration::from_millis(100));
        // Pace 0.8: 24 steps per wall second, a step every 41.7 ms.
        let period = Duration::from_secs_f64(1.0 / 24.0);
        for i in 0..200u32 {
            c.on_snapshot(i, 0.8, t0 + period * i);
        }
        let now = t0 + period * 199 + period / 2;
        let r = c.render_at(now).unwrap();
        assert!((r - (199.5 - 3.0)).abs() < 1e-4, "{r}");
    }

    #[test]
    fn ticks_map_to_steps_at_20_hz() {
        let mut m = TickSteps::default();
        assert_eq!(m.get(5), None);
        // 20 Hz: 1.5 steps a tick, as 1, 2, 1, 2...
        let mut step = 100;
        for tick in 0..40u32 {
            step += 1 + tick % 2;
            if tick != 37 {
                m.put(tick, step); // tick 37's snapshot was lost
            }
        }
        assert_eq!(m.get(36), Some(155.0));
        assert_eq!(m.get(38), Some(158.0));
        // The lost one is estimated from the newest at ~1.5 steps a tick.
        let est = m.get(37).unwrap();
        assert!((est - 157.0).abs() < 0.3, "{est}");
        let ahead = m.get(41).unwrap();
        assert!((ahead - 163.0).abs() < 0.5, "{ahead}");
    }
}
