//! A bot: the client core every player runs (`lattice-client-core`), driven by
//! a wander AI instead of a keyboard. Transport-agnostic: feed it the messages
//! a `lattice_net::Client` delivers and send the input batches it returns.

use std::time::Instant;

pub use lattice_client_core::{ClientConfig, ClientCore, ClientStats, Entities, InputTiming};

use crate::movement::{Input, MoveState, BUTTON_ADS, BUTTON_JUMP, BUTTON_SPRINT};
use crate::msg::Welcome;
use crate::rng::Rng;

pub use lattice_client_core::CORRECTION_EPSILON;

pub struct BotBrain {
    core: ClientCore,
    ai: Wander,
    /// Holds the trigger, aiming level along its heading (firing load).
    trigger: bool,
    /// Fights: aims at the nearest enemy it draws (tracked bots only).
    fighter: Option<Fighter>,
    /// Aims down sights (tests); fighters do whenever they have a target.
    ads: bool,
    seed: u64,
}

/// How a bot moves (tests use the scripted ones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Moves {
    /// The random walk around its anchor.
    Wander,
    /// Stands still (a gunner on a range).
    Hold,
    /// Runs side to side along y at run speed, turning every `period` steps:
    /// the hard case for lag compensation.
    Strafe { period: u32 },
}

impl BotBrain {
    /// A bot that only counts entities until `enable_tracking`.
    pub fn new(seed: u64) -> Self {
        Self::with_config(seed, ClientConfig { track_entities: false, ..Default::default() })
    }

    pub fn with_config(seed: u64, cfg: ClientConfig) -> Self {
        Self { core: ClientCore::new(cfg), ai: Wander::new(seed), trigger: false, fighter: None, ads: false, seed }
    }

    /// Fights: aims at the nearest enemy it draws and fires in bursts. It
    /// needs to draw others, so tracking comes on with it.
    pub fn set_fight(&mut self, cfg: Option<FightConfig>) {
        if cfg.is_some() {
            self.core.enable_tracking();
        }
        self.fighter = cfg.map(|c| Fighter::new(c, self.seed));
    }

    /// Holds the trigger: a shot every 100 ms, level along its heading.
    pub fn set_trigger(&mut self, on: bool) {
        self.trigger = on;
    }

    /// Where it walks (and, held or with the trigger, faces and fires), in
    /// radians from east.
    pub fn set_heading(&mut self, heading: f32) {
        self.ai.heading = heading;
    }

    pub fn set_moves(&mut self, m: Moves) {
        self.ai.moves = m;
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

    pub fn entities(&self) -> Option<&Entities> {
        self.core.entities()
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

    /// Aims down sights (slower, a tighter cone of fire) from now on.
    pub fn set_ads(&mut self, on: bool) {
        self.ads = on;
    }

    /// One tick of the input clock, with the AI choosing each input. Fighters
    /// aim down sights while they have a target, as players do; trigger bots
    /// spray from the hip.
    pub fn tick_inputs(&mut self, now: Instant) -> Option<Vec<u8>> {
        if let Some(f) = &mut self.fighter {
            f.tick(&mut self.core, now);
        } else if self.trigger {
            let yaw = (self.ai.heading.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16;
            self.core.fire(now, yaw, 0, self.ads);
        }
        let ads = self.ads || self.fighter.as_ref().is_some_and(|f| f.cfg.ads && f.target.is_some());
        let ai = &mut self.ai;
        self.core.step_inputs(now, |s, w| {
            let mut input = ai.think(s, w);
            if ads {
                input.buttons = (input.buttons | BUTTON_ADS) & !BUTTON_SPRINT;
            }
            input
        })
    }
}

/// Random walk that stays within the scenario radius around the anchor,
/// turning away when cover or a slope stops it, and jumping now and then.
struct Wander {
    rng: Rng,
    heading: f32,
    sprint: bool,
    moves: Moves,
    steps: u32,
}

impl Wander {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        Self { heading: rng.range(0.0, std::f32::consts::TAU), rng, sprint: false, moves: Moves::Wander, steps: 0 }
    }

    fn think(&mut self, s: &MoveState, w: &Welcome) -> Input {
        self.steps += 1;
        match self.moves {
            Moves::Wander => {}
            // Stands, facing its heading.
            Moves::Hold => return Input { yaw: (self.heading.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16, ..Input::default() },
            Moves::Strafe { period } => {
                let dir = if (self.steps / period.max(1)).is_multiple_of(2) { 127 } else { -127 };
                return Input { move_y: dir, ..Input::default() };
            }
        }
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

/// How a fighting bot shoots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FightConfig {
    /// Aim error per shot, one standard deviation, in radians (on yaw and on
    /// pitch).
    pub aim_error: f32,
    /// Engages enemies within this range, in meters.
    pub range: f32,
    /// Aims down sights while engaged (slower, a tight cone), as players do.
    pub ads: bool,
}

impl Default for FightConfig {
    fn default() -> Self {
        Self { aim_error: 0.006, range: 150.0, ads: true }
    }
}

/// A fighting bot's state: its target, and where it is in a burst.
struct Fighter {
    cfg: FightConfig,
    rng: Rng,
    target: Option<u16>,
    retarget_in: u32,
    burst: u32,
    pause: u32,
}

/// Where to aim at `target` as `core` draws it at render step `r`: at its
/// body (or head), leading for the flight time and the drop at its drawn
/// velocity, plus `extra_lead` steps. The aim as the wire carries it.
pub fn aim_at(core: &ClientCore, target: u16, r: f64, head: bool, extra_lead: f32) -> Option<(u16, i16)> {
    use lattice_game::hit::{BODY_HIGH, BODY_LOW, HEAD_AT};
    use lattice_game::weapon::{EYE_HEIGHT, GRAVITY, MUZZLE_SPEED};
    let ents = core.entities()?;
    let (st, was) = (ents.render_one(target, r)?, ents.render_one(target, r - 0.5)?);
    if st.dead {
        return None;
    }
    let tick_hz = crate::movement::TICK_HZ as f32;
    let vel = [0, 1].map(|k| (st.pos[k] - was.pos[k]) * 2.0 * tick_hz);
    let me = core.predicted();
    let eye = [me.pos[0], me.pos[1], me.z + EYE_HEIGHT];
    let at = st.pos[2] + if head { HEAD_AT } else { (BODY_LOW + BODY_HIGH) / 2.0 };
    let flight = ((st.pos[0] - eye[0]).powi(2) + (st.pos[1] - eye[1]).powi(2)).sqrt() / MUZZLE_SPEED;
    let lead = flight + extra_lead / tick_hz;
    let p = [st.pos[0] + vel[0] * lead, st.pos[1] + vel[1] * lead, at + 0.5 * GRAVITY * flight * flight];
    let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
    let yaw = (d[1].atan2(d[0]).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16;
    let pitch = (d[2].atan2(d[0].hypot(d[1])) / std::f32::consts::FRAC_PI_2 * 32767.0) as i16;
    Some((yaw, pitch))
}

impl Fighter {
    fn new(cfg: FightConfig, seed: u64) -> Self {
        Self { cfg, rng: Rng::new(seed ^ 0x000F_1647), target: None, retarget_in: 0, burst: 0, pause: 0 }
    }

    /// A normal deviate (Box-Muller).
    fn normal(&mut self) -> f32 {
        let (u, v) = (self.rng.unit().max(1e-7), self.rng.unit());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }

    /// The nearest enemy it draws within range, alive and in sight.
    fn pick(&self, core: &ClientCore, r: f64) -> Option<u16> {
        let (ents, world, me) = (core.entities()?, core.world()?, core.welcome()?.entity);
        let side = lattice_game::faction::faction(me);
        let s = core.predicted();
        let eye = [s.pos[0], s.pos[1], s.z + lattice_game::weapon::EYE_HEIGHT];
        let mut near: Vec<(f32, u16, [f32; 3])> = ents
            .ids()
            .filter(|&e| lattice_game::faction::faction(e) != side)
            .filter_map(|e| ents.render_one(e, r).filter(|st| !st.dead).map(|st| (e, st.pos)))
            .map(|(e, p)| (((p[0] - eye[0]).powi(2) + (p[1] - eye[1]).powi(2)).sqrt(), e, p))
            .filter(|&(d, ..)| d <= self.cfg.range)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.into_iter().take(3).find(|&(_, _, p)| lattice_game::hit::line_clear(world, eye, [p[0], p[1], p[2] + 1.0])).map(|(_, e, _)| e)
    }

    /// One tick: pick a target now and then, fire in bursts (0.5-1 s, with
    /// 0.2-0.5 s pauses), each shot aimed with Gaussian error.
    fn tick(&mut self, core: &mut ClientCore, now: Instant) {
        if core.is_dead() {
            self.target = None;
            return;
        }
        let Some(r) = core.render_step(now) else { return };
        if self.retarget_in == 0 || self.target.is_none() {
            self.target = self.pick(core, r);
            self.retarget_in = 10;
        }
        self.retarget_in -= 1;
        if self.burst == 0 {
            if self.pause > 0 {
                self.pause -= 1;
                return;
            }
            self.burst = 15 + (self.rng.next_u64() % 16) as u32;
        }
        self.burst -= 1;
        if self.burst == 0 {
            self.pause = 6 + (self.rng.next_u64() % 10) as u32;
        }
        let Some(target) = self.target else { return };
        let Some((yaw, pitch)) = aim_at(core, target, r, false, 0.0) else {
            self.target = None;
            return;
        };
        let k = self.cfg.aim_error;
        let yaw = (yaw as f32 + self.normal() * k / std::f32::consts::TAU * 65536.0).rem_euclid(65536.0) as u16;
        let pitch = (pitch as f32 + self.normal() * k / std::f32::consts::FRAC_PI_2 * 32767.0).clamp(-32767.0, 32767.0) as i16;
        core.fire(now, yaw, pitch, self.cfg.ads);
    }
}
