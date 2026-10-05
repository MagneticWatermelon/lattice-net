//! # lattice-client-core
//!
//! The client every player runs, bots and humans alike. Sans-IO: feed it the
//! messages a `lattice_net::Client` delivers and send the input batches it
//! returns. What the inputs *are* comes from outside (a bot's AI, a human's
//! keyboard), through `tick_inputs`.
//!
//! - **Own player:** the input clock (paced by the server, steering its input
//!   queue to one spare), prediction, and reconciliation against each
//!   snapshot. `own_render` is where to draw it between input steps.
//! - **Everyone else** (`entities`): one render timeline (`clock`), a fixed
//!   delay behind the newest game step the server sent, with each entity
//!   interpolated, extrapolated or held on it, whatever its tier.
//! - Every input carries the render step it was made at, so the server knows
//!   what the player saw (lag compensation's rewind).

pub mod clock;
pub mod entities;

use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_game::delta;
use lattice_game::movement::{step, Input, MoveState, TICK_HZ};
use lattice_game::msg::{self, ServerMsg, SnapshotHeader, Welcome, INPUT_REDUNDANCY, WAIT_STAND_IN};
use lattice_game::tier::Tier;
use lattice_game::world::World;

pub use clock::{RenderClock, TickSteps};
pub use entities::{Entities, How, Known, RenderState, Sample, SmoothStats};

/// Predicted states kept for reconciliation (~4 s at 30 Hz).
const HISTORY: usize = 128;
/// A server/prediction mismatch above this snaps and replays. Below it the
/// error is left alone; exact determinism makes that case rare.
pub const CORRECTION_EPSILON: f32 = 0.001;
/// Inputs we want queued at the server when it consumes one: the one it needs
/// plus one spare. With a spare, a lost packet is covered by the next packet's
/// redundant copy. Each spare adds a tick of input -> applied latency.
const TARGET_DEPTH: f32 = 2.0;
/// The clock is left alone while the smoothed depth is inside this band.
/// Depth is an integer, so a point target would oscillate: the band holds a
/// steady depth of 2, and puts a steady 3 (two spares, left over after a
/// stall) above the band, so it drains back instead of costing 33 ms forever.
const DEPTH_BAND: (f32, f32) = (1.75, 2.5);
/// Input clock speed change per input of depth error, capped at ±5 %.
const CLOCK_GAIN: f32 = 0.03;
const MAX_CLOCK_ADJUST: f32 = 0.05;
/// After the server reports a stand-in, one extra input goes out at once rather
/// than waiting ~20 ticks for the nudge; then this many ticks pass before the
/// next such bump, so reports still in flight don't stack up bumps.
const BUMP_COOLDOWN: f32 = 10.0;
/// A reported depth this far above target (say, after the server slowed down
/// before we heard) drops an input at once instead of draining at 5%.
const BACKLOG: f32 = 3.0;
/// Own-player corrections are smoothed with this time constant.
const OWN_SMOOTH: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// How far behind the newest server state other entities are drawn.
    /// Near updates come every 33 ms, so 100 ms rides out ~2 late or lost ones.
    pub interp_delay: Duration,
    /// Keep the entity store (interpolation, smoothness stats). Off for most
    /// of a bot swarm, which only counts what it's sent.
    pub track_entities: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self { interp_delay: Duration::from_millis(100), track_entities: true }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClientStats {
    pub snapshots: u64,
    /// Snapshots that arrived after a newer one and were ignored.
    pub stale_snapshots: u64,
    /// Entity updates received per tier (near, mid, far).
    pub tier_seen: [u64; 3],
    /// Snapshots whose authoritative state disagreed with our prediction,
    /// other than by a crowd's push.
    pub corrections: u64,
    /// Snapshots that disagreed because the server pushed us apart from a
    /// crowd (unpredictable by design), and the largest such error, in meters.
    pub push_corrections: u64,
    pub push_error_max: f32,
    /// Sum and max of those position errors, in meters.
    pub correction_error_sum: f64,
    pub correction_error_max: f32,
    /// Snapshots acking an input we no longer remember (way too old, or bogus).
    pub unmatched_acks: u64,
    /// Times the server had consumed seqs we hadn't generated yet (we stalled)
    /// and we jumped ahead. Snapshots queued up during one stall count once.
    pub resyncs: u64,
    /// Calls where the input clock produced two or more inputs / none.
    pub clock_extra: u64,
    pub clock_skipped: u64,
    pub bad_messages: u64,
    /// Round trip: from generating an input to reading the first snapshot that
    /// acks it. This bounds how far reconciliation replays.
    pub latency_samples: u64,
    pub latency_sum_ms: f64,
    /// Server-reported waits (arrival -> applied) for real, non-stand-in inputs.
    pub wait_samples: u64,
    pub wait_sum_ms: f64,
    /// Inputs skipped to drain a backlog at the server.
    pub backlog_skips: u64,
    /// Near messages a tracked bot couldn't decode (a delta against a
    /// baseline it doesn't have). The server only uses acked baselines, so
    /// this should stay 0.
    pub near_decode_errors: u64,
    /// Render frames (`render` calls), and the sum of their actual delay
    /// behind the newest step, in steps.
    pub render_frames: u64,
    pub render_delay_sum: f64,
    /// Own-player corrections, smoothed: the largest offset, in meters.
    pub own_offset_max: f32,
    /// From the latest snapshot: the server's pace (per mille) and levels.
    pub pace: u16,
    pub level: u8,
    pub client_level: u8,
}

/// When one of our inputs was first seen acked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputTiming {
    /// Input generated -> first snapshot acking it read here, in ms (round trip).
    pub seen_ms: u16,
    /// How long it waited on the server, arrival -> applied, in 0.1 ms.
    /// `None` if a stand-in consumed its seq. What other players and hit
    /// detection feel is one-way latency (about RTT / 2) plus this.
    pub server_wait: Option<u16>,
}

#[derive(Clone, Copy, Default)]
struct Predicted {
    seq: u32,
    input: Input,
    /// State after applying `input`.
    state: MoveState,
    /// When the input was generated (and sent).
    sent_at: Option<Instant>,
    /// The render step then, as sent (`msg::render_units`).
    render: Option<u16>,
}

pub struct ClientCore {
    welcome: Option<Welcome>,
    /// The push counter in the last snapshot (see `SnapshotHeader::pushes`);
    /// the server starts it at 0 when we spawn.
    last_pushes: u8,
    /// Built from the Welcome's seed (shared by every bot in the process).
    world: Option<Arc<World>>,
    seq: u32,
    state: MoveState,
    history: Vec<Predicted>,
    /// Input clock: `rate` inputs per 1/30 s, accumulated in `clock`.
    rate: f32,
    /// The server's pace: inputs per 1/30 s before the depth nudge.
    pace: f32,
    clock: f32,
    /// When `clock` was last advanced.
    clock_at: Option<Instant>,
    /// In 1/30 s.
    bump_cooldown: f32,
    buffer_avg: f32,
    resyncing: bool,
    last_server_tick: Option<u32>,
    last_acked: u32,
    /// Timings not yet taken by `drain_latency`.
    latency: Vec<InputTiming>,
    render_clock: RenderClock,
    tick_steps: TickSteps,
    entities: Option<Entities>,
    /// Own-player correction being smoothed away, as of `own_offset_at`.
    own_offset: [f32; 3],
    own_offset_at: Option<Instant>,
    /// Sink mode: entity messages are only counted, and no latency samples
    /// are kept. Inputs, prediction and pacing still run.
    sink: bool,
    pub stats: ClientStats,
}

impl Default for ClientCore {
    fn default() -> Self {
        Self::new(ClientConfig::default())
    }
}

impl ClientCore {
    pub fn new(cfg: ClientConfig) -> Self {
        Self {
            welcome: None,
            last_pushes: 0,
            world: None,
            seq: 0,
            state: MoveState::default(),
            history: vec![Predicted::default(); HISTORY],
            rate: 1.0,
            pace: 1.0,
            // Mid-phase: a rate a hair off 1.0 must drift half a tick before it
            // adds or skips an input, instead of flipping at the edge every tick.
            clock: 0.5,
            clock_at: None,
            bump_cooldown: 0.0,
            buffer_avg: TARGET_DEPTH,
            resyncing: false,
            last_server_tick: None,
            last_acked: 0,
            latency: Vec::new(),
            render_clock: RenderClock::new(cfg.interp_delay),
            tick_steps: TickSteps::default(),
            entities: cfg.track_entities.then(Entities::default),
            own_offset: [0.0; 3],
            own_offset_at: None,
            sink: false,
            stats: ClientStats::default(),
        }
    }

    pub fn welcome(&self) -> Option<&Welcome> {
        self.welcome.as_ref()
    }

    pub fn predicted(&self) -> MoveState {
        self.state
    }

    /// Smoothed count of our inputs the server had queued when it consumed one.
    pub fn server_buffer(&self) -> f32 {
        self.buffer_avg
    }

    /// Moves input timings recorded since the last call into `out`.
    /// Sink mode, for most of a large swarm: the bot keeps playing (inputs,
    /// prediction, pace) but spends nothing on what it's sent beyond counting
    /// it, so a load test measures the server, not the swarm.
    pub fn set_sink(&mut self, sink: bool) {
        self.sink = sink;
        if sink {
            self.entities = None;
        }
    }

    /// Starts keeping the entity store (a map per bot, so a swarm enables it
    /// on a sample).
    pub fn enable_tracking(&mut self) {
        self.entities.get_or_insert_with(Entities::default);
    }

    pub fn entities(&self) -> Option<&Entities> {
        self.entities.as_ref()
    }

    pub fn render_clock(&self) -> &RenderClock {
        &self.render_clock
    }

    /// Moves the update intervals (in server ticks) recorded since the last
    /// call into `out`, per tier.
    pub fn drain_intervals(&mut self, out: &mut [Vec<u16>; 3]) {
        if let Some(t) = &mut self.entities {
            for (o, i) in out.iter_mut().zip(&mut t.intervals) {
                o.append(i);
            }
        }
    }

    /// Moves the pops (mm) recorded since the last call into `out`, per tier.
    pub fn drain_pops(&mut self, out: &mut [Vec<u16>; 3]) {
        if let Some(t) = &mut self.entities {
            for (o, i) in out.iter_mut().zip(&mut t.smooth.pops) {
                o.append(i);
            }
        }
    }

    /// The render step at `now` (advancing the render clock), or `None`
    /// before the first snapshot.
    pub fn render_step(&mut self, now: Instant) -> Option<f64> {
        self.render_clock.render_at(now)
    }

    /// One frame: draws every entity at the render step for `now`, calling
    /// `f(entity, state)`, and returns the step. Counts how each entity was
    /// drawn (`Entities::smooth`).
    pub fn render(&mut self, now: Instant, f: impl FnMut(u16, &RenderState)) -> Option<f64> {
        let r = self.render_clock.render_at(now)?;
        if let Some(newest) = self.render_clock.newest_at(now) {
            self.stats.render_frames += 1;
            self.stats.render_delay_sum += newest - r;
        }
        if let Some(e) = &mut self.entities {
            e.render(r, f);
        }
        Some(r)
    }

    /// Where to draw our own player at `now`: between the last two predicted
    /// steps by the input clock's phase, plus what's left of any smoothed
    /// correction. (Prediction runs ahead of the server; this is "now".)
    pub fn own_render(&self, now: Instant) -> [f32; 3] {
        let cur = self.state;
        let prev = match self.seq {
            0 => cur,
            s => {
                let p = self.history[(s - 1) as usize % HISTORY];
                if p.seq == s - 1 { p.state } else { cur }
            }
        };
        let since = self.clock_at.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        let phase = (self.clock + self.rate * since * TICK_HZ as f32).clamp(0.0, 1.0);
        let o = self.own_offset(now);
        [
            prev.pos[0] + (cur.pos[0] - prev.pos[0]) * phase + o[0],
            prev.pos[1] + (cur.pos[1] - prev.pos[1]) * phase + o[1],
            prev.z + (cur.z - prev.z) * phase + o[2],
        ]
    }

    fn own_offset(&self, now: Instant) -> [f32; 3] {
        let Some(at) = self.own_offset_at else { return [0.0; 3] };
        let k = (-now.saturating_duration_since(at).as_secs_f32() / OWN_SMOOTH.as_secs_f32()).exp();
        self.own_offset.map(|o| o * k)
    }

    pub fn drain_latency(&mut self, out: &mut Vec<InputTiming>) {
        out.append(&mut self.latency);
    }

    pub fn on_message(&mut self, data: &[u8], now: Instant) {
        if data.first() == Some(&delta::MSG_NEAR) {
            // tag:1 | server_tick:4 | n:1 | bits: only tracking clients decode it.
            self.stats.tier_seen[Tier::Near as usize] += data.get(5).copied().unwrap_or(0) as u64;
            if let Some(t) = &mut self.entities {
                let render = self.render_clock.render_at(now);
                let steps = &self.tick_steps;
                if t.on_near(data, |tick| steps.get(tick), render).is_err() {
                    self.stats.near_decode_errors += 1;
                }
            }
            return;
        }
        if self.sink && data.first() == Some(&msg::MSG_ENTITIES) {
            // tag:1 | server_tick:4 | tier:1 | n:1 | blobs
            if let (Some(&tier), Some(&n)) = (data.get(5), data.get(6)) {
                if let Some(t) = self.stats.tier_seen.get_mut(tier as usize) {
                    *t += n as u64;
                }
            }
            return;
        }
        match msg::decode_server_msg(data) {
            Ok(ServerMsg::Welcome(w)) if self.welcome.is_none() => {
                let world = World::shared(w.world_seed);
                // The server starts us exactly here too.
                self.state = MoveState::standing(&world, w.spawn);
                self.world = Some(world);
                self.welcome = Some(w);
                // Start with the spare already queued: at depth 1 every input
                // arrives just in time and any jitter makes it late.
                self.clock += TARGET_DEPTH - 1.0;
            }
            Ok(ServerMsg::Snapshot(h)) if self.welcome.is_some() => self.on_snapshot(&h, now),
            Ok(ServerMsg::Entities { server_tick, tier, blobs }) => {
                let size = msg::blob_size(tier);
                self.stats.tier_seen[tier as usize] += (blobs.len() / size) as u64;
                if let Some(t) = &mut self.entities {
                    let render = self.render_clock.render_at(now);
                    let step = self.tick_steps.get(server_tick);
                    for b in blobs.chunks_exact(size) {
                        t.on_blob(server_tick, step, tier, b, render);
                    }
                }
            }
            Ok(_) => {}
            Err(_) => self.stats.bad_messages += 1,
        }
    }

    fn on_snapshot(&mut self, h: &SnapshotHeader, now: Instant) {
        if self.last_server_tick.is_some_and(|t| h.server_tick.wrapping_sub(t) as i32 <= 0) {
            self.stats.stale_snapshots += 1;
            return;
        }
        self.last_server_tick = Some(h.server_tick);
        self.stats.snapshots += 1;
        (self.stats.pace, self.stats.level, self.stats.client_level) = (h.pace, h.level, h.client_level);
        // The server consumes inputs at `pace` x 30 per wall second (slower
        // under time dilation, or when it falls behind): send at that rate.
        self.pace = (h.pace as f32 / 1000.0).clamp(0.1, 1.0);
        self.tick_steps.put(h.server_tick, h.step);
        self.render_clock.on_snapshot(h.step, self.pace, now);

        if h.ack_seq == 0 {
            return; // server hasn't consumed any of our inputs yet
        }
        self.buffer_avg += (h.buffered as f32 - self.buffer_avg) * 0.1;
        let (lo, hi) = DEPTH_BAND;
        let error = if self.buffer_avg < lo || self.buffer_avg > hi { TARGET_DEPTH - self.buffer_avg } else { 0.0 };
        self.rate = self.pace * (1.0 + (CLOCK_GAIN * error).clamp(-MAX_CLOCK_ADJUST, MAX_CLOCK_ADJUST));
        if self.bump_cooldown <= 0.0 {
            if h.buffered == 0 {
                self.clock += 1.0;
                self.bump_cooldown = BUMP_COOLDOWN;
            } else if h.buffered as f32 >= TARGET_DEPTH + BACKLOG {
                self.clock -= 1.0;
                self.bump_cooldown = BUMP_COOLDOWN;
                self.stats.backlog_skips += 1;
            }
        }

        if h.ack_seq > self.seq {
            // We fell behind (stalled) and the server filled our seqs with
            // stand-ins. Adopt its state and continue after its newest seq;
            // otherwise every input we send would arrive late and be dropped.
            self.stats.resyncs += !self.resyncing as u64;
            if !self.resyncing {
                // Landing exactly on the server's seq leaves no lead: the next
                // input would be late too. Rebuild the spare right away.
                self.clock += TARGET_DEPTH;
            }
            self.resyncing = true;
            self.seq = h.ack_seq;
            self.state = h.own;
            self.history[self.seq as usize % HISTORY] =
                Predicted { seq: self.seq, input: Input::default(), state: h.own, sent_at: None, render: None };
            self.last_acked = h.ack_seq;
            return;
        }
        self.resyncing = false;
        let slot = self.history[h.ack_seq as usize % HISTORY];
        if slot.seq != h.ack_seq {
            self.stats.unmatched_acks += 1;
            return;
        }
        if h.ack_seq > self.last_acked {
            if let Some(sent) = slot.sent_at {
                let ms = (now.saturating_duration_since(sent).as_secs_f64() * 1000.0).round();
                let server_wait = (h.wait != WAIT_STAND_IN).then_some(h.wait);
                if !self.sink {
                    self.latency.push(InputTiming { seen_ms: ms.min(u16::MAX as f64) as u16, server_wait });
                }
                self.stats.latency_samples += 1;
                self.stats.latency_sum_ms += ms;
                if let Some(w) = server_wait {
                    self.stats.wait_samples += 1;
                    self.stats.wait_sum_ms += w as f64 / 10.0;
                }
            }
            self.last_acked = h.ack_seq;
        }
        // The server pushed us apart from a crowd since the last snapshot:
        // a miss now is that push, which we couldn't have predicted.
        let pushed = self.last_pushes != h.pushes;
        self.last_pushes = h.pushes;
        let (s, o) = (&slot.state, &h.own);
        let err = ((s.pos[0] - o.pos[0]).powi(2) + (s.pos[1] - o.pos[1]).powi(2) + (s.z - o.z).powi(2)).sqrt();
        let vel_err = (s.vel[0] - o.vel[0]).abs() + (s.vel[1] - o.vel[1]).abs() + (s.vz - o.vz).abs();
        // Below the epsilon a miss is left alone, but never after a push: a
        // push can be under a millimeter, and left alone it would grow into
        // a "misprediction" later, with no push to explain it.
        let tiny = err <= CORRECTION_EPSILON && vel_err <= CORRECTION_EPSILON && s.grounded == o.grounded;
        if *s == *o || (tiny && !pushed) {
            return;
        }
        if pushed {
            self.stats.push_corrections += 1;
            self.stats.push_error_max = self.stats.push_error_max.max(err);
        } else {
            self.stats.corrections += 1;
            self.stats.correction_error_sum += err as f64;
            self.stats.correction_error_max = self.stats.correction_error_max.max(err);
        }

        // Rebase on the server's state and replay everything it hasn't seen
        // yet. What that moves on screen is smoothed away, not snapped.
        let mut s = h.own;
        let world = self.world.as_ref().expect("welcomed");
        self.history[h.ack_seq as usize % HISTORY].state = s;
        for seq in h.ack_seq + 1..=self.seq {
            let p = &mut self.history[seq as usize % HISTORY];
            s = step(world, s, p.input);
            p.state = s;
        }
        let (old, cur) = (self.state, self.own_offset(now));
        let o = [cur[0] + old.pos[0] - s.pos[0], cur[1] + old.pos[1] - s.pos[1], cur[2] + old.z - s.z];
        let len = (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt();
        self.own_offset = if len > entities::TELEPORT { [0.0; 3] } else { o };
        self.own_offset_at = Some(now);
        self.stats.own_offset_max = self.stats.own_offset_max.max(len);
        self.state = s;
    }

    /// Run the input clock up to `now`, by elapsed time: for a frame loop
    /// (a human client at 60-240 Hz), where each call adds a fraction of an
    /// input and inputs come out every few frames. Each input comes from
    /// `source`, given the predicted state it will be applied to, and carries
    /// the render step of `now`. Returns the batch to send (unreliable), or
    /// `None` when there's nothing new or before the server has welcomed us.
    ///
    /// A caller running at the input rate itself (30 Hz) should use
    /// `step_inputs`: by elapsed time, a millisecond of jitter near the
    /// clock's phase boundary turns one input per call into two then none,
    /// and the early one waits a tick longer on the server (WSL blob: p99
    /// server wait 67-74 ms per call vs 80-102 ms by time).
    pub fn tick_inputs(&mut self, now: Instant, source: impl FnMut(&MoveState, &Welcome) -> Input) -> Option<Vec<u8>> {
        // In 1/30 s; a hitch counts for at most a batch's worth of inputs (a
        // long stall is the server's to fill with stand-ins, then a resync).
        let elapsed = match self.clock_at {
            None => 1.0,
            Some(t) => (now.saturating_duration_since(t).as_secs_f32() * TICK_HZ as f32).min(INPUT_REDUNDANCY as f32),
        };
        self.run_inputs(now, elapsed, source)
    }

    /// Run the input clock for exactly one 1/30 s step: for a caller that
    /// ticks at the input rate (the bots). Usually one input, occasionally
    /// two or none while it steers the server's queue depth into `DEPTH_BAND`.
    pub fn step_inputs(&mut self, now: Instant, source: impl FnMut(&MoveState, &Welcome) -> Input) -> Option<Vec<u8>> {
        self.run_inputs(now, 1.0, source)
    }

    /// Advances the input clock by `elapsed` steps' worth of time.
    fn run_inputs(&mut self, now: Instant, elapsed: f32, mut source: impl FnMut(&MoveState, &Welcome) -> Input) -> Option<Vec<u8>> {
        let w = self.welcome?;
        self.clock_at = Some(now);
        self.clock += self.rate * elapsed;
        self.bump_cooldown -= elapsed;
        let render = self.render_clock.render_at(now).map(msg::render_units);
        let mut made = 0;
        // Never more than one batch carries, or the oldest new input would be lost.
        while self.clock >= 1.0 && made < INPUT_REDUNDANCY {
            self.clock -= 1.0;
            let input = source(&self.state, &w);
            self.seq += 1;
            self.state = step(self.world.as_ref().expect("welcomed"), self.state, input);
            self.history[self.seq as usize % HISTORY] =
                Predicted { seq: self.seq, input, state: self.state, sent_at: Some(now), render };
            made += 1;
        }
        match made {
            0 => {
                self.stats.clock_skipped += 1;
                return None;
            }
            1 => {}
            _ => self.stats.clock_extra += 1,
        }

        // Newest first, stopping at a gap (a resync skips seqs we never generated).
        let mut batch = [(Input::default(), None); INPUT_REDUNDANCY];
        let mut n = 0;
        while n < INPUT_REDUNDANCY && (n as u32) < self.seq {
            let p = self.history[(self.seq as usize - n) % HISTORY];
            if p.seq != self.seq - n as u32 {
                break;
            }
            batch[n] = (p.input, p.render);
            n += 1;
        }
        Some(msg::encode_inputs(self.seq, &batch[..n]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_loop_makes_30_inputs_a_second_and_draws_its_player_smoothly() {
        let t0 = Instant::now();
        let mut c = ClientCore::default();
        let w = Welcome { entity: 1, spawn: [4096.0, 4096.0], anchor: [4096.0, 4096.0], radius: 100.0, world_seed: 7 };
        c.on_message(&msg::encode_welcome(&w), t0);
        let frame = Duration::from_secs_f64(1.0 / 144.0);
        let run = Input { move_x: 127, ..Default::default() };
        let mut steps = Vec::new();
        let mut last = c.own_render(t0);
        for f in 1..=4 * 144 {
            let now = t0 + frame * f;
            let before = c.seq;
            c.tick_inputs(now, |_, _| run);
            // The first call starts with the spare (two inputs); then never
            // more than one per frame.
            assert!(c.seq - before <= if before == 0 { 2 } else { 1 }, "frame {f}");
            let p = c.own_render(now);
            if f > 144 {
                steps.push(((p[0] - last[0]).powi(2) + (p[1] - last[1]).powi(2)).sqrt());
            }
            last = p;
        }
        assert!((119..=123).contains(&c.seq), "{} inputs in 4 s", c.seq);
        // Drawn between input steps: the camera moves every frame, not every
        // fifth (inputs come every 4.8 frames).
        let mean = steps.iter().sum::<f32>() / steps.len() as f32;
        let max = steps.iter().cloned().fold(0.0, f32::max);
        assert!(mean > 0.02, "it runs: {mean} m per frame");
        assert!(max < 2.0 * mean, "smooth: max {max} vs mean {mean} m per frame");
    }
}
