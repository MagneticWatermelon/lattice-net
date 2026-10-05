//! What's drawn: terrain chunks, cover, and every player the core knows.

use std::collections::HashMap;

use bevy::prelude::*;
use lattice_client_core::{How, RenderState};
use lattice_game::faction::faction;
use lattice_game::movement::{HEIGHT, RADIUS};
use lattice_game::world::World;

use crate::coords::{look_rotation, to_bevy, yaw_rotation};
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
    dead: Handle<StandardMaterial>,
    held: Handle<StandardMaterial>,
    new: Handle<StandardMaterial>,
    plain: Handle<StandardMaterial>,
    ghost: Handle<StandardMaterial>,
    own_ghost: Handle<StandardMaterial>,
    nose_mat: Handle<StandardMaterial>,
}

/// Faction colors (red, blue, purple), by `faction(entity)`.
pub const FACTION_COLORS: [Color; 3] = [Color::srgb(0.85, 0.25, 0.2), Color::srgb(0.25, 0.45, 0.9), Color::srgb(0.6, 0.3, 0.85)];

/// A drawn player; its nose (a child) shows where it looks.
#[derive(Component)]
pub struct Player;
#[derive(Component)]
pub struct Nose;

pub fn setup_looks(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mut mat = |c: Color| mats.add(StandardMaterial { base_color: c, perceptual_roughness: 0.8, ..default() });
    let tiers = [mat(Color::srgb(0.25, 0.85, 0.35)), mat(Color::srgb(0.95, 0.75, 0.2)), mat(Color::srgb(0.9, 0.3, 0.25))];
    let factions = FACTION_COLORS.map(&mut mat);
    let dead = mat(Color::srgb(0.25, 0.25, 0.27));
    let (held, new, plain, nose_mat) = (mat(Color::srgb(0.5, 0.5, 0.5)), mat(Color::srgb(0.95, 0.95, 0.95)), mat(Color::srgb(0.35, 0.5, 0.85)), mat(Color::srgb(0.1, 0.1, 0.12)));
    let mut ghost = |c: Color| {
        mats.add(StandardMaterial { base_color: c, alpha_mode: AlphaMode::Blend, unlit: true, ..default() })
    };
    let looks = Looks {
        capsule: meshes.add(Capsule3d::new(RADIUS, HEIGHT - 2.0 * RADIUS)),
        nose: meshes.add(Cuboid::new(0.12, 0.12, 0.3)),
        tiers,
        factions,
        dead,
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
/// everywhere, every cover box, and the light.
pub fn build_world(
    mut commands: Commands,
    net: Res<Net>,
    mut scene: ResMut<Scene>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    settings: Res<Settings>,
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
    for b in world.boxes() {
        let size = [b.max[0] - b.min[0], b.max[1] - b.min[1], b.top - b.bottom];
        let center = to_bevy((b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0, (b.bottom + b.top) / 2.0);
        let low = b.top - world.terrain((b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0) < 1.5;
        commands.spawn((
            Mesh3d(unit.clone()),
            MeshMaterial3d(if low { crate_.clone() } else { wall.clone() }),
            Transform::from_translation(center).with_scale(Vec3::new(size[0], size[2], size[1])),
        ));
    }
    commands.spawn((
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: settings.shadows, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.6, -0.9, 0.0)),
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
/// The dead are grey either way.
fn material<'a>(looks: &'a Looks, entity: u16, s: &RenderState, tier_colors: bool) -> &'a Handle<StandardMaterial> {
    if s.dead {
        return &looks.dead;
    }
    if !tier_colors {
        return &looks.factions[faction(entity) as usize];
    }
    match s.how {
        How::Held => &looks.held,
        How::New => &looks.new,
        _ => &looks.tiers[s.tier as usize],
    }
}

/// Centered capsule transform for feet at game (x, y, z), facing `yaw`;
/// the dead lie on the ground, along where they faced.
fn body(pos: [f32; 3], yaw: f32, dead: bool) -> Transform {
    if dead {
        let rot = yaw_rotation(yaw) * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        return Transform::from_translation(to_bevy(pos[0], pos[1], pos[2] + RADIUS)).with_rotation(rot);
    }
    Transform::from_translation(to_bevy(pos[0], pos[1], pos[2] + HEIGHT / 2.0)).with_rotation(yaw_rotation(yaw))
}

type BodyParts<'a> = (&'a mut Transform, &'a mut MeshMaterial3d<StandardMaterial>, &'a Children);

/// Draws every entity at the render step: spawns, moves and despawns
/// capsules to match, and the server ghosts when they're on.
#[allow(clippy::too_many_arguments)]
pub fn sync_players(
    mut commands: Commands,
    mut net: ResMut<Net>,
    frame: Res<Frame>,
    settings: Res<Settings>,
    looks: Res<Looks>,
    mut scene: ResMut<Scene>,
    mut bodies: Query<BodyParts, (With<Player>, Without<Nose>)>,
    mut noses: Query<&mut Transform, (With<Nose>, Without<Player>)>,
) {
    if scene.world.is_none() {
        return;
    }
    let own = net.0.core.welcome().map(|w| w.entity);
    let mut drawn = Vec::new();
    net.0.core.render(frame.now, |e, s| {
        if Some(e) != own {
            drawn.push((e, *s));
        }
    });
    scene.drawn = [0; 3];
    let mut seen = std::collections::HashSet::with_capacity(drawn.len());
    for (e, s) in &drawn {
        seen.insert(*e);
        scene.drawn[s.tier as usize] += 1;
        let t = body(s.pos, s.yaw, s.dead);
        let mat = material(&looks, *e, s, settings.tier_colors).clone();
        match scene.players.get(e).copied() {
            Some(id) => {
                if let Ok((mut tf, mut m, children)) = bodies.get_mut(id) {
                    *tf = t;
                    if m.0 != mat {
                        m.0 = mat;
                    }
                    for c in children.iter() {
                        if let Ok(mut n) = noses.get_mut(c) {
                            *n = nose(s.pitch);
                        }
                    }
                }
            }
            None => {
                let id = commands
                    .spawn((Player, Mesh3d(looks.capsule.clone()), MeshMaterial3d(mat), t))
                    .with_child((Nose, Mesh3d(looks.nose.clone()), MeshMaterial3d(looks.nose_mat.clone()), nose(s.pitch)))
                    .id();
                scene.players.insert(*e, id);
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
        drawn.iter().filter_map(|(e, _)| ents.and_then(|x| x.newest(*e)).map(|n| (*e, n.pos, n.yaw, n.dead))).collect()
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

/// Our own body, drawn in the chase and spectator views.
#[derive(Component)]
pub struct OwnBody;

pub fn own_body(
    mut commands: Commands,
    view: Res<View>,
    looks: Res<Looks>,
    net: Res<Net>,
    mut q: Query<(Entity, &mut Transform, &mut MeshMaterial3d<StandardMaterial>), With<OwnBody>>,
) {
    let show = view.mode != crate::controls::Mode::FirstPerson && view.has_body;
    let dead = net.0.core.is_dead();
    let t = body(view.feet, view.yaw, dead);
    let mat = match net.0.core.welcome() {
        _ if dead => looks.dead.clone(),
        Some(w) => looks.factions[faction(w.entity) as usize].clone(),
        None => looks.plain.clone(),
    };
    match (q.single_mut(), show) {
        (Ok((_, mut tf, mut m)), true) => {
            *tf = t;
            if m.0 != mat {
                m.0 = mat;
            }
        }
        (Ok((e, _, _)), false) => commands.entity(e).despawn(),
        (Err(_), true) => {
            commands
                .spawn((OwnBody, Mesh3d(looks.capsule.clone()), MeshMaterial3d(mat), t))
                .with_child((Nose, Mesh3d(looks.nose.clone()), MeshMaterial3d(looks.nose_mat.clone()), nose(view.pitch)));
        }
        (Err(_), false) => {}
    }
}
