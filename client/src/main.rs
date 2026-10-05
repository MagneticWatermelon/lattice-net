//! lattice-client: the first playable client. A Bevy window on
//! `lattice-client-core`, the same client code the bots run.

mod controls;
mod coords;
mod hud;
mod net;
mod scene;
mod terrain;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::window::PresentMode;
use lattice_client_core::ClientConfig;

use crate::controls::{Mode, View};
use crate::net::Session;

const USAGE: &str = "\
lattice-client: play on a lattice-server

  --server ADDR        the server [127.0.0.1:40000]; from Windows, a WSL server is at
                       the WSL IP (`hostname -I` in WSL) and binds 0.0.0.0 by default
  --user N             user id for the dev connect token [random]
  --token-key HEX      the server's --token-key [the public dev key]
  --server-id N        [1]
  --near-ms MS         render delay of near players behind the newest server step, at
                       least [67]; it grows to cover how late near updates come, up to
  --near-max-ms MS     [133]
  --mid-ms MS          render delay of mid and far players [200]
  --view first|chase|spectator   starting view [first]
  --spectate X,Y,Z,YAW,PITCH     start in free flight there (meters, degrees)
  --no-vsync           present as fast as possible (frame times in the net graph)
  --no-shadows
  --ghosts             start with the server ghosts on (G)
  --autoplay           wander instead of reading the keyboard
  --autofire           hold the trigger (with --autoplay: a self-check)
  --screenshot PATH    save a frame to PATH after --after seconds [5], then
  --exit-after S       quit after S seconds, printing a summary";

/// The connection, as a Bevy resource.
#[derive(Resource)]
pub struct Net(pub Session);

/// This frame's clock: one `Instant` for every system.
#[derive(Resource)]
pub struct Frame {
    pub now: Instant,
    pub start: Instant,
    /// Seconds since start, and since the last frame.
    pub secs: f32,
    pub dt: f32,
}

#[derive(Resource)]
pub struct Settings {
    pub ghosts: bool,
    pub tier_colors: bool,
    pub net_graph: bool,
    pub shadows: bool,
    pub autoplay: bool,
    pub autofire: bool,
    screenshot: Option<(String, f32)>,
    exit_after: Option<f32>,
}

/// Recent frame times (ms), for the net graph and the exit summary.
#[derive(Resource, Default)]
pub struct FrameTimes {
    pub recent: Vec<f32>,
    pub all: Vec<f32>,
    pub fps: f32,
    pub p50: f32,
    pub p99: f32,
}

struct Args {
    server: SocketAddr,
    user: u64,
    key: [u8; 32],
    server_id: u64,
    /// Near (least, most) and mid render delays.
    delays: (Duration, Duration, Duration),
    mode: Mode,
    spectate: Option<([f32; 3], f32, f32)>,
    vsync: bool,
    shadows: bool,
    ghosts: bool,
    autoplay: bool,
    autofire: bool,
    screenshot: Option<(String, f32)>,
    exit_after: Option<f32>,
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}\n\n{USAGE}");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut a = Args {
        server: "127.0.0.1:40000".parse().unwrap(),
        user: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64) | 1 << 40,
        key: lattice_net::token::DEV_TOKEN_KEY,
        server_id: 1,
        delays: (Duration::from_secs(2) / 30, Duration::from_secs(4) / 30, Duration::from_millis(200)),
        mode: Mode::FirstPerson,
        spectate: None,
        vsync: true,
        shadows: true,
        ghosts: false,
        autoplay: false,
        autofire: false,
        screenshot: None,
        exit_after: None,
    };
    let mut after = 5.0;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| die(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--server" => a.server = val().parse().unwrap_or_else(|e| die(&format!("--server: {e}"))),
            "--user" => a.user = val().parse().unwrap_or_else(|e| die(&format!("--user: {e}"))),
            "--server-id" => a.server_id = val().parse().unwrap_or_else(|e| die(&format!("--server-id: {e}"))),
            "--token-key" => {
                let v = val();
                if v.len() != 64 {
                    die("--token-key: expected 64 hex digits");
                }
                for (i, k) in a.key.iter_mut().enumerate() {
                    *k = u8::from_str_radix(&v[2 * i..2 * i + 2], 16).unwrap_or_else(|e| die(&format!("--token-key: {e}")));
                }
            }
            "--near-ms" => a.delays.0 = Duration::from_secs_f64(val().parse::<f64>().unwrap_or_else(|e| die(&format!("--near-ms: {e}"))) / 1000.0),
            "--near-max-ms" => a.delays.1 = Duration::from_secs_f64(val().parse::<f64>().unwrap_or_else(|e| die(&format!("--near-max-ms: {e}"))) / 1000.0),
            "--mid-ms" => a.delays.2 = Duration::from_secs_f64(val().parse::<f64>().unwrap_or_else(|e| die(&format!("--mid-ms: {e}"))) / 1000.0),
            "--view" => {
                a.mode = match val().as_str() {
                    "first" => Mode::FirstPerson,
                    "chase" => Mode::Chase,
                    "spectator" => Mode::Spectator,
                    v => die(&format!("--view: unknown view {v:?}")),
                }
            }
            "--spectate" => a.spectate = Some(controls::parse_spectate(&val()).unwrap_or_else(|| die("--spectate: X,Y,Z,YAW,PITCH"))),
            "--no-vsync" => a.vsync = false,
            "--no-shadows" => a.shadows = false,
            "--ghosts" => a.ghosts = true,
            "--autoplay" => a.autoplay = true,
            "--autofire" => a.autofire = true,
            "--screenshot" => a.screenshot = Some((val(), 0.0)),
            "--after" => after = val().parse().unwrap_or_else(|e| die(&format!("--after: {e}"))),
            "--exit-after" => a.exit_after = Some(val().parse().unwrap_or_else(|e| die(&format!("--exit-after: {e}")))),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            f => die(&format!("unknown option {f}")),
        }
    }
    if let Some(s) = &mut a.screenshot {
        s.1 = after;
    }
    a
}

fn main() {
    let args = parse_args();
    let now = Instant::now();
    let token = net::dev_token(&args.key, args.server_id, args.user);
    let cfg = ClientConfig { near_delay: args.delays.0, near_delay_max: args.delays.1, mid_delay: args.delays.2, track_entities: true };
    let mut session = Session::connect(args.server, token, cfg, now).unwrap_or_else(|e| die(&format!("socket: {e}")));
    session.core.keep_news(true);
    println!("connecting to {} as user {}", args.server, args.user);
    let mut view = View::new(args.mode);
    if let Some((eye, yaw, pitch)) = args.spectate {
        view.spectate(eye, yaw, pitch);
    }

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "lattice".into(),
                resolution: (1280, 720).into(),
                present_mode: if args.vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync },
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.62, 0.74, 0.86)))
        .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 250.0, ..default() })
        .insert_resource(Net(session))
        .insert_resource(Frame { now, start: now, secs: 0.0, dt: 0.0 })
        .insert_resource(Settings {
            ghosts: args.ghosts,
            tier_colors: false,
            net_graph: true,
            shadows: args.shadows,
            autoplay: args.autoplay,
            autofire: args.autofire,
            screenshot: args.screenshot,
            exit_after: args.exit_after,
        })
        .insert_resource(view)
        .init_resource::<FrameTimes>()
        .init_resource::<scene::Scene>()
        .init_resource::<hud::Rates>()
        .init_resource::<controls::Tracers>()
        .init_resource::<hud::Combat>()
        .add_systems(Startup, (setup_camera, scene::setup_looks, hud::setup))
        .add_systems(First, begin_frame)
        .add_systems(PreUpdate, poll)
        .add_systems(
            Update,
            (
                controls::toggles,
                controls::play,
                scene::build_world,
                controls::place_camera,
                scene::stream_terrain,
                scene::sync_players,
                scene::own_body,
                controls::tracers,
                hud::update,
                hud::vitals,
                hud::combat,
                shots,
            )
                .chain(),
        )
        .add_systems(PostUpdate, flush)
        .run();
}

fn setup_camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { fov: 80f32.to_radians(), far: 6000.0, ..default() }),
        DistanceFog {
            color: Color::srgb(0.62, 0.74, 0.86),
            falloff: FogFalloff::Linear { start: 900.0, end: 5000.0 },
            ..default()
        },
        Transform::from_xyz(4096.0, 300.0, -4096.0),
    ));
}

fn begin_frame(mut frame: ResMut<Frame>, mut times: ResMut<FrameTimes>) {
    // A stalled frame (window dragged) counts as at most MAX_FRAME.
    let now = Instant::now().min(frame.now + net::MAX_FRAME.max(Duration::from_secs(3600)));
    let dt = (now - frame.now).min(net::MAX_FRAME).as_secs_f32();
    *frame = Frame { now, start: frame.start, secs: (now - frame.start).as_secs_f32(), dt };
    if dt > 0.0 {
        let ms = dt * 1000.0;
        if times.recent.len() == net::GAPS {
            times.recent.remove(0);
        }
        times.recent.push(ms);
        times.all.push(ms);
        let mut v = times.recent.clone();
        v.sort_by(f32::total_cmp);
        let at = |p: f32| v[((v.len() as f32 * p) as usize).min(v.len() - 1)];
        let (p50, p99) = (at(0.5), at(0.99));
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        (times.p50, times.p99, times.fps) = (p50, p99, 1000.0 / mean);
    }
}

fn poll(frame: Res<Frame>, mut net: ResMut<Net>) {
    if let Err(e) = net.0.poll(frame.now) {
        warn!("receive: {e}");
    }
}

fn flush(frame: Res<Frame>, mut net: ResMut<Net>) {
    net.0.flush(frame.now);
}

/// `--screenshot` and `--exit-after`.
fn shots(mut commands: Commands, frame: Res<Frame>, mut settings: ResMut<Settings>, mut net: ResMut<Net>, times: Res<FrameTimes>, mut exit: MessageWriter<AppExit>) {
    if let Some((path, at)) = settings.screenshot.clone() {
        if frame.secs >= at {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            settings.screenshot = None;
        }
    }
    if settings.exit_after.is_some_and(|t| frame.secs >= t) {
        settings.exit_after = None;
        summary(&net.0, &times);
        net.0.disconnect(frame.now);
        exit.write(AppExit::Success);
    }
}

fn summary(s: &Session, times: &FrameTimes) {
    let st = &s.core.stats;
    let mut all = times.all.clone();
    all.sort_by(f32::total_cmp);
    let at = |p: f32| all.get(((all.len() as f32 * p) as usize).min(all.len().saturating_sub(1))).copied().unwrap_or(0.0);
    println!("== client summary ==");
    println!("  frames {} | frame time p50 {:.1} p99 {:.1} max {:.1} ms", all.len(), at(0.5), at(0.99), at(1.0));
    println!(
        "  snapshots {} | corrections {} (largest {:.3} m) | push corrections {} | resyncs {} | own correction offset largest {:.3} m | shots {} | deaths/respawns {}",
        st.snapshots, st.corrections, st.correction_error_max, st.push_corrections, st.resyncs, st.own_offset_max, st.shots, st.life_events
    );
    if let Some(e) = s.core.entities() {
        for (t, name) in ["near", "mid", "far"].iter().enumerate() {
            let f = e.smooth.frames[t];
            let n = f.iter().sum::<u64>().max(1) as f64;
            println!(
                "  {name}: {} entity-frames, {:.2}% interpolated, {:.2}% extrapolated, {:.2}% held, {:.2}% new",
                f.iter().sum::<u64>(),
                100.0 * f[0] as f64 / n,
                100.0 * f[1] as f64 / n,
                100.0 * f[2] as f64 / n,
                100.0 * f[3] as f64 / n
            );
        }
    }
    println!(
        "  hits confirmed {} (kills {}) | hit {} times for {} damage | kills heard {} | others' shots seen {}",
        st.hits_confirmed, st.kills_confirmed, st.hurts, st.damage_taken, st.kills_heard, st.shots_seen
    );
    let delay = st.render_delay_sum / st.render_frames.max(1) as f64 * 1000.0 / 30.0;
    let mid = s.core.entities().map_or(0.0, |e| e.mid_lag()) * 1000.0 / 30.0;
    println!("  render delay near {delay:.1} ms, mid/far +{mid:.0} ms, clock snaps {}", s.core.render_clock().snaps);
}
