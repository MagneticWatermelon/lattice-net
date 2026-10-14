//! The C API: `lt_*` functions over a `Handle` (an `LtSession*` to the
//! caller) and a shared `World` (an `LtWorld*`). Declared through `ffi_fns!`,
//! which also records each signature (`FNS`) for the C# generator.
//!
//! **The rules for callers:**
//! - One thread at a time per session (the engine's main thread). With
//!   `LT_CONFIG_NET_THREAD` the plugin's own net thread runs the session too
//!   (`net_thread`): every call takes the session's lock, and `lt_poll`,
//!   `lt_inputs` (which then only sets the intent) and `lt_flush` hand their
//!   work to it. The frame's calls and their order stay the same.
//! - Buffers belong to the caller: functions fill `out` arrays of `cap`
//!   items and return how many there are (which can be more than `cap`: grow
//!   the buffer). Nothing they return points into the plugin.
//! - Strings are NUL-terminated UTF-8.
//! - Nothing panics across the boundary: a panic inside a call is caught,
//!   its message kept for `lt_last_error`, and a session it happened in is
//!   closed (its calls do nothing and its state reads `LT_STATE_ERROR`).
//!
//! A frame, in order (GDD §13): `lt_begin_frame`, `lt_poll`; then `lt_fire`
//! (if the trigger is held) and `lt_inputs`; then `lt_own`, `lt_render` and
//! `lt_combat`, with the drains (`lt_events`, `lt_fx`, `lt_tracers`); last,
//! `lt_flush`.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::CStr;
use std::net::{SocketAddr, ToSocketAddrs};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_client_core::invite::Invite;
use lattice_client_core::tracers::segment_hit;
use lattice_client_core::ClientConfig;
use lattice_game::world::{World, CELLS, TERRAIN_N, TERRAIN_RES};

use crate::abi::*;
use crate::handle::{Handle, Player};
use crate::net_thread::{lock, NetThread, Shared};
use crate::session::dev_token;

/// The API's version: bump it with any change a caller must follow.
/// 2: `LT_CONFIG_NET_THREAD`.
/// 3: crouch and the rifle's rounds (`LtOwn`, `LT_BUTTON_CROUCH`,
/// `LT_BUTTON_RELOAD`, `LT_FLAG_CROUCHED`, `LT_FLAG_RELOADING`).
/// 4: worlds by id (Kiln), lanes, points, materials.
/// 5: the collision kit (K1): turned boxes, wedges and shields in `LtBox`,
/// one-way lanes, `lt_world_volumes`, `lt_world_holes`.
/// 8: turned force volumes (`LtVolume`'s center, halves and axis).
/// 9: the shell (U4): `lt_deploy`, `lt_shell`, `lt_spawn_options`,
/// `lt_recap`, `lt_fights`; `LtEvent::value`, deploy and reward events.
/// 10: classes and their weapons (U6): `LtOwn::kit`, `LtEntity::kit`,
/// `LtFx::kit`, `lt_weapon`.
pub const VERSION: u32 = 10;

/// A function's signature, as the C# generator sees it.
#[derive(Debug, Clone, Copy)]
pub struct FnInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub args: &'static [(&'static str, &'static str)],
    pub ret: &'static str,
}

/// An open session (opaque to callers): the handle behind its lock, and the
/// net thread if it has one.
pub struct LtSession {
    handle: Shared,
    net: Option<NetThread>,
}

/// A world, shared (opaque to callers).
pub struct LtWorld {
    world: Arc<World>,
}

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

#[cfg(test)]
pub(crate) fn tests_set_error(msg: &str) {
    set_error(msg);
}

/// What a call returns when it can't run (a panic, a null session).
pub trait Fallback {
    fn fallback() -> Self;
}

macro_rules! fallback {
    ($($t:ty = $v:expr),*) => { $(impl Fallback for $t { fn fallback() -> Self { $v } })* };
}
fallback!(() = (), u8 = 0, u16 = 0, u32 = 0, u64 = 0, i32 = -1, f32 = 0.0, f64 = 0.0);

impl<T> Fallback for *mut T {
    fn fallback() -> Self {
        std::ptr::null_mut()
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_else(|| "a panic".into())
}

/// Runs a call, catching any panic.
fn guard<R: Fallback>(name: &str, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(e) => {
            set_error(format!("{name}: {}", panic_text(&*e)));
            R::fallback()
        }
    }
}

/// Runs a call on an open session, under its lock; a panic closes it.
unsafe fn with<R: Fallback>(s: *mut LtSession, f: impl FnOnce(&mut Handle) -> R) -> R {
    let Some(s) = s.as_ref() else {
        set_error("null session");
        return R::fallback();
    };
    let mut g = lock(&s.handle);
    let Some(h) = g.as_mut() else {
        // Closed by the net thread: say why.
        if let Some(e) = s.net.as_ref().and_then(NetThread::error) {
            set_error(e);
        }
        return R::fallback();
    };
    match catch_unwind(AssertUnwindSafe(|| f(h))) {
        Ok(r) => r,
        Err(e) => {
            set_error(format!("session closed after a panic: {}", panic_text(&*e)));
            *g = None;
            R::fallback()
        }
    }
}

unsafe fn world<'a>(w: *const LtWorld) -> Option<&'a World> {
    w.as_ref().map(|w| &*w.world)
}

unsafe fn text<'a>(p: *const u8) -> Result<&'a str, String> {
    if p.is_null() {
        return Err("null string".into());
    }
    CStr::from_ptr(p.cast()).to_str().map_err(|e| format!("not UTF-8: {e}"))
}

/// Copies `s` into `buf` (`cap` bytes, NUL-terminated, cut at a character
/// boundary); returns the bytes it needs, without the NUL.
unsafe fn put_text(s: &str, buf: *mut u8, cap: i32) -> i32 {
    if !buf.is_null() && cap > 0 {
        let mut n = s.len().min(cap as usize - 1);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    s.len() as i32
}

unsafe fn slice<'a, T>(out: *mut T, cap: i32) -> &'a mut [T] {
    if out.is_null() || cap <= 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out, cap as usize)
    }
}

fn config(cfg: Option<&LtConfig>) -> ClientConfig {
    let mut c = ClientConfig::default();
    if let Some(cfg) = cfg {
        let ms = |v: f32| (v > 0.0 && v.is_finite()).then(|| Duration::from_secs_f64(v as f64 / 1000.0));
        if let Some(d) = ms(cfg.near_delay_ms) {
            c.near_delay = d;
        }
        if let Some(d) = ms(cfg.near_delay_max_ms) {
            c.near_delay_max = d;
        }
        if let Some(d) = ms(cfg.mid_delay_ms) {
            c.mid_delay = d;
        }
    }
    c
}

fn resolve(addr: &str) -> Result<SocketAddr, String> {
    addr.to_socket_addrs().map_err(|e| format!("{addr}: {e}"))?.next().ok_or_else(|| format!("{addr}: no address"))
}

/// A session for the caller: on the net thread if `cfg` asks.
fn open(h: std::io::Result<Handle>, cfg: Option<&LtConfig>) -> *mut LtSession {
    let mut h = match h {
        Ok(h) => h,
        Err(e) => {
            set_error(format!("socket: {e}"));
            return std::ptr::null_mut();
        }
    };
    let threaded = cfg.is_some_and(|c| c.flags & CONFIG_NET_THREAD != 0);
    h.set_threaded(threaded);
    let handle: Shared = Arc::new(Mutex::new(Some(h)));
    let net = if threaded {
        match NetThread::spawn(Arc::clone(&handle)) {
            Ok(t) => Some(t),
            Err(e) => {
                set_error(format!("net thread: {e}"));
                if let Some(h) = lock(&handle).as_mut() {
                    h.close();
                }
                return std::ptr::null_mut();
            }
        }
    } else {
        None
    };
    Box::into_raw(Box::new(LtSession { handle, net }))
}

macro_rules! ffi_fns {
    ($( $(#[doc = $doc:literal])* fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty $body:block )*) => {
        $(
            $(#[doc = $doc])*
            #[no_mangle]
            #[allow(clippy::unused_unit)]
            pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
                guard(stringify!($name), move || -> $ret { $body })
            }
        )*
        /// Every exported function's signature, in declaration order.
        pub const FNS: &[FnInfo] = &[ $( FnInfo {
            name: stringify!($name),
            doc: concat!($($doc, "\n"),*),
            args: &[$((stringify!($arg), stringify!($ty))),*],
            ret: stringify!($ret),
        } ),* ];
    };
}

ffi_fns! {
    /// The API's version (`VERSION`): the C# side checks it, and the layout
    /// hash, when it loads the plugin.
    fn lt_version() -> u32 {
        VERSION
    }

    /// A hash of every exported struct's layout and function's signature.
    fn lt_layout_hash() -> u64 {
        layout_hash(FNS)
    }

    /// The last error on this thread (a failed connect, a caught panic):
    /// copied into `buf`; returns its length in bytes.
    fn lt_last_error(buf: *mut u8, cap: i32) -> i32 {
        LAST_ERROR.with(|e| put_text(&e.borrow(), buf, cap))
    }

    /// Connects to `addr` ("host:port") with a dev connect token for `user`,
    /// minted with the server's `--token-key` (`key_hex`: 64 hex digits, or
    /// null for the public dev key) and `--server-id`. `cfg` may be null.
    /// Null on failure (`lt_last_error`).
    fn lt_connect_dev(addr: *const u8, user: u64, key_hex: *const u8, server_id: u64, cfg: *const LtConfig) -> *mut LtSession {
        let run = || -> Result<*mut LtSession, String> {
            let addr_text = text(addr)?;
            let server = resolve(addr_text)?;
            let mut key = lattice_net::token::DEV_TOKEN_KEY;
            if !key_hex.is_null() {
                let hex = text(key_hex)?;
                if hex.len() != 64 {
                    return Err("token key: expected 64 hex digits".into());
                }
                for (i, k) in key.iter_mut().enumerate() {
                    *k = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|e| format!("token key: {e}"))?;
                }
            }
            let token = dev_token(&key, server_id, user);
            let player = Player { server: addr_text.to_string(), user, name: String::new() };
            Ok(open(Handle::connect(server, token, config(cfg.as_ref()), player), cfg.as_ref()))
        };
        run().unwrap_or_else(|e| {
            set_error(e);
            std::ptr::null_mut()
        })
    }

    /// Connects with a playtest invite (`lattice-invite`): the server, who we
    /// are, and the next of its connect tokens (each connects once; the
    /// invite's `.used` file beside it counts them). Null on failure.
    fn lt_connect_invite(path: *const u8, cfg: *const LtConfig) -> *mut LtSession {
        let run = || -> Result<*mut LtSession, String> {
            let path = text(path)?;
            let invite = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}")).and_then(|t| Invite::parse(&t))?;
            let server = resolve(&invite.server)?;
            let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let token = match invite.next_token(std::path::Path::new(path), unix) {
                Ok(Some((_, t))) => t,
                Ok(None) => return Err("no tokens left in the invite (each connects once, and they expire)".into()),
                Err(e) => return Err(format!("counting the invite's used tokens: {e}")),
            };
            let player = Player { server: invite.server.clone(), user: invite.user, name: invite.name.clone() };
            Ok(open(Handle::connect(server, token, config(cfg.as_ref()), player), cfg.as_ref()))
        };
        run().unwrap_or_else(|e| {
            set_error(e);
            std::ptr::null_mut()
        })
    }

    /// Disconnects (the server hears at once) and frees the session (its net
    /// thread stopped first).
    fn lt_close(s: *mut LtSession) -> () {
        if !s.is_null() {
            let mut s = Box::from_raw(s);
            if let Some(net) = s.net.as_mut() {
                net.stop();
            }
            let mut g = lock(&s.handle);
            if let Some(h) = g.as_mut() {
                h.close();
            }
        }
    }

    /// Starts a frame: reads the plugin's clock once; every call until the
    /// next frame uses it. Returns seconds since connecting.
    fn lt_begin_frame(s: *mut LtSession) -> f64 {
        with(s, |h| h.begin_frame(Instant::now()))
    }

    /// Starts a frame `secs` after connecting (for tests that drive time).
    fn lt_begin_frame_at(s: *mut LtSession, secs: f64) -> f64 {
        with(s, |h| h.begin_frame_at(secs))
    }

    /// Receives and delivers everything that arrived; returns the state
    /// (`LT_STATE_*`). On the net thread, the thread receives: this only
    /// returns the state.
    fn lt_poll(s: *mut LtSession) -> i32 {
        with(s, |h| match h.poll() {
            Ok(state) => state,
            Err(e) => {
                set_error(format!("receive: {e}"));
                h.state()
            }
        })
    }

    /// The connection's state (`LT_STATE_*`).
    fn lt_state(s: *mut LtSession) -> i32 {
        with(s, |h| h.state())
    }

    /// Pulls the trigger. Returns 1 if a shot fired (the rifle was ready, we
    /// are alive), with its direction and tracer origin in `out` (may be
    /// null); its tracer flies at once. Call before `lt_inputs`: the shot
    /// rides this frame's input.
    fn lt_fire(s: *mut LtSession, fire: *const LtFire, out: *mut LtShot) -> i32 {
        let Some(f) = fire.as_ref() else { return 0 };
        with(s, |h| match h.fire(f) {
            Some(shot) => {
                if let Some(o) = out.as_mut() {
                    *o = shot;
                }
                1
            }
            None => 0,
        })
    }

    /// This frame's intent to the input clock: inputs (with any shot) go out
    /// at the server's pace. On the net thread, the thread makes them from
    /// the latest intent, every tick, frames or not.
    fn lt_inputs(s: *mut LtSession, intent: *const LtIntent) -> () {
        if let Some(i) = intent.as_ref() {
            with(s, |h| h.inputs(i))
        }
    }

    /// Our own player: where to draw it, its vitals, the server ghost and
    /// the reticle's cone (`ads`: down the sights).
    fn lt_own(s: *mut LtSession, ads: u8, out: *mut LtOwn) -> () {
        if let Some(o) = out.as_mut() {
            *o = with(s, |h| Wrapped(h.own(ads != 0))).0;
        }
    }

    /// Draws every other player at this frame's render step: fills `out` (up
    /// to `cap`) and returns how many there are. Exactly once per frame.
    fn lt_render(s: *mut LtSession, out: *mut LtEntity, cap: i32) -> i32 {
        with(s, |h| h.render(slice(out, cap)) as i32)
    }

    /// The newest sample of every player, undelayed (server ghosts): fills
    /// `out` (up to `cap`), returns how many it wrote.
    fn lt_ghosts(s: *mut LtSession, out: *mut LtEntity, cap: i32) -> i32 {
        with(s, |h| h.ghosts(slice(out, cap)) as i32)
    }

    /// This frame's combat, after `lt_render`: news to events, shots starting
    /// as their shooters are drawn firing, tracers flown to now (with their
    /// impacts). Then drain `lt_events`, `lt_fx` and read `lt_tracers`.
    fn lt_combat(s: *mut LtSession) -> () {
        with(s, |h| h.combat())
    }

    /// Moves up to `cap` combat events into `out`, oldest first; returns how
    /// many it moved.
    fn lt_events(s: *mut LtSession, out: *mut LtEvent, cap: i32) -> i32 {
        with(s, |h| h.take_events(slice(out, cap)) as i32)
    }

    /// Moves up to `cap` effects (shots drawn firing, impacts) into `out`,
    /// oldest first; returns how many it moved.
    fn lt_fx(s: *mut LtSession, out: *mut LtFx, cap: i32) -> i32 {
        with(s, |h| h.take_fx(slice(out, cap)) as i32)
    }

    /// Every tracer in flight, as streaks: fills `out` (up to `cap`) and
    /// returns how many there are.
    fn lt_tracers(s: *mut LtSession, out: *mut LtTracer, cap: i32) -> i32 {
        with(s, |h| h.tracers(slice(out, cap)) as i32)
    }

    /// Where others' drawn muzzles are from their eyes (meters right, up and
    /// forward of their aim): their tracers leave from there.
    fn lt_set_others_muzzle(s: *mut LtSession, offset: *const LtVec3) -> () {
        if let Some(o) = offset.as_ref() {
            with(s, |h| h.set_others_muzzle((*o).into()))
        }
    }

    /// End of the frame: packets out. On the net thread, the thread sends:
    /// this does nothing.
    fn lt_flush(s: *mut LtSession) -> () {
        with(s, |h| h.flush())
    }

    /// Kiln's clock (K4): the day and the clockwork at the step we're
    /// predicting.
    fn lt_clock(s: *mut LtSession, out: *mut LtClock) -> () {
        if let Some(o) = out.as_mut() {
            *o = with(s, |h| Wrapped(h.day())).0;
        }
    }

    /// Every site's state (who holds it, its fight; U5): fills `out` (up to
    /// `cap`, in `lt_world_sites`' order), returns how many sites there are
    /// (0 before we're welcomed).
    fn lt_territory(s: *mut LtSession, out: *mut LtSiteState, cap: i32) -> i32 {
        with(s, |h| h.territory(slice(out, cap)) as i32)
    }

    /// Asks to deploy at site `site` with loadout `kit` (U4; when the
    /// server has us deploy, `LtShell::deploys`): we must be dead or have no
    /// body. The answer comes as an `LT_EVENT_DEPLOYING` or `LT_EVENT_REFUSED`,
    /// and in `lt_shell`. Returns 1 if asked, 0 if not (the server doesn't
    /// deploy, or we aren't connected).
    fn lt_deploy(s: *mut LtSession, site: u16, kit: u8) -> i32 {
        with(s, |h| h.deploy(site, kit) as i32)
    }

    /// The shell's state (U4): deploying, recaps, fights, XP.
    fn lt_shell(s: *mut LtSession, out: *mut LtShell) -> () {
        if let Some(o) = out.as_mut() {
            *o = with(s, |h| Wrapped(h.shell())).0;
        }
    }

    /// Every site as a spawn for us now (`lt_world_sites`' order): whether
    /// our faction may deploy there, and its lattice transit. Fills `out`
    /// (up to `cap`), returns how many sites there are (0 before we're
    /// welcomed, or on a world without territory).
    fn lt_spawn_options(s: *mut LtSession, out: *mut LtSpawnOption, cap: i32) -> i32 {
        with(s, |h| h.spawn_options(slice(out, cap)) as i32)
    }

    /// The newest death recap into `out`; returns how many recaps have
    /// come (0: none yet, and `out` is left alone).
    fn lt_recap(s: *mut LtSession, out: *mut LtRecap) -> i32 {
        match out.as_mut() {
            Some(o) => with(s, |h| h.recap(o) as i32),
            None => 0,
        }
    }

    /// The continent's fights, as the server last said (every 5 s): fills
    /// `out` (up to `cap`), returns how many there are.
    fn lt_fights(s: *mut LtSession, out: *mut LtFight, cap: i32) -> i32 {
        with(s, |h| h.fights(slice(out, cap)) as i32)
    }

    /// Kit `kit`'s weapon (a class's, `LT_CLASS_*`) into `out`; returns 1,
    /// or 0 for no such kit (`out` is then the assault rifle's).
    fn lt_weapon(kit: u8, out: *mut LtWeapon) -> i32 {
        let Some(o) = out.as_mut() else { return 0 };
        let w = lattice_game::weapon::of(kit);
        let steps = |n: f32| n / lattice_game::movement::TICK_HZ as f32;
        *o = LtWeapon {
            speed: w.speed,
            interval_secs: steps(w.interval as f32 / 256.0),
            damage: w.damage,
            far_damage: w.far_damage,
            falloff: w.falloff.map(|f| f.min(1e6)),
            pellets: w.pellets,
            magazine: w.magazine,
            reserve: w.reserve,
            pellet_cone: w.pellet_cone,
            range_secs: steps(w.range_steps as f32),
            reload_secs: steps(w.reload_steps as f32),
            reload_empty_secs: steps(w.reload_empty_steps as f32),
            cone: w.cone,
            recoil_up: w.recoil_up,
            recoil_first: w.recoil_first,
            recoil_side: w.recoil_side,
        };
        (kit < lattice_game::class::CLASSES) as i32
    }

    /// The net graph's numbers.
    fn lt_stats(s: *mut LtSession, out: *mut LtStats) -> () {
        if let Some(o) = out.as_mut() {
            *o = with(s, |h| Wrapped(h.stats())).0;
            if o.state == 0 && s.as_ref().is_none_or(|s| lock(&s.handle).is_none()) {
                o.state = STATE_ERROR;
            }
        }
    }

    /// Gaps between the last snapshots' arrivals (ms, oldest first): fills
    /// `out` (up to `cap`), returns how many it wrote.
    fn lt_snapshot_gaps(s: *mut LtSession, out: *mut f32, cap: i32) -> i32 {
        with(s, |h| {
            let gaps = &h.session.snapshot_gaps;
            let out = slice(out, cap);
            let n = out.len().min(gaps.len());
            out[..n].copy_from_slice(&gaps[gaps.len() - n..]);
            n as i32
        })
    }

    /// The report for `lattice-report.txt` (who, where, then the
    /// connection's numbers): copied into `buf`; returns its length.
    fn lt_summary(s: *mut LtSession, buf: *mut u8, cap: i32) -> i32 {
        with(s, |h| put_text(&h.summary(), buf, cap))
    }

    /// A world from a seed (the server's is in its Welcome), shared with any
    /// session in the process that uses it. Free with `lt_world_free`.
    fn lt_world_new(seed: u64) -> *mut LtWorld {
        Box::into_raw(Box::new(LtWorld { world: World::shared(seed) }))
    }

    /// The session's world, once welcomed (else null). Free with
    /// `lt_world_free`.
    fn lt_session_world(s: *mut LtSession) -> *mut LtWorld {
        with(s, |h| h.world().map_or(std::ptr::null_mut(), |world| Box::into_raw(Box::new(LtWorld { world }))))
    }

    fn lt_world_free(w: *mut LtWorld) -> () {
        if !w.is_null() {
            drop(Box::from_raw(w));
        }
    }

    fn lt_world_info(w: *const LtWorld, out: *mut LtWorldInfo) -> () {
        if let (Some(w), Some(o)) = (world(w), out.as_mut()) {
            *o = LtWorldInfo {
                seed: w.seed,
                digest: w.digest(),
                size: lattice_game::movement::WORLD_SIZE,
                res: TERRAIN_RES,
                samples: TERRAIN_N as u32,
                boxes: w.boxes().len() as u32,
                sites: w.sites().len() as u32,
                continent: w.continent as u8,
                lanes: w.lanes().len() as u32,
                lane_points: w.lanes().iter().map(|l| l.path.len() as u32).sum(),
                points: w.points().len() as u32,
                volumes: w.volumes().len() as u32,
                holes: if w.has_holes() { (0..CELLS).map(|y| (0..CELLS).filter(|&x| w.hole_cell(x, y)).count() as u32).sum() } else { 0 },
            };
        }
    }

    /// Terrain heights (m) of the `nx` × `ny` samples from sample (`x0`,
    /// `y0`), row by row (y north), into `out`; samples past the edge repeat
    /// it. Returns how many it wrote.
    fn lt_world_heights(w: *const LtWorld, x0: i32, y0: i32, nx: i32, ny: i32, out: *mut f32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        if out.is_null() || nx <= 0 || ny <= 0 {
            return 0;
        }
        let out = std::slice::from_raw_parts_mut(out, nx as usize * ny as usize);
        let last = TERRAIN_N as i32 - 1;
        for iy in 0..ny {
            let sy = (y0 + iy).clamp(0, last) as usize;
            for ix in 0..nx {
                let sx = (x0 + ix).clamp(0, last) as usize;
                out[(iy * nx + ix) as usize] = w.sample(sx, sy);
            }
        }
        nx * ny
    }

    /// Every cover box: fills `out` (up to `cap`), returns how many there are.
    fn lt_world_boxes(w: *const LtWorld, out: *mut LtBox, cap: i32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        for (o, b) in slice(out, cap).iter_mut().zip(w.boxes()) {
            *o = LtBox {
                min: [b.min[0], b.min[1], b.bottom].into(),
                max: [b.max[0], b.max[1], b.top].into(),
                center_x: b.center[0],
                center_y: b.center[1],
                half_along: b.half[0],
                half_across: b.half[1],
                axis_x: b.axis[0],
                axis_y: b.axis[1],
                low: b.low,
                kind: b.kind as u8,
                facing: b.facing,
                pass: b.pass,
                site: b.site,
                when: b.when,
            };
        }
        w.boxes().len() as i32
    }

    /// Every base and outpost: fills `out` (up to `cap`), returns how many.
    fn lt_world_sites(w: *const LtWorld, out: *mut LtSite, cap: i32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        for (o, s) in slice(out, cap).iter_mut().zip(w.sites()) {
            *o = LtSite { x: s.center[0], y: s.center[1], half: s.half, level: s.level, base: s.base as u8, kind: s.kind as u8, sector: s.sector };
        }
        w.sites().len() as i32
    }

    /// Every route (lanes, and ways inside sites): fills `out` (up to `cap`),
    /// returns how many there are. Their points are `lt_world_lane_points`.
    fn lt_world_lanes(w: *const LtWorld, out: *mut LtLane, cap: i32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        let mut first = 0u32;
        let all: Vec<LtLane> = w
            .lanes()
            .iter()
            .map(|l| {
                let lane = LtLane { a: l.a, b: l.b, first, count: l.path.len() as u32, one_way: l.one_way as u8, when: l.when };
                first += l.path.len() as u32;
                lane
            })
            .collect();
        for (o, l) in slice(out, cap).iter_mut().zip(&all) {
            *o = *l;
        }
        all.len() as i32
    }

    /// Every route's points, one route after another (feet height in z):
    /// fills `out` (up to `cap`), returns how many there are.
    fn lt_world_lane_points(w: *const LtWorld, out: *mut LtVec3, cap: i32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        let mut n = 0;
        let out = slice(out, cap);
        for p in w.lanes().iter().flat_map(|l| l.path.iter()) {
            if let Some(o) = out.get_mut(n) {
                *o = (*p).into();
            }
            n += 1;
        }
        n as i32
    }

    /// Every control point: fills `out` (up to `cap`), returns how many.
    fn lt_world_points(w: *const LtWorld, out: *mut LtPoint, cap: i32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        for (o, p) in slice(out, cap).iter_mut().zip(w.points()) {
            *o = LtPoint { pos: p.pos.into(), site: p.site };
        }
        w.points().len() as i32
    }

    /// Every force volume: fills `out` (up to `cap`), returns how many.
    fn lt_world_volumes(w: *const LtWorld, out: *mut LtVolume, cap: i32) -> i32 {
        let Some(w) = world(w) else { return 0 };
        for (o, v) in slice(out, cap).iter_mut().zip(w.volumes()) {
            let c = v.center();
            let half = if v.axis == lattice_game::world::UNTURNED { [(v.max[0] - v.min[0]) * 0.5, (v.max[1] - v.min[1]) * 0.5] } else { v.half };
            *o = LtVolume {
                min: v.min.into(),
                max: v.max.into(),
                center_x: c[0],
                center_y: c[1],
                half_along: half[0],
                half_across: half[1],
                axis_x: v.axis[0],
                axis_y: v.axis[1],
                vel: v.vel.into(),
                force: v.force as u8,
                when: v.when,
                phase: v.phase,
            };
        }
        w.volumes().len() as i32
    }

    /// Whether each of the `nx` × `ny` height cells from cell (`x0`, `y0`)
    /// (the cell between samples x..x+1, y..y+1) is a hole in the terrain: 1
    /// or 0, row by row, into `out`. Returns how many it wrote.
    fn lt_world_holes(w: *const LtWorld, x0: i32, y0: i32, nx: i32, ny: i32, out: *mut u8) -> i32 {
        let Some(w) = world(w) else { return 0 };
        if out.is_null() || nx <= 0 || ny <= 0 {
            return 0;
        }
        let out = std::slice::from_raw_parts_mut(out, nx as usize * ny as usize);
        let last = CELLS as i32 - 1;
        for iy in 0..ny {
            let cy = (y0 + iy).clamp(0, last) as usize;
            for ix in 0..nx {
                let cx = (x0 + ix).clamp(0, last) as usize;
                out[(iy * nx + ix) as usize] = w.hole_cell(cx, cy) as u8;
            }
        }
        nx * ny
    }

    /// What the ground is (`LT_GROUND_*`) at the `nx` × `ny` samples from
    /// sample (`x0`, `y0`), row by row, into `out`. Returns how many it wrote.
    fn lt_world_materials(w: *const LtWorld, x0: i32, y0: i32, nx: i32, ny: i32, out: *mut u8) -> i32 {
        let Some(w) = world(w) else { return 0 };
        if out.is_null() || nx <= 0 || ny <= 0 {
            return 0;
        }
        let out = std::slice::from_raw_parts_mut(out, nx as usize * ny as usize);
        let last = TERRAIN_N as i32 - 1;
        for iy in 0..ny {
            let sy = (y0 + iy).clamp(0, last) as usize;
            for ix in 0..nx {
                let sx = (x0 + ix).clamp(0, last) as usize;
                out[(iy * nx + ix) as usize] = w.ground_at(sx, sy).unwrap_or(255);
            }
        }
        nx * ny
    }

    /// The terrain's height at (`x`, `y`), as movement samples it.
    fn lt_world_height(w: *const LtWorld, x: f32, y: f32) -> f32 {
        world(w).map_or(0.0, |w| w.terrain(x, y))
    }

    /// The highest surface at (`x`, `y`) feet at `z` can stand on: the
    /// terrain, or a box's top no higher than `z + reach`.
    fn lt_world_ground(w: *const LtWorld, x: f32, y: f32, z: f32, reach: f32) -> f32 {
        world(w).map_or(0.0, |w| w.ground(x, y, z, reach, lattice_game::clock::Live::OFF))
    }

    /// Where the segment `from` -> `to` first hits the ground or cover
    /// (shots' rules; every shield stops it): 1 and `out` filled, or 0.
    fn lt_world_raycast(w: *const LtWorld, from: *const LtVec3, to: *const LtVec3, out: *mut LtHit) -> i32 {
        let (Some(w), Some(a), Some(b)) = (world(w), from.as_ref(), to.as_ref()) else { return 0 };
        let (a, b): ([f32; 3], [f32; 3]) = ((*a).into(), (*b).into());
        match segment_hit(w, a, b, lattice_game::movement::NO_FACTION) {
            Some(hit) => {
                if let Some(o) = out.as_mut() {
                    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    let t = if len2 > 0.0 { ((hit.pos[0] - a[0]) * d[0] + (hit.pos[1] - a[1]) * d[1] + (hit.pos[2] - a[2]) * d[2]) / len2 } else { 0.0 };
                    *o = LtHit { pos: hit.pos.into(), normal: hit.normal.into(), t, surface: hit.cover.unwrap_or(SURFACE_TERRAIN) };
                }
                1
            }
            None => 0,
        }
    }
}

/// A struct returned through `with` (which needs a fallback for it).
struct Wrapped<T>(T);

impl<T: Default> Fallback for Wrapped<T> {
    fn fallback() -> Self {
        Wrapped(T::default())
    }
}
