//! Keyboard and mouse: the player's input each frame, the camera, and the
//! toggles. `--autoplay` replaces the keyboard with a wander (self-checks).

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use lattice_game::movement::{Input, BUTTON_ADS, BUTTON_JUMP, BUTTON_SPRINT};
use lattice_game::weapon::{aim, Flight, RANGE_STEPS, SUBSTEPS};

use crate::coords::{look_rotation, pitch_i16, stick, to_bevy, yaw_u16};
use crate::{Frame, Net, Settings};

/// Eye height above the feet.
pub const EYE: f32 = 1.6;
const SENSITIVITY: f32 = 0.0025;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    FirstPerson,
    /// Behind and above our player.
    Chase,
    /// Free flight; our player stands still.
    Spectator,
}

/// The camera and what the player aims at, in game space.
#[derive(Resource)]
pub struct View {
    pub mode: Mode,
    /// Where the camera is.
    pub eye: [f32; 3],
    /// The player's aim (first person and chase) or the camera's (spectator).
    pub yaw: f32,
    pub pitch: f32,
    /// Our player's drawn feet, once we have a state.
    pub feet: [f32; 3],
    pub has_body: bool,
    /// The spectator camera's own aim, kept while in other views.
    spec_yaw: f32,
    spec_pitch: f32,
    /// The spectator camera has a place (else it starts above our player).
    spec_placed: bool,
    grabbed: bool,
    /// The click that grabbed the mouse doesn't fire (until released).
    swallow: bool,
    wander: Wander,
}

impl View {
    /// The mouse is ours (clicks fire, the right button aims).
    pub fn grabbed(&self) -> bool {
        self.grabbed
    }

    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            eye: [4096.0, 4096.0, 200.0],
            yaw: 0.0,
            pitch: 0.0,
            feet: [0.0; 3],
            has_body: false,
            spec_yaw: 0.0,
            spec_pitch: -0.4,
            spec_placed: false,
            grabbed: false,
            swallow: false,
            wander: Wander { seed: 0x9E37_79B9, heading: 0.0, until: 0.0, sprint: false },
        }
    }
}

/// `--autoplay`: run in a random direction, change it every 1-3 s, sprint
/// and jump now and then, look where it runs.
struct Wander {
    seed: u64,
    heading: f32,
    until: f32,
    sprint: bool,
}

impl Wander {
    fn rand(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.seed >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Click grabs the mouse, Esc releases it; V cycles views; G, T and N
/// toggle the ghosts, tier colors (vs faction colors) and net graph.
pub fn toggles(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut view: ResMut<View>,
    mut settings: ResMut<Settings>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if let Ok(mut c) = cursor.single_mut() {
        if buttons.just_pressed(MouseButton::Left) && !view.grabbed {
            c.grab_mode = CursorGrabMode::Confined;
            c.visible = false;
            (view.grabbed, view.swallow) = (true, true);
        }
        if keys.just_pressed(KeyCode::Escape) && view.grabbed {
            c.grab_mode = CursorGrabMode::None;
            c.visible = true;
            view.grabbed = false;
        }
    }
    if keys.just_pressed(KeyCode::KeyV) {
        view.mode = match view.mode {
            Mode::FirstPerson => Mode::Chase,
            Mode::Chase => Mode::Spectator,
            Mode::Spectator => Mode::FirstPerson,
        };
        if view.mode == Mode::Spectator {
            (view.spec_yaw, view.spec_pitch) = (view.yaw, view.pitch.min(-0.3));
            view.spec_placed = true;
        }
    }
    if keys.just_pressed(KeyCode::KeyG) {
        settings.ghosts = !settings.ghosts;
    }
    if keys.just_pressed(KeyCode::KeyT) {
        settings.tier_colors = !settings.tier_colors;
    }
    if keys.just_pressed(KeyCode::KeyN) {
        settings.net_graph = !settings.net_graph;
    }
}

/// Tracers draw as their own gizmo group: thick, constant pixel width.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct TracerGizmos;

/// Tracer lines' width, in pixels, and how much of the path behind a
/// projectile shows (a fading trail), in meters.
pub const TRACER_WIDTH: f32 = 4.0;
const TRAIL: f32 = 20.0;
/// Our tracers leave from a muzzle below and right of the eye, aimed to
/// converge on the line of sight this far out (from the eye itself they'd
/// be a dot under the crosshair).
const CONVERGE: f32 = 120.0;

pub fn setup_tracer_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (cfg, _) = store.config_mut::<TracerGizmos>();
    cfg.line.width = TRACER_WIDTH;
}

/// One tracer: its flight, where it started, seconds flown and not yet
/// flown, and, for one of our own shots, how to re-aim it.
pub struct Tracer {
    flight: Flight,
    start: [f32; 3],
    age: f32,
    owed: f32,
    ours: bool,
    own: Option<OwnShot>,
}

/// One of our shots: when it fired (the server's step) and the view it left
/// from. Its tracer leaves along a pick of our own in the cone of fire (the
/// server's is secret) until the server says where the shot really went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OwnShot {
    step: f64,
    eye: [f32; 3],
    yaw: f32,
    hip: f32,
    /// Re-aimed along the real shot already.
    real: bool,
}

/// Seconds per flight segment.
const SEG: f32 = 1.0 / (SUBSTEPS * 30) as f32;

/// Whether a segment from `a` to `b` hits the ground or cover.
fn segment_hits(world: &lattice_game::world::World, a: [f32; 3], b: [f32; 3]) -> bool {
    let mut hit = lattice_game::hit::terrain(world, a, b).is_some();
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    world.boxes_near(mid[0], mid[1], 6.0, |c| {
        hit |= lattice_game::hit::aabb(a, b, [c.min[0], c.min[1], c.bottom], [c.max[0], c.max[1], c.top]).is_some();
    });
    hit
}

impl Tracer {
    pub fn new(origin: [f32; 3], dir: [f32; 3]) -> Self {
        Self { flight: Flight::new(origin, dir), start: origin, age: 0.0, owed: 0.0, ours: false, own: None }
    }

    /// Ours: from the muzzle (low right from the hip, under the eye down the
    /// sights: `hip` 1 to 0), converging on what the eye at `eye` aims at
    /// along `aim_dir` (a direction in the cone of fire). `step`: when it
    /// fired, in the server's steps, to re-aim it along the real shot.
    pub fn ours(eye: [f32; 3], yaw: f32, aim_dir: [f32; 3], hip: f32, step: Option<f64>) -> Self {
        let (s, c) = yaw.sin_cos();
        let right = [s, -c, 0.0];
        let down = 0.12 + 0.08 * hip;
        let muzzle = [0, 1, 2].map(|k| eye[k] + right[k] * 0.25 * hip + aim_dir[k] * 0.6 - if k == 2 { down } else { 0.0 });
        let target = [0, 1, 2].map(|k| eye[k] + aim_dir[k] * CONVERGE);
        let d = [0, 1, 2].map(|k| target[k] - muzzle[k]);
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let own = step.map(|step| OwnShot { step, eye, yaw, hip, real: false });
        Self { ours: true, own, ..Self::new(muzzle, d.map(|v| v / len)) }
    }
}

/// Cosmetic tracers (ours and others'), flown with the server's kinematics.
#[derive(Resource, Default)]
pub struct Tracers {
    pub flying: Vec<Tracer>,
    /// Others' shots waiting for their shooter to be drawn firing:
    /// (shot, when it arrived).
    pub pending: Vec<(lattice_game::events::SeenShot, f32)>,
    /// Our own shots as the server says they went, to re-aim their tracers.
    pub real: Vec<lattice_game::events::SeenShot>,
}

impl Tracers {
    /// Our shot `shot` really went along its direction: re-aim its tracer,
    /// still in flight, from the same muzzle and as far along, so it lands
    /// where the shot did; if the real path already hit something by then,
    /// it ends. One that already landed (close in, before the server's word
    /// came, about a round trip) keeps its own path.
    pub fn correct(&mut self, shot: &lattice_game::events::SeenShot, world: &lattice_game::world::World) {
        let close = |t: &&mut Tracer| t.own.is_some_and(|o| !o.real && (o.step - shot.step).abs() < 0.5);
        let Some((i, t)) = self.flying.iter_mut().enumerate().filter(|(_, t)| close(t)).min_by(|a, b| {
            let d = |t: &Tracer| t.own.map_or(f64::MAX, |o| (o.step - shot.step).abs());
            d(a.1).total_cmp(&d(b.1))
        }) else {
            return;
        };
        let o = t.own.expect("matched an own shot");
        let mut real = Tracer::ours(o.eye, o.yaw, aim(shot.yaw, shot.pitch), o.hip, Some(o.step));
        let mut flown = 0.0;
        while flown + SEG <= t.age + 1e-6 {
            let next = real.flight.advance();
            if segment_hits(world, real.flight.pos, next.pos) {
                self.flying.swap_remove(i);
                return;
            }
            real.flight = next;
            flown += SEG;
        }
        (real.age, real.owed) = (t.age, t.owed);
        real.own = Some(OwnShot { real: true, ..o });
        *t = real;
    }
}

/// Mouse look, then this frame's input (and shot) to the input clock.
#[allow(clippy::too_many_arguments)]
pub fn play(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut tracers: ResMut<Tracers>,
    frame: Res<Frame>,
    settings: Res<Settings>,
    mut view: ResMut<View>,
    mut net: ResMut<Net>,
    mut gun: ResMut<crate::gun::Gun>,
    mut fire_times: ResMut<crate::gun::FireTimes>,
) {
    let view = &mut *view;
    if view.grabbed {
        let d = motion.delta * SENSITIVITY * gun.sensitivity();
        let (yaw, pitch) = if view.mode == Mode::Spectator { (&mut view.spec_yaw, &mut view.spec_pitch) } else { (&mut view.yaw, &mut view.pitch) };
        *yaw = (*yaw - d.x).rem_euclid(TAU);
        *pitch = (*pitch - d.y).clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
    }
    let axis = |pos: KeyCode, neg: KeyCode| keys.pressed(pos) as i32 as f32 - keys.pressed(neg) as i32 as f32;
    let (fwd, right) = (axis(KeyCode::KeyW, KeyCode::KeyS), axis(KeyCode::KeyD, KeyCode::KeyA));
    let input = if settings.autoplay {
        let w = &mut view.wander;
        let t = frame.secs;
        if t > w.until {
            w.heading = w.rand() * TAU;
            w.until = t + 1.0 + 2.0 * w.rand();
            w.sprint = w.rand() < 0.4;
        }
        let jump = w.rand() < 0.004;
        let heading = w.heading;
        // Turn the view smoothly toward where it runs.
        let turn = (heading - view.yaw + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI;
        view.yaw = (view.yaw + turn * (frame.dt * 4.0).min(1.0)).rem_euclid(TAU);
        let (mx, my) = stick(heading, 1.0, 0.0);
        Input {
            move_x: mx,
            move_y: my,
            yaw: yaw_u16(view.yaw),
            pitch: pitch_i16(view.pitch),
            buttons: if w.sprint { BUTTON_SPRINT } else { 0 } | if jump { BUTTON_JUMP } else { 0 },
        }
    } else if view.mode == Mode::Spectator {
        // Our player stands, keeping its aim.
        Input { yaw: yaw_u16(view.yaw), pitch: pitch_i16(view.pitch), ..default() }
    } else {
        let (mx, my) = stick(view.yaw, fwd, right);
        let mut buttons = 0;
        // Down the sights there's no sprint (the server agrees: BUTTON_ADS).
        if gun.ads {
            buttons |= BUTTON_ADS;
        } else if keys.pressed(KeyCode::ShiftLeft) {
            buttons |= BUTTON_SPRINT;
        }
        if keys.pressed(KeyCode::Space) {
            buttons |= BUTTON_JUMP;
        }
        Input { move_x: mx, move_y: my, yaw: yaw_u16(view.yaw), pitch: pitch_i16(view.pitch), buttons }
    };
    // Held left button (once the mouse is ours): a shot whenever the rifle is
    // ready, from the eye along the view, riding this frame's input.
    if !buttons.pressed(MouseButton::Left) {
        view.swallow = false;
    }
    let trigger = view.grabbed && !view.swallow && view.mode != Mode::Spectator && buttons.pressed(MouseButton::Left);
    if trigger || settings.autofire {
        let (yaw, pitch) = (yaw_u16(view.yaw), pitch_i16(view.pitch));
        if let Some(dir) = net.0.core.fire(frame.now, yaw, pitch, gun.ads) {
            let f = view.feet;
            let step = net.0.core.last_shot_step();
            tracers.flying.push(Tracer::ours([f[0], f[1], f[2] + EYE], view.yaw, dir, 1.0 - gun.blend, step));
            gun.shot(view, frame.secs);
            fire_times.0.insert(crate::scene::OWN, frame.secs);
        }
    }
    net.0.send_inputs(frame.now, input);

    // Free flight: WASD along the view, Space/C up and down, Shift faster.
    if view.mode == Mode::Spectator {
        let speed = if keys.pressed(KeyCode::ShiftLeft) { 120.0 } else { 25.0 } * frame.dt;
        let (s, c) = view.spec_yaw.sin_cos();
        let up = axis(KeyCode::Space, KeyCode::KeyC);
        view.eye[0] += (c * fwd + s * right) * speed;
        view.eye[1] += (s * fwd - c * right) * speed;
        view.eye[2] += up * speed;
    }
}

/// Flies and draws tracers: a streak along each one's last segments, until
/// it hits the ground or cover, or runs out of range. Others' shots start
/// when their shooter is drawn at the moment it fired, from its drawn eye.
pub fn tracers(
    frame: Res<Frame>,
    net: Res<Net>,
    mut tracers: ResMut<Tracers>,
    mut fire_times: ResMut<crate::gun::FireTimes>,
    mut gizmos: Gizmos<TracerGizmos>,
) {
    let Some(w) = net.0.core.welcome() else { return };
    let world = lattice_game::world::World::shared(w.world_seed);
    let core = &net.0.core;
    if let (Some(r), Some(ents)) = (core.render_clock().last_render(), core.entities()) {
        let tracers = &mut *tracers;
        tracers.pending.retain(|(shot, at)| {
            match ents.render_one(shot.shooter, r) {
                Some(st) if st.at >= shot.step => {
                    let o = [st.pos[0], st.pos[1], st.pos[2] + EYE];
                    tracers.flying.push(Tracer::new(o, aim(shot.yaw, shot.pitch)));
                    // Its rifle kicks and flashes as it's drawn firing.
                    fire_times.0.insert(shot.shooter, frame.secs);
                    false
                }
                // Not drawn (yet): wait up to a second.
                _ => frame.secs - *at < 1.0,
            }
        });
    }
    // Our shots as they really went: their tracers follow.
    for shot in std::mem::take(&mut tracers.real) {
        tracers.correct(&shot, &world);
    }
    let range = RANGE_STEPS as f32 / 30.0;
    tracers.flying.retain_mut(|t| {
        let Tracer { flight: f, start, age, owed, ours, .. } = t;
        *owed += frame.dt;
        while *owed >= SEG {
            let next = f.advance();
            let hit = segment_hits(&world, f.pos, next.pos);
            *owed -= SEG;
            *age += SEG;
            if hit || *age > range {
                return false;
            }
            *f = next;
        }
        // A trail behind the head, no longer than the path flown, fading out.
        let p = f.pos;
        let flown = ((p[0] - start[0]).powi(2) + (p[1] - start[1]).powi(2) + (p[2] - start[2]).powi(2)).sqrt();
        let speed = (f.vel[0] * f.vel[0] + f.vel[1] * f.vel[1] + f.vel[2] * f.vel[2]).sqrt().max(1.0);
        let back = TRAIL.min(flown) / speed;
        let tail = [p[0] - f.vel[0] * back, p[1] - f.vel[1] * back, p[2] - f.vel[2] * back];
        let head = if *ours { Color::srgb(1.0, 0.95, 0.55) } else { Color::srgb(1.0, 0.55, 0.15) };
        gizmos.line_gradient(to_bevy(tail[0], tail[1], tail[2]), to_bevy(p[0], p[1], p[2]), head.with_alpha(0.0), head);
        true
    });
}

/// The first-person rifle.
#[derive(Component)]
pub struct ViewModel;

/// The world camera (the first-person rifle has its own, as its child).
#[derive(Component)]
pub struct MainCamera;

/// Shows the first-person rifle in the first-person view, while alive.
pub fn viewmodel(view: Res<View>, net: Res<Net>, mut q: Query<&mut Visibility, With<ViewModel>>) {
    let show = view.mode == Mode::FirstPerson && view.has_body && !net.0.core.is_dead();
    for mut v in &mut q {
        let want = if show { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}

/// Places the camera for the view, on our player's drawn position.
pub fn place_camera(frame: Res<Frame>, net: Res<Net>, mut view: ResMut<View>, mut cam: Query<&mut Transform, With<MainCamera>>) {
    let core = &net.0.core;
    if core.welcome().is_some() {
        view.feet = core.own_render(frame.now);
        view.has_body = true;
    }
    let Ok(mut tf) = cam.single_mut() else { return };
    let f = view.feet;
    match view.mode {
        Mode::FirstPerson => {
            view.eye = [f[0], f[1], f[2] + EYE];
            *tf = Transform::from_translation(to_bevy(f[0], f[1], f[2] + EYE)).with_rotation(look_rotation(view.yaw, view.pitch));
        }
        Mode::Chase => {
            // 6 m behind and 2.5 m above the eye, looking where the player looks.
            let (s, c) = view.yaw.sin_cos();
            view.eye = [f[0] - c * 6.0, f[1] - s * 6.0, f[2] + EYE + 2.5];
            let at = to_bevy(f[0] + c * 4.0, f[1] + s * 4.0, f[2] + EYE);
            *tf = Transform::from_translation(to_bevy(view.eye[0], view.eye[1], view.eye[2])).looking_at(at, Vec3::Y);
        }
        Mode::Spectator => {
            if !view.spec_placed && view.has_body {
                // Above and behind our player, looking at it.
                view.eye = [f[0] - 35.0, f[1] - 35.0, f[2] + 30.0];
                (view.spec_yaw, view.spec_pitch, view.spec_placed) = (std::f32::consts::FRAC_PI_4, -0.55, true);
            }
            *tf = Transform::from_translation(to_bevy(view.eye[0], view.eye[1], view.eye[2]))
                .with_rotation(look_rotation(view.spec_yaw, view.spec_pitch));
        }
    }
}

/// `--spectate X,Y,Z,YAW,PITCH`: start in free flight there (degrees).
pub fn parse_spectate(s: &str) -> Option<([f32; 3], f32, f32)> {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse().ok()).collect::<Option<_>>()?;
    (v.len() == 5).then(|| ([v[0], v[1], v[2]], v[3].to_radians(), v[4].to_radians()))
}

impl View {
    pub fn spectate(&mut self, eye: [f32; 3], yaw: f32, pitch: f32) {
        self.mode = Mode::Spectator;
        self.eye = eye;
        self.spec_placed = true;
        (self.spec_yaw, self.spec_pitch) = (yaw, pitch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_game::events::SeenShot;
    use lattice_game::weapon::angles;

    /// One of our tracers 40 m above the ground at (2000, 2000), fired
    /// east at step 100 and flown `segments` segments.
    fn flying(world: &lattice_game::world::World, segments: u32) -> Tracers {
        let eye = [2000.0, 2000.0, world.terrain(2000.0, 2000.0) + 40.0];
        let mut t = Tracer::ours(eye, 0.0, aim(0, 0), 1.0, Some(100.0));
        for _ in 0..segments {
            t.flight = t.flight.advance();
        }
        t.age = segments as f32 * SEG;
        Tracers { flying: vec![t], ..Default::default() }
    }

    fn shot(step: f64, dir: [f32; 3]) -> SeenShot {
        let (yaw, pitch) = angles(dir);
        SeenShot { shooter: 7, step, yaw, pitch }
    }

    #[test]
    fn our_tracer_follows_the_real_shot() {
        let world = lattice_game::world::World::shared(1);
        let real = aim(300, 120); // ~1.6 degrees right and ~0.3 up of the aim
        let mut tracers = flying(&world, 6);
        // Another shot's word (3 steps later) leaves it alone.
        tracers.correct(&shot(103.0, real), &world);
        assert!(tracers.flying[0].own.is_some_and(|o| !o.real));
        tracers.correct(&shot(100.0, real), &world);
        let t = &tracers.flying[0];
        assert!(t.own.is_some_and(|o| o.real), "re-aimed");
        // Exactly where the real shot's tracer is after as many segments.
        let mut want = Tracer::ours(t.own.unwrap().eye, 0.0, aim(shot(0.0, real).yaw, shot(0.0, real).pitch), 1.0, None);
        for _ in 0..6 {
            want.flight = want.flight.advance();
        }
        assert_eq!(t.flight, want.flight);
        assert_eq!(t.age, 6.0 * SEG);
        // A second word for the same shot changes nothing.
        let before = t.flight;
        tracers.correct(&shot(100.0, aim(0, -200)), &world);
        assert_eq!(tracers.flying[0].flight, before);
    }

    #[test]
    fn a_real_shot_that_already_landed_ends_its_tracer() {
        let world = lattice_game::world::World::shared(1);
        // Flown 30 segments (300 m): straight down, the real shot hit the
        // ground 40 m below long before.
        let mut tracers = flying(&world, 30);
        tracers.correct(&shot(100.0, aim(0, -32000)), &world);
        assert!(tracers.flying.is_empty());
    }
}
