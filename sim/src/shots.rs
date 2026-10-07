//! The shots phase (M3d.2): projectiles in the shooter's timeline.
//!
//! A shot fired at step `tau0` (on the server's timeline: input seq covers
//! steps seq - 1 to seq, and `frac` places it within) when its shooter drew
//! near entities at render step R_near and mid/far ones at R_mid keeps
//! D = tau0 - R for each tier, capped (`NEAR_CAP`, `MID_CAP`). At projectile
//! time tau it's tested against targets as they were at tau - D: what the
//! shooter saw, advanced by the flight time. So what you aim at (leading for
//! the flight) is what you hit, within the cap.
//!
//! Each half-step segment is tested against the terrain, the cover boxes
//! near it, and the players near it (from the shared grid, padded by how
//! far one can have moved since the rewound time), placed from the lag-comp
//! `History`. The nearest hit along the segment wins.

use std::collections::HashMap;

use lattice_game::faction::faction;
use lattice_game::hit;
use lattice_game::movement::{TICK_HZ, WORLD_SIZE};
use lattice_game::weapon::{Flight, SUBSTEPS};
use lattice_game::world::World;

use crate::grid::Grid;

/// Rewind caps, in steps: RTT ≤ 100 ms fully compensated (decided for M3d):
/// near targets 300 ms, mid and far 367 ms.
pub const NEAR_CAP: f64 = 9.0;
pub const MID_CAP: f64 = 11.0;
/// Ticks of history kept: at least `MID_CAP` + a step + a segment at 30 Hz,
/// and more steps at 20 Hz (1-2 a tick).
pub const HISTORY_TICKS: usize = 16;
/// Fastest a player moves (sprint), for padding candidate searches, m/s.
const MAX_SPEED: f32 = 9.0;

/// Where an entity was at the end of a tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct HistEntry {
    /// Feet.
    pub pos: [f32; 3],
    pub life: u8,
    /// Connected and not dead.
    pub live: bool,
}

#[derive(Debug, Default)]
struct HistTick {
    tick: u32,
    step: u32,
    at: Vec<HistEntry>,
}

/// Every entity's position for the last `HISTORY_TICKS` ticks, tick-major.
#[derive(Debug)]
pub struct History {
    ticks: Vec<HistTick>,
    /// Index of the newest tick.
    newest: Option<usize>,
}

impl Default for History {
    fn default() -> Self {
        Self { ticks: (0..HISTORY_TICKS).map(|_| HistTick { tick: u32::MAX, ..Default::default() }).collect(), newest: None }
    }
}

/// Two history ticks either side of a fractional step, and where between.
#[derive(Debug, Clone, Copy)]
pub struct Bracket {
    a: usize,
    b: usize,
    t: f32,
}

impl History {
    pub fn record(&mut self, tick: u32, step: u32, entries: impl Iterator<Item = HistEntry>) {
        let i = tick as usize % HISTORY_TICKS;
        let slot = &mut self.ticks[i];
        (slot.tick, slot.step) = (tick, step);
        slot.at.clear();
        slot.at.extend(entries);
        self.newest = Some(i);
    }

    /// The ticks around step `s`; `None` past either end of the history.
    pub fn bracket(&self, s: f64) -> Option<Bracket> {
        let newest = self.newest?;
        let mut b = newest;
        if s > self.ticks[b].step as f64 + 1e-9 {
            return None;
        }
        for _ in 0..HISTORY_TICKS - 1 {
            let a = (b + HISTORY_TICKS - 1) % HISTORY_TICKS;
            let (ta, tb) = (&self.ticks[a], &self.ticks[b]);
            if ta.tick != tb.tick.wrapping_sub(1) {
                return None; // ran off the recorded ticks
            }
            if ta.step as f64 <= s {
                let span = (tb.step - ta.step) as f64;
                let t = if span > 0.0 { ((s - ta.step as f64) / span) as f32 } else { 1.0 };
                return Some(Bracket { a, b, t });
            }
            b = a;
        }
        None
    }

    /// Entity `e`'s feet at the bracket's time, if it was alive and on one
    /// life throughout (never across a death, respawn or teleport), and that
    /// life's counter.
    pub fn at(&self, br: Bracket, e: u16) -> Option<([f32; 3], u8)> {
        let (a, b) = (self.ticks[br.a].at.get(e as usize)?, self.ticks[br.b].at.get(e as usize)?);
        if !a.live || !b.live || a.life != b.life {
            return None;
        }
        Some(([0, 1, 2].map(|k| a.pos[k] + (b.pos[k] - a.pos[k]) * br.t), a.life))
    }

    /// The newest recorded entry for `e`.
    pub fn now(&self, e: u16) -> Option<HistEntry> {
        self.ticks[self.newest?].at.get(e as usize).copied()
    }
}

/// A shot leaving the muzzle (from an applied input).
#[derive(Debug, Clone, Copy)]
pub struct Fire {
    pub shooter: u16,
    /// The input that carried it.
    pub seq: u32,
    pub origin: [f32; 3],
    pub dir: [f32; 3],
    /// The aim as sent (for others' tracers).
    pub yaw: u16,
    pub pitch: i16,
    /// When it fired, in steps.
    pub tau0: f64,
    /// How far the shooter's view was behind `tau0`, near and mid/far, in steps.
    pub behind: [f64; 2],
    /// Fired after its input's seq was taken by a stand-in.
    pub late: bool,
    /// How long its input waited on the server, in steps (0 if late).
    pub wait: f64,
}

/// Slack on top of the plausible rewind, in steps: jitter and frame timing.
pub const TRIM_SLACK: f64 = 2.0;

/// The most rewind (near, mid/far) an honest client can need for a shot:
/// its RTT and its input's wait on the server (in steps), plus the longest
/// render delays the protocol allows, plus slack. `None`: no RTT yet.
pub fn plausible(rtt: Option<f64>, wait: f64) -> Option<[f64; 2]> {
    let base = rtt? + wait + TRIM_SLACK;
    Some([base + lattice_game::msg::MAX_NEAR_DELAY, base + lattice_game::msg::MAX_MID_DELAY])
}

#[derive(Debug, Clone, Copy)]
pub struct Projectile {
    pub id: u64,
    pub shooter: u16,
    pub flight: Flight,
    /// Projectile time, in steps.
    pub tau: f64,
    /// Time it falls out of range.
    pub end: f64,
    /// Rewind per tier (near, mid/far), capped, in steps.
    pub d: [f64; 2],
}

/// How a shot's claimed rewinds were cut.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cut {
    /// Past a cap: the shooter leads (fair).
    pub capped: bool,
    /// Past what it could plausibly have seen: trimmed (a backtrack claim).
    pub trimmed: bool,
    /// How far past what it could plausibly have seen, in steps (the larger
    /// tier's, below its cap); 0 when not trimmed. Honest shooters that get
    /// trimmed land just past the bound; a backtrack lands far past it.
    pub excess: f64,
    /// The trimmed tier with the larger excess was mid/far (else near).
    pub mid: bool,
}

impl Projectile {
    /// From a fire, its rewinds limited to the caps and to `plausible`.
    pub fn new(id: u64, f: &Fire, plausible: Option<[f64; 2]>) -> (Self, Cut) {
        let caps = [NEAR_CAP, MID_CAP];
        let mut cut = Cut::default();
        let d = [0, 1].map(|t| {
            let limit = plausible.map_or(caps[t], |p| p[t].min(caps[t]));
            cut.capped |= f.behind[t] > caps[t];
            if let Some(p) = plausible.filter(|p| f.behind[t] > p[t] && p[t] < caps[t]) {
                cut.trimmed = true;
                let over = f.behind[t].min(caps[t]) - p[t];
                if over > cut.excess {
                    (cut.excess, cut.mid) = (over, t == 1);
                }
            }
            f.behind[t].clamp(0.0, limit)
        });
        let end = f.tau0 + lattice_game::weapon::RANGE_STEPS as f64;
        (Self { id, shooter: f.shooter, flight: Flight::new(f.origin, f.dir), tau: f.tau0, end, d }, cut)
    }
}

/// What ended a projectile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    Ground,
    Cover,
    /// `life`: the target's life counter where it was hit (it takes damage
    /// only if that's still its life now).
    Player { target: u16, head: bool, rewind: f64, life: u8 },
    /// Out of range or out of the world.
    Expired,
}

/// Work counted while flying.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlyStats {
    pub segments: u64,
    pub candidates: u64,
}

impl std::ops::Add for FlyStats {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self { segments: self.segments + o.segments, candidates: self.candidates + o.candidates }
    }
}

/// What a flight reads.
pub struct Sky<'a> {
    pub world: &'a World,
    pub grid: &'a Grid,
    pub history: &'a History,
    /// Each shooter's near set (sorted): targets in it are rewound by the
    /// near delay, the rest by the mid one.
    pub near: &'a HashMap<u16, Vec<u16>>,
}

/// Flies `p` up to step `until`, a half step at a time. Returns how it
/// ended, with the point, if it did.
pub fn fly(p: &mut Projectile, until: f64, sky: &Sky, stats: &mut FlyStats) -> Option<(Outcome, [f32; 3])> {
    let half = 1.0 / SUBSTEPS as f64;
    let near = sky.near.get(&p.shooter);
    let side = faction(p.shooter);
    while p.tau + half <= until + 1e-9 {
        if p.tau >= p.end {
            return Some((Outcome::Expired, p.flight.pos));
        }
        let next = p.flight.advance();
        let (p0, p1) = (p.flight.pos, next.pos);
        stats.segments += 1;
        let mut best: Option<(f32, Outcome)> = None;
        let mut take = |t: f32, o: Outcome| {
            if best.is_none_or(|(b, _)| t < b) {
                best = Some((t, o));
            }
        };
        if let Some(t) = hit::terrain(sky.world, p0, p1) {
            take(t, Outcome::Ground);
        }
        let mid = [(p0[0] + p1[0]) / 2.0, (p0[1] + p1[1]) / 2.0];
        let half_len = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt() / 2.0;
        sky.world.boxes_near(mid[0], mid[1], half_len + 1.0, |b| {
            if let Some(t) = hit::aabb(p0, p1, [b.min[0], b.min[1], b.bottom], [b.max[0], b.max[1], b.top]) {
                take(t, Outcome::Cover);
            }
        });
        // Players, at where the shooter saw them: tau - D for their tier.
        let at = p.tau + half / 2.0;
        let brackets = [sky.history.bracket(at - p.d[0]), sky.history.bracket(at - p.d[1])];
        let pad = MAX_SPEED * (p.d[1] as f32 + 1.0) / TICK_HZ as f32;
        sky.grid.for_each_within(mid, half_len + hit::BODY_RADIUS + pad + 0.5, |j, _, _| {
            let j = j as u16;
            if j == p.shooter || faction(j) == side {
                return;
            }
            stats.candidates += 1;
            let tier = if near.is_some_and(|n| n.binary_search(&j).is_ok()) { 0 } else { 1 };
            let Some((feet, life)) = brackets[tier].and_then(|br| sky.history.at(br, j)) else { return };
            if let Some((t, head)) = hit::player(p0, p1, feet) {
                take(t, Outcome::Player { target: j, head, rewind: p.d[tier], life });
            }
        });
        if let Some((t, o)) = best {
            let point = [0, 1, 2].map(|k| p0[k] + (p1[k] - p0[k]) * t);
            return Some((o, point));
        }
        p.flight = next;
        p.tau += half;
        let q = p.flight.pos;
        if !(0.0..=WORLD_SIZE).contains(&q[0]) || !(0.0..=WORLD_SIZE).contains(&q[1]) {
            return Some((Outcome::Expired, q));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(x: f32, life: u8, live: bool) -> HistEntry {
        HistEntry { pos: [x, 0.0, 0.0], life, live }
    }

    #[test]
    fn history_interpolates_between_ticks_but_not_across_lives() {
        let mut h = History::default();
        // 20 Hz-like: ticks of 1 or 2 steps.
        for (tick, step) in [(10u32, 100u32), (11, 101), (12, 103)] {
            let x = step as f32;
            h.record(tick, step, [entry(x, 0, true), entry(x, (tick == 12) as u8, true), entry(x, 0, tick != 11)].into_iter());
        }
        let br = h.bracket(102.0).unwrap();
        assert_eq!(h.at(br, 0), Some(([102.0, 0.0, 0.0], 0)), "halfway between steps 101 and 103");
        assert_eq!(h.at(br, 1), None, "respawned in between");
        assert_eq!(h.at(br, 2), None, "dead at one end");
        assert_eq!(h.at(h.bracket(100.5).unwrap(), 2), None);
        assert!(h.bracket(103.5).is_none(), "the future");
        assert!(h.bracket(99.0).is_none(), "before the history");
        assert_eq!(h.at(h.bracket(103.0).unwrap(), 0), Some(([103.0, 0.0, 0.0], 0)));
    }

    #[test]
    fn rewinds_are_capped() {
        let f = Fire { shooter: 1, seq: 1, origin: [0.0; 3], dir: [1.0, 0.0, 0.0], yaw: 0, pitch: 0, tau0: 100.0, behind: [4.0, 8.0], late: false, wait: 1.5 };
        let (p, cut) = Projectile::new(1, &f, None);
        assert_eq!((p.d, cut), ([4.0, 8.0], Cut::default()));
        let (p, cut) = Projectile::new(2, &Fire { behind: [12.0, 16.0], ..f }, None);
        assert_eq!((p.d, cut.capped, cut.trimmed), ([NEAR_CAP, MID_CAP], true, false));
        // RTT 1 step + wait 1.5: honest near rewinds are at most 1 + 1.5 + 4 + 2.
        let ok = plausible(Some(1.0), 1.5);
        let (p, cut) = Projectile::new(3, &f, ok);
        assert_eq!((p.d, cut), ([4.0, 8.0], Cut::default()), "an honest claim stands");
        let (p, cut) = Projectile::new(4, &Fire { behind: [8.0 + 4.0, 10.5 + 4.0], ..f }, ok);
        assert_eq!((p.d, cut.trimmed), ([8.5, 10.5], true), "a backtrack claim is trimmed");
    }
}
