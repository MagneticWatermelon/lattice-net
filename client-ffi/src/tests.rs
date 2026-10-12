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
            let intent = LtIntent { yaw, pitch, forward: 0.0, right: 0.0, buttons: if fire { LT_BUTTON_ADS } else { 0 } };
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

#[test]
fn two_sessions_join_see_each_other_and_fight_through_the_c_api() {
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
        assert_eq!(info.seed, server.sim.config().world_seed);
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
    // waiting for its step's input when the session closed).
    let fired = threaded.fired as u64;
    assert!(fired > 60 && (fired - 1..=fired).contains(&s.shots), "{fired} fired, {} reached the server", s.shots);
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
