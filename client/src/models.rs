//! The models (`assets/models`, made with Meshy; see `import-assets`): every
//! cover box drawn as what it is, rocks scattered for scale, and soldiers
//! with rifles, animated, for the players near the camera.
//!
//! Props are normalized to fill [-0.5, 0.5] x [0, 1] x [-0.5, 0.5] with x
//! along their long side and their front facing +z, so one transform puts a
//! prop exactly in its collision box: what you see is what blocks you.
//! Walls are tiled with concrete blocks. Far away (where a model is a few
//! pixels) the plain boxes take over again.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI};
use std::time::Duration;

use bevy::camera::visibility::{RenderLayers, VisibilityRange};
use bevy::prelude::*;
use bevy::world_serialization::{WorldAssetRoot, WorldInstanceReady};
use lattice_game::world::{CoverBox, Kind, World};

use crate::coords::to_bevy;

/// Models are drawn within these distances (meters); plain boxes beyond.
const PROP_RANGE: f32 = 700.0;
const BUILDING_RANGE: f32 = 1800.0;
const ROCK_RANGE: f32 = 300.0;
/// The wall block's front, width over height: walls are tiled with blocks
/// of about this shape.
const WALL_ASPECT: f32 = 1.28;
/// Players nearer than this get a soldier (with hysteresis), at most
/// `MAX_SOLDIERS` of them, nearest first; the rest stay capsules.
pub const SOLDIER_RANGE: f32 = 120.0;
const MAX_SOLDIERS: usize = 160;
/// The first-person rifle's render layer (drawn over the world by its own
/// camera, so it never sinks into a wall).
pub const VIEWMODEL_LAYER: usize = 1;

/// Animation clips, in the graph's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clip {
    Idle,
    /// Walking and slow moves, rifle up ("Run and Shoot", in place).
    Aim,
    /// Running, rifle held ("Rifle Charge", in place).
    Run,
    /// Sprinting, arms pumping (Meshy's running).
    Sprint,
    Jump,
    Death,
}
const CLIP_FILES: [&str; 6] = ["anim_idle", "anim_run_aim", "anim_sprint", "anim_run", "anim_jump", "anim_death"];

#[derive(Resource)]
pub struct Models {
    /// By `Kind`: mesh and material.
    props: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    rocks: (Handle<Mesh>, Handle<StandardMaterial>),
    pub rifle: (Handle<Mesh>, Handle<StandardMaterial>),
    soldier: Handle<WorldAsset>,
    soldier_material: Handle<StandardMaterial>,
    graph: Handle<AnimationGraph>,
    clips: Vec<AnimationNodeIndex>,
    /// The soldier's material, tinted: by tint color (as bits).
    tinted: HashMap<[u32; 3], Handle<StandardMaterial>>,
}

fn model(name: &str) -> String {
    format!("models/{name}.glb")
}

pub fn load(mut commands: Commands, assets: Res<AssetServer>, mut graphs: ResMut<Assets<AnimationGraph>>) {
    let prop = |name: &str| {
        let path = model(name);
        (assets.load(GltfAssetLabel::Primitive { mesh: 0, primitive: 0 }.from_asset(path.clone())), assets.load(format!("{path}#Material0/std")))
    };
    let props = ["wall", "crate", "container", "sandbags", "post", "command", "bunker"].map(prop).to_vec();
    let clips = CLIP_FILES.map(|f| assets.load(GltfAssetLabel::Animation(0).from_asset(model(f))));
    let (graph, nodes) = AnimationGraph::from_clips(clips);
    commands.insert_resource(Models {
        props,
        rocks: prop("rocks"),
        rifle: prop("rifle"),
        soldier: assets.load(GltfAssetLabel::Scene(0).from_asset(model("soldier"))),
        soldier_material: assets.load(format!("{}#Material0/std", model("soldier"))),
        graph: graphs.add(graph),
        clips: nodes,
        tinted: HashMap::new(),
    });
}

/// The rotation (about up) that turns a prop's front (+z) to `facing` (0
/// east, 1 north, 2 west, 3 south), and whether its long side then runs
/// north-south.
fn turn(facing: u8) -> (Quat, bool) {
    match facing % 4 {
        0 => (Quat::from_rotation_y(FRAC_PI_2), true),
        1 => (Quat::from_rotation_y(PI), false),
        2 => (Quat::from_rotation_y(-FRAC_PI_2), true),
        _ => (Quat::IDENTITY, false),
    }
}

/// Spawns every cover box as its model (walls as rows of blocks) near the
/// camera, and as a plain box beyond.
pub fn spawn_cover(commands: &mut Commands, models: &Models, world: &World, unit: &Handle<Mesh>, plain: [&Handle<StandardMaterial>; 2]) {
    for b in world.boxes() {
        let (rot, ns) = turn(b.facing);
        let (sx, sy) = (b.max[0] - b.min[0], b.max[1] - b.min[1]);
        let (len, depth) = if ns { (sy, sx) } else { (sx, sy) };
        let h = b.top - b.bottom;
        let base = to_bevy((b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0, b.bottom);
        let building = matches!(b.kind, Kind::Command | Kind::Bunker | Kind::Post);
        let range = if building { BUILDING_RANGE } else { PROP_RANGE };
        let (mesh, mat) = &models.props[b.kind as usize];
        let tiles = if b.kind == Kind::Wall { (len / (h * WALL_ASPECT)).round().max(1.0) as usize } else { 1 };
        let tile = len / tiles as f32;
        for i in 0..tiles {
            let along = -len / 2.0 + (i as f32 + 0.5) * tile;
            let tf = Transform::from_translation(base + rot * Vec3::new(along, 0.0, 0.0)).with_rotation(rot).with_scale(Vec3::new(tile, h, depth));
            commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), tf, VisibilityRange::abrupt(0.0, range)));
        }
        // The plain box, for the distance.
        let low = matches!(b.kind, Kind::Crate | Kind::Sandbags);
        let center = to_bevy((b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0, (b.bottom + b.top) / 2.0);
        commands.spawn((
            Mesh3d(unit.clone()),
            MeshMaterial3d(plain[low as usize].clone()),
            Transform::from_translation(center).with_scale(Vec3::new(sx, h, sy)),
            VisibilityRange::abrupt(range, f32::MAX),
        ));
    }
}

/// Small rock clusters (cosmetic: too low to look like cover), one in about
/// every third 32 m cell, off the sites and clear of boxes.
pub fn spawn_rocks(commands: &mut Commands, models: &Models, world: &World) {
    let cells = (lattice_game::movement::WORLD_SIZE / 32.0) as u64;
    let on_site = |x: f32, y: f32| {
        world.sites().iter().any(|s| (s.center[0] - x).abs().max((s.center[1] - y).abs()) < s.half + lattice_game::world::SITE_BLEND)
    };
    for cy in 0..cells {
        for cx in 0..cells {
            let mut h = (world.seed ^ 0x5EED_0F0C).wrapping_add(cx.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ cy.wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
            let mut next = || {
                h ^= h >> 33;
                h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
                h ^= h >> 29;
                (h >> 40) as f32 / (1u64 << 24) as f32
            };
            if next() > 0.33 {
                continue;
            }
            let (x, y) = ((cx as f32 + next()) * 32.0, (cy as f32 + next()) * 32.0);
            let mut clear = !on_site(x, y);
            world.boxes_near(x, y, 2.0, |b: &CoverBox| {
                clear &= x < b.min[0] - 2.0 || x > b.max[0] + 2.0 || y < b.min[1] - 2.0 || y > b.max[1] + 2.0;
            });
            if !clear {
                continue;
            }
            let (w, tall, yaw) = (0.8 + 0.8 * next(), 0.35 + 0.3 * next(), next() * std::f32::consts::TAU);
            let tf = Transform::from_translation(to_bevy(x, y, world.terrain(x, y) - 0.08))
                .with_rotation(Quat::from_rotation_y(yaw))
                .with_scale(Vec3::new(w, tall, w * (0.8 + 0.4 * next())));
            commands.spawn((Mesh3d(models.rocks.0.clone()), MeshMaterial3d(models.rocks.1.clone()), tf, VisibilityRange::abrupt(0.0, ROCK_RANGE)));
        }
    }
}

/// A player's soldier: the spawned scene under its body, and once it's
/// ready, its animation player and the meshes to tint.
#[derive(Component)]
pub struct Soldier {
    player: Option<Entity>,
    meshes: Vec<Entity>,
    clip: Option<Clip>,
    tint: Option<[u32; 3]>,
}

/// How a player moves, for its animation: its last drawn position and
/// smoothed horizontal speed and direction (relative to its facing).
#[derive(Component, Default)]
pub struct Motion {
    last: Option<Vec3>,
    speed: f32,
    backward: bool,
}

/// What a player is doing, as the scene draws it.
pub struct Pose {
    pub feet: Vec3,
    pub yaw: f32,
    pub airborne: bool,
    pub dead: bool,
    pub tint: Color,
}

/// Adds or removes `root`'s soldier (as a child, turned to face the body's
/// forward, -z), and keeps its animation and tint up to date.
#[allow(clippy::too_many_arguments)]
pub fn drive<F: bevy::ecs::query::QueryFilter>(
    commands: &mut Commands,
    models: &mut Models,
    mats: &mut Assets<StandardMaterial>,
    root: Entity,
    want: bool,
    pose: &Pose,
    dt: f32,
    motion: &mut Motion,
    soldier: Option<(Entity, &mut Soldier)>,
    players: &mut Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    material_q: &mut Query<&mut MeshMaterial3d<StandardMaterial>, F>,
) -> bool {
    // Speed from the drawn positions, smoothed over ~0.15 s.
    let speed = match motion.last {
        Some(last) if dt > 0.0 => {
            let d = Vec3::new(pose.feet.x - last.x, 0.0, pose.feet.z - last.z);
            let v = (d.length() / dt).min(30.0);
            let fwd = Quat::from_rotation_y(pose.yaw - FRAC_PI_2) * Vec3::NEG_Z;
            if v > 0.5 {
                motion.backward = d.normalize().dot(fwd) < -0.3;
            }
            v
        }
        _ => 0.0,
    };
    motion.last = Some(pose.feet);
    let k = (dt / 0.15).min(1.0);
    motion.speed += (speed - motion.speed) * k;

    let Some((entity, soldier)) = soldier else {
        if want {
            let child = commands
                .spawn((
                    WorldAssetRoot(models.soldier.clone()),
                    Transform::from_rotation(Quat::from_rotation_y(PI)),
                    Soldier { player: None, meshes: Vec::new(), clip: None, tint: None },
                ))
                .observe(ready)
                .id();
            commands.entity(root).add_child(child);
        }
        return false;
    };
    if !want {
        commands.entity(entity).despawn();
        return false;
    }
    let Some(player_entity) = soldier.player else { return false };

    // Tint: the soldier's white armor takes the player's color.
    let c = pose.tint.to_linear();
    let key = [c.red.to_bits(), c.green.to_bits(), c.blue.to_bits()];
    if soldier.tint != Some(key) {
        let handle = match models.tinted.get(&key) {
            Some(h) => h.clone(),
            None => {
                let Some(base) = mats.get(&models.soldier_material).cloned() else { return false };
                let h = mats.add(StandardMaterial { base_color: pose.tint, ..base });
                models.tinted.insert(key, h.clone());
                h
            }
        };
        for &m in &soldier.meshes {
            if let Ok(mut mm) = material_q.get_mut(m) {
                mm.0 = handle.clone();
            }
        }
        soldier.tint = Some(key);
    }

    // Animation: by state and speed.
    let v = motion.speed;
    let (clip, rate) = if pose.dead {
        (Clip::Death, 1.0)
    } else if pose.airborne {
        (Clip::Jump, 1.4)
    } else if v < 0.4 {
        (Clip::Idle, 1.0)
    } else if v < 4.5 {
        (Clip::Aim, (v / 1.54).clamp(0.6, 2.2))
    } else if v < 7.5 {
        (Clip::Run, v / 4.7)
    } else {
        (Clip::Sprint, v / 7.0)
    };
    let rate = if motion.backward && matches!(clip, Clip::Aim | Clip::Run) { -rate } else { rate };
    if let Ok((mut player, mut transitions)) = players.get_mut(player_entity) {
        let node = models.clips[clip as usize];
        if soldier.clip != Some(clip) {
            let fade = if clip == Clip::Death || soldier.clip == Some(Clip::Death) { 0.1 } else { 0.2 };
            let active = transitions.play(&mut player, node, Duration::from_secs_f32(fade));
            if clip == Clip::Death {
                active.set_repeat(bevy::animation::RepeatAnimation::Never);
            } else {
                active.repeat();
            }
            if clip == Clip::Jump {
                active.seek_to(0.35);
            }
            soldier.clip = Some(clip);
        }
        if let Some(active) = player.animation_mut(node) {
            active.set_speed(rate);
        }
    }
    true
}

/// When a soldier's scene has spawned: hook its animation player to the
/// shared graph, note its meshes (to tint), and put the rifle in its right
/// hand.
#[allow(clippy::too_many_arguments)]
fn ready(
    ev: On<WorldInstanceReady>,
    mut commands: Commands,
    models: Res<Models>,
    children: Query<&Children>,
    names: Query<&Name>,
    players: Query<(), With<AnimationPlayer>>,
    meshes: Query<(), With<MeshMaterial3d<StandardMaterial>>>,
    mut soldiers: Query<&mut Soldier>,
) {
    let root = ev.entity;
    let Ok(mut soldier) = soldiers.get_mut(root) else { return };
    for e in children.iter_descendants(root) {
        if players.contains(e) {
            commands.entity(e).insert((AnimationGraphHandle(models.graph.clone()), AnimationTransitions::new()));
            soldier.player = Some(e);
        }
        if meshes.contains(e) {
            soldier.meshes.push(e);
        }
        if names.get(e).is_ok_and(|n| n.as_str() == "RightHand") {
            commands.entity(e).with_child((Mesh3d(models.rifle.0.clone()), MeshMaterial3d(models.rifle.1.clone()), rifle_in_hand()));
        }
    }
}

/// The rifle in the right hand bone's frame (the skeleton is in
/// centimeters): 75 cm long, along the hand, muzzle forward.
/// `LATTICE_RIFLE="x,y,z,rx,ry,rz"` (cm, degrees) overrides it, for tuning.
fn rifle_in_hand() -> Transform {
    let v: Vec<f32> = std::env::var("LATTICE_RIFLE")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .filter(|v: &Vec<f32>| v.len() == 6)
        .unwrap_or_else(|| vec![2.0, 10.0, 4.0, 0.0, 90.0, -90.0]);
    let r = |d: f32| d.to_radians();
    Transform::from_translation(Vec3::new(v[0], v[1], v[2]))
        .with_rotation(Quat::from_euler(EulerRot::XYZ, r(v[3]), r(v[4]), r(v[5])))
        .with_scale(Vec3::splat(75.0))
}

/// The first-person rifle, in the view camera's frame: low and to the
/// right, muzzle (its -x) forward.
pub fn viewmodel(models: &Models) -> impl Bundle {
    (
        Mesh3d(models.rifle.0.clone()),
        MeshMaterial3d(models.rifle.1.clone()),
        Transform::from_translation(Vec3::new(0.2, -0.2, -0.5)).with_rotation(Quat::from_rotation_y(-FRAC_PI_2)).with_scale(Vec3::splat(0.7)),
        RenderLayers::layer(VIEWMODEL_LAYER),
    )
}

/// Which of the drawn players (by distance from the camera) get soldiers:
/// the nearest, within range; those that have one keep it a little longer.
pub fn soldier_set(mut near: Vec<(f32, u16, bool)>) -> std::collections::HashSet<u16> {
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.iter()
        .enumerate()
        .filter(|&(i, &(d, _, has))| if has { d < SOLDIER_RANGE + 20.0 && i < MAX_SOLDIERS + 10 } else { d < SOLDIER_RANGE && i < MAX_SOLDIERS })
        .map(|(_, &(_, e, _))| e)
        .collect()
}
