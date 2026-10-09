//! lattice-client: the first playable client. A Bevy window on
//! `lattice-client-core`, the same client code the bots run.

mod controls;
mod coords;
mod gun;
mod hud;
mod models;
mod net;
mod scene;
mod terrain;

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::window::PresentMode;
use lattice_client_core::invite::Invite;
use lattice_client_core::ClientConfig;
use lattice_net::ConnectToken;

use crate::controls::{Mode, View};
use crate::net::Session;

const USAGE: &str = "\
lattice-client: play on a lattice-server

  --invite FILE        play with a playtest invite (lattice-invite): the server, who you
                       are, and a connect token for each launch, instead of the four below
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
  --autoaim            hold the sights up (a self-check)
  --viewer             no game: a row of soldiers (bind pose, then each animation) beside
                       their collision capsules, to look at the models
  --screenshot PATH    save a frame to PATH after --after seconds [5], then
  --exit-after S       quit after S seconds, printing a summary

However it ends, the game appends that summary (frame times, connection, corrections,
smoothness, combat) to lattice-report.txt in the folder it was started in.";

/// Where the summary goes on exit, for playtesters to send back.
const REPORT: &str = "lattice-report.txt";

/// The connection, as a Bevy resource.
#[derive(Resource)]
pub struct Net(pub Session);

/// Who's playing, and where: for the report.
#[derive(Resource)]
pub struct Player {
    pub server: String,
    pub user: u64,
    pub name: String,
}

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
    pub autoaim: bool,
    pub viewer: bool,
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
    invite: Option<String>,
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
    autoaim: bool,
    viewer: bool,
    screenshot: Option<(String, f32)>,
    exit_after: Option<f32>,
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}\n\n{USAGE}");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut a = Args {
        invite: None,
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
        autoaim: false,
        viewer: false,
        screenshot: None,
        exit_after: None,
    };
    let mut after = 5.0;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| die(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--invite" => a.invite = Some(val()),
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
            "--autoaim" => a.autoaim = true,
            "--viewer" => a.viewer = true,
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

/// The server, a token and who we are, from a playtest invite: the next of
/// its tokens (each connects once).
fn from_invite(path: &str) -> (SocketAddr, ConnectToken, Player) {
    let fail = |e: &dyn std::fmt::Display| -> ! {
        eprintln!("error: --invite {path}: {e}");
        std::process::exit(2);
    };
    let invite = std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| Invite::parse(&t)).unwrap_or_else(|e| fail(&e));
    let server = invite.server.to_socket_addrs().ok().and_then(|mut a| a.next()).unwrap_or_else(|| fail(&format!("can't find {}", invite.server)));
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (i, token) = match invite.next_token(std::path::Path::new(path), unix) {
        Ok(Some(t)) => t,
        Ok(None) => fail(&"no tokens left (each connects once, and they expire): ask for a new invite"),
        Err(e) => fail(&format!("counting its used tokens: {e}")),
    };
    println!("token {} of {} from {path}", i + 1, invite.tokens.len());
    (server, token, Player { server: invite.server, user: invite.user, name: invite.name })
}

fn main() {
    let args = parse_args();
    let now = Instant::now();
    let (server, token, player) = match &args.invite {
        Some(path) => from_invite(path),
        None => {
            let player = Player { server: args.server.to_string(), user: args.user, name: String::new() };
            (args.server, net::dev_token(&args.key, args.server_id, args.user), player)
        }
    };
    let cfg = ClientConfig { near_delay: args.delays.0, near_delay_max: args.delays.1, mid_delay: args.delays.2, track_entities: true };
    let mut session = Session::connect(server, token, cfg, now).unwrap_or_else(|e| die(&format!("socket: {e}")));
    session.core.keep_news(true);
    println!("connecting to {server} as {} (user {})", if player.name.is_empty() { "-" } else { &player.name }, player.user);
    let mut view = View::new(args.mode);
    if let Some((eye, yaw, pitch)) = args.spectate {
        view.spectate(eye, yaw, pitch);
    } else if args.viewer {
        // In front of the row of soldiers, at eye height.
        view.spectate([0.0, -7.0, 1.5], std::f32::consts::FRAC_PI_2, -0.08);
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
        .insert_resource(player)
        .insert_resource(Frame { now, start: now, secs: 0.0, dt: 0.0 })
        .insert_resource(Settings {
            ghosts: args.ghosts,
            tier_colors: false,
            net_graph: true,
            shadows: args.shadows,
            autoplay: args.autoplay,
            autofire: args.autofire,
            autoaim: args.autoaim,
            viewer: args.viewer,
            screenshot: args.screenshot,
            exit_after: args.exit_after,
        })
        .insert_resource(view)
        .init_resource::<FrameTimes>()
        .init_resource::<scene::Scene>()
        .init_resource::<hud::Rates>()
        .init_resource::<controls::Tracers>()
        .init_gizmo_group::<controls::TracerGizmos>()
        .init_resource::<hud::Combat>()
        .init_resource::<gun::Gun>()
        .init_resource::<gun::FireTimes>()
        .add_systems(
            Startup,
            (
                (models::load, setup_camera, models::spawn_viewer.run_if(|s: Res<Settings>| s.viewer)).chain(),
                scene::setup_looks,
                hud::setup,
                gun::setup,
                controls::setup_tracer_gizmos,
            ),
        )
        .add_systems(First, begin_frame)
        .add_systems(PreUpdate, poll)
        .add_systems(
            Update,
            (
                controls::toggles,
                gun::aim,
                controls::play,
                scene::build_world,
                controls::place_camera,
                scene::stream_terrain,
                scene::sync_players,
                controls::viewmodel,
                gun::viewmodel,
                gun::reticle,
                controls::tracers,
                hud::update,
                hud::vitals,
                hud::combat,
                shots,
            )
                .chain(),
        )
        .add_systems(PostUpdate, flush)
        .add_systems(Last, report_on_exit)
        // Rifles go to their hands once the skeletons are posed.
        .add_systems(
            PostUpdate,
            models::hold_rifles
                .after(bevy::transform::TransformSystems::Propagate)
                .before(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
        )
        .run();
}

fn setup_camera(mut commands: Commands, models: Res<models::Models>) {
    commands
        .spawn((
            controls::MainCamera,
            Camera3d::default(),
            Projection::Perspective(PerspectiveProjection { fov: 80f32.to_radians(), far: 6000.0, ..default() }),
            DistanceFog {
                color: Color::srgb(0.62, 0.74, 0.86),
                falloff: FogFalloff::Linear { start: 900.0, end: 5000.0 },
                ..default()
            },
            Transform::from_xyz(4096.0, 300.0, -4096.0),
        ))
        .with_children(|cam| {
            // The first-person rifle, drawn after the world by its own
            // camera (depth cleared), so it never sinks into a wall.
            cam.spawn((
                Camera3d::default(),
                Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
                Projection::Perspective(PerspectiveProjection { fov: 70f32.to_radians(), near: 0.01, far: 10.0, ..default() }),
                bevy::camera::visibility::RenderLayers::layer(models::VIEWMODEL_LAYER),
            ))
            .with_children(|cam| {
                cam.spawn((controls::ViewModel, models::viewmodel(&models))).with_child(gun::view_flash(&models));
            });
        });
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
fn shots(mut commands: Commands, frame: Res<Frame>, mut settings: ResMut<Settings>, net: Res<Net>, times: Res<FrameTimes>, mut exit: MessageWriter<AppExit>) {
    if let Some((path, at)) = settings.screenshot.clone() {
        if frame.secs >= at {
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            settings.screenshot = None;
        }
    }
    if settings.exit_after.is_some_and(|t| frame.secs >= t) {
        settings.exit_after = None;
        print!("{}", summary(&net.0, &times));
        exit.write(AppExit::Success); // (`report_on_exit` disconnects)
    }
}

/// However the game ends (the window closed, `--exit-after`): tells the
/// server at once, so its session log closes the session, and appends the
/// summary to `REPORT`.
fn report_on_exit(mut exits: MessageReader<AppExit>, frame: Res<Frame>, mut net: ResMut<Net>, times: Res<FrameTimes>, player: Res<Player>, mut done: Local<bool>) {
    if exits.read().next().is_none() || std::mem::replace(&mut *done, true) {
        return;
    }
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let who = if player.name.is_empty() { format!("user {}", player.user) } else { format!("{} (user {})", player.name, player.user) };
    // Before disconnecting: the transport's numbers go with the connection.
    let text = format!("== {} | {who} on {} | played {:.0} s ==\n{}\n", utc(unix), player.server, frame.secs, summary(&net.0, &times));
    if net.0.state() == lattice_net::ClientState::Connected {
        net.0.disconnect(frame.now);
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(REPORT).and_then(|mut f| f.write_all(text.as_bytes())) {
        Ok(()) => println!("summary appended to {REPORT}"),
        Err(e) => eprintln!("couldn't write {REPORT}: {e}"),
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC` for unix seconds (Howard Hinnant's
/// civil_from_days; no time zone data).
fn utc(unix: u64) -> String {
    let (days, secs) = ((unix / 86_400) as i64, unix % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", secs / 3600, secs / 60 % 60, secs % 60)
}

fn summary(s: &Session, times: &FrameTimes) -> String {
    use std::fmt::Write;
    let st = &s.core.stats;
    let mut all = times.all.clone();
    all.sort_by(f32::total_cmp);
    let at = |p: f32| all.get(((all.len() as f32 * p) as usize).min(all.len().saturating_sub(1))).copied().unwrap_or(0.0);
    let mut out = String::from("== client summary ==\n");
    let _ = writeln!(out, "  frames {} | frame time p50 {:.1} p99 {:.1} max {:.1} ms", all.len(), at(0.5), at(0.99), at(1.0));
    if let Some(t) = s.transport().stats() {
        let _ = writeln!(
            out,
            "  rtt {:.1} ms (last seconds {:.1}-{:.1}) | loss {:.2}% | packets sent {} received {} lost {}",
            t.rtt_ms,
            t.rtt_min_ms,
            t.rtt_max_ms,
            t.loss * 100.0,
            t.packets_sent,
            t.packets_received,
            t.packets_lost
        );
    }
    let _ = writeln!(
        out,
        "  snapshots {} | corrections {} (largest {:.3} m) | push corrections {} | resyncs {} | own correction offset largest {:.3} m | shots {} | deaths/respawns {}",
        st.snapshots, st.corrections, st.correction_error_max, st.push_corrections, st.resyncs, st.own_offset_max, st.shots, st.life_events
    );
    if let Some(e) = s.core.entities() {
        for (t, name) in ["near", "mid", "far"].iter().enumerate() {
            let f = e.smooth.frames[t];
            let n = f.iter().sum::<u64>().max(1) as f64;
            let _ = writeln!(
                out,
                "  {name}: {} entity-frames, {:.2}% interpolated, {:.2}% extrapolated, {:.2}% held, {:.2}% new",
                f.iter().sum::<u64>(),
                100.0 * f[0] as f64 / n,
                100.0 * f[1] as f64 / n,
                100.0 * f[2] as f64 / n,
                100.0 * f[3] as f64 / n
            );
        }
    }
    let _ = writeln!(
        out,
        "  hits confirmed {} (kills {}) | hit {} times for {} damage | kills heard {} | others' shots seen {} | distant fights: {} cells, {} shots, {} ambient",
        st.hits_confirmed, st.kills_confirmed, st.hurts, st.damage_taken, st.kills_heard, st.shots_seen, st.activity_cells, st.activity_shots, st.ambient
    );
    let delay = st.render_delay_sum / st.render_frames.max(1) as f64 * 1000.0 / 30.0;
    let mid = s.core.entities().map_or(0.0, |e| e.mid_lag()) * 1000.0 / 30.0;
    let _ = writeln!(out, "  render delay near {delay:.1} ms, mid/far +{mid:.0} ms, clock snaps {}", s.core.render_clock().snaps);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_dates() {
        assert_eq!(super::utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(super::utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(super::utc(1_791_552_658), "2026-10-09 13:30:58 UTC");
        assert_eq!(super::utc(4_102_444_800), "2100-01-01 00:00:00 UTC");
    }
}
