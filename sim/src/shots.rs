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

use lattice_game::faction::faction;
use lattice_game::hit;
use lattice_game::movement::{TICK_HZ, WORLD_SIZE};
use lattice_game::weapon::{Flight, SUBSTEPS};
use lattice_game::world::World;

use std::collections::VecDeque;
use std::time::Instant;

use ring::hmac;

use crate::grid::Grid;
use crate::interest::NearState;

/// Rewind caps, in steps: RTT ≤ 100 ms fully compensated (decided for M3d):
/// near targets 300 ms, mid and far 367 ms.
pub const NEAR_CAP: f64 = 9.0;
pub const MID_CAP: f64 = 11.0;
/// Ticks of history kept: at least `MID_CAP` + a step + a segment at 30 Hz,
/// and more steps at 20 Hz (1-2 a tick).
pub const HISTORY_TICKS: usize = 16;
/// Fastest a player moves (sprint), for padding candidate searches, m/s.
const MAX_SPEED: f32 = 9.0;

/// The secret that picks where in its cone of fire each shot goes
/// (`weapon::spread`). It's never sent, and a pick is HMAC-SHA256 of the
/// shooter's spawn and the shot's seq under it, so knowing a shot's id and
/// seq (or seeing where earlier shots went: everyone near sees tracers) tells
/// a client nothing about where the next shot will go.
pub struct SpreadKey(hmac::Key);

impl SpreadKey {
    /// From `secret`, or 32 bytes from the OS when there's none.
    pub fn new(secret: Option<[u8; 32]>) -> Self {
        let bytes = secret.unwrap_or_else(|| {
            let mut b = [0u8; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).expect("OS randomness");
            b
        });
        Self(hmac::Key::new(hmac::HMAC_SHA256, &bytes))
    }

    /// The pick for shot `seq` of `spawn` (a number no other connection
    /// shares while this key lives).
    pub fn pick(&self, spawn: u64, seq: u32) -> u64 {
        let mut msg = [0u8; 12];
        msg[..8].copy_from_slice(&spawn.to_le_bytes());
        msg[8..].copy_from_slice(&seq.to_le_bytes());
        let tag = hmac::sign(&self.0, &msg);
        u64::from_le_bytes(tag.as_ref()[..8].try_into().unwrap())
    }
}

impl std::fmt::Debug for SpreadKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("SpreadKey(..)")
    }
}

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
    /// How far its claimed view was moved to fit the client's render clock
    /// (`RenderFloor`), in steps (0: it fit); `behind` is already moved.
    pub held: f64,
    /// Slack the backtrack bound adds while the client's clocks settle, in
    /// steps: its first seconds (`SETTLE_SLACK`), or after the server filled
    /// in a missing input (`STAND_IN_SLACK`).
    pub settling: f64,
}

/// Slack on top of the plausible rewind, in steps: jitter and frame timing.
pub const TRIM_SLACK: f64 = 2.0;
/// More slack in a client's first seconds: its RTT isn't measured yet, and
/// its render clock is still catching up to its target (a step behind it
/// after an early snapshot came late).
pub const SETTLE_SLACK: f64 = 2.0;
/// And for a while after the server filled in a missing input (the client
/// stalled): its input clock runs a step ahead to rebuild its spare, and its
/// render clock catches up from the backlog it read late.
pub const STAND_IN_SLACK: f64 = 1.0;

/// The render floor's rates: an honest near render clock runs at least 90%
/// of real time (it slews at most 10% to change its delay; clock.rs), and
/// mid and far lag it by a lag that slews 10% too, so they run at least 80%.
pub const FLOOR_RATE_NEAR: f64 = 0.9;
pub const FLOOR_RATE_MID: f64 = 0.8;
/// Slack under the render floor, in steps: rounding (near render steps are
/// in 1/64 steps, the mid lag in 1/8) and when a client sends what it made.
pub const FLOOR_SLACK_NEAR: f64 = 0.1;
pub const FLOOR_SLACK_MID: f64 = 0.25;
/// The most delay variation (jitter) the floor allows for, in seconds: a
/// link that varies more (or a client pretending to) gets no more slack
/// than this.
pub const FLOOR_JITTER_MAX: f64 = 0.1;
/// A client's first seconds of claims get all of it: its RTTs haven't been
/// measured long enough to show how its link varies.
const FLOOR_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
/// Fresh claims remembered per client: about a second and a half.
const ANCHORS: usize = 48;
/// Inputs whose claims are kept for bracketing shots.
const RECENT: usize = 8;

/// What a client's render clock is judged by: the game's steps per wall
/// second (30 × the lowest recent pace) and how much its link's delay
/// varies (jitter), in seconds.
#[derive(Debug, Clone, Copy)]
pub struct ClockRate {
    pub rate: f64,
    pub jitter: f64,
}

/// Holds a client's claimed render steps to its render clock.
///
/// The backtrack bound (`plausible`) caps how old a claim can be, but that
/// cap is the longest render delay a client may use: one that draws near
/// players 2 steps behind could claim 4, plus slack, and pick for each shot
/// whichever moment of a target's past lined up with its crosshair. Every
/// input carries the render step it was made at, and an honest render clock
/// never runs backwards and keeps to at least 90% of real time. So:
///
/// - **Inputs:** each input's claim is held to the client's earlier fresh
///   claims (a message's newest input, made just before it was sent),
///   carried forward at that rate over the time between their arrivals, less
///   the link's measured jitter. An input that came in a later message than
///   the newest (a copy, after a loss) is judged as of when it was made.
/// - **Shots:** a shot fires between two inputs being made: it rides the
///   next input made after it. So its claim is held between the claims of
///   the input before it and its own: it can neither claim an older view
///   than the client said it had, nor a fresher one than its own input.
///
/// A cheat that varies its claims shot by shot gets held to its inputs, and
/// one that makes its inputs stale too can only drift staler at 10% of real
/// time (plus the jitter allowance): it can't swing back and forth.
#[derive(Debug, Clone, Default)]
pub struct RenderFloor {
    /// Recent fresh claims, held: (seq, near, mid, arrival).
    anchors: VecDeque<(u32, f64, f64, Instant)>,
    /// The last inputs' held claims, by seq: (seq, near, mid, dip).
    recent: [(u32, f64, f64, f64); RECENT],
    /// When the first claim arrived.
    first: Option<Instant>,
    /// How far near claims dipped under the floor the jitter allowance
    /// lowers (`dips`), after the client's first seconds: every input's
    /// (sum, sum of squares, count), and the inputs' right before shots
    /// (sum, count).
    dips_all: (f64, f64, u32),
    dips_shots: (f64, u32),
}

/// A client's pre-shot dips into the render floor's jitter allowance
/// (`RenderFloor::dips`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dips {
    /// Beyond its other inputs' dips, on average, in steps.
    pub excess: f64,
    pub shots: u32,
    /// The excess in standard errors.
    pub z: f64,
}

impl Dips {
    /// Judged (enough shots), and spending the allowance on stale shots.
    pub fn judged(&self) -> bool {
        self.shots >= DIP_MIN_SHOTS
    }
    pub fn flagged(&self) -> bool {
        self.judged() && self.excess > DIP_FLAG && self.z > DIP_Z
    }
}

/// Shots a client must have fired before its dips are judged (`dips`); how
/// far (steps) its pre-shot dips must exceed its others' to be flagged as
/// spending the jitter allowance on stale shots, and by how many standard
/// errors (its dips vary with its link: an honest client's excess is noise).
pub const DIP_MIN_SHOTS: u32 = 30;
pub const DIP_FLAG: f64 = 0.3;
pub const DIP_Z: f64 = 4.0;

impl RenderFloor {
    /// The oldest near and mid claims the client's fresh claims before input
    /// `seq` allow for something made at `made`; and the near one without
    /// the jitter allowance. Whether the client is past its first seconds.
    fn floor(&self, seq: u32, made: Instant, clock: ClockRate) -> ([f64; 2], f64, bool) {
        let settled = self.first.is_some_and(|t| made.saturating_duration_since(t) >= FLOOR_SETTLE);
        let (rate, jitter) = (clock.rate, if settled { clock.jitter.clamp(0.0, FLOOR_JITTER_MAX) } else { FLOOR_JITTER_MAX });
        let (mut f, mut strict) = ([f64::MIN; 2], f64::MIN);
        for &(s, near, mid, at) in &self.anchors {
            if s < seq {
                // Real time from this claim's arrival to `made`, at least:
                // less how much the link's delay varies, never back in time.
                let since = made.saturating_duration_since(at).as_secs_f64();
                let run = (since - jitter).max(0.0) * rate;
                f[0] = f[0].max(near + run * FLOOR_RATE_NEAR);
                f[1] = f[1].max(mid + run * FLOOR_RATE_MID);
                strict = strict.max(near + since * rate * FLOOR_RATE_NEAR);
            }
        }
        ([f[0] - FLOOR_SLACK_NEAR, f[1] - FLOOR_SLACK_MID], strict - FLOOR_SLACK_NEAR, settled)
    }

    /// Input `seq` arrived (for the first time) claiming it was made at
    /// render steps `claim` (near, mid; absolute), in a message that arrived
    /// at `at` and whose newest input was `newest`. Returns the claim held
    /// to the floor.
    pub fn input(&mut self, seq: u32, claim: [f64; 2], newest: u32, at: Instant, clock: ClockRate) -> [f64; 2] {
        self.first.get_or_insert(at);
        let (f, strict, settled) = self.floor(seq, Self::made(seq, newest, at, clock), clock);
        let held = [claim[0].max(f[0]), claim[1].max(f[1])];
        // How much of the jitter allowance this claim used.
        let dip = (strict - claim[0]).max(0.0);
        if settled {
            let (sum, sq, n) = self.dips_all;
            self.dips_all = (sum + dip, sq + dip * dip, n + 1);
        }
        self.recent[seq as usize % RECENT] = (seq, held[0], held[1], dip);
        if seq == newest {
            if self.anchors.len() == ANCHORS {
                self.anchors.pop_front();
            }
            self.anchors.push_back((seq, held[0], held[1], at));
        }
        held
    }

    /// When input `seq` was made, at the latest, in a message whose newest
    /// input was `newest` that arrived at `at`: a copy was made (newest -
    /// seq) steps before it, and a step more if the client's input clock
    /// skipped a call in between (it does, now and then, to steer its spare).
    fn made(seq: u32, newest: u32, at: Instant, clock: ClockRate) -> Instant {
        let steps = match newest - seq {
            0 => 0,
            n => n + 1,
        };
        let behind = std::time::Duration::from_secs_f64(steps as f64 / clock.rate.max(1.0));
        at.checked_sub(behind).unwrap_or(at)
    }

    /// A shot in input `seq` whose message carried no render steps for its
    /// inputs (a client's first inputs, or a couple of messages after a
    /// resync), claiming `claim`: held to the floor of the client's earlier
    /// claims alone. `None` if it has made none.
    pub fn bare_shot(&self, seq: u32, claim: [f64; 2], newest: u32, at: Instant, clock: ClockRate) -> Option<[f64; 2]> {
        if self.anchors.is_empty() {
            return None;
        }
        let (f, ..) = self.floor(seq, Self::made(seq, newest, at, clock), clock);
        Some([claim[0].max(f[0]), claim[1].max(f[1])])
    }

    /// A shot riding input `seq` (already given to `input`) claims render
    /// steps `claim` (near, mid): held between the claims of the input before
    /// it (less `before` steps of slack) and its own. Returns the held claim.
    /// The slack is for an input clock running ahead of what the client has
    /// made: the inputs between are made after the shot, and claim fresher
    /// views. That happens in a client's first seconds (`before` = None:
    /// only its own input bounds the shot) and for a step after the server
    /// fills in a missing input (the client then runs its clock a step ahead).
    pub fn shot(&mut self, seq: u32, claim: [f64; 2], before: Option<f64>) -> [f64; 2] {
        let known = |s: u32| self.recent[s as usize % RECENT].0 == s;
        let mut held = claim;
        if known(seq) {
            let (_, n, m, _) = self.recent[seq as usize % RECENT];
            held = [held[0].min(n + FLOOR_SLACK_NEAR), held[1].min(m + FLOOR_SLACK_MID)];
        }
        if let (Some(slack), true) = (before, seq > 1 && known(seq - 1)) {
            let (_, n, m, dip) = self.recent[(seq - 1) as usize % RECENT];
            held = [held[0].max(n - slack - FLOOR_SLACK_NEAR), held[1].max(m - slack - FLOOR_SLACK_MID)];
            // The input before a shot bounds how old its view can be.
            self.dips_shots = (self.dips_shots.0 + dip, self.dips_shots.1 + 1);
        }
        held
    }

    /// How far, on average, the client's claims dipped into the jitter
    /// allowance right before its shots, beyond how far they dipped at all,
    /// in steps; over how many shots; and how many standard errors that is.
    /// An honest client's dips come from its link, whenever it shoots; a
    /// cheat spending the allowance on stale views dips before shots. `None`
    /// before it has made a claim.
    pub fn dips(&self) -> Option<Dips> {
        let ((sum, sq, n), (shots, k)) = (self.dips_all, self.dips_shots);
        (n > 0 && k > 0).then(|| {
            let mean = sum / n as f64;
            let sd = (sq / n as f64 - mean * mean).max(0.0).sqrt().max(0.01);
            let excess = shots / k as f64 - mean;
            Dips { excess, shots: k, z: excess / (sd / (k as f64).sqrt()) }
        })
    }
}

/// The most rewind (near, mid/far) an honest client can need for a shot:
/// its RTT (the highest of the last second or two) and its input's wait on
/// the server, plus how late in its tick the server sent the snapshots the
/// client drew from (`send`; RTTs leave it out, since sends are stamped when
/// they go, but what the client saw was that much older), all in steps,
/// plus the longest render delays the protocol allows, plus slack (and
/// `settling` steps more while the client's clocks settle). `None`: no RTT
/// yet.
pub fn plausible(rtt: Option<f64>, wait: f64, send: f64, settling: f64) -> Option<[f64; 2]> {
    let base = rtt? + wait + send + TRIM_SLACK + settling;
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
    /// Each shooter's near set, if it has a client: targets in it are
    /// rewound by the near delay, the rest by the mid one.
    pub near: &'a (dyn Fn(u16) -> Option<&'a NearState> + Sync),
}

/// Flies `p` up to step `until`, a half step at a time. Returns how it
/// ended, with the point, if it did.
pub fn fly(p: &mut Projectile, until: f64, sky: &Sky, stats: &mut FlyStats) -> Option<(Outcome, [f32; 3])> {
    let half = 1.0 / SUBSTEPS as f64;
    let near = (sky.near)(p.shooter);
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
            let tier = if near.is_some_and(|n| n.contains(j)) { 0 } else { 1 };
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
        let f = Fire { shooter: 1, seq: 1, origin: [0.0; 3], dir: [1.0, 0.0, 0.0], yaw: 0, pitch: 0, tau0: 100.0, behind: [4.0, 8.0], late: false, wait: 1.5, held: 0.0, settling: 0.0 };
        let (p, cut) = Projectile::new(1, &f, None);
        assert_eq!((p.d, cut), ([4.0, 8.0], Cut::default()));
        let (p, cut) = Projectile::new(2, &Fire { behind: [12.0, 16.0], ..f }, None);
        assert_eq!((p.d, cut.capped, cut.trimmed), ([NEAR_CAP, MID_CAP], true, false));
        // RTT 1 step + wait 1.5: honest near rewinds are at most 1 + 1.5 + 4 + 2.
        assert_eq!(plausible(Some(1.0), 1.5, 0.5, 0.0), plausible(Some(1.5), 1.5, 0.0, 0.0), "a late send counts like RTT");
        assert_eq!(plausible(Some(1.0), 1.5, 0.0, SETTLE_SLACK), plausible(Some(3.0), 1.5, 0.0, 0.0), "settling: two more steps");
        let ok = plausible(Some(1.0), 1.5, 0.0, 0.0);
        let (p, cut) = Projectile::new(3, &f, ok);
        assert_eq!((p.d, cut), ([4.0, 8.0], Cut::default()), "an honest claim stands");
        let (p, cut) = Projectile::new(4, &Fire { behind: [8.0 + 4.0, 10.5 + 4.0], ..f }, ok);
        assert_eq!((p.d, cut.trimmed), ([8.5, 10.5], true), "a backtrack claim is trimmed");
    }

    /// A client as `RenderFloor` sees it: messages of inputs (each made at
    /// a render step), the newest of which may carry a shot.
    struct Client {
        floor: RenderFloor,
        t0: Instant,
        seq: u32,
        /// Inputs made but not yet in a message that arrived.
        unsent: Vec<f64>,
    }

    const STEP: f64 = 1.0 / TICK_HZ as f64;
    const RATE: f64 = TICK_HZ as f64;

    impl Client {
        fn new() -> Self {
            Self { floor: RenderFloor::default(), t0: Instant::now(), seq: 0, unsent: Vec::new() }
        }

        /// Makes an input drawn at render step `r` (not sent yet).
        fn make(&mut self, r: f64) {
            self.seq += 1;
            self.unsent.push(r);
        }

        /// A message carrying the unsent inputs arrives at `secs` (with
        /// `jitter` allowed); the newest carries a shot claiming `shot`.
        /// Returns how far the shot was held (near), if it had one.
        fn arrive(&mut self, secs: f64, jitter: f64, shot: Option<f64>) -> Option<f64> {
            let at = self.t0 + std::time::Duration::from_secs_f64(secs);
            let first = self.seq + 1 - self.unsent.len() as u32;
            for (k, r) in std::mem::take(&mut self.unsent).into_iter().enumerate() {
                self.floor.input(first + k as u32, [r, r - 3.0], self.seq, at, ClockRate { rate: RATE, jitter });
            }
            shot.map(|r| {
                let h = self.floor.shot(self.seq, [r, r - 3.0], Some(0.0));
                (h[0] - r).abs().max((h[1] - (r - 3.0)).abs())
            })
        }

        /// One input a step, drawn at `r(k)`, arriving 40 ms after it's made
        /// (± `wobble`); a shot rides every third, claiming `shot(k)`.
        /// Returns the most any shot was held.
        fn run(&mut self, steps: u32, from: f64, r: impl Fn(f64) -> f64, wobble: f64, jitter: f64, shot: impl Fn(f64) -> f64) -> f64 {
            let mut most: f64 = 0.0;
            for k in 0..steps {
                let t = from + k as f64 * STEP;
                self.make(r(t));
                let late = if k % 2 == 0 { wobble } else { -wobble };
                let held = self.arrive(t + 0.040 + late, jitter, (k % 3 == 0).then(|| shot(t)));
                most = most.max(held.unwrap_or(0.0));
            }
            most
        }
    }

    #[test]
    fn honest_render_clocks_are_never_held() {
        // A shot drawn as its input was made (a bot), or up to a step before
        // (a frame loop that fired early in the step).
        let mut c = Client::new();
        assert_eq!(c.run(300, 0.0, |t| t * 30.0, 0.0, 0.0, |t| t * 30.0), 0.0, "steady");
        assert_eq!(c.run(60, 10.0, |t| t * 30.0, 0.0, 0.0, |t| t * 30.0 - 0.9), 0.0, "fired early in the step");
        // Slowing 10% for a second to take a longer delay.
        assert_eq!(c.run(30, 12.0, |t| 360.0 + (t - 12.0) * 27.0, 0.0, 0.0, |t| 360.0 + (t - 12.0) * 27.0), 0.0, "slewing");
        // ±20 ms of delay variation, allowed for.
        let mut c = Client::new();
        assert_eq!(c.run(300, 0.0, |t| t * 30.0, 0.020, 0.045, |t| t * 30.0), 0.0, "jitter");
    }

    #[test]
    fn hitches_and_losses_are_never_held() {
        let mut c = Client::new();
        c.run(30, 0.0, |t| t * 30.0, 0.0, 0.0, |t| t * 30.0);
        // A 200 ms hitch: six inputs made at once at the frame's render
        // step, in two messages arriving together; a shot rides the last.
        let (t, r) = (1.2, 36.0);
        for _ in 0..3 {
            c.make(r);
        }
        assert_eq!(c.arrive(t + 0.040, 0.0, None), None);
        for _ in 0..3 {
            c.make(r);
        }
        assert_eq!(c.arrive(t + 0.040, 0.0, Some(r)), Some(0.0), "a catch-up");
        // A lost message: its input (and shot) arrive a step later as a copy.
        c.make(r + 1.0);
        c.make(r + 2.0);
        let (seq, at) = (c.seq, c.t0 + std::time::Duration::from_secs_f64(t + 2.0 * STEP + 0.040));
        let first = seq - 1;
        let clock = ClockRate { rate: RATE, jitter: 0.0 };
        c.floor.input(first, [r + 1.0, r - 2.0], seq, at, clock);
        let h = c.floor.shot(first, [r + 1.0, r - 2.0], Some(0.0));
        assert_eq!(h, [r + 1.0, r - 2.0], "a shot in a copy");
        c.floor.input(seq, [r + 2.0, r - 1.0], seq, at, clock);
        c.unsent.clear();
    }

    #[test]
    fn shots_are_held_to_the_inputs_around_them() {
        // Honest inputs, a shot claiming 3 steps further back than it drew:
        // held to the input made before it (a step back), less the slack.
        let mut c = Client::new();
        c.run(60, 0.0, |t| t * 30.0, 0.0, 0.0, |t| t * 30.0);
        c.make(60.0);
        let held = c.arrive(2.0 + 0.040, 0.0, Some(57.0)).unwrap();
        assert!((1.85..=1.95).contains(&held), "{held}");
        // Inputs all claiming 4 steps stale, a shot claiming what it really
        // saw: held to its own input's stale claim, so it can't pick fresh
        // and stale shot by shot.
        let mut c = Client::new();
        c.run(60, 0.0, |t| t * 30.0 - 4.0, 0.0, 0.0, |t| t * 30.0 - 4.0);
        c.make(56.0);
        let held = c.arrive(2.0 + 0.040, 0.0, Some(60.0)).unwrap();
        assert!((3.85..=3.95).contains(&held), "{held}");
        // Inputs that freeze their render step to grow stale: held to 90% of
        // real time, so the shot after a second of it is held ~27 steps.
        let mut c = Client::new();
        c.run(60, 0.0, |t| t * 30.0, 0.0, 0.0, |t| t * 30.0);
        let held = c.run(30, 2.0, |_| 59.0, 0.0, 0.0, |_| 59.0);
        assert!(held > 20.0, "{held}");
        // A message without render steps can't dodge the floor.
        let at = c.t0 + std::time::Duration::from_secs_f64(3.0 + 0.040);
        let bare = c.floor.bare_shot(c.seq + 1, [59.0, 56.0], c.seq + 1, at, ClockRate { rate: RATE, jitter: 0.0 }).unwrap();
        assert!(bare[0] > 85.0, "{bare:?}");
        assert_eq!(RenderFloor::default().bare_shot(1, [5.0, 2.0], 1, at, ClockRate { rate: RATE, jitter: 0.0 }), None);
        // Jitter buys slack, but only up to FLOOR_JITTER_MAX.
        let (mut a, mut b) = (Client::new(), Client::new());
        let freeze = |c: &mut Client, jitter| {
            c.run(60, 0.0, |t| t * 30.0, 0.0, jitter, |t| t * 30.0);
            c.run(30, 2.0, |_| 59.0, 0.0, jitter, |_| 59.0)
        };
        assert_eq!(freeze(&mut a, 1.0), freeze(&mut b, FLOOR_JITTER_MAX));
    }

    #[test]
    fn spread_picks_depend_on_the_secret() {
        let (a, b) = (SpreadKey::new(Some([1; 32])), SpreadKey::new(Some([2; 32])));
        assert_eq!(a.pick(5, 9), SpreadKey::new(Some([1; 32])).pick(5, 9), "the same secret: the same picks");
        assert_ne!(a.pick(5, 9), b.pick(5, 9), "another secret: other picks");
        assert_ne!(a.pick(5, 9), a.pick(6, 9), "another spawn");
        assert_ne!(a.pick(5, 9), a.pick(5, 10), "another shot");
        let os = SpreadKey::new(None);
        assert_ne!(os.pick(5, 9), SpreadKey::new(None).pick(5, 9), "a fresh secret from the OS each time");
        // The picks' bits are spread evenly (the cone takes 48 of them).
        let ones: u32 = (0..1000).map(|seq| a.pick(1, seq).count_ones()).sum();
        assert!((31_000..33_000).contains(&ones), "{ones} of 64,000 bits set");
    }
}
