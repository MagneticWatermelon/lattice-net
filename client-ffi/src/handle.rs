//! What one `LtSession` handle holds: the `Session`, this frame's clock, the
//! tracers, and buffers reused frame to frame. Plain Rust (the C API in
//! `api` is a thin layer over it), so it's tested directly too.
//!
//! **Time** is the plugin's own monotonic clock: `begin_frame` reads it once
//! per frame and every call in that frame uses the same `now`. The engine's
//! time never reaches the netcode.
//!
//! **On the net thread** (`set_threaded`), the thread polls, runs the input
//! clock with the latest intent and flushes (`net_tick`); `poll` and `flush`
//! do nothing and `inputs` only keeps the intent. The caller's calls that read
//! the clocks (`fire`, `own`, `render`, `combat`) then take the time as they
//! run, under the session's lock: the thread may have run the input and
//! render clocks past the frame's start, and a call made at an earlier time
//! would be held at theirs (a judder of up to a thread tick, in our own
//! camera most of all). Lock order keeps every call's time increasing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_client_core::aim;
use lattice_client_core::distant::AmbientShot;
use lattice_client_core::tracers::{Fired, Tracers};
use lattice_client_core::{ClientConfig, How};
use lattice_game::events::{Event, SeenShot};
use lattice_game::faction::faction;
use lattice_game::movement::{Input, BUTTON_ADS, BUTTON_CROUCH, BUTTON_JUMP, BUTTON_RELOAD, BUTTON_SPRINT, TICK_HZ};
use lattice_game::weapon::eye_height;
use lattice_game::world::World;
use lattice_net::{ClientState, ConnectToken};

use crate::abi::*;
use crate::session::{Session, MAX_FRAME};

/// Events kept for the caller at most, if it never drains them.
const MAX_EVENTS: usize = 1024;
/// Effects kept for the caller at most per frame.
const MAX_FX: usize = 8192;
/// A sample moving what's on screen by more than this is a pop, in mm.
const POP_MM: u16 = 50;

/// Who's playing, and where: for the report.
#[derive(Debug, Clone, Default)]
pub struct Player {
    pub server: String,
    pub user: u64,
    pub name: String,
}

pub struct Handle {
    pub session: Session,
    pub player: Player,
    start: Instant,
    now: Instant,
    /// When `combat` last flew the tracers.
    combat_at: Option<Instant>,
    tracers: Tracers,
    /// Where others' muzzles are, from their eyes (right, up, forward): the
    /// caller sets it from its soldiers' rifles.
    others_muzzle: [f32; 3],
    events: Vec<LtEvent>,
    fx: Vec<LtFx>,
    news: Vec<Event>,
    seen: Vec<SeenShot>,
    ambient: Vec<AmbientShot>,
    drained: [Vec<u16>; 3],
    drawn: [u32; 3],
    pops: [u64; 3],
    pop_max: [u32; 3],
    /// Run by the net thread (see the module's doc).
    threaded: bool,
    /// The latest intent, as an input: the net thread makes inputs from it.
    intent: Input,
}

impl Handle {
    pub fn connect(server: SocketAddr, token: ConnectToken, cfg: ClientConfig, player: Player) -> std::io::Result<Self> {
        let now = Instant::now();
        let mut session = Session::connect(server, token, cfg, now)?;
        session.core.keep_news(true);
        Ok(Self {
            session,
            player,
            start: now,
            now,
            combat_at: None,
            tracers: Tracers::default(),
            others_muzzle: [0.18, -0.12, 0.55],
            events: Vec::new(),
            fx: Vec::new(),
            news: Vec::new(),
            seen: Vec::new(),
            ambient: Vec::new(),
            drained: Default::default(),
            drawn: [0; 3],
            pops: [0; 3],
            pop_max: [0; 3],
            threaded: false,
            intent: Input::default(),
        })
    }

    /// Hands the polling, the input clock and the flushing to a net thread
    /// (which calls `net_tick`), or back to the caller's frames.
    pub fn set_threaded(&mut self, on: bool) {
        self.threaded = on;
    }

    pub fn threaded(&self) -> bool {
        self.threaded
    }

    /// The net thread's work, every tick: receive and deliver, run the input
    /// clock with the latest intent (inputs carry any shot pulled since),
    /// send. `now` is read under the session's lock.
    pub fn net_tick(&mut self, now: Instant) -> std::io::Result<()> {
        let r = self.session.poll(now);
        self.session.send_inputs(now, self.intent);
        self.session.flush(now);
        r
    }

    /// The time for a call that reads the clocks: the frame's, or on the net
    /// thread, now (see the module's doc).
    fn clock(&mut self) -> Instant {
        if self.threaded {
            self.now = self.now.max(Instant::now());
        }
        self.now
    }

    /// Starts a frame at `now`: every call until the next one uses it.
    /// Returns the seconds since connecting.
    pub fn begin_frame(&mut self, now: Instant) -> f64 {
        self.now = now.max(self.now);
        self.secs()
    }

    /// Starts a frame `secs` after connecting (tests drive time this way).
    pub fn begin_frame_at(&mut self, secs: f64) -> f64 {
        self.begin_frame(self.start + Duration::from_secs_f64(secs.max(0.0)))
    }

    pub fn secs(&self) -> f64 {
        (self.now - self.start).as_secs_f64()
    }

    pub fn state(&self) -> i32 {
        match self.session.state() {
            ClientState::Connecting => STATE_CONNECTING,
            ClientState::Connected => STATE_CONNECTED,
            ClientState::Denied(_) => STATE_DENIED,
            ClientState::TimedOut => STATE_TIMED_OUT,
            ClientState::Disconnected => STATE_DISCONNECTED,
        }
    }

    /// Receives everything that arrived and delivers it (on the net thread,
    /// the thread does: this only reads the state).
    pub fn poll(&mut self) -> std::io::Result<i32> {
        if !self.threaded {
            self.session.poll(self.now)?;
        }
        Ok(self.state())
    }

    fn own_faction(&self) -> u8 {
        self.session.core.welcome().map_or(0, |w| faction(w.entity))
    }

    /// Pulls the trigger: a shot rides this frame's input if the rifle is
    /// ready, and its tracer leaves from the muzzle at once.
    pub fn fire(&mut self, f: &LtFire) -> Option<LtShot> {
        let now = self.clock();
        let faction = self.own_faction();
        let core = &mut self.session.core;
        let dir = core.fire(now, aim::yaw_u16(f.yaw), aim::pitch_i16(f.pitch), f.ads != 0)?;
        let feet = core.own_render(now);
        let eye = [feet[0], feet[1], feet[2] + eye_height(&core.predicted())];
        let step = core.last_shot_step();
        let fired = self.tracers.fire_own(eye, f.yaw, f.pitch, dir, f.muzzle.into(), step, faction);
        Some(LtShot { dir: dir.into(), muzzle: fired.origin.into(), eye: eye.into() })
    }

    /// This frame's intent to the input clock, and any batch out (on the net
    /// thread, kept for the thread's next inputs).
    pub fn inputs(&mut self, i: &LtIntent) {
        let clamp = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        let (yaw, pitch) = (if i.yaw.is_finite() { i.yaw } else { 0.0 }, if i.pitch.is_finite() { i.pitch } else { 0.0 });
        let (move_x, move_y) = aim::stick(yaw, clamp(i.forward), clamp(i.right));
        let buttons = i.buttons & (BUTTON_SPRINT | BUTTON_JUMP | BUTTON_ADS | BUTTON_CROUCH | BUTTON_RELOAD);
        let input = Input { move_x, move_y, yaw: aim::yaw_u16(yaw), pitch: aim::pitch_i16(pitch), buttons };
        if self.threaded {
            self.intent = input;
        } else {
            self.session.send_inputs(self.now, input);
        }
    }

    pub fn own(&mut self, ads: bool) -> LtOwn {
        let now = self.clock();
        let core = &self.session.core;
        let Some(w) = core.welcome() else { return LtOwn::default() };
        let p = core.predicted();
        let ghost = core.server_own().map_or([0.0; 3], |(s, _)| [s.pos[0], s.pos[1], s.z]);
        let ammo = core.ammo();
        LtOwn {
            pos: core.own_render(now).into(),
            vel: [p.vel[0], p.vel[1], p.vz].into(),
            server_pos: ghost.into(),
            cone_deg: core.cone(now, ads),
            seq: core.predicted_seq(),
            entity: w.entity,
            health: core.health(),
            dead: core.is_dead() as u8,
            grounded: p.grounded as u8,
            welcomed: 1,
            faction: faction(w.entity),
            life: core.life(),
            crouched: p.crouched as u8,
            mag: ammo.mag,
            reserve: ammo.reserve,
            reload: ammo.reload as f32 / TICK_HZ as f32,
        }
    }

    /// Draws every other player at this frame's render step into `out` (as
    /// many as fit), and returns how many there were. Once per frame: it
    /// also adapts the near render delay.
    pub fn render(&mut self, out: &mut [LtEntity]) -> usize {
        let now = self.clock();
        let (mut n, mut total, mut drawn) = (0, 0, [0u32; 3]);
        self.session.core.render(now, |id, st| {
            total += 1;
            drawn[st.tier as usize] += 1;
            if let Some(e) = out.get_mut(n) {
                let flags = if st.airborne { FLAG_AIRBORNE } else { 0 }
                    | if st.dead { FLAG_DEAD } else { 0 }
                    | if st.ads { FLAG_ADS } else { 0 }
                    | if st.crouched { FLAG_CROUCHED } else { 0 }
                    | if st.reloading { FLAG_RELOADING } else { 0 };
                *e = LtEntity {
                    id,
                    faction: faction(id),
                    tier: st.tier as u8,
                    how: match st.how {
                        How::Interpolated => 0,
                        How::Extrapolated => 1,
                        How::Held => 2,
                        How::New => 3,
                    },
                    flags,
                    health: st.health,
                    reserved: 0,
                    pos: st.pos.into(),
                    yaw: st.yaw,
                    pitch: st.pitch,
                    at: st.at,
                };
                n += 1;
            }
        });
        self.drawn = drawn;
        // Pops and update intervals pile up until drained: count the pops,
        // drop the rest (a bot keeps them; a game only needs the counts).
        self.session.core.drain_pops(&mut self.drained);
        for (t, pops) in self.drained.iter_mut().enumerate() {
            for &mm in pops.iter() {
                if mm > POP_MM {
                    self.pops[t] += 1;
                }
                self.pop_max[t] = self.pop_max[t].max(mm as u32);
            }
            pops.clear();
        }
        self.session.core.drain_intervals(&mut self.drained);
        self.drained.iter_mut().for_each(Vec::clear);
        total
    }

    /// The newest sample of every player (no delay, no smoothing): the
    /// server ghosts.
    pub fn ghosts(&self, out: &mut [LtEntity]) -> usize {
        let Some(ents) = self.session.core.entities() else { return 0 };
        let mut n = 0;
        for id in ents.ids() {
            let (Some(s), Some(e)) = (ents.newest(id), out.get_mut(n)) else { continue };
            *e = LtEntity {
                id,
                faction: faction(id),
                tier: s.tier as u8,
                how: 0,
                flags: if s.airborne { FLAG_AIRBORNE } else { 0 }
                    | if s.dead { FLAG_DEAD } else { 0 }
                    | if s.ads { FLAG_ADS } else { 0 }
                    | if s.crouched { FLAG_CROUCHED } else { 0 }
                    | if s.reloading { FLAG_RELOADING } else { 0 },
                health: s.health,
                reserved: 0,
                pos: s.pos.into(),
                yaw: s.yaw,
                pitch: s.pitch,
                at: s.step,
            };
            n += 1;
        }
        n
    }

    /// This frame's combat: the news (hit markers, damage taken, kills) to
    /// `events`; others' shots start their tracers when their shooters are
    /// drawn firing, distant fights' at once (each a `LT_FX_SHOT`); every
    /// tracer flies on to now, and those that hit something end in an
    /// `LT_FX_IMPACT`. Call it after `render`.
    pub fn combat(&mut self) {
        let now = self.clock();
        let dt = self.combat_at.map_or(0.0, |t| now.saturating_duration_since(t).min(MAX_FRAME).as_secs_f32());
        self.combat_at = Some(now);
        let secs = self.secs() as f32;
        let core = &mut self.session.core;
        core.drain_news(&mut self.news, &mut self.seen);
        for ev in self.news.drain(..) {
            if self.events.len() >= MAX_EVENTS {
                self.events.remove(0);
            }
            self.events.push(match ev {
                Event::Hit { target, damage, head, killed } => LtEvent {
                    kind: EVENT_HIT,
                    flags: if head { EVENT_HEAD } else { 0 } | if killed { EVENT_KILLED } else { 0 },
                    amount: damage,
                    dir: 0,
                    a: target,
                    b: 0,
                },
                Event::Hurt { from, amount, dir } => LtEvent { kind: EVENT_HURT, flags: 0, amount, dir, a: from, b: 0 },
                Event::Kill { killer, victim, head } => {
                    LtEvent { kind: EVENT_KILL, flags: if head { EVENT_HEAD } else { 0 }, amount: 0, dir: 0, a: killer, b: victim }
                }
            });
        }
        let own = core.welcome().map(|w| w.entity);
        self.tracers.on_shots(own, self.seen.drain(..), secs);
        core.drain_ambient(now, &mut self.ambient);
        let fx = &mut self.fx;
        self.tracers.on_ambient(&self.ambient, self.others_muzzle, |f| push_fx(fx, shot_fx(f)));
        self.ambient.clear();
        let Some(world) = core.world().map(Arc::clone) else { return };
        let r = core.render_clock().last_render();
        self.tracers.update(core.entities(), r, secs, &world, self.others_muzzle, |f| push_fx(fx, shot_fx(f)));
        self.tracers.fly(dt, &world, |t, hit| {
            push_fx(
                fx,
                LtFx {
                    kind: FX_IMPACT,
                    tracer: t.kind as u8,
                    faction: t.faction,
                    surface: hit.cover.unwrap_or(SURFACE_TERRAIN),
                    shooter: NOBODY,
                    reserved: 0,
                    pos: hit.pos.into(),
                    dir: hit.dir.into(),
                    normal: hit.normal.into(),
                },
            )
        });
    }

    /// Moves up to `out.len()` events (oldest first) out; returns how many.
    pub fn take_events(&mut self, out: &mut [LtEvent]) -> usize {
        let n = out.len().min(self.events.len());
        out[..n].copy_from_slice(&self.events[..n]);
        self.events.drain(..n);
        n
    }

    /// Moves up to `out.len()` effects (oldest first) out; returns how many.
    pub fn take_fx(&mut self, out: &mut [LtFx]) -> usize {
        let n = out.len().min(self.fx.len());
        out[..n].copy_from_slice(&self.fx[..n]);
        self.fx.drain(..n);
        n
    }

    /// Every tracer in flight, as streaks to draw (as many as fit); returns
    /// how many there are.
    pub fn tracers(&self, out: &mut [LtTracer]) -> usize {
        for (o, t) in out.iter_mut().zip(&self.tracers.flying) {
            let (tail, head) = t.streak();
            *o = LtTracer { head: head.into(), tail: tail.into(), kind: t.kind as u8, faction: t.faction, reserved: 0, age: t.age };
        }
        self.tracers.flying.len()
    }

    pub fn set_others_muzzle(&mut self, offset: [f32; 3]) {
        self.others_muzzle = offset;
    }

    /// Packets out (on the net thread, the thread sends: this does nothing).
    pub fn flush(&mut self) {
        if !self.threaded {
            self.session.flush(self.now);
        }
    }

    pub fn stats(&self) -> LtStats {
        let core = &self.session.core;
        let st = &core.stats;
        let t = self.session.transport().stats().cloned().unwrap_or_default();
        let clock = core.render_clock();
        let ents = core.entities();
        let mut frames = [0u64; 12];
        let mut streaks = [0u64; 3];
        if let Some(e) = ents {
            for tier in 0..3 {
                for how in 0..4 {
                    frames[tier * 4 + how] = e.smooth.frames[tier][how];
                }
            }
            streaks = e.smooth.streaks;
        }
        let ms = |steps: f64| (steps * 1000.0 / 30.0) as f32;
        LtStats {
            state: self.state(),
            rtt_ms: t.rtt_ms,
            rtt_min_ms: t.rtt_min_ms,
            rtt_max_ms: t.rtt_max_ms,
            loss: t.loss,
            wait_ms: (st.wait_sum_ms / st.wait_samples.max(1) as f64) as f32,
            near_delay_ms: ms(core.near_delay()),
            render_delay_ms: clock.newest_at(self.now).zip(clock.last_render()).map_or(0.0, |(n, r)| ms(n - r)),
            mid_lag_ms: ms(ents.map_or(0.0, |e| e.mid_lag())),
            correction_error_max: st.correction_error_max,
            push_error_max: st.push_error_max,
            own_offset_max: st.own_offset_max,
            server_step: core.server_own().map_or(0, |(_, step)| step),
            entities: ents.map_or(0, |e| e.len() as u32),
            drawn: self.drawn,
            pace: st.pace,
            level: st.level,
            client_level: st.client_level,
            bytes_sent: t.bytes_sent,
            bytes_received: t.bytes_received,
            packets_sent: t.packets_sent,
            packets_received: t.packets_received,
            packets_lost: t.packets_lost,
            snapshots: st.snapshots,
            corrections: st.corrections,
            push_corrections: st.push_corrections,
            ammo_corrections: st.ammo_corrections,
            resyncs: st.resyncs,
            shots_dropped: st.shots_dropped,
            life_events: st.life_events,
            shots: st.shots,
            hits_confirmed: st.hits_confirmed,
            kills_confirmed: st.kills_confirmed,
            hurts: st.hurts,
            damage_taken: st.damage_taken,
            kills_heard: st.kills_heard,
            shots_seen: st.shots_seen,
            activity_cells: st.activity_cells,
            activity_shots: st.activity_shots,
            ambient: st.ambient,
            clock_snaps: clock.snaps,
            near_decode_errors: st.near_decode_errors,
            bad_messages: st.bad_messages,
            frames,
            streaks,
            pops: self.pops,
            pop_max_mm: self.pop_max,
        }
    }

    /// The report: who, where, how long, then the connection's summary.
    pub fn summary(&self) -> String {
        let who = if self.player.name.is_empty() { format!("user {}", self.player.user) } else { format!("{} (user {})", self.player.name, self.player.user) };
        format!("{who} on {} | connected {:.0} s\n{}", self.player.server, self.secs(), self.session.summary())
    }

    /// Disconnects at once (the server's session log closes the session).
    pub fn close(&mut self) {
        if self.session.state() == ClientState::Connected {
            self.session.disconnect(Instant::now().max(self.now));
        }
    }

    /// The world, once welcomed.
    pub fn world(&self) -> Option<Arc<World>> {
        self.session.core.world().map(Arc::clone)
    }

    pub fn tracer_count(&self) -> usize {
        self.tracers.flying.len()
    }
}

/// A shooter drawn firing, as an effect.
fn shot_fx(f: Fired) -> LtFx {
    LtFx {
        kind: FX_SHOT,
        tracer: f.kind as u8,
        faction: f.faction,
        surface: 0,
        shooter: f.shooter.unwrap_or(NOBODY),
        reserved: 0,
        pos: f.origin.into(),
        dir: f.dir.into(),
        normal: LtVec3::default(),
    }
}

fn push_fx(fx: &mut Vec<LtFx>, e: LtFx) {
    if fx.len() < MAX_FX {
        fx.push(e);
    }
}
