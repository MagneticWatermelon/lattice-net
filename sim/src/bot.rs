//! A bot's brain: wander AI, client-side prediction and reconciliation.
//! Transport-agnostic: feed it the messages a `lattice_net::Client` delivers and
//! send the input batches it returns.

use crate::movement::{step, Input, MoveState, BUTTON_SPRINT};
use crate::msg::{self, ServerMsg, SnapshotHeader, Welcome, INPUT_REDUNDANCY};
use crate::rng::Rng;

/// Predicted states kept for reconciliation (~4 s at 30 Hz).
const HISTORY: usize = 128;
/// A server/prediction mismatch above this snaps and replays. Below it the
/// error is left alone; exact determinism makes that case rare.
pub const CORRECTION_EPSILON: f32 = 0.001;
/// Inputs we want queued at the server when it consumes one: the one it needs
/// plus 1-2 spare. With a spare, a lost packet is covered by the next packet's
/// redundant copy; more spares only add input latency. The clock is left alone
/// inside this band (depth is an integer, so a point target would oscillate).
const TARGET_DEPTH: (f32, f32) = (2.0, 3.0);
const DEAD_BAND: f32 = 0.25;
/// Input clock speed change per input of depth error, capped at ±5 %.
const CLOCK_GAIN: f32 = 0.03;
const MAX_CLOCK_ADJUST: f32 = 0.05;

#[derive(Debug, Clone, Default)]
pub struct BotStats {
    pub snapshots: u64,
    /// Snapshots that arrived after a newer one and were ignored.
    pub stale_snapshots: u64,
    pub entities_seen: u64,
    /// Snapshots whose authoritative state disagreed with our prediction.
    pub corrections: u64,
    /// Sum and max of those position errors, in meters.
    pub correction_error_sum: f64,
    pub correction_error_max: f32,
    /// Snapshots acking an input we no longer remember (way too old, or bogus).
    pub unmatched_acks: u64,
    /// Times the server had consumed seqs we hadn't generated yet (we stalled)
    /// and we jumped ahead. Snapshots queued up during one stall count once.
    pub resyncs: u64,
    /// Ticks where the input clock produced two inputs / none.
    pub clock_extra: u64,
    pub clock_skipped: u64,
    pub bad_messages: u64,
}

#[derive(Clone, Copy, Default)]
struct Predicted {
    seq: u32,
    input: Input,
    /// State after applying `input`.
    state: MoveState,
}

pub struct BotBrain {
    rng: Rng,
    welcome: Option<Welcome>,
    heading: f32,
    sprint: bool,
    seq: u32,
    state: MoveState,
    history: Vec<Predicted>,
    /// Input clock: `rate` inputs per tick, accumulated in `clock`.
    rate: f32,
    clock: f32,
    buffer_avg: f32,
    resyncing: bool,
    last_server_tick: Option<u32>,
    pub stats: BotStats,
}

impl BotBrain {
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        Self {
            heading: rng.range(0.0, std::f32::consts::TAU),
            rng,
            welcome: None,
            sprint: false,
            seq: 0,
            state: MoveState::default(),
            history: vec![Predicted::default(); HISTORY],
            rate: 1.0,
            // Mid-phase: a rate a hair off 1.0 must drift half a tick before it
            // adds or skips an input, instead of flipping at the edge every tick.
            clock: 0.5,
            buffer_avg: (TARGET_DEPTH.0 + TARGET_DEPTH.1) / 2.0,
            resyncing: false,
            last_server_tick: None,
            stats: BotStats::default(),
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

    pub fn on_message(&mut self, data: &[u8]) {
        match msg::decode_server_msg(data) {
            Ok(ServerMsg::Welcome(w)) if self.welcome.is_none() => {
                self.state = MoveState { pos: w.spawn, vel: [0.0; 2] };
                self.welcome = Some(w);
            }
            Ok(ServerMsg::Snapshot(h, _)) if self.welcome.is_some() => self.on_snapshot(&h),
            Ok(_) => {}
            Err(_) => self.stats.bad_messages += 1,
        }
    }

    fn on_snapshot(&mut self, h: &SnapshotHeader) {
        if self.last_server_tick.is_some_and(|t| h.server_tick.wrapping_sub(t) as i32 <= 0) {
            self.stats.stale_snapshots += 1;
            return;
        }
        self.last_server_tick = Some(h.server_tick);
        self.stats.snapshots += 1;
        self.stats.entities_seen += h.count as u64;

        if h.ack_seq == 0 {
            return; // server hasn't consumed any of our inputs yet
        }
        self.buffer_avg += (h.buffered as f32 - self.buffer_avg) * 0.1;
        let (lo, hi) = (TARGET_DEPTH.0 - DEAD_BAND, TARGET_DEPTH.1 + DEAD_BAND);
        let error = if self.buffer_avg < lo {
            TARGET_DEPTH.0 - self.buffer_avg
        } else if self.buffer_avg > hi {
            TARGET_DEPTH.1 - self.buffer_avg
        } else {
            0.0
        };
        self.rate = 1.0 + (CLOCK_GAIN * error).clamp(-MAX_CLOCK_ADJUST, MAX_CLOCK_ADJUST);

        if h.ack_seq > self.seq {
            // We fell behind (stalled) and the server filled our seqs with
            // stand-ins. Adopt its state and continue after its newest seq;
            // otherwise every input we send would arrive late and be dropped.
            self.stats.resyncs += !self.resyncing as u64;
            self.resyncing = true;
            self.seq = h.ack_seq;
            self.state = h.own;
            self.history[self.seq as usize % HISTORY] = Predicted { seq: self.seq, input: Input::default(), state: h.own };
            return;
        }
        self.resyncing = false;
        let slot = self.history[h.ack_seq as usize % HISTORY];
        if slot.seq != h.ack_seq {
            self.stats.unmatched_acks += 1;
            return;
        }
        let err = ((slot.state.pos[0] - h.own.pos[0]).powi(2) + (slot.state.pos[1] - h.own.pos[1]).powi(2)).sqrt();
        let vel_err = (slot.state.vel[0] - h.own.vel[0]).abs() + (slot.state.vel[1] - h.own.vel[1]).abs();
        if err <= CORRECTION_EPSILON && vel_err <= CORRECTION_EPSILON {
            return;
        }
        self.stats.corrections += 1;
        self.stats.correction_error_sum += err as f64;
        self.stats.correction_error_max = self.stats.correction_error_max.max(err);

        // Rebase on the server's state and replay everything it hasn't seen yet.
        let mut s = h.own;
        self.history[h.ack_seq as usize % HISTORY].state = s;
        for seq in h.ack_seq + 1..=self.seq {
            let p = &mut self.history[seq as usize % HISTORY];
            s = step(s, p.input);
            p.state = s;
        }
        self.state = s;
    }

    /// Run the input clock for one tick: usually one input, occasionally two or
    /// none while it steers the server's queue depth into `TARGET_DEPTH`. Returns
    /// the batch to send (unreliable), or `None` when there's nothing new or
    /// before the server has welcomed us.
    pub fn tick_inputs(&mut self) -> Option<Vec<u8>> {
        let w = self.welcome?;
        self.clock += self.rate;
        let mut made = 0;
        while self.clock >= 1.0 {
            self.clock -= 1.0;
            let input = self.think(&w);
            self.seq += 1;
            self.state = step(self.state, input);
            self.history[self.seq as usize % HISTORY] = Predicted { seq: self.seq, input, state: self.state };
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
        let mut batch = [Input::default(); INPUT_REDUNDANCY];
        let mut n = 0;
        while n < INPUT_REDUNDANCY && (n as u32) < self.seq {
            let p = self.history[(self.seq as usize - n) % HISTORY];
            if p.seq != self.seq - n as u32 {
                break;
            }
            batch[n] = p.input;
            n += 1;
        }
        Some(msg::encode_inputs(self.seq, &batch[..n]))
    }

    /// Random walk that stays within the scenario radius around the anchor.
    fn think(&mut self, w: &Welcome) -> Input {
        if self.rng.chance(1.0 / 45.0) {
            self.heading = self.rng.range(0.0, std::f32::consts::TAU);
            self.sprint = self.rng.chance(0.3);
        }
        let d = [w.anchor[0] - self.state.pos[0], w.anchor[1] - self.state.pos[1]];
        if d[0] * d[0] + d[1] * d[1] > w.radius * w.radius {
            self.heading = d[1].atan2(d[0]) + self.rng.range(-0.5, 0.5);
        }
        let (sin, cos) = self.heading.sin_cos();
        Input {
            move_x: (cos * 127.0) as i8,
            move_y: (sin * 127.0) as i8,
            yaw: (self.heading.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16,
            buttons: if self.sprint { BUTTON_SPRINT } else { 0 },
        }
    }
}
