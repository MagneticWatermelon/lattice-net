//! A bot: the client core every player runs (`lattice-client-core`), driven by
//! a wander AI instead of a keyboard. Transport-agnostic: feed it the messages
//! a `lattice_net::Client` delivers and send the input batches it returns.

use std::time::Instant;

pub use lattice_client_core::{ClientCore, ClientStats, InputTiming, Tracker};

use crate::movement::{Input, MoveState, BUTTON_JUMP, BUTTON_SPRINT};
use crate::msg::Welcome;
use crate::rng::Rng;

pub use lattice_client_core::CORRECTION_EPSILON;

pub struct BotBrain {
    core: ClientCore,
    ai: Wander,
}

impl BotBrain {
    pub fn new(seed: u64) -> Self {
        Self { core: ClientCore::new(), ai: Wander::new(seed) }
    }

    pub fn core(&self) -> &ClientCore {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut ClientCore {
        &mut self.core
    }

    pub fn stats(&self) -> &ClientStats {
        &self.core.stats
    }

    pub fn welcome(&self) -> Option<&Welcome> {
        self.core.welcome()
    }

    pub fn predicted(&self) -> MoveState {
        self.core.predicted()
    }

    pub fn server_buffer(&self) -> f32 {
        self.core.server_buffer()
    }

    pub fn set_sink(&mut self, sink: bool) {
        self.core.set_sink(sink);
    }

    pub fn enable_tracking(&mut self) {
        self.core.enable_tracking();
    }

    pub fn tracker(&self) -> Option<&Tracker> {
        self.core.tracker()
    }

    pub fn drain_intervals(&mut self, out: &mut [Vec<u16>; 3]) {
        self.core.drain_intervals(out);
    }

    pub fn drain_latency(&mut self, out: &mut Vec<InputTiming>) {
        self.core.drain_latency(out);
    }

    pub fn on_message(&mut self, data: &[u8], now: Instant) {
        self.core.on_message(data, now);
    }

    /// One tick of the input clock, with the AI choosing each input.
    pub fn tick_inputs(&mut self, now: Instant) -> Option<Vec<u8>> {
        let ai = &mut self.ai;
        self.core.tick_inputs(now, |s, w| ai.think(s, w))
    }
}

/// Random walk that stays within the scenario radius around the anchor,
/// turning away when cover or a slope stops it, and jumping now and then.
struct Wander {
    rng: Rng,
    heading: f32,
    sprint: bool,
}

impl Wander {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        Self { heading: rng.range(0.0, std::f32::consts::TAU), rng, sprint: false }
    }

    fn think(&mut self, s: &MoveState, w: &Welcome) -> Input {
        let stopped = s.grounded && s.vel[0] * s.vel[0] + s.vel[1] * s.vel[1] < 0.25;
        if self.rng.chance(1.0 / 45.0) || (stopped && self.rng.chance(0.3)) {
            self.heading = self.rng.range(0.0, std::f32::consts::TAU);
            self.sprint = self.rng.chance(0.3);
        }
        let d = [w.anchor[0] - s.pos[0], w.anchor[1] - s.pos[1]];
        if d[0] * d[0] + d[1] * d[1] > w.radius * w.radius {
            self.heading = d[1].atan2(d[0]) + self.rng.range(-0.5, 0.5);
        }
        let (sin, cos) = self.heading.sin_cos();
        Input {
            move_x: (cos * 127.0) as i8,
            move_y: (sin * 127.0) as i8,
            yaw: (self.heading.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16,
            pitch: 0,
            buttons: if self.sprint { BUTTON_SPRINT } else { 0 } | if self.rng.chance(1.0 / 90.0) { BUTTON_JUMP } else { 0 },
        }
    }
}
