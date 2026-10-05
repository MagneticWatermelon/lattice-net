//! The net graph (N): connection, server, timeline and smoothness numbers,
//! and sparklines of frame time and snapshot arrival gaps. Plus a crosshair
//! and the key help.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::text::FontSize;
use lattice_net::ClientState;

use crate::net::GAPS;
use crate::scene::Scene;
use crate::{FrameTimes, Net, Settings};

/// Bars per sparkline, and their height in px at full scale.
const BARS: usize = GAPS;
const BAR_H: f32 = 40.0;

#[derive(Component)]
pub struct GraphText;
#[derive(Component)]
pub struct GraphPanel;
#[derive(Component)]
pub struct FrameBar(usize);
#[derive(Component)]
pub struct HealthFill;
#[derive(Component)]
pub struct HitMark;
#[derive(Component)]
pub struct DamageArrow(usize);
#[derive(Component)]
pub struct KillFeed;

/// What the combat UI shows, from the news.
#[derive(Resource, Default)]
pub struct Combat {
    /// The last hit marker: (when, head, killed).
    mark: Option<(f32, bool, bool)>,
    /// Hits taken: (when, direction in 1/256 turns, as the game's yaw).
    hurt: Vec<(f32, u8)>,
    /// Kills heard of: (when, killer, victim, head).
    feed: Vec<(f32, u16, u16, bool)>,
}

const FACTION_NAMES: [&str; 3] = ["Red", "Blue", "Purple"];
/// How long marks, arrows and feed lines stay, in seconds.
const MARK_SECS: f32 = 0.25;
const ARROW_SECS: f32 = 1.2;
const FEED_SECS: f32 = 6.0;
const ARROW_RADIUS: f32 = 70.0;
#[derive(Component)]
pub struct DeathText;
#[derive(Component)]
pub struct GapBar(usize);

/// Last second's counters, for rates.
#[derive(Resource)]
pub struct Rates {
    at: Instant,
    text_at: Instant,
    bytes: (u64, u64),
    frames: [[u64; 4]; 3],
    pub down_kbps: f32,
    pub up_kbps: f32,
    /// Share of entity-frames interpolated per tier over the last second.
    pub interpolated: [f32; 3],
}

impl Default for Rates {
    fn default() -> Self {
        Self { at: Instant::now(), text_at: Instant::now() - Duration::from_secs(1), bytes: (0, 0), frames: [[0; 4]; 3], down_kbps: 0.0, up_kbps: 0.0, interpolated: [1.0; 3] }
    }
}

pub fn setup(mut commands: Commands) {
    let font = TextFont { font_size: FontSize::Px(13.0), ..default() };
    commands
        .spawn((
            GraphPanel,
            Node { position_type: PositionType::Absolute, left: Val::Px(8.0), top: Val::Px(8.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), padding: UiRect::all(Val::Px(6.0)), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        ))
        .with_children(|p| {
            p.spawn((GraphText, Text::new("connecting..."), font.clone(), TextColor(Color::WHITE)));
            for (label, frame) in [("frame time (0-33 ms)", true), ("snapshot gaps (0-100 ms)", false)] {
                p.spawn((Text::new(label), TextFont { font_size: FontSize::Px(11.0), ..default() }, TextColor(Color::srgb(0.7, 0.7, 0.7))));
                p.spawn(Node { height: Val::Px(BAR_H), align_items: AlignItems::FlexEnd, column_gap: Val::Px(1.0), ..default() }).with_children(|row| {
                    for i in 0..BARS {
                        let node = Node { width: Val::Px(2.0), height: Val::Px(1.0), ..default() };
                        if frame {
                            row.spawn((FrameBar(i), node, BackgroundColor(Color::srgb(0.4, 0.9, 0.5))));
                        } else {
                            row.spawn((GapBar(i), node, BackgroundColor(Color::srgb(0.4, 0.7, 1.0))));
                        }
                    }
                });
            }
        });
    // Health, bottom center; the death notice in the middle.
    commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), bottom: Val::Px(28.0), margin: UiRect::left(Val::Px(-100.0)), width: Val::Px(200.0), height: Val::Px(10.0), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.5)),
        ))
        .with_child((HealthFill, Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(Color::srgb(0.3, 0.85, 0.4))));
    commands.spawn((
        DeathText,
        Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(40.0), margin: UiRect::left(Val::Px(-160.0)), width: Val::Px(320.0), justify_content: JustifyContent::Center, ..default() },
        Text::new(""),
        TextFont { font_size: FontSize::Px(24.0), ..default() },
        TextColor(Color::srgb(1.0, 0.85, 0.8)),
    ));
    // Hit marker over the crosshair; damage arrows around it; the kill feed.
    commands.spawn((
        HitMark,
        Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), margin: UiRect { left: Val::Px(-9.0), top: Val::Px(-16.0), ..default() }, ..default() },
        Text::new(""),
        TextFont { font_size: FontSize::Px(26.0), ..default() },
        TextColor(Color::WHITE),
    ));
    for i in 0..4 {
        commands.spawn((
            DamageArrow(i),
            Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), width: Val::Px(12.0), height: Val::Px(12.0), ..default() },
            BackgroundColor(Color::srgba(0.95, 0.15, 0.1, 0.0)),
        ));
    }
    commands.spawn((
        KillFeed,
        Node { position_type: PositionType::Absolute, right: Val::Px(12.0), top: Val::Px(10.0), ..default() },
        Text::new(""),
        TextFont { font_size: FontSize::Px(15.0), ..default() },
        TextColor(Color::WHITE),
    ));
    // Crosshair and help.
    commands.spawn((
        Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), margin: UiRect { left: Val::Px(-2.0), top: Val::Px(-2.0), ..default() }, width: Val::Px(4.0), height: Val::Px(4.0), ..default() },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.8)),
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, left: Val::Px(8.0), bottom: Val::Px(6.0), ..default() },
        Text::new("click: grab mouse, then fire  Esc: release  WASD Shift Space: move  V: view  G: server ghosts  T: tier/faction colors  N: net graph"),
        TextFont { font_size: FontSize::Px(12.0), ..default() },
        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.7)),
    ));
}

type MarkText<'a> = (&'a mut Text, &'a mut TextColor, &'a mut TextFont);

/// Takes the news: hit markers, damage arrows and the kill feed; others'
/// shots and distant fights' ambient shots go to the tracers.
#[allow(clippy::too_many_arguments)]
pub fn combat(
    mut net: ResMut<Net>,
    frame: Res<crate::Frame>,
    view: Res<crate::View>,
    mut combat: ResMut<Combat>,
    mut tracers: ResMut<crate::controls::Tracers>,
    mut mark: Query<MarkText, (With<HitMark>, Without<KillFeed>)>,
    mut arrows: Query<(&DamageArrow, &mut Node, &mut BackgroundColor)>,
    mut feed: Query<&mut Text, (With<KillFeed>, Without<HitMark>)>,
) {
    use lattice_game::events::Event;
    let (mut events, mut shots) = (Vec::new(), Vec::new());
    net.0.core.drain_news(&mut events, &mut shots);
    let now = frame.secs;
    tracers.pending.extend(shots.into_iter().map(|s| (s, now)));
    // Distant fights: ambient shots from players drawn there (or the spot).
    let mut ambient = Vec::new();
    net.0.core.drain_ambient(frame.now, &mut ambient);
    tracers.flying.extend(ambient.iter().map(|a| crate::controls::Tracer::new(a.origin, a.dir, false)));
    for ev in events {
        match ev {
            Event::Hit { head, killed, .. } => combat.mark = Some((now, head, killed)),
            Event::Hurt { dir, .. } => {
                combat.hurt.push((now, dir));
                if combat.hurt.len() > 4 {
                    combat.hurt.remove(0);
                }
            }
            Event::Kill { killer, victim, head } => {
                combat.feed.push((now, killer, victim, head));
                if combat.feed.len() > 5 {
                    combat.feed.remove(0);
                }
            }
        }
    }
    // Hit marker: white body, orange head, a bigger red one for a kill.
    if let Ok((mut t, mut c, mut f)) = mark.single_mut() {
        match combat.mark.filter(|m| now - m.0 < MARK_SECS) {
            Some((at, head, killed)) => {
                let a = 1.0 - (now - at) / MARK_SECS;
                t.0 = "X".into();
                c.0 = if killed { Color::srgba(1.0, 0.2, 0.15, a) } else if head { Color::srgba(1.0, 0.6, 0.1, a) } else { Color::srgba(1.0, 1.0, 1.0, a) };
                f.font_size = FontSize::Px(if killed { 34.0 } else { 26.0 });
            }
            None => t.0.clear(),
        }
    }
    // Damage arrows: around the crosshair, toward the shooter as we face now
    // (straight up is ahead).
    combat.hurt.retain(|h| now - h.0 < ARROW_SECS);
    for (a, mut n, mut c) in &mut arrows {
        match combat.hurt.get(a.0) {
            Some(&(at, dir)) => {
                let rel = dir as f32 / 256.0 * std::f32::consts::TAU - view.yaw;
                n.margin = UiRect { left: Val::Px(-rel.sin() * ARROW_RADIUS - 6.0), top: Val::Px(-rel.cos() * ARROW_RADIUS - 6.0), ..default() };
                c.0 = Color::srgba(0.95, 0.15, 0.1, 0.85 * (1.0 - (now - at) / ARROW_SECS));
            }
            None => c.0 = Color::srgba(0.0, 0.0, 0.0, 0.0),
        }
    }
    // Kill feed: the last five, for six seconds each.
    combat.feed.retain(|k| now - k.0 < FEED_SECS);
    let me = net.0.core.welcome().map(|w| w.entity);
    let name = |e: u16| if Some(e) == me { "you".to_string() } else { format!("{} #{e}", FACTION_NAMES[lattice_game::faction::faction(e) as usize]) };
    let text: Vec<String> = combat.feed.iter().map(|&(_, k, v, head)| format!("{} > {}{}", name(k), name(v), if head { " (head)" } else { "" })).collect();
    if let Ok(mut t) = feed.single_mut() {
        let joined = text.join("\n");
        if t.0 != joined {
            t.0 = joined;
        }
    }
}

/// The health bar, and while dead, the time to respawn (from when we heard).
pub fn vitals(
    net: Res<Net>,
    mut died_at: Local<Option<Instant>>,
    mut fill: Query<(&mut Node, &mut BackgroundColor), With<HealthFill>>,
    mut text: Query<&mut Text, With<DeathText>>,
) {
    let core = &net.0.core;
    let hp = core.health() as f32 / lattice_game::faction::MAX_HEALTH as f32;
    if let Ok((mut n, mut c)) = fill.single_mut() {
        n.width = Val::Percent(hp * 100.0);
        c.0 = if hp > 0.5 { Color::srgb(0.3, 0.85, 0.4) } else if hp > 0.25 { Color::srgb(0.95, 0.75, 0.2) } else { Color::srgb(0.9, 0.3, 0.25) };
    }
    let msg = if core.is_dead() {
        let at = *died_at.get_or_insert_with(Instant::now);
        let left = lattice_game::faction::RESPAWN_STEPS as f32 / 30.0 - at.elapsed().as_secs_f32();
        format!("You died. Respawning in {:.0} s", left.max(0.0).ceil())
    } else {
        *died_at = None;
        String::new()
    };
    if let Ok(mut t) = text.single_mut() {
        if t.0 != msg {
            t.0 = msg;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn update(
    net: Res<Net>,
    settings: Res<Settings>,
    times: Res<FrameTimes>,
    scene: Res<Scene>,
    mut rates: ResMut<Rates>,
    mut panel: Query<&mut Visibility, With<GraphPanel>>,
    mut text: Query<&mut Text, With<GraphText>>,
    mut frame_bars: Query<(&FrameBar, &mut Node), Without<GapBar>>,
    mut gap_bars: Query<(&GapBar, &mut Node), Without<FrameBar>>,
) {
    if let Ok(mut v) = panel.single_mut() {
        *v = if settings.net_graph { Visibility::Inherited } else { Visibility::Hidden };
    }
    if !settings.net_graph {
        return;
    }
    let s = &net.0;
    let (frames, gaps) = (&times.recent, &s.snapshot_gaps);
    for (b, mut n) in &mut frame_bars {
        let v = frames.get(frames.len().wrapping_sub(BARS) .wrapping_add(b.0)).copied().unwrap_or(0.0);
        n.height = Val::Px((v / 33.3 * BAR_H).clamp(1.0, BAR_H));
    }
    for (b, mut n) in &mut gap_bars {
        let v = gaps.get(gaps.len().wrapping_sub(BARS).wrapping_add(b.0)).copied().unwrap_or(0.0);
        n.height = Val::Px((v / 100.0 * BAR_H).clamp(1.0, BAR_H));
    }

    // Text four times a second; rates over the last second.
    let now = Instant::now();
    if now - rates.text_at < Duration::from_millis(250) {
        return;
    }
    rates.text_at = now;
    let core = &s.core;
    let st = &core.stats;
    let tstats = s.transport().stats();
    if now - rates.at >= Duration::from_secs(1) {
        let secs = (now - rates.at).as_secs_f32();
        let bytes = tstats.map_or((0, 0), |t| (t.bytes_received, t.bytes_sent));
        rates.down_kbps = (bytes.0 - rates.bytes.0.min(bytes.0)) as f32 * 8.0 / 1000.0 / secs;
        rates.up_kbps = (bytes.1 - rates.bytes.1.min(bytes.1)) as f32 * 8.0 / 1000.0 / secs;
        rates.bytes = bytes;
        if let Some(e) = core.entities() {
            for t in 0..3 {
                let (now_f, then) = (e.smooth.frames[t], rates.frames[t]);
                let total: u64 = (0..4).map(|h| now_f[h] - then[h]).sum();
                if total > 0 {
                    rates.interpolated[t] = (now_f[0] - then[0]) as f32 / total as f32;
                }
            }
            rates.frames = e.smooth.frames;
        }
        rates.at = now;
    }
    let state = match s.state() {
        ClientState::Connected => "connected",
        ClientState::Connecting => "connecting",
        ClientState::Denied(_) => "DENIED (token key or server id?)",
        ClientState::TimedOut => "TIMED OUT",
        ClientState::Disconnected => "disconnected",
    };
    let (rtt, loss) = tstats.map_or((0.0, 0.0), |t| (t.rtt_ms, t.loss * 100.0));
    let wait = st.wait_sum_ms / st.wait_samples.max(1) as f64;
    let clock = core.render_clock();
    let delay = clock.newest_at(now).zip(clock.last_render()).map_or(0.0, |(n, r)| (n - r) * 1000.0 / 30.0);
    let step = core.server_own().map_or(0, |(_, step)| step);
    let mut lines = vec![
        format!("{:.0} fps  frame p50 {:.1} p99 {:.1} ms", times.fps, times.p50, times.p99),
        format!("{state}  rtt {rtt:.1} ms  loss {loss:.2}%  down {:.0} up {:.0} kbps", rates.down_kbps, rates.up_kbps),
        format!("server step {step}  level {}  pace {:.2}  bandwidth level {}", st.level, st.pace as f32 / 1000.0, st.client_level),
        format!("input -> applied ~{:.0} ms (rtt/2 + server wait {wait:.0})", rtt / 2.0 + wait as f32),
        format!(
            "render delay near {delay:.0} ms, mid/far +{:.0} ms  clock snaps {}",
            core.entities().map_or(0.0, |e| e.mid_lag()) * 1000.0 / 30.0,
            core.render_clock().snaps
        ),
        format!(
            "drawn near {} mid {} far {}  interpolated {:.1}% / {:.1}% / {:.1}%",
            scene.drawn[0],
            scene.drawn[1],
            scene.drawn[2],
            rates.interpolated[0] * 100.0,
            rates.interpolated[1] * 100.0,
            rates.interpolated[2] * 100.0
        ),
        format!(
            "corrections {} (largest {:.3} m)  push corrections {} (largest {:.2} m)  deaths/respawns {}",
            st.corrections, st.correction_error_max, st.push_corrections, st.push_error_max, st.life_events
        ),
        format!("others' shots {}  distant fights: {} cells, {} shots, {} ambient", st.shots_seen, st.activity_cells, st.activity_shots, st.ambient),
    ];
    if settings.ghosts {
        lines.push("ghosts: blue = newest server sample, pink = our server state".into());
    }
    if let Ok(mut t) = text.single_mut() {
        t.0 = lines.join("\n");
    }
}
