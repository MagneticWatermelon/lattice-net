//! A bot: the client core every player runs (`lattice-client-core`), driven by
//! a wander AI instead of a keyboard. Transport-agnostic: feed it the messages
//! a `lattice_net::Client` delivers and send the input batches it returns.

use std::sync::Arc;
use std::time::Instant;

use lattice_game::nav::Nav;
use lattice_game::territory::{self, Territory};

pub use lattice_client_core::{ClientConfig, ClientCore, ClientStats, Entities, InputTiming};

use crate::movement::{Input, MoveState, BUTTON_ADS, BUTTON_CROUCH, BUTTON_JUMP, BUTTON_RELOAD, BUTTON_SPRINT};
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
    /// Presses reload with its next input (`reload`).
    reload_once: bool,
    /// Walks the lane it was spawned at an end of (`set_run_lanes`).
    run_lanes: bool,
    /// Takes sites (`set_capture`).
    campaign: Option<Campaign>,
    seed: u64,
    /// Messages for the reliable channel (a Deploy), until `take_reliable`.
    reliable: Vec<Vec<u8>>,
    /// Calls it's been dead (or bodiless) and not deploying, for `deploy`.
    dead_steps: u32,
    /// The class it deploys as (None: one by its seed, so a swarm's spread
    /// over them).
    kit: Option<u8>,
    /// Uses its class's ability now and then (`set_abilities`): inputs since
    /// it started, on its own phase.
    abilities: Option<u32>,
    /// Holds its ability's button (`hold_ability`, tests).
    ability_held: bool,
    /// Holds these tool keys (`gadget::TOOL_*`; `hold_tools`, tests), and
    /// aims this far up or down (`set_pitch`).
    tools_held: u8,
    pitch: i16,
}

/// A dead bot asks to deploy this long after it fell (steps), and again
/// this often while refused.
const RELEASE_STEPS: u32 = 60;
const ASK_AGAIN: u32 = 90;

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
    /// Walks the world's lane `lane` (from its far end if `backward`),
    /// sprinting, then stands (`BotBrain::lane_result`).
    Lane { lane: u16, backward: bool },
}

impl BotBrain {
    /// A bot that only counts entities until `enable_tracking`.
    pub fn new(seed: u64) -> Self {
        Self::with_config(seed, ClientConfig { track_entities: false, ..Default::default() })
    }

    pub fn with_config(seed: u64, cfg: ClientConfig) -> Self {
        Self {
            core: ClientCore::new(cfg),
            ai: Wander::new(seed),
            trigger: false,
            fighter: None,
            ads: false,
            reload_once: false,
            run_lanes: false,
            campaign: None,
            seed,
            reliable: Vec::new(),
            dead_steps: 0,
            kit: None,
            abilities: None,
            ability_held: false,
            tools_held: 0,
            pitch: 0,
        }
    }

    /// Deploys as class `kit` (`class`).
    pub fn set_kit(&mut self, kit: u8) {
        self.kit = Some(kit);
    }

    /// The next message for the reliable channel, if any.
    pub fn take_reliable(&mut self) -> Option<Vec<u8>> {
        (!self.reliable.is_empty()).then(|| self.reliable.remove(0))
    }

    /// Deploys (`msg::RULE_DEPLOY`): dead or not yet deployed, and not on
    /// its way, it asks a couple of seconds after it fell, at the spawn its
    /// faction holds that's quickest to reach from where it fell (by
    /// lattice transit); joining, at a front (a spawn linked to a site its
    /// faction doesn't hold), as a player picking a fight would.
    fn deploy(&mut self) {
        let core = &self.core;
        if !core.deploys() || !core.is_dead() || core.shell.deploying.is_some() {
            self.dead_steps = 0;
            return;
        }
        self.dead_steps += 1;
        if self.dead_steps < RELEASE_STEPS || !(self.dead_steps - RELEASE_STEPS).is_multiple_of(ASK_AGAIN) {
            return;
        }
        let Some(t) = core.territory() else { return };
        let (f, step) = (core.faction(), core.server_step());
        // (Where the graph has a way out of the spawn room: a bot can't find
        // its own.)
        let nav = core.world().map(|w| (Nav::shared(w), w.clone()));
        let mut options: Vec<(u32, usize)> = (0..t.len())
            .filter(|&i| nav.as_ref().is_none_or(|(n, w)| n.has_way_out(w, i)))
            .filter_map(|i| core.spawn_option(i).ok().map(|s| (s, i)))
            .collect();
        if core.shell.died_in.is_none() {
            let fronts: Vec<(u32, usize)> = options.iter().copied().filter(|&(_, i)| t.links(i).iter().any(|&j| t.sites[j as usize].owner_at(step) != f)).collect();
            if !fronts.is_empty() {
                options = fronts;
            }
        } else if let Some(best) = options.iter().map(|o| o.0).min() {
            options.retain(|o| o.0 == best);
        }
        if options.is_empty() {
            return;
        }
        let site = options[(self.ai.rng.next_u64() % options.len() as u64) as usize].1;
        // A class by its seed (or `set_kit`), and one of its weapons by its seed.
        let kit = self.kit.unwrap_or((self.seed % lattice_game::class::CLASSES as u64) as u8);
        // An Exo is one every 3 minutes: refused one, it goes as an assault.
        let kit = if self.core.shell.refused.is_some_and(|r| r.1 == lattice_game::territory::REFUSED_EXO) { lattice_game::class::ASSAULT } else { kit };
        let arms = lattice_game::class::WEAPONS_OF[lattice_game::class::valid(kit) as usize];
        let arms = arms[(self.seed / 7 % arms.len() as u64) as usize];
        if let Some(m) = self.core.deploy(site as u16, kit, arms) {
            self.reliable.push(m);
        }
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

    /// Presses reload once, with its next input.
    pub fn reload(&mut self) {
        self.reload_once = true;
    }

    /// Where it walks (and, held or with the trigger, faces and fires), in
    /// radians from east.
    pub fn set_heading(&mut self, heading: f32) {
        self.ai.heading = heading;
    }

    pub fn set_moves(&mut self, m: Moves) {
        self.ai.moves = m;
    }

    /// Walks a lane once welcomed: the one whose end the server spawned it
    /// at (`lattice-server --spawn lanes`).
    pub fn set_run_lanes(&mut self, on: bool) {
        self.run_lanes = on;
    }

    /// Uses its class's ability now and then (U6): jets for most of a second
    /// every 10 s, the rest pressed every 15 s (on their own phase each).
    pub fn set_abilities(&mut self, on: bool) {
        self.abilities = on.then_some((self.seed % 900) as u32);
    }

    /// Holds its ability's button down (or lets it go) from its next input.
    pub fn hold_ability(&mut self, on: bool) {
        self.ability_held = on;
    }

    /// Holds these tool keys (`gadget::TOOL_*`) from its next input (0 lets go).
    pub fn hold_tools(&mut self, tools: u8) {
        self.tools_held = tools;
    }

    /// Aims this far up (or down: negative; an input's pitch) from its next input.
    pub fn set_pitch(&mut self, pitch: i16) {
        self.pitch = pitch;
    }

    /// Takes sites (K3, with `lattice-server --spawn sites`): goes to the
    /// nearest few its faction may attack or must defend, by the world's
    /// routes (`lattice_game::nav`), and holds a point there.
    pub fn set_capture(&mut self, on: bool) {
        self.campaign = on.then(|| Campaign::new(self.seed));
    }

    /// The site it's in now (within its half size and 20 m), alive.
    pub fn site_now(&self) -> Option<usize> {
        let world = self.core.world()?;
        if self.core.welcome().is_none() || self.core.is_dead() {
            return None;
        }
        let s = self.core.predicted();
        world.sites().iter().position(|site| {
            let (dx, dy) = (site.center[0] - s.pos[0], site.center[1] - s.pos[1]);
            dx * dx + dy * dy <= (site.half + 20.0) * (site.half + 20.0)
        })
    }

    /// The site it's going to take or keep, if it takes sites.
    pub fn objective(&self) -> Option<usize> {
        self.campaign.as_ref().and_then(|c| c.site)
    }

    /// The lane it walks, which way, and how long it took (s) once there.
    pub fn lane_result(&self) -> Option<(u16, bool, Option<f32>)> {
        match self.ai.moves {
            Moves::Lane { lane, backward } => Some((lane, backward, self.ai.lane_secs)),
            _ => None,
        }
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
    /// aim down sights while they have a target, as players do (some of them
    /// crouched), and top up their magazine between fights; trigger bots
    /// spray from the hip.
    pub fn tick_inputs(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.deploy();
        if let Some(f) = &mut self.fighter {
            f.tick(&mut self.core, now);
        } else if self.trigger {
            let yaw = (self.ai.heading.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16;
            self.core.fire(now, yaw, self.pitch, self.ads);
        }
        let engaged = self.fighter.as_ref().is_some_and(|f| f.target.is_some());
        let ads = self.ads || self.fighter.as_ref().is_some_and(|f| f.cfg.ads && engaged);
        let crouch = self.fighter.as_ref().is_some_and(|f| f.crouches && engaged);
        let ammo = self.core.ammo();
        let reload = self.fighter.is_some() && !engaged && ammo.reload == 0 && ammo.mag < lattice_game::weapon::MAGAZINE / 3;
        let mut reload_once = std::mem::take(&mut self.reload_once);
        if self.run_lanes && self.ai.moves == Moves::Wander {
            if let (Some(w), Some(world)) = (self.core.welcome(), self.core.world()) {
                // The lane we were spawned at one end of, its other end our anchor.
                let near = |p: [f32; 3], at: [f32; 2]| (p[0] - at[0]).abs() < 0.5 && (p[1] - at[1]).abs() < 0.5;
                let found = world.lanes().iter().enumerate().filter(|(_, l)| l.a != l.b).find_map(|(i, l)| {
                    [false, true].into_iter().find(|&back| near(l.waypoint(0, back), w.spawn) && near(l.waypoint(0, !back), w.anchor)).map(|back| (i, back))
                });
                if let Some((lane, backward)) = found {
                    self.ai.moves = Moves::Lane { lane: lane as u16, backward };
                    (self.ai.lane_next, self.ai.lane_from) = (1, self.ai.steps);
                }
            }
        }
        let world = self.core.world().cloned();
        if let (Some(c), Some(world), Some(t)) = (&mut self.campaign, &world, self.core.territory()) {
            c.plan(world, t, self.core.predicted(), self.core.faction(), self.core.step(), self.ai.steps);
        }
        let ai = &mut self.ai;
        let campaign = &mut self.campaign;
        let (abilities, jets, held) = (&mut self.abilities, self.core.kit() == lattice_game::class::SKIRMISHER, self.ability_held);
        let (tools_held, pitch) = (self.tools_held, self.pitch);
        let out = self.core.step_inputs(now, |s, w| {
            let mut input = match campaign.as_mut().filter(|c| c.site.is_some()) {
                Some(c) => {
                    ai.steps += 1;
                    c.steer(s, ai.steps)
                }
                None => ai.think(s, w, world.as_deref()),
            };
            if ads {
                input.buttons = (input.buttons | BUTTON_ADS) & !BUTTON_SPRINT;
            }
            if crouch {
                input.buttons = (input.buttons | BUTTON_CROUCH) & !(BUTTON_SPRINT | BUTTON_JUMP);
            }
            if reload || reload_once {
                input.buttons |= BUTTON_RELOAD;
                reload_once = false;
            }
            if held {
                input.buttons |= lattice_game::ability::BUTTON_ABILITY;
            }
            input.tools |= tools_held;
            if pitch != 0 {
                input.pitch = pitch;
            }
            if let Some(t) = abilities.as_mut() {
                *t += 1;
                let (period, held) = if jets { (300, 24) } else { (450, 2) };
                if *t % period < held {
                    input.buttons |= lattice_game::ability::BUTTON_ABILITY;
                }
                // Its tools too: the first every 20 s, the second every 37,
                // and the use key held (a medic revives whoever it faces).
                use lattice_game::gadget::{TOOL_ONE, TOOL_TWO, TOOL_USE};
                input.tools |= TOOL_USE | if *t % 600 < 2 { TOOL_ONE } else { 0 } | if *t % 1110 == 555 { TOOL_TWO } else { 0 };
            }
            input
        });
        // No input made this tick: the press waits for the next.
        self.reload_once = reload_once;
        out
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
    /// Walking a lane: the next waypoint, the step it started at, the time
    /// it took once at its end.
    lane_next: usize,
    lane_from: u32,
    lane_secs: Option<f32>,
}

impl Wander {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        Self { heading: rng.range(0.0, std::f32::consts::TAU), rng, sprint: false, moves: Moves::Wander, steps: 0, lane_next: 1, lane_from: 0, lane_secs: None }
    }

    fn think(&mut self, s: &MoveState, w: &Welcome, world: Option<&lattice_game::world::World>) -> Input {
        self.steps += 1;
        match self.moves {
            Moves::Wander => {}
            Moves::Lane { lane, backward } => {
                let Some(l) = world.and_then(|w| w.lanes().get(lane as usize)) else { return Input::default() };
                return match l.steer([s.pos[0], s.pos[1], s.z], &mut self.lane_next, backward) {
                    Some((x, y)) => {
                        let yaw = ((y as f32).atan2(x as f32).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16;
                        Input { move_x: x, move_y: y, yaw, pitch: 0, buttons: BUTTON_SPRINT, tools: 0 }
                    }
                    None => {
                        self.lane_secs.get_or_insert((self.steps - self.lane_from) as f32 / crate::movement::TICK_HZ as f32);
                        Input::default()
                    }
                };
            }
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
            tools: 0,
        }
    }
}

/// Taking sites: the objective (a site its faction may attack, or one of
/// its own under attack), a point of it, and the way there: down the
/// world's graph (`Nav`) node by node, then straight to the point, where it
/// stands. A new objective when that one's taken or kept, and now and then.
struct Campaign {
    rng: Rng,
    nav: Option<Arc<Nav>>,
    site: Option<usize>,
    point: [f32; 3],
    /// The node it's heading for (None: find the nearest).
    next: Option<u32>,
    /// It's past the graph's end: straight to the point.
    straight: bool,
    chosen: u32,
    last: [f32; 3],
    /// Unsticking: where it was a while ago, and steps left sidestepping.
    mark: ([f32; 3], u32),
    dodge: (u32, f32),
}

/// How often a bot takes stock of the fronts, and how long it stays on one
/// at most before it may look again (steps).
const REPLAN: u32 = 60;
const STAY: u32 = 30 * 180;

impl Campaign {
    fn new(seed: u64) -> Self {
        Self { rng: Rng::new(seed ^ 0xCA97), nav: None, site: None, point: [0.0; 3], next: None, straight: false, chosen: 0, last: [0.0; 3], mark: ([0.0; 3], 0), dodge: (0, 0.0) }
    }

    /// Whether faction `f` would go to site `s`: one it may attack, or one
    /// of its own someone's taking.
    fn wanted(t: &Territory, s: usize, f: u8, step: u32) -> bool {
        let st = &t.sites[s];
        t.attackable(s, f, step) || (st.owner_at(step) == f && st.attacker != territory::NONE && st.attacker != f)
    }

    /// Takes stock: keeps its objective while it's still wanted (for a few
    /// minutes), else picks among the nearest three by the graph (its own
    /// under attack counted nearer).
    fn plan(&mut self, world: &Arc<lattice_game::world::World>, t: &Territory, me: MoveState, f: u8, step: u32, steps: u32) {
        let nav = self.nav.get_or_insert_with(|| Nav::shared(world)).clone();
        let pos = [me.pos[0], me.pos[1], me.z];
        if steps.is_multiple_of(60) && std::env::var_os("LATTICE_BOT_TRACE").is_some() {
            let next = self.next.map(|n| nav.node(n));
            eprintln!("bot f{f} at ({:.0}, {:.0}, {:.1}) site {:?} point {:?} next {next:?} straight {} cost {:?}", pos[0], pos[1], pos[2], self.site, self.point, self.straight, self.site.zip(self.next).map(|(s, n)| nav.cost(s, n)));
        }
        // Respawned, or thrown far: find the way again.
        let moved = (pos[0] - self.last[0]).abs() + (pos[1] - self.last[1]).abs();
        if moved > 30.0 {
            (self.next, self.straight) = (None, false);
        }
        self.last = pos;
        // Still wanted, and not there for long: keep it. Else look again,
        // every couple of seconds.
        if self.site.is_some_and(|s| Self::wanted(t, s, f, step)) && steps.wrapping_sub(self.chosen) < STAY {
            return;
        }
        if !steps.is_multiple_of(REPLAN) {
            return;
        }
        let mut options: Vec<(f32, usize)> = (0..t.len())
            .filter(|&s| Self::wanted(t, s, f, step))
            .filter_map(|s| {
                let n = nav.nearest(pos, s)?;
                let ours = t.sites[s].owner_at(step) == f;
                Some((nav.cost(s, n) * if ours { 0.6 } else { 1.0 }, s))
            })
            .collect();
        options.sort_by(|a, b| a.0.total_cmp(&b.0));
        let pick = match self.rng.unit() {
            u if u < 0.5 => 0,
            u if u < 0.8 => 1,
            _ => 2,
        };
        let Some(&(_, s)) = options.get(pick.min(options.len().saturating_sub(1))) else {
            self.site = None;
            return;
        };
        if self.site != Some(s) {
            let pts = t.points(s);
            self.point = pts[(self.rng.next_u64() % pts.len().max(1) as u64) as usize];
            (self.next, self.straight) = (None, false);
        }
        self.site = Some(s);
        self.chosen = steps;
    }

    /// One input toward the objective.
    fn steer(&mut self, s: &MoveState, steps: u32) -> Input {
        let (Some(site), Some(nav)) = (self.site, self.nav.clone()) else { return Input::default() };
        let pos = [s.pos[0], s.pos[1], s.z];
        let flat = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]) * (a[0] - b[0]) + (a[1] - b[1]) * (a[1] - b[1])).sqrt();
        let p = self.point;
        // On the point: stand there (fighters fight).
        if flat(pos, p) < 3.0 && (pos[2] - p[2]).abs() < 3.0 {
            return Input::default();
        }
        // Stuck (not moved in 3 s): sidestep a little, jumping, and look for the way again.
        if steps.wrapping_sub(self.mark.1) >= 90 {
            if flat(pos, self.mark.0) < 1.0 && s.grounded {
                self.dodge = (30, self.rng.range(0.0, std::f32::consts::TAU));
                (self.next, self.straight) = (None, false);
            }
            self.mark = (pos, steps);
        }
        if self.dodge.0 > 0 {
            self.dodge.0 -= 1;
            let (sin, cos) = self.dodge.1.sin_cos();
            return Input { move_x: (cos * 127.0) as i8, move_y: (sin * 127.0) as i8, buttons: if self.dodge.0 == 25 { BUTTON_JUMP } else { 0 }, ..Input::default() };
        }
        if self.next.is_none() && !self.straight {
            self.next = nav.nearest(pos, site);
            self.straight = self.next.is_none();
        }
        let target = match self.next {
            Some(n) => {
                let q = nav.node(n);
                if flat(pos, q) < 1.5 && (pos[2] - q[2]).abs() < 2.5 {
                    self.next = nav.toward(site, n);
                    if self.next.is_none() {
                        self.straight = true;
                    }
                    self.next.map_or(p, |m| nav.node(m))
                } else if flat(pos, q) < 1.5 {
                    // Right under it or over it: carried there (a lift, a drop shaft).
                    return Input::default();
                } else {
                    q
                }
            }
            None => p,
        };
        let (dx, dy) = (target[0] - pos[0], target[1] - pos[1]);
        let d = (dx * dx + dy * dy).sqrt().max(1e-3);
        let heading = dy.atan2(dx);
        Input {
            move_x: (dx / d * 127.0) as i8,
            move_y: (dy / d * 127.0) as i8,
            yaw: (heading.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16,
            pitch: 0,
            buttons: BUTTON_SPRINT,
            tools: 0,
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
    /// Crouches while it fights (two bots in five).
    crouches: bool,
    target: Option<u16>,
    retarget_in: u32,
    burst: u32,
    pause: u32,
}

/// Where to aim at `target` as `core` draws it at render step `r`: at its
/// body (or head), standing or crouched, leading for the flight time and the
/// drop at its drawn velocity, plus `extra_lead` steps. The aim as the wire
/// carries it.
pub fn aim_at(core: &ClientCore, target: u16, r: f64, head: bool, extra_lead: f32) -> Option<(u16, i16)> {
    use lattice_game::hit::{BODY_HIGH, BODY_LOW, CROUCH_BODY_HIGH, CROUCH_HEAD_AT, HEAD_AT};
    use lattice_game::weapon::{eye_height, GRAVITY};
    let ents = core.entities()?;
    let (st, was) = (ents.render_one(target, r)?, ents.render_one(target, r - 0.5)?);
    if st.dead {
        return None;
    }
    let tick_hz = crate::movement::TICK_HZ as f32;
    let vel = [0, 1].map(|k| (st.pos[k] - was.pos[k]) * 2.0 * tick_hz);
    let me = core.predicted();
    let eye = [me.pos[0], me.pos[1], me.z + eye_height(&me)];
    let at = st.pos[2]
        + match (head, st.crouched) {
            (true, false) => HEAD_AT,
            (true, true) => CROUCH_HEAD_AT,
            (false, false) => (BODY_LOW + BODY_HIGH) / 2.0,
            (false, true) => (BODY_LOW + CROUCH_BODY_HIGH) / 2.0,
        };
    let flight = ((st.pos[0] - eye[0]).powi(2) + (st.pos[1] - eye[1]).powi(2)).sqrt() / lattice_game::weapon::of(core.weapon()).speed;
    let lead = flight + extra_lead / tick_hz;
    let p = [st.pos[0] + vel[0] * lead, st.pos[1] + vel[1] * lead, at + 0.5 * GRAVITY * flight * flight];
    let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
    let yaw = (d[1].atan2(d[0]).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0) as u32 as u16;
    let pitch = (d[2].atan2(d[0].hypot(d[1])) / std::f32::consts::FRAC_PI_2 * 32767.0) as i16;
    Some((yaw, pitch))
}

impl Fighter {
    fn new(cfg: FightConfig, seed: u64) -> Self {
        let mut rng = Rng::new(seed ^ 0x000F_1647);
        let crouches = rng.chance(0.4);
        Self { cfg, rng, crouches, target: None, retarget_in: 0, burst: 0, pause: 0 }
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
        let eye = [s.pos[0], s.pos[1], s.z + lattice_game::weapon::eye_height(&s)];
        let mut near: Vec<(f32, u16, [f32; 3])> = ents
            .ids()
            .filter(|&e| lattice_game::faction::faction(e) != side)
            .filter_map(|e| ents.render_one(e, r).filter(|st| !st.dead).map(|st| (e, [st.pos[0], st.pos[1], st.pos[2] + lattice_game::hit::chest(st.crouched)])))
            .map(|(e, p)| (((p[0] - eye[0]).powi(2) + (p[1] - eye[1]).powi(2)).sqrt(), e, p))
            .filter(|&(d, ..)| d <= self.cfg.range)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        // (Shields as at the start; the clockwork as at its step.)
        let who = lattice_game::movement::Mover { faction: side, owners: &[], live: lattice_game::clock::Live::at(core.step()) };
        near.into_iter().take(3).find(|&(_, _, p)| lattice_game::hit::line_clear(world, eye, p, who)).map(|(_, e, _)| e)
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
