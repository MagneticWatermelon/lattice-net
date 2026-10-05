//! Keyboard and mouse: the player's input each frame, the camera, and the
//! toggles. `--autoplay` replaces the keyboard with a wander (self-checks).

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use lattice_game::movement::{Input, BUTTON_JUMP, BUTTON_SPRINT};
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

/// Cosmetic tracers of our own shots, flown with the server's kinematics.
#[derive(Resource, Default)]
pub struct Tracers {
    /// (flight, seconds flown, seconds not yet flown).
    pub flying: Vec<(Flight, f32, f32)>,
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
) {
    let view = &mut *view;
    if view.grabbed {
        let d = motion.delta * SENSITIVITY;
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
        if keys.pressed(KeyCode::ShiftLeft) {
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
        if net.0.core.fire(frame.now, yaw, pitch) {
            let f = view.feet;
            tracers.flying.push((Flight::new([f[0], f[1], f[2] + EYE], aim(yaw, pitch)), 0.0, 0.0));
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

/// Flies and draws our tracers: a streak along each one's last segments,
/// until it hits the ground or cover, or runs out of range.
pub fn tracers(frame: Res<Frame>, net: Res<Net>, mut tracers: ResMut<Tracers>, mut gizmos: Gizmos) {
    let Some(w) = net.0.core.welcome() else { return };
    let world = lattice_game::world::World::shared(w.world_seed);
    let seg = 1.0 / (SUBSTEPS * 30) as f32;
    let range = RANGE_STEPS as f32 / 30.0;
    tracers.flying.retain_mut(|(f, age, owed)| {
        *owed += frame.dt;
        let tail = f.pos;
        while *owed >= seg {
            let next = f.advance();
            let mut hit = lattice_game::hit::terrain(&world, f.pos, next.pos).is_some();
            let mid = [(f.pos[0] + next.pos[0]) / 2.0, (f.pos[1] + next.pos[1]) / 2.0];
            world.boxes_near(mid[0], mid[1], 6.0, |b| {
                hit |= lattice_game::hit::aabb(f.pos, next.pos, [b.min[0], b.min[1], b.bottom], [b.max[0], b.max[1], b.top]).is_some();
            });
            *owed -= seg;
            *age += seg;
            if hit || *age > range {
                return false;
            }
            *f = next;
        }
        gizmos.line(to_bevy(tail[0], tail[1], tail[2]), to_bevy(f.pos[0], f.pos[1], f.pos[2]), Color::srgb(1.0, 0.85, 0.4));
        true
    });
}

/// Places the camera for the view, on our player's drawn position.
pub fn place_camera(frame: Res<Frame>, net: Res<Net>, mut view: ResMut<View>, mut cam: Query<&mut Transform, With<Camera3d>>) {
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
