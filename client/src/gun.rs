//! The rifle in the player's hands, PlanetSide 2 style: aiming down sights
//! (right mouse: zoom, slower aim and movement, a tight cone of fire),
//! recoil (each shot kicks the view, which drifts back after the burst), the
//! first-person rifle's motion (to the sights, kick, bob, lowered while
//! sprinting), its muzzle flash, and the reticle (the cone's size from the
//! hip, a red dot down the sights). The cone itself is the game's rule
//! (`weapon::Bloom`), the same one the server shoots with.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use lattice_game::weapon::{ADS_ZOOM, BURST_GAP, RECOIL_DELAY, RECOIL_FIRST, RECOIL_RECOVERY, RECOIL_SIDE, RECOIL_UP};

use crate::controls::{Mode, View, ViewModel};
use crate::models::{Models, VIEWMODEL_LAYER};
use crate::{Frame, Net};

/// Seconds to bring the sights up (or down).
const ADS_TIME: f32 = 0.2;
/// The world camera's and the first-person rifle camera's fields of view
/// (vertical, degrees), from the hip.
pub const FOV: f32 = 80.0;
pub const VIEW_FOV: f32 = 70.0;
/// The first-person rifle, in its camera's frame: from the hip (low right),
/// and down the sights (its sight posts, 0.2 above its center line and the
/// rear one 0.28 behind its center, just under the line of sight, 24 cm
/// from the eye).
const SCALE: f32 = 0.7;
const HIP: Vec3 = Vec3::new(0.2, -0.2, -0.5);
const SIGHTS: Vec3 = Vec3::new(0.0, -0.2 * SCALE - 0.006, -0.24 - 0.28 * SCALE);
/// Kick per shot: back (m) and up (radians), from the hip and down the
/// sights; it eases off with this time constant (s).
const KICK: [(f32, f32); 2] = [(0.04, 0.07), (0.02, 0.03)];
const KICK_TAU: f32 = 0.07;
/// The muzzle flash shows this long (s), at the muzzle (the rifle's model
/// space: 1 m long, muzzle at -x).
const FLASH_SECS: f32 = 0.045;
pub const MUZZLE: Vec3 = Vec3::new(-0.53, 0.065, 0.0);
/// Sprinting lowers the rifle: offset and turn, in the camera's frame.
const SPRINT_OFFSET: Vec3 = Vec3::new(0.05, -0.1, 0.08);

/// The gun's state.
#[derive(Resource)]
pub struct Gun {
    /// The right button: aiming down sights (the inputs say so).
    pub ads: bool,
    /// 0 at the hip, 1 down the sights (eases between them).
    pub blend: f32,
    sprint: f32,
    bob: f32,
    /// The latest shot's kick, easing to 0.
    kick: f32,
    /// Recoil not yet recovered (degrees: up, right), and the last shot.
    debt: [f32; 2],
    last_shot: f32,
    flash_until: f32,
    rng: u64,
}

impl Default for Gun {
    fn default() -> Self {
        Self { ads: false, blend: 0.0, sprint: 0.0, bob: 0.0, kick: 0.0, debt: [0.0; 2], last_shot: -10.0, flash_until: 0.0, rng: 0x5EED_6A11 }
    }
}

impl Gun {
    fn rand(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Mouse sensitivity now: slower down the sights, as zoomed.
    pub fn sensitivity(&self) -> f32 {
        1.0 - self.blend * (1.0 - 1.0 / ADS_ZOOM)
    }

    /// A shot fired at `secs`: recoil kicks the view (the first shot of a
    /// burst harder), the rifle kicks back, the muzzle flashes.
    pub fn shot(&mut self, view: &mut View, secs: f32) {
        let first = secs - self.last_shot > BURST_GAP;
        let up = RECOIL_UP * if first { RECOIL_FIRST } else { 1.0 };
        let side = RECOIL_SIDE[0] + self.rand() * (RECOIL_SIDE[1] - RECOIL_SIDE[0]);
        view.pitch = (view.pitch + up.to_radians()).min(FRAC_PI_2 - 0.01);
        view.yaw -= side.to_radians();
        self.debt = [self.debt[0] + up, self.debt[1] + side];
        self.kick = 1.0;
        self.last_shot = secs;
        self.flash_until = secs + FLASH_SECS;
    }
}

/// The last shot of each player drawn (their rifles kick and flash), by the
/// scene's player key.
#[derive(Resource, Default)]
pub struct FireTimes(pub HashMap<u16, f32>);

/// The first-person muzzle flash.
#[derive(Component)]
pub struct ViewFlash;
#[derive(Component)]
pub struct Reticle(usize);
#[derive(Component)]
pub struct RedDot;
/// The hip-fire crosshair's center dot.
#[derive(Component)]
pub struct CenterDot;

/// The muzzle flash's look: a short, bright, additive blob.
pub fn flash_material(mats: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.78, 0.4, 0.9),
        emissive: LinearRgba::rgb(12.0, 7.0, 2.5),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        ..default()
    })
}

/// The reticle: four ticks around the center (the cone, from the hip) and a
/// red dot (down the sights).
pub fn setup(mut commands: Commands) {
    for i in 0..4 {
        commands.spawn((
            Reticle(i),
            Node { position_type: PositionType::Absolute, width: Val::Px(2.0), height: Val::Px(2.0), ..default() },
            BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.85)),
        ));
    }
    commands.spawn((
        RedDot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            margin: UiRect { left: Val::Px(-2.0), top: Val::Px(-2.0), ..default() },
            width: Val::Px(4.0),
            height: Val::Px(4.0),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BackgroundColor(Color::srgb(1.0, 0.15, 0.1)),
        Visibility::Hidden,
    ));
}

/// The sights up or down, recoil's recovery, and the zoom.
#[allow(clippy::too_many_arguments)]
pub fn aim(
    frame: Res<Frame>,
    settings: Res<crate::Settings>,
    buttons: Res<ButtonInput<MouseButton>>,
    net: Res<Net>,
    mut view: ResMut<View>,
    mut gun: ResMut<Gun>,
    mut cams: Query<(&mut Projection, Has<crate::controls::MainCamera>), With<Camera3d>>,
) {
    let alive = view.has_body && !net.0.core.is_dead();
    let held = buttons.pressed(MouseButton::Right) && view.grabbed();
    gun.ads = (held || settings.autoaim) && alive && view.mode != Mode::Spectator;
    let target = if gun.ads { 1.0 } else { 0.0 };
    let step = frame.dt / ADS_TIME;
    gun.blend = if gun.blend < target { (gun.blend + step).min(target) } else { (gun.blend - step).max(target) };
    // Recoil drifts back down once the burst is over.
    if frame.secs - gun.last_shot > RECOIL_DELAY && gun.debt[0] > 0.0 {
        let back = (RECOIL_RECOVERY * frame.dt).min(gun.debt[0]);
        let side = gun.debt[1] * back / gun.debt[0];
        view.pitch -= back.to_radians();
        view.yaw += side.to_radians();
        gun.debt = [gun.debt[0] - back, gun.debt[1] - side];
    }
    // Zoom down the sights, in first person.
    let zoom = if view.mode == Mode::FirstPerson { ease(gun.blend) } else { 0.0 };
    let zoomed = |fov: f32| (1.0 - zoom) * fov + zoom * 2.0 * ((fov / 2.0).to_radians().tan() / ADS_ZOOM).atan().to_degrees();
    for (mut p, main) in &mut cams {
        if let Projection::Perspective(p) = &mut *p {
            p.fov = zoomed(if main { FOV } else { VIEW_FOV }).to_radians();
        }
    }
}

fn ease(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Moves the first-person rifle: between the hip and the sights, with the
/// shot's kick, a bob while walking, lowered while sprinting; and its flash.
#[allow(clippy::type_complexity)]
pub fn viewmodel(
    frame: Res<Frame>,
    net: Res<Net>,
    mut gun: ResMut<Gun>,
    mut rifle: Query<&mut Transform, (With<ViewModel>, Without<ViewFlash>)>,
    mut flash: Query<(&mut Transform, &mut Visibility), (With<ViewFlash>, Without<ViewModel>)>,
) {
    let s = net.0.core.predicted();
    let speed = (s.vel[0] * s.vel[0] + s.vel[1] * s.vel[1]).sqrt();
    let dt = frame.dt;
    let sprinting = speed > 7.5 && !gun.ads && s.grounded;
    gun.sprint += ((sprinting as u8 as f32) - gun.sprint) * (dt / 0.12).min(1.0);
    if s.grounded {
        gun.bob += speed * dt / 2.4 * std::f32::consts::TAU;
    }
    gun.kick *= (-dt / KICK_TAU).exp();
    let a = ease(gun.blend);
    let (back, up) = (KICK[0].0 * (1.0 - a) + KICK[1].0 * a, KICK[0].1 * (1.0 - a) + KICK[1].1 * a);
    let moving = (speed / 6.0).min(1.0) * (1.0 - 0.85 * a) * (s.grounded as u8 as f32);
    let bob = Vec3::new(gun.bob.sin() * 0.012, -(gun.bob * 2.0).sin().abs() * 0.008, 0.0) * moving;
    let at = HIP.lerp(SIGHTS, a) + SPRINT_OFFSET * gun.sprint + bob + Vec3::Z * back * gun.kick;
    let turn = Quat::from_rotation_y(0.55 * gun.sprint) * Quat::from_rotation_x(-0.35 * gun.sprint + up * gun.kick);
    for mut tf in &mut rifle {
        *tf = Transform::from_translation(at).with_rotation(turn * Quat::from_rotation_y(-FRAC_PI_2)).with_scale(Vec3::splat(SCALE));
    }
    let on = frame.secs < gun.flash_until;
    let (size, roll) = (0.9 + 0.5 * gun.rand(), gun.rand() * std::f32::consts::TAU);
    for (mut tf, mut vis) in &mut flash {
        if on {
            *tf = Transform::from_translation(MUZZLE).with_rotation(Quat::from_rotation_x(roll)).with_scale(Vec3::new(0.16, 0.09, 0.09) * size);
        }
        let want = if on { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
}

/// The first-person muzzle flash, a child of the first-person rifle.
pub fn view_flash(models: &Models) -> impl Bundle {
    (ViewFlash, Mesh3d(models.flash.0.clone()), MeshMaterial3d(models.flash.1.clone()), Transform::from_translation(MUZZLE), Visibility::Hidden, RenderLayers::layer(VIEWMODEL_LAYER), bevy::light::NotShadowCaster)
}

/// The reticle: from the hip, four ticks at the cone of fire's edge (it
/// blooms as you fire, widens as you move); down the sights, a red dot.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn reticle(
    frame: Res<Frame>,
    net: Res<Net>,
    view: Res<View>,
    gun: Res<Gun>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut ticks: Query<(&Reticle, &mut Node, &mut Visibility), (Without<RedDot>, Without<CenterDot>)>,
    mut dot: Query<&mut Visibility, (With<RedDot>, Without<Reticle>, Without<CenterDot>)>,
    mut center: Query<&mut Visibility, (With<CenterDot>, Without<Reticle>, Without<RedDot>)>,
) {
    let Ok(w) = window.single() else { return };
    let shown = view.mode != Mode::Spectator && view.has_body && !net.0.core.is_dead();
    let sights = gun.blend > 0.7 && view.mode == Mode::FirstPerson;
    let fov = (FOV / 2.0).to_radians();
    let cone = net.0.core.cone(frame.now, gun.ads).to_radians();
    let r = (cone.tan() / fov.tan() * w.height() / 2.0).max(5.0);
    let (cx, cy) = (w.width() / 2.0, w.height() / 2.0);
    for (Reticle(i), mut node, mut vis) in &mut ticks {
        let (dx, dy, long_x) = match i {
            0 => (r, 0.0, true),
            1 => (-r, 0.0, true),
            2 => (0.0, r, false),
            _ => (0.0, -r, false),
        };
        let (wd, ht) = if long_x { (8.0, 2.0) } else { (2.0, 8.0) };
        let sx = if dx > 0.0 { 0.0 } else if dx < 0.0 { -wd } else { -wd / 2.0 };
        let sy = if dy > 0.0 { 0.0 } else if dy < 0.0 { -ht } else { -ht / 2.0 };
        node.left = Val::Px(cx + dx + sx);
        node.top = Val::Px(cy + dy + sy);
        node.width = Val::Px(wd);
        node.height = Val::Px(ht);
        let want = if shown && !sights { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
    for mut vis in &mut dot {
        let want = if shown && sights { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
    for mut vis in &mut center {
        let want = if sights { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want {
            *vis = want;
        }
    }
}
