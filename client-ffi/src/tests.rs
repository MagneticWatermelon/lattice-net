//! The C API end to end: two sessions, opened and driven only through
//! `lt_*` calls in a 120 Hz frame loop (GDD §13's order), join an
//! in-process server over loopback UDP, see each other, and one shoots the
//! other. Prediction must stay exact through it all.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_sim::server::{Counters, SimConfig, SpawnMode};

use crate::abi::*;
use crate::api::*;
use crate::session::tests::LoopServer;

/// The tests that run a frame loop in real time hold this to read; one that
/// loads the machine (building Kiln) holds it to write, so it never runs
/// beside them (a stalled frame loop resyncs, and they'd fail for it).
static QUIET: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn quiet() -> std::sync::RwLockReadGuard<'static, ()> {
    QUIET.read().unwrap_or_else(|e| e.into_inner())
}

fn hex(key: &[u8]) -> CString {
    CString::new(key.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap()
}

fn last_error() -> String {
    let mut buf = [0u8; 512];
    let n = unsafe { lt_last_error(buf.as_mut_ptr(), buf.len() as i32) } as usize;
    String::from_utf8_lossy(&buf[..n.min(buf.len() - 1)]).into_owned()
}

/// One client's frame state, as an engine would keep it.
struct Client {
    s: *mut LtSession,
    own: LtOwn,
    ents: Vec<LtEntity>,
    drawn: usize,
    events: Vec<LtEvent>,
    shots_fx: Vec<LtFx>,
    impacts: Vec<LtFx>,
    most_tracers: i32,
    fired: u32,
    /// Held every frame (`LT_BUTTON_*`), besides the sights while firing.
    buttons: u8,
}

impl Client {
    fn connect(addr: &str, user: u64, key: &CString, server_id: u64) -> Self {
        let addr = CString::new(addr).unwrap();
        let cfg = LtConfig::default();
        let s = unsafe { lt_connect_dev(addr.as_ptr().cast(), user, key.as_ptr().cast(), server_id, &cfg) };
        assert!(!s.is_null(), "{}", last_error());
        Self {
            s,
            own: LtOwn::default(),
            ents: vec![LtEntity::default(); 64],
            drawn: 0,
            events: Vec::new(),
            shots_fx: Vec::new(),
            impacts: Vec::new(),
            most_tracers: 0,
            fired: 0,
            buttons: 0,
        }
    }

    /// One frame: `aim` at a point (else look east), `fire` if asked.
    fn frame(&mut self, aim: Option<[f32; 3]>, fire: bool) {
        unsafe {
            lt_begin_frame(self.s);
            lt_poll(self.s);
            let eye = [self.own.pos.x, self.own.pos.y, self.own.pos.z + lattice_game::weapon::EYE_HEIGHT];
            let (yaw, pitch) = match aim {
                Some(t) => {
                    let d = [t[0] - eye[0], t[1] - eye[1], t[2] - eye[2]];
                    (d[1].atan2(d[0]), (d[2] / (d[0] * d[0] + d[1] * d[1]).sqrt()).atan())
                }
                None => (0.0, 0.0),
            };
            if fire {
                let f = LtFire { yaw, pitch, ads: 1, muzzle: LtVec3 { x: 0.2, y: -0.15, z: 0.6 } };
                let mut shot = LtShot::default();
                if lt_fire(self.s, &f, &mut shot) == 1 {
                    self.fired += 1;
                    let len = (shot.dir.x.powi(2) + shot.dir.y.powi(2) + shot.dir.z.powi(2)).sqrt();
                    assert!((len - 1.0).abs() < 1e-4, "a unit direction");
                }
            }
            let intent = LtIntent { yaw, pitch, forward: 0.0, right: 0.0, buttons: self.buttons | if fire { LT_BUTTON_ADS } else { 0 } };
            lt_inputs(self.s, &intent);
            lt_own(self.s, fire as u8, &mut self.own);
            let n = lt_render(self.s, self.ents.as_mut_ptr(), self.ents.len() as i32);
            self.drawn = (n as usize).min(self.ents.len());
            lt_combat(self.s);
            let mut ev = [LtEvent::default(); 16];
            let n = lt_events(self.s, ev.as_mut_ptr(), ev.len() as i32);
            self.events.extend_from_slice(&ev[..n as usize]);
            let mut fx = [LtFx::default(); 64];
            let n = lt_fx(self.s, fx.as_mut_ptr(), fx.len() as i32);
            for f in &fx[..n as usize] {
                if f.kind == FX_SHOT {
                    self.shots_fx.push(*f);
                } else {
                    self.impacts.push(*f);
                }
            }
            let mut tr = [LtTracer::default(); 64];
            self.most_tracers = self.most_tracers.max(lt_tracers(self.s, tr.as_mut_ptr(), tr.len() as i32));
            lt_flush(self.s);
        }
    }

    fn stats(&self) -> LtStats {
        let mut st = LtStats::default();
        unsafe { lt_stats(self.s, &mut st) };
        st
    }
}

const LT_BUTTON_ADS: u8 = lattice_game::movement::BUTTON_ADS;
const LT_BUTTON_CROUCH: u8 = lattice_game::movement::BUTTON_CROUCH;
const LT_BUTTON_ABILITY: u8 = lattice_game::ability::BUTTON_ABILITY;

#[test]
fn two_sessions_join_see_each_other_and_fight_through_the_c_api() {
    let _quiet = quiet();
    assert_eq!(unsafe { lt_version() }, VERSION);
    assert_eq!(unsafe { lt_layout_hash() }, layout_hash(FNS));

    let start = Instant::now();
    // Two players in one 20 m disk, on different sides (no squads: factions
    // alternate by spawn).
    let mut cfg = SimConfig { spawn: SpawnMode::Disk(20.0), ..Default::default() };
    cfg.interest.squad_size = 0;
    cfg.immortal = true; // hits land and are confirmed, nobody dies mid-test
    let mut server = LoopServer::with(cfg, start);
    let id = server.sim.config().identity.clone();
    let key = hex(&id.token_key);
    let addr = server.addr().to_string();
    let mut a = Client::connect(&addr, 1, &key, id.server_id);
    let mut b = Client::connect(&addr, 2, &key, id.server_id);

    // 120 Hz frames for 6 s of wall time; A shoots at B (its drawn chest)
    // from 2 s on.
    let frame = Duration::from_micros(8333);
    let mut next = start;
    while next < start + Duration::from_secs(6) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let shooting = next > start + Duration::from_secs(2);
        let target = a.ents[..a.drawn].iter().find(|e| e.id == b.own.entity && b.own.welcomed == 1).map(|e| [e.pos.x, e.pos.y, e.pos.z + 1.0]);
        a.frame(target, shooting && target.is_some());
        b.frame(None, false);
        server.run(Instant::now());
        next += frame;
    }

    for (name, c) in [("A", &a), ("B", &b)] {
        let st = c.stats();
        assert_eq!(st.state, STATE_CONNECTED, "{name} connected");
        assert_eq!(c.own.welcomed, 1, "{name} welcomed");
        assert!(st.snapshots > 100, "{name}: {} snapshots", st.snapshots);
        assert_eq!(st.corrections, 0, "{name}: prediction is exact through the C API");
        assert_eq!(st.resyncs, 0, "{name}: no resyncs");
        assert_eq!(c.drawn, 1, "{name} draws the other");
        assert_eq!(c.ents[0].tier, 0, "{name}: the other is near");
    }
    assert_ne!(a.own.faction, b.own.faction, "different sides");
    assert_eq!(a.ents[0].id, b.own.entity);
    assert_eq!(a.ents[0].faction, b.own.faction);

    // A fired, at the rifle's rate: ~10 a second for ~4 s.
    let sa = a.stats();
    assert!((30..=45).contains(&a.fired), "{} shots", a.fired);
    // Every shot rode an input (the last pull may still be waiting for its
    // step's input when the loop ends: a test machine under load moves it).
    assert!((a.fired as u64 - 1..=a.fired as u64).contains(&sa.shots), "every shot rode an input: {} of {}", sa.shots, a.fired);
    assert!(a.most_tracers >= 1, "our tracers flew");
    // Down the sights at a few meters, nearly every shot hits: confirmed to
    // A, felt by B, from A's direction.
    let hits = a.events.iter().filter(|e| e.kind == EVENT_HIT && e.a == b.own.entity).count();
    let hurts = b.events.iter().filter(|e| e.kind == EVENT_HURT && e.a == a.own.entity).count();
    assert!(hits as u32 >= a.fired * 8 / 10, "{hits} of {} shots hit", a.fired);
    assert_eq!(hits, hurts, "every hit felt");
    // B saw A's shots: each a muzzle flash where A is drawn, along A's aim.
    let seen: Vec<&LtFx> = b.shots_fx.iter().filter(|f| f.shooter == a.own.entity).collect();
    assert!(seen.len() as u32 >= a.fired * 8 / 10, "{} of {} shots seen", seen.len(), a.fired);
    for f in &seen {
        assert_eq!(f.tracer, 1, "a seen shot");
        assert_eq!(f.faction, a.own.faction);
        let d = ((f.pos.x - a.own.pos.x).powi(2) + (f.pos.y - a.own.pos.y).powi(2)).sqrt();
        assert!(d < 3.0, "from where A stands: {d} m off");
    }
    // Tracers pass through players (the server decides hits) and end in the
    // ground behind B: impacts, facing up out of it.
    let ground: Vec<&LtFx> = a.impacts.iter().filter(|f| f.surface == SURFACE_TERRAIN).collect();
    assert!(!ground.is_empty(), "impacts on the ground");
    assert!(ground.iter().all(|f| f.normal.z > 0.3));

    // The world: the session's is the server's.
    unsafe {
        let w = lt_session_world(a.s);
        assert!(!w.is_null());
        let mut info = LtWorldInfo::default();
        lt_world_info(w, &mut info);
        assert_eq!(info.seed, server.sim.config().world);
        assert_eq!(info.samples as usize, lattice_game::world::TERRAIN_N);
        let mut h = [0f32; 6];
        assert_eq!(lt_world_heights(w, 1000, 1000, 3, 2, h.as_mut_ptr()), 6);
        let world = lattice_game::world::World::shared(info.seed);
        assert_eq!(h[4], world.sample(1001, 1001));
        assert_eq!(lt_world_height(w, 4000.0, 4000.0), world.terrain(4000.0, 4000.0));
        let n = lt_world_boxes(w, std::ptr::null_mut(), 0);
        assert_eq!(n as usize, world.boxes().len(), "a count without a buffer");
        let mut boxes = vec![LtBox::default(); n as usize];
        lt_world_boxes(w, boxes.as_mut_ptr(), n);
        assert_eq!(boxes[0].max.z, world.boxes()[0].top);
        let (from, to) = (LtVec3 { x: 4000.0, y: 4000.0, z: 400.0 }, LtVec3 { x: 4000.0, y: 4000.0, z: -10.0 });
        let mut hit = LtHit::default();
        assert_eq!(lt_world_raycast(w, &from, &to, &mut hit), 1);
        assert!(hit.t > 0.0 && hit.t < 1.0);
        lt_world_free(w);
    }

    // The report.
    let mut buf = vec![0u8; 4096];
    let n = unsafe { lt_summary(a.s, buf.as_mut_ptr(), buf.len() as i32) } as usize;
    let text = String::from_utf8_lossy(&buf[..n]);
    assert!(text.contains("corrections 0") && text.contains("user 1"), "{text}");

    unsafe {
        lt_close(a.s);
        lt_close(b.s);
    }
}

/// Crouch and the rifle's rounds through the C API. B crouches: it says so,
/// A sees it (the flag), and A's shots at a standing head's height pass
/// over it, while at its crouched body they hit. A empties its magazine
/// (it can't fire while the reload runs; B sees it reloading) and fires
/// again from the next: predicted exactly, no correction, no shot refused.
/// (B can't die: invulnerable, not an immortal server, whose rounds never
/// run out.)
/// A Skirmisher's jets through the C API (U6): held a second, our own state
/// says they burn and we climb, the other client draws us with the
/// ability's flag, and both predict exactly.
#[test]
fn jump_jets_through_the_c_api() {
    let _quiet = quiet();
    let start = Instant::now();
    let mut cfg = SimConfig { spawn: SpawnMode::Disk(20.0), ..Default::default() };
    cfg.interest.squad_size = 0;
    let mut server = LoopServer::with(cfg, start);
    let id = server.sim.config().identity.clone();
    let key = hex(&id.token_key);
    let addr = server.addr().to_string();
    let mut a = Client::connect(&addr, 1, &key, id.server_id);
    let mut b = Client::connect(&addr, 2, &key, id.server_id);
    let frame = Duration::from_micros(8333);
    let mut next = start;
    let (mut kitted, mut ground, mut top, mut burned, mut seen) = (false, None, f32::MIN, false, false);
    while next < start + Duration::from_secs(6) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let t = (next - start).as_secs_f64();
        if a.own.welcomed == 1 && !kitted {
            server.sim.set_kit(a.own.entity, lattice_game::class::SKIRMISHER);
            kitted = true;
        }
        let jets = (3.0..4.0).contains(&t);
        a.buttons = if jets { LT_BUTTON_ABILITY } else { 0 };
        if t > 2.5 && ground.is_none() {
            ground = Some(a.own.pos.z);
        }
        if jets {
            top = top.max(a.own.pos.z);
            burned |= a.own.ability_on == 1 && a.own.ability_energy < 1.0;
        }
        a.frame(None, false);
        b.frame(None, false);
        seen |= b.ents[..b.drawn].iter().any(|e| e.id == a.own.entity && e.flags & FLAG_ABILITY != 0);
        server.run(Instant::now());
        next += frame;
    }
    for (name, st) in [("A", a.stats()), ("B", b.stats())] {
        assert_eq!((st.corrections, st.ammo_corrections, st.resyncs), (0, 0, 0), "{name}: predicted exactly");
    }
    assert_eq!(a.own.kit, lattice_game::class::SKIRMISHER);
    assert!(burned, "the jets burned their fuel");
    let climb = top - ground.unwrap();
    assert!(climb > 2.0, "they climbed {climb} m");
    assert!(seen, "B drew A's jets");
}

#[test]
fn crouch_and_the_magazine_through_the_c_api() {
    let _quiet = quiet();
    let start = Instant::now();
    let mut cfg = SimConfig { spawn: SpawnMode::Disk(20.0), ..Default::default() };
    cfg.interest.squad_size = 0;
    let mut server = LoopServer::with(cfg, start);
    let id = server.sim.config().identity.clone();
    let key = hex(&id.token_key);
    let addr = server.addr().to_string();
    let mut a = Client::connect(&addr, 1, &key, id.server_id);
    let mut b = Client::connect(&addr, 2, &key, id.server_id);
    b.buttons = LT_BUTTON_CROUCH;

    let hits = |a: &Client, b: &Client| a.events.iter().filter(|e| e.kind == EVENT_HIT && e.a == b.own.entity).count() as u32;
    let frame = Duration::from_micros(8333);
    let mut next = start;
    let (mut seen_crouched, mut emptied, mut blocked, mut seen_reloading) = (false, false, 0u32, false);
    let (mut high_fired, mut high_hits, mut reload_seen) = (0u32, None, 0f32);
    while next < start + Duration::from_secs(11) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let t = (next - start).as_secs_f64();
        let drawn = a.ents[..a.drawn].iter().find(|e| e.id == b.own.entity && b.own.welcomed == 1).copied();
        seen_crouched |= drawn.is_some_and(|e| e.flags & FLAG_CROUCHED != 0);
        // Until 5 s, at a standing player's head; after, at the crouched body.
        let high = t < 5.0;
        let target = drawn.map(|e| [e.pos.x, e.pos.y, e.pos.z + if high { 1.6 } else { 0.6 }]);
        let shooting = t > 2.0 && target.is_some();
        // (With a step or two of the reload left, the next input's step may
        // finish it, and a shot in that step fires.)
        let (before, reloading) = (a.fired, a.own.reload > 0.07);
        a.frame(target, shooting);
        if shooting && reloading {
            blocked += 1;
            assert_eq!(a.fired, before, "no shot while the reload runs");
        }
        if high {
            high_fired = a.fired;
        }
        if a.own.mag == 0 && a.fired > 0 {
            emptied = true;
            reload_seen = reload_seen.max(a.own.reload);
        }
        // The high shots have all landed (or not) before the reload ends.
        if t > 6.5 && high_hits.is_none() {
            high_hits = Some(hits(&a, &b));
        }
        b.frame(None, false);
        if b.own.welcomed == 1 {
            server.sim.set_invulnerable(b.own.entity, true);
        }
        seen_reloading |= b.ents[..b.drawn].iter().any(|e| e.id == a.own.entity && a.own.welcomed == 1 && e.flags & FLAG_RELOADING != 0);
        server.run(Instant::now());
        next += frame;
    }
    let (sa, sb) = (a.stats(), b.stats());
    for (name, st) in [("A", &sa), ("B", &sb)] {
        assert_eq!((st.corrections, st.ammo_corrections, st.resyncs), (0, 0, 0), "{name}: predicted exactly");
    }
    assert_eq!(server.sim.counters().shots_refused, 0, "no shot without a round");
    // B crouched, and A saw it; A reloaded, and B saw that.
    assert_eq!(b.own.crouched, 1);
    assert!(seen_crouched, "A drew B crouched");
    assert!(seen_reloading, "B drew A reloading");
    // A's magazine: 30 at its head's height, a reload from empty, then more.
    let r = lattice_game::weapon::RELOAD_EMPTY_STEPS as f32 / lattice_game::movement::TICK_HZ as f32;
    assert!(emptied && reload_seen > r - 0.1 && reload_seen <= r, "emptied, reloaded ({reload_seen} s)");
    assert!(blocked > 100, "the trigger held through the reload: {blocked} frames");
    assert_eq!(high_fired, lattice_game::weapon::MAGAZINE as u32, "a magazine's worth before the reload");
    let low_fired = a.fired - high_fired;
    assert!(low_fired >= 20, "{low_fired} after the reload");
    assert_eq!(a.own.mag as u32, lattice_game::weapon::MAGAZINE as u32 - low_fired.min(30), "the second magazine's count");
    assert_eq!(a.own.reserve, lattice_game::weapon::RESERVE - lattice_game::weapon::MAGAZINE as u16);
    // Over its head: nothing. At its body: nearly every shot.
    let high_hits = high_hits.expect("measured");
    let low_hits = hits(&a, &b) - high_hits;
    assert!(high_hits <= 1, "{high_hits} of {high_fired} hit over a crouched head");
    assert!(low_hits >= low_fired * 8 / 10, "{low_hits} of {low_fired} hit the crouched body");
    unsafe {
        lt_close(a.s);
        lt_close(b.s);
    }
}

#[test]
fn bad_calls_fail_softly() {
    unsafe {
        assert_eq!(lt_poll(std::ptr::null_mut()), STATE_ERROR);
        assert_eq!(last_error(), "null session");
        let mut st = LtStats::default();
        lt_stats(std::ptr::null_mut(), &mut st);
        assert_eq!(st.state, STATE_ERROR);
        let bad = CString::new("not an address").unwrap();
        assert!(lt_connect_dev(bad.as_ptr().cast(), 1, std::ptr::null(), 1, std::ptr::null()).is_null());
        assert!(!last_error().is_empty());
        let addr = CString::new("127.0.0.1:9").unwrap();
        let short = CString::new("abcd").unwrap();
        assert!(lt_connect_dev(addr.as_ptr().cast(), 1, short.as_ptr().cast(), 1, std::ptr::null()).is_null());
        assert!(last_error().contains("64 hex digits"));
        // A session to nowhere stays connecting; closing it is fine.
        let s = lt_connect_dev(addr.as_ptr().cast(), 1, std::ptr::null(), 1, std::ptr::null());
        assert!(!s.is_null());
        lt_begin_frame(s);
        assert_eq!(lt_poll(s), STATE_CONNECTING);
        let mut own = LtOwn { welcomed: 9, ..Default::default() };
        lt_own(s, 0, &mut own);
        assert_eq!(own.welcomed, 0);
        assert!(lt_session_world(s).is_null(), "no world before the welcome");
        lt_close(s);
        // Text is cut at a character boundary and always terminated.
        // ("é" is two bytes: a 3-byte buffer holds "h" and the NUL.)
        let mut small = [0xAAu8; 3];
        assert_eq!(put_text_for_tests("h\u{e9}llo", &mut small), 6);
        assert_eq!(&small, b"h\0\xAA");
    }
}

/// What a run with frozen frames came to: the client's numbers, the
/// server's, and the shots the client fired.
struct FreezeRun {
    client: LtStats,
    server: Counters,
    fired: u32,
    /// Our drawn speed frame to frame (distance over the time between the
    /// `lt_own` calls), away from the freezes: its spread is the judder.
    speeds: Vec<f32>,
}

/// One client, alone on a server that ticks on its own thread (the frames
/// freeze, the server mustn't), running in a slow circle and firing from
/// 1.5 s on, in 144 Hz frames through the C API; at each `(at, secs)` its
/// frames stop: no calls at all, as when the engine stalls.
fn run_with_freezes(threaded: bool, freezes: &[(f64, f64)], secs: f64) -> FreezeRun {
    let start = Instant::now();
    let server = LoopServer::with(SimConfig { spawn: SpawnMode::Uniform, immortal: true, ..Default::default() }, start);
    let id = server.sim.config().identity.clone();
    let addr = CString::new(server.addr().to_string()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let ticking = Arc::clone(&stop);
    let srv = std::thread::spawn(move || {
        let mut server = server;
        while !ticking.load(Ordering::Acquire) {
            server.run(Instant::now());
            std::thread::sleep(Duration::from_micros(500));
        }
        server
    });

    let key = hex(&id.token_key);
    let cfg = LtConfig { flags: if threaded { CONFIG_NET_THREAD } else { 0 }, ..Default::default() };
    let s = unsafe { lt_connect_dev(addr.as_ptr().cast(), 7, key.as_ptr().cast(), id.server_id, &cfg) };
    assert!(!s.is_null(), "{}", last_error());
    let frame = Duration::from_micros(6944);
    let (mut next, mut frozen, mut fired) = (start, vec![false; freezes.len()], 0);
    let mut own = LtOwn::default();
    let mut ents = vec![LtEntity::default(); 64];
    let (mut ev, mut fx) = ([LtEvent::default(); 16], [LtFx::default(); 64]);
    let (mut speeds, mut last): (Vec<f32>, Option<(Instant, LtVec3)>) = (Vec::new(), None);
    while next < start + Duration::from_secs_f64(secs) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let t = start.elapsed().as_secs_f64();
        for (i, &(at, len)) in freezes.iter().enumerate() {
            if !frozen[i] && t >= at {
                frozen[i] = true;
                std::thread::sleep(Duration::from_secs_f64(len));
                next = Instant::now();
            }
        }
        let yaw = (t * 0.5) as f32;
        unsafe {
            lt_begin_frame(s);
            lt_poll(s);
            if t > 1.5 && own.welcomed == 1 {
                let f = LtFire { yaw, pitch: 0.05, ads: 0, muzzle: LtVec3 { x: 0.2, y: -0.15, z: 0.6 } };
                fired += lt_fire(s, &f, std::ptr::null_mut()) as u32;
            }
            lt_inputs(s, &LtIntent { yaw, pitch: 0.05, forward: 1.0, right: 0.0, buttons: 0 });
            let at = Instant::now();
            lt_own(s, 0, &mut own);
            // Away from the start and the freezes (a freeze's own frame moves
            // a whole stall at once).
            let calm = t > 2.0 && freezes.iter().all(|&(f, len)| t < f - 0.1 || t > f + len + 0.5);
            if let (Some((t0, p0)), true) = (last, calm) {
                let d = ((own.pos.x - p0.x).powi(2) + (own.pos.y - p0.y).powi(2)).sqrt();
                speeds.push(d / (at - t0).as_secs_f32());
            }
            last = Some((at, own.pos));
            lt_render(s, ents.as_mut_ptr(), ents.len() as i32);
            lt_combat(s);
            while lt_events(s, ev.as_mut_ptr(), ev.len() as i32) == ev.len() as i32 {}
            while lt_fx(s, fx.as_mut_ptr(), fx.len() as i32) == fx.len() as i32 {}
            lt_flush(s);
        }
        next += frame;
    }
    let mut client = LtStats::default();
    unsafe {
        lt_stats(s, &mut client);
        lt_close(s);
    }
    std::thread::sleep(Duration::from_millis(50));
    stop.store(true, Ordering::Release);
    let server = srv.join().unwrap().sim.counters().clone();
    FreezeRun { client, server, fired, speeds }
}

/// The spread of drawn speeds: (the 1st and 99th percentiles over the median).
fn judder(speeds: &[f32]) -> (f32, f32) {
    let mut v = speeds.to_vec();
    v.sort_by(f32::total_cmp);
    let q = |p: f32| v[((v.len() - 1) as f32 * p) as usize];
    (q(0.01) / q(0.5), q(0.99) / q(0.5))
}

/// U3's pass bar (GDD §20): with the session on the net thread, frames that
/// stop for 100 ms, 300 ms and 1 s while running and firing cost nothing:
/// the server never stands in for an input, refuses, holds or trims a shot,
/// and the client never resyncs or mispredicts. The same freezes in pump mode
/// (the control) leave the server standing in for the inputs never made.
#[test]
fn the_net_thread_rides_out_frozen_frames() {
    let _quiet = quiet();
    let freezes = [(3.0, 0.1), (5.0, 0.3), (7.0, 1.0)];
    let threaded = std::thread::spawn(move || run_with_freezes(true, &freezes, 10.0));
    let pumped = run_with_freezes(false, &freezes, 10.0);
    let threaded = threaded.join().unwrap();
    for (name, r) in [("net thread", &threaded), ("pump", &pumped)] {
        eprintln!(
            "{name}: fired {}, shots {}, stand-ins {} (frozen {}), late {}, refused {}, held {}, trimmed {}; client resyncs {}, corrections {}",
            r.fired,
            r.server.shots,
            r.server.repeated,
            r.server.frozen,
            r.server.shots_late,
            r.server.shots_refused,
            r.server.renders_held,
            r.server.rewinds_trimmed,
            r.client.resyncs,
            r.client.corrections
        );
    }
    let (c, s) = (&threaded.client, &threaded.server);
    assert_eq!(c.state, STATE_CONNECTED);
    assert_eq!(c.corrections, 0, "prediction exact");
    assert_eq!(c.resyncs, 0, "no resyncs");
    assert_eq!(s.repeated + s.frozen, 0, "no stand-ins: the inputs kept coming");
    assert_eq!((s.shots_refused, s.renders_held, s.rewinds_trimmed), (0, 0, 0), "no shot refused, held or trimmed");
    // Every shot reached the server (the last pull may still have been
    // waiting for its step's input when the session closed). A magazine is
    // 30 rounds, then a 2.7 s reload: ~50 shots in the run.
    let fired = threaded.fired as u64;
    assert!(fired > 40 && (fired - 1..=fired).contains(&s.shots), "{fired} fired, {} reached the server", s.shots);
    assert!(pumped.server.repeated + pumped.server.frozen > 20, "the control: pump mode's freezes cost inputs");
    // Our own player is drawn as smoothly as in pump mode: the thread's
    // ticks between a frame's start and its calls must not hold the drawn
    // position back (each call takes the time as it runs).
    let (lo, hi) = judder(&threaded.speeds);
    let (plo, phi) = judder(&pumped.speeds);
    eprintln!("drawn speed, 1st-99th percentile over the median: net thread {lo:.3}-{hi:.3}, pump {plo:.3}-{phi:.3}");
    assert!(lo > 0.9 && hi < 1.1, "our player judders on the net thread: {lo:.3}-{hi:.3} of its median speed");
}

/// And a 10 s scene load (U3: the connection must survive it, past the 5 s
/// timeout): connected after, nothing stood in.
#[test]
fn the_net_thread_keeps_the_connection_through_a_scene_load() {
    let _quiet = quiet();
    let r = run_with_freezes(true, &[(2.5, 10.0)], 15.0);
    eprintln!("scene load: stand-ins {}, resyncs {}, corrections {}, fired {}", r.server.repeated + r.server.frozen, r.client.resyncs, r.client.corrections, r.fired);
    assert_eq!(r.client.state, STATE_CONNECTED, "still connected");
    assert_eq!((r.client.resyncs, r.client.corrections), (0, 0));
    assert_eq!(r.server.repeated + r.server.frozen, 0, "no stand-ins");
    assert!(r.fired > 20, "{} fired", r.fired);
}

fn put_text_for_tests(s: &str, buf: &mut [u8]) -> i32 {
    let e = CString::new(s).unwrap();
    // Through lt_last_error's copy: set the error, read it back small.
    crate::api::tests_set_error(e.to_str().unwrap());
    unsafe { lt_last_error(buf.as_mut_ptr(), buf.len() as i32) }
}

#[test]
fn the_kit_world_has_its_turned_boxes_volumes_and_holes() {
    use lattice_game::kit;
    unsafe {
        let w = lt_world_new(kit::KIT);
        let mut info = LtWorldInfo::default();
        lt_world_info(w, &mut info);
        assert_eq!(info.volumes, 3, "a lift, a drop shaft, a spring");
        assert_eq!(info.holes, 24, "two trenches, two cells across and six along");
        let mut vols = vec![LtVolume::default(); info.volumes as usize];
        assert_eq!(lt_world_volumes(w, vols.as_mut_ptr(), info.volumes as i32), 3);
        assert_eq!(vols[0].force, lattice_game::world::Force::Lift as u8);
        assert!(vols[1].vel.z < 0.0);
        let mut boxes = vec![LtBox::default(); info.boxes as usize];
        lt_world_boxes(w, boxes.as_mut_ptr(), info.boxes as i32);
        let turned = boxes[0];
        assert!((turned.axis_x - 30f32.to_radians().cos()).abs() < 1e-6 && turned.half_along == 10.0, "{turned:?}");
        assert!(boxes.iter().any(|b| b.low != b.max.z), "a wedge");
        assert!(boxes.iter().any(|b| b.pass != lattice_game::world::SOLID && b.kind == lattice_game::world::Kind::Shield as u8), "a shield");
        // The first trench's cells, as holes; beside them, none.
        let (cx, cy) = ((kit::TUNNEL_IN.0 / 4.0) as i32, (996.0 / 4.0) as i32);
        let mut cells = [0u8; 8];
        assert_eq!(lt_world_holes(w, cx - 1, cy - 1, 4, 2, cells.as_mut_ptr()), 8);
        assert_eq!(cells, [0, 0, 0, 0, 0, 1, 1, 1], "row cy-1: none; row cy: from cx");
        lt_world_free(w);
    }
}

#[test]
fn deploying_through_the_c_api() {
    let _alone = QUIET.write().unwrap_or_else(|e| e.into_inner());
    use lattice_game::events::NOBODY;
    // Kiln, players deploying (U4). (Built first: see `territory_through_the_c_api`.)
    let _kiln = lattice_game::world::World::shared(lattice_game::world::KILN);
    let start = Instant::now();
    let cfg = SimConfig { world: lattice_game::world::KILN, spawn: SpawnMode::Uniform, deploy: true, ..Default::default() };
    let mut server = LoopServer::with(cfg, start);
    let id = server.sim.config().identity.clone();
    let key = hex(&id.token_key);
    let mut a = Client::connect(&server.addr().to_string(), 1, &key, id.server_id);
    let frame = Duration::from_micros(8333);
    let mut next = start;
    let shell = |a: &Client| {
        let mut sh = LtShell::default();
        unsafe { lt_shell(a.s, &mut sh) };
        sh
    };
    let run = |a: &mut Client, server: &mut LoopServer, next: &mut Instant, secs: f32, until: &dyn Fn(&Client) -> bool| {
        let end = Instant::now() + Duration::from_secs_f32(secs);
        while Instant::now() < end && !until(a) {
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            a.frame(None, false);
            server.run(Instant::now());
            *next += frame;
        }
    };
    run(&mut a, &mut server, &mut next, 5.0, &|a| a.own.welcomed == 1 && shell(a).deploys == 1);
    let sh = shell(&a);
    assert_eq!((sh.deploys, sh.away, a.own.dead, sh.died_in), (1, 1, 1, lattice_game::world::NO_SITE), "joined with no body");
    let us = a.own.faction;
    let t = server.sim.territory();
    let (home, enemy) = (t.landing(us).unwrap(), t.landing((us + 1) % 3).unwrap());
    let mut opts = vec![LtSpawnOption::default(); 128];
    let n = unsafe { lt_spawn_options(a.s, opts.as_mut_ptr(), opts.len() as i32) } as usize;
    assert_eq!(n, server.sim.world().sites().len());
    assert_eq!((opts[home].ok, opts[home].transit_secs), (1, 6.0), "our Landing, from orbit");
    assert_eq!((opts[enemy].ok, opts[enemy].reason), (0, lattice_game::territory::REFUSED_NOT_HELD));
    // Asking for theirs is refused; ours is granted.
    assert_eq!(unsafe { lt_deploy(a.s, enemy as u16, 0, 0) }, 1);
    run(&mut a, &mut server, &mut next, 2.0, &|a| shell(a).refusals > 0);
    assert_eq!((shell(&a).refused_site, shell(&a).refused_reason), (enemy as u16, lattice_game::territory::REFUSED_NOT_HELD));
    assert!(a.events.iter().any(|e| e.kind == EVENT_REFUSED && e.a == enemy as u16));
    assert_eq!(unsafe { lt_deploy(a.s, home as u16, 0, 0) }, 1);
    run(&mut a, &mut server, &mut next, 2.0, &|a| shell(a).deploying == 1);
    let sh = shell(&a);
    assert_eq!(sh.deploy_site, home as u16);
    assert!(sh.deploy_secs > 5.0 && sh.deploy_secs <= 6.0, "transit {}", sh.deploy_secs);
    assert!(a.events.iter().any(|e| e.kind == EVENT_DEPLOYING && e.a == home as u16 && e.value > 0));
    // We appear in its spawn room.
    run(&mut a, &mut server, &mut next, 8.0, &|a| a.own.dead == 0);
    assert_eq!((a.own.dead, shell(&a).away, shell(&a).deploying), (0, 0, 0), "deployed");
    let room = server.sim.world().spawn_rooms().iter().find(|r| r.site as usize == home).copied().unwrap();
    let d = ((a.own.pos.x - room.center[0]).powi(2) + (a.own.pos.y - room.center[1]).powi(2)).sqrt();
    assert!(d < room.half[0].max(room.half[1]) + 0.5, "in our Landing's spawn room ({d} m from its center)");
    // Killed (by a test's damage: no one), a recap comes.
    server.sim.damage(a.own.entity, 100);
    run(&mut a, &mut server, &mut next, 2.0, &|a| shell(a).recaps > 0);
    let mut r = LtRecap::default();
    assert_eq!(unsafe { lt_recap(a.s, &mut r) }, 1);
    assert_eq!((r.killer, r.lines, r.from[0], r.damage[0]), (NOBODY, 1, NOBODY, 100));
    assert_eq!((a.own.dead, shell(&a).away, shell(&a).died_in), (1, 0, home as u16), "a corpse, where it fell");
    // Deploying again releases it.
    assert_eq!(unsafe { lt_deploy(a.s, home as u16, 0, 0) }, 1);
    run(&mut a, &mut server, &mut next, 2.0, &|a| shell(a).deploying == 1);
    assert_eq!(shell(&a).away, 1, "released: no body");
    run(&mut a, &mut server, &mut next, 8.0, &|a| a.own.dead == 0);
    assert_eq!(a.own.dead, 0, "back");
    let st = a.stats();
    assert_eq!((st.corrections, st.resyncs), (0, 0), "prediction stayed exact through every life");
    unsafe { lt_close(a.s) };
}

#[test]
fn territory_through_the_c_api() {
    let _alone = QUIET.write().unwrap_or_else(|e| e.into_inner());
    use lattice_game::territory::NONE;
    // Kiln, its meters 120 times faster: the Hollow (5 min) flips in 2.5 s.
    // (Built first: the server's ticks run from `start`, and would catch up
    // on the build's time in bursts, starving our inputs.)
    let _kiln = lattice_game::world::World::shared(lattice_game::world::KILN);
    let start = Instant::now();
    let cfg = SimConfig { world: lattice_game::world::KILN, spawn: SpawnMode::Uniform, capture_speed: 120, ..Default::default() };
    let mut server = LoopServer::with(cfg, start);
    let id = server.sim.config().identity.clone();
    let key = hex(&id.token_key);
    let mut a = Client::connect(&server.addr().to_string(), 1, &key, id.server_id);
    let frame = Duration::from_micros(8333);
    let mut next = start;
    let mut states = vec![LtSiteState::default(); 128];
    let read = |a: &Client, states: &mut Vec<LtSiteState>| unsafe { lt_territory(a.s, states.as_mut_ptr(), states.len() as i32) } as usize;
    let (mut teleported, mut flipped_at, mut meter_seen) = (None, None, 0f32);
    while next < start + Duration::from_secs(12) && flipped_at.is_none() {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        a.frame(None, false);
        let n = read(&a, &mut states);
        if a.own.welcomed == 1 && n > 0 {
            let us = lattice_game::faction::faction(a.own.entity);
            let hollow = states[0];
            if teleported.is_none() {
                // At the start: the Hollow no one's, open to every faction.
                assert_eq!(n, server.sim.world().sites().len());
                assert_eq!((hollow.owner, hollow.attackable, hollow.locked_secs), (NONE, 0b111, 0.0));
                assert!(hollow.point_count >= 1 && hollow.points[..hollow.point_count as usize].iter().all(|&p| p == NONE));
                for (s, st) in server.sim.world().sites().iter().zip(&states[..n]).skip(1) {
                    assert_eq!(st.owner, s.sector, "the rest their sectors'");
                }
                let p = server.sim.territory().points(0)[0];
                server.sim.teleport(a.own.entity, [p[0], p[1]]);
                teleported = Some(Instant::now());
            } else {
                if hollow.attacker == us {
                    meter_seen = meter_seen.max(hollow.meter);
                }
                if hollow.owner == us {
                    assert_eq!(hollow.points[0], us, "and we hold its point");
                    assert!(hollow.locked_secs > 170.0, "locked for 3 minutes: {}", hollow.locked_secs);
                    flipped_at = Some(Instant::now());
                }
            }
        }
        server.run(Instant::now());
        next += frame;
    }
    let took = flipped_at.expect("the Hollow flipped to us") - teleported.unwrap();
    assert!(meter_seen > 0.5, "the meter climbed ({meter_seen})");
    assert!(took.as_secs_f32() < 5.0, "in {took:?}");
    assert_eq!((a.stats().corrections, a.stats().resyncs), (0, 0), "prediction stayed exact");
    unsafe { lt_close(a.s) };
}
