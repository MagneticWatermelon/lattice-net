//! What's drawn: terrain chunks, cover (as its models), and every player
//! the core knows: soldiers near the camera, capsules farther out.

use std::collections::HashMap;

use bevy::prelude::*;
use lattice_client_core::{How, RenderState};
use lattice_game::faction::faction;
use lattice_game::movement::{HEIGHT, RADIUS};
use lattice_game::world::World;

use crate::coords::{look_rotation, to_bevy, yaw_rotation};
use crate::models::{self, Aim, Models, Motion, Pose, Soldier};
use crate::terrain::{self, CHUNKS, COARSE, FINE};
use crate::{Frame, Net, Settings, View};

/// Chunks within this many chunk widths of the camera get the fine mesh
/// (8 = ~1 km).
const FINE_RADIUS: i32 = 8;
/// Fine meshes built per frame at most, nearest first (a hitch otherwise).
const FINE_PER_FRAME: usize = 6;

#[derive(Resource, Default)]
pub struct Scene {
    world: Option<std::sync::Arc<World>>,
    /// Every chunk's entity, and which level it shows (true: fine).
    chunks: Vec<(Entity, bool)>,
    coarse: Vec<Handle<Mesh>>,
    terrain_mat: Handle<StandardMaterial>,
    players: HashMap<u16, Entity>,
    ghosts: HashMap<u16, Entity>,
    own_ghost: Option<Entity>,
    pub drawn: [usize; 3],
}

#[derive(Resource)]
pub struct Looks {
    capsule: Handle<Mesh>,
    nose: Handle<Mesh>,
    tiers: [Handle<StandardMaterial>; 3],
    factions: [Handle<StandardMaterial>; 3],
    held: Handle<StandardMaterial>,
    new: Handle<StandardMaterial>,
    plain: Handle<StandardMaterial>,
    ghost: Handle<StandardMaterial>,
    own_ghost: Handle<StandardMaterial>,
    nose_mat: Handle<StandardMaterial>,
}

/// Faction colors (red, blue, purple), by `faction(entity)`.
pub const FACTION_COLORS: [Color; 3] = [Color::srgb(0.85, 0.25, 0.2), Color::srgb(0.25, 0.45, 0.9), Color::srgb(0.6, 0.3, 0.85)];
/// With tier colors on (T): near, mid, far; held and new.
const TIER_COLORS: [Color; 3] = [Color::srgb(0.25, 0.85, 0.35), Color::srgb(0.95, 0.75, 0.2), Color::srgb(0.9, 0.3, 0.25)];
const HELD_COLOR: Color = Color::srgb(0.5, 0.5, 0.5);
const NEW_COLOR: Color = Color::srgb(0.95, 0.95, 0.95);

/// A drawn player: a root at its feet, turned to its facing, with a capsule
/// child (and a nose on it, showing where it looks), and a soldier child
/// while it's near the camera.
#[derive(Component)]
pub struct Player;
#[derive(Component)]
pub struct Capsule;
#[derive(Component)]
pub struct Nose;

/// `Scene::players`' key for our own body (drawn in the chase and spectator
/// views).
const OWN: u16 = u16::MAX;

pub fn setup_looks(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mut mat = |c: Color| mats.add(StandardMaterial { base_color: c, perceptual_roughness: 0.8, ..default() });
    let tiers = TIER_COLORS.map(&mut mat);
    let factions = FACTION_COLORS.map(&mut mat);
    let (held, new, plain, nose_mat) = (mat(HELD_COLOR), mat(NEW_COLOR), mat(Color::srgb(0.35, 0.5, 0.85)), mat(Color::srgb(0.1, 0.1, 0.12)));
    let mut ghost = |c: Color| {
        mats.add(StandardMaterial { base_color: c, alpha_mode: AlphaMode::Blend, unlit: true, ..default() })
    };
    let looks = Looks {
        capsule: meshes.add(Capsule3d::new(RADIUS, HEIGHT - 2.0 * RADIUS)),
        nose: meshes.add(Cuboid::new(0.12, 0.12, 0.3)),
        tiers,
        factions,
        held,
        new,
        plain,
        ghost: ghost(Color::srgba(0.6, 0.85, 1.0, 0.35)),
        own_ghost: ghost(Color::srgba(1.0, 0.4, 0.9, 0.45)),
        nose_mat,
    };
    commands.insert_resource(looks);
}

/// Once welcomed: the world from its seed, the terrain at the coarse level
/// everywhere, every cover box, rocks, and the light.
#[allow(clippy::too_many_arguments)]
pub fn build_world(
    mut commands: Commands,
    net: Res<Net>,
    mut scene: ResMut<Scene>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    settings: Res<Settings>,
    models: Res<Models>,
) {
    if scene.world.is_some() {
        return;
    }
    let Some(w) = net.0.core.welcome() else { return };
    let world = World::shared(w.world_seed);
    scene.terrain_mat = mats.add(StandardMaterial { base_color: Color::WHITE, perceptual_roughness: 0.95, ..default() });
    for cy in 0..CHUNKS {
        for cx in 0..CHUNKS {
            let mesh = meshes.add(terrain::chunk(&world, cx, cy, COARSE).into_mesh());
            scene.coarse.push(mesh.clone());
            let e = commands.spawn((Mesh3d(mesh), MeshMaterial3d(scene.terrain_mat.clone()), Transform::default())).id();
            scene.chunks.push((e, false));
        }
    }
    let unit = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let wall = mats.add(StandardMaterial { base_color: Color::srgb(0.62, 0.62, 0.6), perceptual_roughness: 0.9, ..default() });
    let crate_ = mats.add(StandardMaterial { base_color: Color::srgb(0.55, 0.38, 0.2), perceptual_roughness: 0.9, ..default() });
    models::spawn_cover(&mut commands, &models, &world, &unit, [&wall, &crate_]);
    models::spawn_rocks(&mut commands, &models, &world);
    // The sun lights the first-person rifle too.
    commands.spawn((
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: settings.shadows, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.6, -0.9, 0.0)),
        bevy::camera::visibility::RenderLayers::from_layers(&[0, models::VIEWMODEL_LAYER]),
    ));
    scene.world = Some(world);
}

/// Fine meshes for the chunks around the camera, coarse ones farther out.
pub fn stream_terrain(view: Res<View>, mut scene: ResMut<Scene>, mut meshes: ResMut<Assets<Mesh>>, mut q: Query<&mut Mesh3d>) {
    let Some(world) = scene.world.clone() else { return };
    let (ccx, ccy) = terrain::chunk_of(view.eye[0], view.eye[1]);
    let mut want = Vec::new();
    for k in 0..scene.chunks.len() {
        let (e, fine) = scene.chunks[k];
        let (cx, cy) = ((k % CHUNKS) as i32, (k / CHUNKS) as i32);
        let d = (cx - ccx).abs().max((cy - ccy).abs());
        if d <= FINE_RADIUS && !fine {
            want.push((d, k));
        } else if d > FINE_RADIUS + 1 && fine {
            // Back to coarse; the fine mesh is freed with its last handle.
            if let Ok(mut m) = q.get_mut(e) {
                m.0 = scene.coarse[k].clone();
            }
            scene.chunks[k].1 = false;
        }
    }
    want.sort_unstable();
    for &(_, k) in want.iter().take(FINE_PER_FRAME) {
        let (e, _) = scene.chunks[k];
        if let Ok(mut m) = q.get_mut(e) {
            m.0 = meshes.add(terrain::chunk(&world, k % CHUNKS, k / CHUNKS, FINE).into_mesh());
            scene.chunks[k].1 = true;
        }
    }
}

/// Faction colors, or with `tier_colors` how it's drawn (tier, held, new).
/// The dead keep theirs (they lie down).
fn material<'a>(looks: &'a Looks, entity: u16, s: &RenderState, tier_colors: bool) -> &'a Handle<StandardMaterial> {
    if !tier_colors {
        return &looks.factions[faction(entity) as usize];
    }
    match s.how {
        How::Held => &looks.held,
        How::New => &looks.new,
        _ => &looks.tiers[s.tier as usize],
    }
}

/// The same, as a color (a soldier's armor takes it).
fn tint(entity: u16, s: &RenderState, tier_colors: bool) -> Color {
    if !tier_colors {
        return FACTION_COLORS[faction(entity) as usize];
    }
    match s.how {
        How::Held => HELD_COLOR,
        How::New => NEW_COLOR,
        _ => TIER_COLORS[s.tier as usize],
    }
}

/// Centered capsule transform for feet at game (x, y, z), facing `yaw`;
/// the dead lie on the ground, along where they faced. (For the ghosts.)
fn body(pos: [f32; 3], yaw: f32, dead: bool) -> Transform {
    let root = Transform::from_translation(to_bevy(pos[0], pos[1], pos[2])).with_rotation(yaw_rotation(yaw));
    root.mul_transform(capsule(dead))
}

/// The capsule in its player's frame (feet, facing -z).
fn capsule(dead: bool) -> Transform {
    if dead {
        Transform::from_xyz(0.0, RADIUS, 0.0).with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
    } else {
        Transform::from_xyz(0.0, HEIGHT / 2.0, 0.0)
    }
}

/// One body to draw this frame.
struct Drawn {
    key: u16,
    feet: [f32; 3],
    yaw: f32,
    pitch: f32,
    airborne: bool,
    dead: bool,
    mat: Handle<StandardMaterial>,
    tint: Color,
}

type Roots<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static mut Motion, &'static mut Aim, &'static Children),
    (With<Player>, Without<Capsule>, Without<Nose>),
>;
type Capsules<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static mut MeshMaterial3d<StandardMaterial>, &'static mut Visibility, &'static Children),
    (With<Capsule>, Without<Player>, Without<Nose>),
>;
type Noses<'w, 's> = Query<'w, 's, &'static mut Transform, (With<Nose>, Without<Player>, Without<Capsule>)>;
type SoldierMeshes<'w, 's> = Query<'w, 's, (&'static mut Mesh3d, &'static mut MeshMaterial3d<StandardMaterial>), (Without<Capsule>, Without<Nose>)>;

/// Draws every entity at the render step (and our own body in the chase
/// and spectator views): spawns, moves and despawns bodies to match, gives
/// the nearest ones animated soldiers, and the server ghosts when they're
/// on.
#[allow(clippy::too_many_arguments)]
pub fn sync_players(
    mut commands: Commands,
    mut net: ResMut<Net>,
    frame: Res<Frame>,
    settings: Res<Settings>,
    view: Res<View>,
    looks: Res<Looks>,
    mut scene: ResMut<Scene>,
    mut models: ResMut<Models>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut roots: Roots,
    mut capsules: Capsules,
    mut noses: Noses,
    mut soldiers: Query<&mut Soldier>,
    mut anim: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    mut soldier_meshes: SoldierMeshes,
) {
    if scene.world.is_none() {
        return;
    }
    let own = net.0.core.welcome().map(|w| w.entity);
    let mut states = Vec::new();
    net.0.core.render(frame.now, |e, s| {
        if Some(e) != own {
            states.push((e, *s));
        }
    });
    scene.drawn = [0; 3];
    let mut drawn: Vec<Drawn> = Vec::with_capacity(states.len() + 1);
    for (e, s) in &states {
        scene.drawn[s.tier as usize] += 1;
        let mat = material(&looks, *e, s, settings.tier_colors).clone();
        drawn.push(Drawn { key: *e, feet: s.pos, yaw: s.yaw, pitch: s.pitch, airborne: s.airborne, dead: s.dead, mat, tint: tint(*e, s, settings.tier_colors) });
    }
    if view.mode != crate::controls::Mode::FirstPerson && view.has_body {
        let dead = net.0.core.is_dead();
        let (mat, tint) = match own {
            Some(e) => (looks.factions[faction(e) as usize].clone(), FACTION_COLORS[faction(e) as usize]),
            None => (looks.plain.clone(), Color::WHITE),
        };
        let airborne = !net.0.core.predicted().grounded;
        drawn.push(Drawn { key: OWN, feet: view.feet, yaw: view.yaw, pitch: view.pitch, airborne, dead, mat, tint });
    }

    // Everyone's a soldier (up to the cap, nearest first).
    let eye = Vec3::from(view.eye);
    let distance = |d: &Drawn| if d.key == OWN { 0.0 } else { eye.distance(Vec3::from(d.feet)) };
    let want = models::soldier_set(drawn.iter().map(|d| (distance(d), d.key)).collect());

    let mut seen = std::collections::HashSet::with_capacity(drawn.len());
    for d in &drawn {
        seen.insert(d.key);
        let root_tf = Transform::from_translation(to_bevy(d.feet[0], d.feet[1], d.feet[2])).with_rotation(yaw_rotation(d.yaw));
        let Some(&root) = scene.players.get(&d.key) else {
            let root = commands
                .spawn((Player, root_tf, Visibility::default(), Motion::default(), Aim { pitch: d.pitch }))
                .with_children(|p| {
                    p.spawn((Capsule, Mesh3d(looks.capsule.clone()), MeshMaterial3d(d.mat.clone()), capsule(d.dead))).with_child((
                        Nose,
                        Mesh3d(looks.nose.clone()),
                        MeshMaterial3d(looks.nose_mat.clone()),
                        nose(d.pitch),
                    ));
                })
                .id();
            scene.players.insert(d.key, root);
            continue;
        };
        let Ok((mut tf, mut motion, mut aim, children)) = roots.get_mut(root) else { continue };
        *tf = root_tf;
        aim.pitch = d.pitch;
        let pose = Pose { feet: tf.translation, yaw: d.yaw, airborne: d.airborne, dead: d.dead, tint: d.tint, distance: distance(d) };
        let child = children.iter().find(|&c| soldiers.contains(c));
        let mut soldier = child.and_then(|c| soldiers.get_mut(c).ok().map(|s| (c, s)));
        let arg = soldier.as_mut().map(|(c, s)| (*c, &mut **s));
        let shown = models::drive(&mut commands, &mut models, &mut mats, root, want.contains(&d.key), &pose, frame.dt, &mut motion, arg, &mut anim, &mut soldier_meshes);
        for c in children.iter() {
            if let Ok((mut ctf, mut m, mut vis, kids)) = capsules.get_mut(c) {
                *ctf = capsule(d.dead);
                if m.0 != d.mat {
                    m.0 = d.mat.clone();
                }
                let v = if shown { Visibility::Hidden } else { Visibility::Inherited };
                if *vis != v {
                    *vis = v;
                }
                for k in kids.iter() {
                    if let Ok(mut n) = noses.get_mut(k) {
                        *n = nose(d.pitch);
                    }
                }
            }
        }
    }
    scene.players.retain(|e, id| {
        let keep = seen.contains(e);
        if !keep {
            commands.entity(*id).despawn();
        }
        keep
    });

    // Server ghosts: where the newest samples say everyone is, and where the
    // server has us.
    let wanted: Vec<(u16, [f32; 3], f32, bool)> = if settings.ghosts {
        let ents = net.0.core.entities();
        states.iter().filter_map(|(e, _)| ents.and_then(|x| x.newest(*e)).map(|n| (*e, n.pos, n.yaw, n.dead))).collect()
    } else {
        Vec::new()
    };
    let mut keep = std::collections::HashSet::with_capacity(wanted.len());
    for (e, pos, yaw, dead) in wanted {
        keep.insert(e);
        let t = body(pos, yaw, dead);
        match scene.ghosts.get(&e) {
            Some(&id) => {
                commands.entity(id).insert(t);
            }
            None => {
                let id = commands.spawn((Mesh3d(looks.capsule.clone()), MeshMaterial3d(looks.ghost.clone()), t)).id();
                scene.ghosts.insert(e, id);
            }
        }
    }
    scene.ghosts.retain(|e, id| {
        let k = keep.contains(e);
        if !k {
            commands.entity(*id).despawn();
        }
        k
    });
    let own_server = net.0.core.server_own().filter(|_| settings.ghosts);
    match (own_server, scene.own_ghost) {
        (Some((s, _)), Some(id)) => {
            commands.entity(id).insert(body([s.pos[0], s.pos[1], s.z], 0.0, false));
        }
        (Some((s, _)), None) => {
            let t = body([s.pos[0], s.pos[1], s.z], 0.0, false);
            scene.own_ghost = Some(commands.spawn((Mesh3d(looks.capsule.clone()), MeshMaterial3d(looks.own_ghost.clone()), t)).id());
        }
        (None, Some(id)) => {
            commands.entity(id).despawn();
            scene.own_ghost = None;
        }
        (None, None) => {}
    }
}

/// The nose sits at eye height, in front, pitched with the aim.
fn nose(pitch: f32) -> Transform {
    let rot = look_rotation(std::f32::consts::FRAC_PI_2, pitch); // facing local -z
    Transform::from_translation(Vec3::new(0.0, HEIGHT / 2.0 - 0.3, 0.0) + rot * Vec3::new(0.0, 0.0, -0.35)).with_rotation(rot)
}
