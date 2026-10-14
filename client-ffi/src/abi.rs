//! The plugin's C ABI types: blittable `#[repr(C)]` structs that cross the
//! boundary, and the codes they carry. Each struct is declared through
//! `ffi_structs!`, which also records its layout (`STRUCTS`: size, and every
//! field's type and offset from `offset_of!`), so the C# bindings are
//! generated from the real layout (`csharp`), never retyped.
//!
//! Positions are game space: x east, y north, z up, meters. Angles are
//! radians: yaw counterclockwise from east, pitch up.

/// A field of an exported struct, as the C# generator sees it.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    /// The Rust type, as written (`f32`, `LtVec3`, `[u64; 12]`).
    pub ty: &'static str,
    pub offset: usize,
    pub doc: &'static str,
}

/// An exported struct's layout.
#[derive(Debug, Clone, Copy)]
pub struct StructInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub size: usize,
    pub align: usize,
    pub fields: &'static [Field],
}

macro_rules! ffi_structs {
    ($( $(#[doc = $sdoc:literal])* pub struct $name:ident { $( $(#[doc = $fdoc:literal])* pub $field:ident : $ty:ty ),* $(,)? } )*) => {
        $(
            $(#[doc = $sdoc])*
            #[repr(C)]
            #[derive(Debug, Clone, Copy, Default, PartialEq)]
            pub struct $name { $( $(#[doc = $fdoc])* pub $field: $ty ),* }
        )*
        /// Every exported struct's layout, in declaration order.
        pub const STRUCTS: &[StructInfo] = &[ $(
            StructInfo {
                name: stringify!($name),
                doc: concat!($($sdoc, "\n"),*),
                size: std::mem::size_of::<$name>(),
                align: std::mem::align_of::<$name>(),
                fields: &[ $( Field {
                    name: stringify!($field),
                    ty: stringify!($ty),
                    offset: std::mem::offset_of!($name, $field),
                    doc: concat!($($fdoc, "\n"),*),
                } ),* ],
            },
        )* ];
    };
}

ffi_structs! {
    /// A point or direction in game space (x east, y north, z up).
    pub struct LtVec3 {
        pub x: f32,
        pub y: f32,
        pub z: f32,
    }

    /// How to connect: the render delays (0 = the defaults: near 67 ms,
    /// growing up to 133 ms; mid and far 200 ms), and how to run.
    pub struct LtConfig {
        pub near_delay_ms: f32,
        pub near_delay_max_ms: f32,
        pub mid_delay_ms: f32,
        /// `LT_CONFIG_*`.
        pub flags: u32,
    }

    /// This frame's intent: where the player looks and wants to go.
    pub struct LtIntent {
        /// Radians, counterclockwise from east.
        pub yaw: f32,
        /// Radians, up from level.
        pub pitch: f32,
        /// -1..1 each, from the keys or a stick; turned into a world-space
        /// stick by the plugin.
        pub forward: f32,
        pub right: f32,
        /// `LT_BUTTON_*`.
        pub buttons: u8,
    }

    /// A trigger pull.
    pub struct LtFire {
        /// The view as the shot leaves (before its own recoil kick).
        pub yaw: f32,
        pub pitch: f32,
        /// Aiming down sights (the inputs must say so too, `LT_BUTTON_ADS`).
        pub ads: u8,
        /// Where the tracer leaves from: meters right, up and forward of
        /// the eye, in the view's frame (the drawn muzzle).
        pub muzzle: LtVec3,
    }

    /// A shot that fired.
    pub struct LtShot {
        /// Where it goes: in the cone of fire, along our own pick (the
        /// server's is secret; the tracer is re-aimed when it says).
        pub dir: LtVec3,
        /// The tracer's muzzle, and the eye the shot left from.
        pub muzzle: LtVec3,
        pub eye: LtVec3,
    }

    /// Our own player.
    pub struct LtOwn {
        /// Where to draw us (the feet), between predicted steps, corrections
        /// smoothed away.
        pub pos: LtVec3,
        /// The newest predicted velocity (z: vertical), m/s.
        pub vel: LtVec3,
        /// Where the server last had us (the server ghost): a round trip
        /// behind the prediction.
        pub server_pos: LtVec3,
        /// The cone of fire a shot now would have, degrees (the reticle).
        pub cone_deg: f32,
        /// The newest input seq.
        pub seq: u32,
        pub entity: u16,
        pub health: u8,
        pub dead: u8,
        pub grounded: u8,
        /// The server has welcomed us: everything above is real.
        pub welcomed: u8,
        pub faction: u8,
        /// Changes with every death and respawn.
        pub life: u8,
        /// Crouching (predicted, like the position).
        pub crouched: u8,
        /// The rifle's rounds (predicted): in the magazine, and carried.
        pub mag: u8,
        pub reserve: u16,
        /// Seconds left of the reload under way; 0: none.
        pub reload: f32,
    }

    /// Another player, as drawn this frame.
    pub struct LtEntity {
        pub id: u16,
        pub faction: u8,
        /// `LT_TIER_*` of its newest sample.
        pub tier: u8,
        /// `LT_HOW_*`: interpolated, extrapolated, held, new.
        pub how: u8,
        /// `LT_FLAG_*`: airborne, dead, aiming down sights, crouched, reloading.
        pub flags: u8,
        pub health: u8,
        pub reserved: u8,
        /// The feet.
        pub pos: LtVec3,
        pub yaw: f32,
        pub pitch: f32,
        /// The game step it's drawn at.
        pub at: f64,
    }

    /// Combat news.
    pub struct LtEvent {
        /// `LT_EVENT_*`.
        pub kind: u8,
        /// `LT_EVENT_HEAD`, `LT_EVENT_KILLED`.
        pub flags: u8,
        /// Hit: damage dealt; Hurt: damage taken.
        pub amount: u8,
        /// Hurt: the yaw from us to the shooter, in 1/256 turns.
        pub dir: u8,
        /// Hit: the target; Hurt: the shooter; Kill: the killer.
        pub a: u16,
        /// Kill: the victim.
        pub b: u16,
    }

    /// A moment to show: a shooter drawn firing, or a tracer's impact.
    pub struct LtFx {
        /// `LT_FX_*`.
        pub kind: u8,
        /// `LT_TRACER_*`: whose shot.
        pub tracer: u8,
        pub faction: u8,
        /// Impacts: `LT_SURFACE_TERRAIN`, or the cover's `LT_COVER_*`.
        pub surface: u8,
        /// The shooter, or `LT_NOBODY`.
        pub shooter: u16,
        pub reserved: u16,
        /// Shots: the muzzle; impacts: the point hit.
        pub pos: LtVec3,
        /// The shot's direction (at impact, the flight's).
        pub dir: LtVec3,
        /// Impacts: the surface's normal.
        pub normal: LtVec3,
    }

    /// A tracer to draw this frame: a streak from its tail to its head.
    pub struct LtTracer {
        pub head: LtVec3,
        pub tail: LtVec3,
        /// `LT_TRACER_*`.
        pub kind: u8,
        pub faction: u8,
        pub reserved: u16,
        /// Seconds flown.
        pub age: f32,
    }

    /// The connection and the client's numbers, for the net graph.
    pub struct LtStats {
        /// `LT_STATE_*`.
        pub state: i32,
        pub rtt_ms: f32,
        pub rtt_min_ms: f32,
        pub rtt_max_ms: f32,
        /// 0..1.
        pub loss: f32,
        /// Average server wait (arrival -> applied) of our inputs, ms.
        pub wait_ms: f32,
        /// Near render delay now (adaptive), and actual delay of the last
        /// render behind the newest step, ms.
        pub near_delay_ms: f32,
        pub render_delay_ms: f32,
        /// Mid and far are drawn this much behind near, ms.
        pub mid_lag_ms: f32,
        pub correction_error_max: f32,
        pub push_error_max: f32,
        pub own_offset_max: f32,
        pub server_step: u32,
        /// Players known, and drawn last frame per tier.
        pub entities: u32,
        pub drawn: [u32; 3],
        /// The server's pace, per mille; its load level; our bandwidth level.
        pub pace: u16,
        pub level: u8,
        pub client_level: u8,
        pub bytes_sent: u64,
        pub bytes_received: u64,
        pub packets_sent: u64,
        pub packets_received: u64,
        pub packets_lost: u64,
        pub snapshots: u64,
        pub corrections: u64,
        pub push_corrections: u64,
        /// Snapshots whose rifle rounds disagreed with ours (replayed).
        pub ammo_corrections: u64,
        pub resyncs: u64,
        pub shots_dropped: u64,
        pub life_events: u64,
        pub shots: u64,
        pub hits_confirmed: u64,
        pub kills_confirmed: u64,
        pub hurts: u64,
        pub damage_taken: u64,
        pub kills_heard: u64,
        pub shots_seen: u64,
        pub activity_cells: u64,
        pub activity_shots: u64,
        pub ambient: u64,
        pub clock_snaps: u64,
        pub near_decode_errors: u64,
        pub bad_messages: u64,
        /// Entity-frames per tier (near, mid, far) and how they were drawn
        /// (interpolated, extrapolated, held, new): [tier * 4 + how].
        pub frames: [u64; 12],
        /// Entity-frames drawn moving faster than any player runs, per tier.
        pub streaks: [u64; 3],
        /// Samples that moved what's on screen by more than 5 cm, and the
        /// largest such pop (mm), per tier.
        pub pops: [u64; 3],
        pub pop_max_mm: [u32; 3],
        /// 1: the server's world differs from the one we built from its id
        /// (its Welcome's digest): an older or newer plugin than the server.
        pub world_mismatch: u8,
    }

    /// The world's size and contents.
    pub struct LtWorldInfo {
        /// Its id: `LT_WORLD_KILN`, or a seed for Harrow.
        pub seed: u64,
        /// Of the terrain and cover: two machines with the same digest built
        /// the same world.
        pub digest: u64,
        /// Edge length, m.
        pub size: f32,
        /// Height sample spacing, m.
        pub res: f32,
        /// Height samples per side.
        pub samples: u32,
        pub boxes: u32,
        pub sites: u32,
        /// `LT_CONTINENT_*`: which generator made it (how it's drawn).
        pub continent: u8,
        /// Routes (`lt_world_lanes`), all their points, control points.
        pub lanes: u32,
        pub lane_points: u32,
        pub points: u32,
        /// Force volumes (`lt_world_volumes`), and how many height cells are
        /// holes in the terrain (`lt_world_holes`).
        pub volumes: u32,
        pub holes: u32,
    }

    /// A cover box: blocks movement and shots. Turned about the vertical (its
    /// axis), its top level or rising along its axis (a wedge), or a shield
    /// that lets one faction through.
    pub struct LtBox {
        /// Its footprint's bounds (a turned box's: around it); z: its bottom.
        pub min: LtVec3,
        /// z: its top (a wedge's at the high end of its axis).
        pub max: LtVec3,
        /// Its footprint's center.
        pub center_x: f32,
        pub center_y: f32,
        /// Half its size along its axis and across it.
        pub half_along: f32,
        pub half_across: f32,
        /// Its axis, a unit vector: (1, 0) for a box that isn't turned.
        pub axis_x: f32,
        pub axis_y: f32,
        /// A wedge's top at the low end of its axis (`max.z` for a box).
        pub low: f32,
        /// `LT_COVER_*`: what it's drawn as.
        pub kind: u8,
        /// Which way its front faces, in its own frame: 0 along its axis, 1 a
        /// quarter turn left of it, 2, 3.
        pub facing: u8,
        /// A shield's faction, the one it lets through; `LT_SOLID` for the rest.
        pub pass: u8,
        /// A shield that belongs to a site: it lets through whoever holds
        /// the site (`lt_territory`), and `pass` is who held it at the start.
        /// `LT_NO_SITE` for the rest.
        pub site: u16,
        /// When it's there (`LT_WHEN_ALWAYS`, or `LT_WHEN_BRIDGE`: a living
        /// bridge's, at night: `LtClock::bridges`).
        pub when: u8,
    }

    /// A force volume (an axis-aligned box of air): a lift, a drop shaft or
    /// a spring.
    pub struct LtVolume {
        pub min: LtVec3,
        pub max: LtVec3,
        /// What it does (`LT_FORCE_*`): a lift's speed up (z), a drop shaft's
        /// down (z, negative), a spring's throw.
        pub vel: LtVec3,
        pub force: u8,
        /// When it's there (`LT_WHEN_*`): always; a sandfall's, in its set's
        /// turn (`LtClock::falls`); a vent lift, always there but running
        /// for `LT_VENT_ON` steps of every `LT_VENT_CYCLE`, `phase` steps
        /// into its cycle (running while `(step + phase) % LT_VENT_CYCLE <
        /// LT_VENT_ON`), resting it only slows a fall.
        pub when: u8,
        pub phase: u16,
    }

    /// A site: a base, an outpost, a great site or a Landing.
    pub struct LtSite {
        pub x: f32,
        pub y: f32,
        /// Half its size (Harrow's: the side of its flat square).
        pub half: f32,
        /// The height of its main ground.
        pub level: f32,
        /// A base (else an outpost or a great site).
        pub base: u8,
        /// `LT_SITE_*`.
        pub kind: u8,
        /// Its sector, 0-2 (Kiln's factions' starting thirds); 255: none.
        pub sector: u8,
    }

    /// A route: a lane between two sites (`a` != `b`), or a way inside one.
    /// Its points are `count` of `lt_world_lane_points` from `first`.
    pub struct LtLane {
        pub a: u16,
        pub b: u16,
        pub first: u32,
        pub count: u32,
        /// 1: only walked from `a` to `b` (up a lift, down a drop shaft).
        pub one_way: u8,
        /// When it can be walked (`LT_WHEN_*`: over a living bridge, down a
        /// sandfall; else always).
        pub when: u8,
    }

    /// A control point: holding it holds the site's tap.
    pub struct LtPoint {
        pub pos: LtVec3,
        pub site: u16,
    }

    /// A site's state (U5): who holds it, and its fight, as the server last
    /// said, at the step we're predicting.
    pub struct LtSiteState {
        /// How far its meter is toward `attacker` taking it, 0..1.
        pub meter: f32,
        /// Seconds since it last changed hands (since the start: large).
        pub held_secs: f32,
        /// Seconds till it can be attacked again (0: it can).
        pub locked_secs: f32,
        /// Its holder (`LT_NO_ONE`: no one).
        pub owner: u8,
        /// The faction its meter is moving toward (`LT_NO_ONE`: none).
        pub attacker: u8,
        /// Bit f: faction f may attack it now (linked to a site it holds,
        /// not its own, not locked, not a Landing).
        pub attackable: u8,
        /// How many points it has; who holds each (`LT_NO_ONE`: no one);
        /// bit i: point i is contested.
        pub point_count: u8,
        pub points: [u8; 6],
        pub contested: u8,
    }

    /// Kiln's clock (K4): the day and the clockwork, from the game step
    /// we're predicting (`lt_clock`). The same on every client: it's the
    /// step's.
    pub struct LtClock {
        /// The game step.
        pub step: u32,
        /// Its place in the cycle (`LT_CYCLE_STEPS`, from the day's start).
        pub cycle_step: u32,
        /// Seconds into the cycle, between steps (for the sky).
        pub cycle_secs: f32,
        /// `LT_PHASE_*`.
        pub phase: u8,
        /// 1: the living bridges stand (`LT_WHEN_BRIDGE` boxes are there).
        pub bridges: u8,
        /// Which sandfalls pour: `LT_WHEN_FALLS_A` or `LT_WHEN_FALLS_B`.
        pub falls: u8,
    }

    /// Where a segment first hits the ground or cover.
    pub struct LtHit {
        pub pos: LtVec3,
        pub normal: LtVec3,
        /// How far along the segment, 0..1.
        pub t: f32,
        /// `LT_SURFACE_TERRAIN`, or the cover's `LT_COVER_*`.
        pub surface: u8,
    }
}

/// Integer codes the structs carry, generated into C# as constants.
pub const CODES: &[(&str, i64, &str)] = &[
    ("LT_STATE_CONNECTING", 0, "Connecting."),
    ("LT_STATE_CONNECTED", 1, "Connected."),
    ("LT_STATE_DENIED", 2, "The server refused the token (key or server id?)."),
    ("LT_STATE_TIMED_OUT", 3, "No word from the server for 5 s."),
    ("LT_STATE_DISCONNECTED", 4, "Closed, by either side."),
    ("LT_STATE_ERROR", -1, "A null or broken session (see lt_last_error)."),
    ("LT_BUTTON_SPRINT", lattice_game::movement::BUTTON_SPRINT as i64, "Sprint (ignored down the sights)."),
    ("LT_BUTTON_JUMP", lattice_game::movement::BUTTON_JUMP as i64, "Jump."),
    ("LT_BUTTON_ADS", lattice_game::movement::BUTTON_ADS as i64, "Aim down sights: half speed, no sprint, a tight cone."),
    ("LT_BUTTON_CROUCH", lattice_game::movement::BUTTON_CROUCH as i64, "Crouch while held: lower, slower, no sprint or jump, a tighter cone."),
    ("LT_BUTTON_RELOAD", lattice_game::movement::BUTTON_RELOAD as i64, "Reload the rifle (an empty magazine reloads by itself)."),
    ("LT_TIER_NEAR", 0, "30 Hz, within 150 m (plus squad and contacts)."),
    ("LT_TIER_MID", 1, "10 Hz, within 500 m."),
    ("LT_TIER_FAR", 2, "2 Hz, within 1.5 km."),
    ("LT_HOW_INTERPOLATED", 0, "Between two samples."),
    ("LT_HOW_EXTRAPOLATED", 1, "Past the newest sample."),
    ("LT_HOW_HELD", 2, "Its updates stopped: held still."),
    ("LT_HOW_NEW", 3, "Before its first sample: it just appeared."),
    ("LT_FLAG_AIRBORNE", 1, "In the air."),
    ("LT_FLAG_DEAD", 2, "Dead, waiting to respawn."),
    ("LT_FLAG_ADS", 4, "Aiming down sights (near tier only)."),
    ("LT_FLAG_CROUCHED", 8, "Crouched."),
    ("LT_FLAG_RELOADING", 16, "Reloading (near tier only)."),
    ("LT_EVENT_HIT", 1, "Our shot hit `a` for `amount`."),
    ("LT_EVENT_HURT", 2, "`a` hit us for `amount`, from `dir`."),
    ("LT_EVENT_KILL", 3, "`a` killed `b`."),
    ("LT_EVENT_HEAD", 1, "A head shot."),
    ("LT_EVENT_KILLED", 2, "The hit killed."),
    ("LT_FX_SHOT", 1, "A shooter drawn firing: a muzzle flash and a shot's sound at `pos`."),
    ("LT_FX_IMPACT", 2, "A tracer hit the ground or cover at `pos`."),
    ("LT_TRACER_OWN", 0, "Our shot."),
    ("LT_TRACER_SEEN", 1, "A near player's shot, drawn exactly."),
    ("LT_TRACER_AMBIENT", 2, "A distant fight's shot."),
    ("LT_SURFACE_TERRAIN", 255, "The ground."),
    ("LT_COVER_WALL", lattice_game::world::Kind::Wall as i64, "A concrete wall."),
    ("LT_COVER_CRATE", lattice_game::world::Kind::Crate as i64, "A low crate."),
    ("LT_COVER_CONTAINER", lattice_game::world::Kind::Container as i64, "A shipping container."),
    ("LT_COVER_SANDBAGS", lattice_game::world::Kind::Sandbags as i64, "A low sandbag wall."),
    ("LT_COVER_POST", lattice_game::world::Kind::Post as i64, "A guard post by a base's gate."),
    ("LT_COVER_COMMAND", lattice_game::world::Kind::Command as i64, "A base's command building."),
    ("LT_COVER_BUNKER", lattice_game::world::Kind::Bunker as i64, "A bunker."),
    ("LT_COVER_BLOCK", lattice_game::world::Kind::Block as i64, "A blockout volume: a building, wall, tower or deck (Kiln's sites until they have art)."),
    ("LT_COVER_ROCK", lattice_game::world::Kind::Rock as i64, "Natural rock: a boulder, a hoodoo, a crag, an arch."),
    ("LT_COVER_CRYSTAL", lattice_game::world::Kind::Crystal as i64, "Glass and crystal: fulgurite spikes, a Geode's crystals."),
    ("LT_COVER_BONE", lattice_game::world::Kind::Bone as i64, "The Dry Sea's bones."),
    ("LT_COVER_SHIELD", lattice_game::world::Kind::Shield as i64, "A faction's shield: drawn in its colors, see-through."),
    ("LT_COVER_CORAL", lattice_game::world::Kind::Coral as i64, "Lattice coral: a living bridge (K4), there only at night."),
    ("LT_WHEN_ALWAYS", lattice_game::world::ALWAYS as i64, "There always."),
    ("LT_WHEN_BRIDGE", lattice_game::world::BRIDGE as i64, "A living bridge's: from nightfall to a minute into dawn."),
    ("LT_WHEN_FALLS_A", lattice_game::world::FALLS_A as i64, "A sandfall of the first set: pouring in its turn."),
    ("LT_WHEN_FALLS_B", lattice_game::world::FALLS_B as i64, "A sandfall of the second set."),
    ("LT_WHEN_VENT", lattice_game::world::VENT as i64, "A vent lift: there always, running on its cycle."),
    ("LT_PHASE_DAY", lattice_game::clock::Phase::Day as i64, "Day: 70 minutes."),
    ("LT_PHASE_DUSK", lattice_game::clock::Phase::Dusk as i64, "Dusk: 10 minutes; the living bridges grow."),
    ("LT_PHASE_NIGHT", lattice_game::clock::Phase::Night as i64, "Night: 30 minutes; the bridges stand."),
    ("LT_PHASE_DAWN", lattice_game::clock::Phase::Dawn as i64, "Dawn: 10 minutes; the bridges wither after a minute's warning."),
    ("LT_SOLID", lattice_game::world::SOLID as i64, "`LtBox::pass` of everything but shields: it blocks every faction."),
    ("LT_NO_SITE", lattice_game::world::NO_SITE as i64, "`LtBox::site` of a box that belongs to no site."),
    ("LT_NO_ONE", lattice_game::territory::NONE as i64, "No faction: who holds a site or a point no one holds."),
    ("LT_FORCE_LIFT", lattice_game::world::Force::Lift as i64, "An updraft: a lift or a vent lift."),
    ("LT_FORCE_DROP", lattice_game::world::Force::Drop as i64, "A drop shaft: a fall slowed."),
    ("LT_FORCE_SPRING", lattice_game::world::Force::Spring as i64, "A lattice spring: a throw."),
    ("LT_WORLD_KILN", lattice_game::world::KILN as i64, "Kiln's world id (any other is a seed for Harrow)."),
    ("LT_WORLD_KIT", lattice_game::kit::KIT as i64, "The collision kit's test level (K1)."),
    ("LT_CONTINENT_HARROW", lattice_game::world::Continent::Harrow as i64, "Harrow, the procedural hills."),
    ("LT_CONTINENT_KILN", lattice_game::world::Continent::Kiln as i64, "Kiln, the desert continent."),
    ("LT_GROUND_NONE", 255, "The world doesn't say what the ground is (Harrow)."),
    ("LT_GROUND_SAND", lattice_game::world::Ground::Sand as i64, "Sand."),
    ("LT_GROUND_GLASS", lattice_game::world::Ground::Glass as i64, "The Glasslands' fused glass."),
    ("LT_GROUND_SALT", lattice_game::world::Ground::Salt as i64, "The Dry Sea's salt."),
    ("LT_GROUND_OCHRE", lattice_game::world::Ground::Ochre as i64, "Banded red and ochre rock: mesas and cliffs."),
    ("LT_GROUND_BASALT", lattice_game::world::Ground::Basalt as i64, "Dark basalt: the rift floors and the Hollow's."),
    ("LT_GROUND_PALE", lattice_game::world::Ground::Pale as i64, "Pale stone: the Crown and the Spire layer."),
    ("LT_GROUND_ROAD", lattice_game::world::Ground::Road as i64, "A lane's road."),
    ("LT_GROUND_PAD", lattice_game::world::Ground::Pad as i64, "A site's ground."),
    ("LT_GROUND_SCREE", lattice_game::world::Ground::Scree as i64, "Scree and gravel on steep slopes."),
    ("LT_SITE_BASE", lattice_game::world::SiteKind::Base as i64, "Harrow's walled base."),
    ("LT_SITE_OUTPOST", lattice_game::world::SiteKind::Outpost as i64, "Harrow's open outpost."),
    ("LT_SITE_LANDING", lattice_game::world::SiteKind::Landing as i64, "A faction's Landing (can't be taken)."),
    ("LT_SITE_HOLLOW", lattice_game::world::SiteKind::Hollow as i64, "Kiln's central crater."),
    ("LT_SITE_SPAN", lattice_game::world::SiteKind::Span as i64, "A bridge fortress across a rift."),
    ("LT_SITE_CLIFFHOLD", lattice_game::world::SiteKind::Cliffhold as i64, "A fortress in a rift wall."),
    ("LT_SITE_GEODE", lattice_game::world::SiteKind::Geode as i64, "A crystal dome."),
    ("LT_SITE_MESA_CITADEL", lattice_game::world::SiteKind::MesaCitadel as i64, "A walled town on a mesa."),
    ("LT_SITE_SLOT_HOLD", lattice_game::world::SiteKind::SlotHold as i64, "A base across a knot of slot canyons."),
    ("LT_SITE_RIFT_LEDGE", lattice_game::world::SiteKind::RiftLedge as i64, "A base on a ledge halfway down a rift wall."),
    ("LT_SITE_CENOTE", lattice_game::world::SiteKind::Cenote as i64, "A base round a cenote."),
    ("LT_SITE_REEF_WARREN", lattice_game::world::SiteKind::ReefWarren as i64, "A base in a petrified reef."),
    ("LT_SITE_OSSUARY", lattice_game::world::SiteKind::Ossuary as i64, "A base in a colossal skeleton."),
    ("LT_SITE_GLASS_VAULT", lattice_game::world::SiteKind::GlassVault as i64, "A base round a buried vault."),
    ("LT_SITE_ARCH", lattice_game::world::SiteKind::Arch as i64, "An outpost under a natural arch."),
    ("LT_SITE_VENT", lattice_game::world::SiteKind::Vent as i64, "An outpost at a vent."),
    ("LT_SITE_TUNNEL_MOUTH", lattice_game::world::SiteKind::TunnelMouth as i64, "An outpost at a tunnel's mouth."),
    ("LT_SITE_SPIRE_PERCH", lattice_game::world::SiteKind::SpirePerch as i64, "An outpost on a spire."),
    ("LT_SITE_WELL_SHRINE", lattice_game::world::SiteKind::WellShrine as i64, "An outpost at a small well."),
    ("LT_SITE_WRECK", lattice_game::world::SiteKind::Wreck as i64, "An outpost in a crashed lattice ship."),
    ("LT_NOBODY", 0xFFFF, "No shooter (a distant fight's shot from no one drawn)."),
    (
        "LT_CONFIG_NET_THREAD",
        CONFIG_NET_THREAD as i64,
        "Run the session on the plugin's net thread (U3): it receives, runs the input clock with the latest intent and sends about a thousand times a second, so the connection and the inputs keep going while the caller's frames stall. lt_poll and lt_flush then do nothing, and lt_inputs only sets the intent.",
    ),
];

pub const STATE_CONNECTING: i32 = 0;
pub const STATE_CONNECTED: i32 = 1;
pub const STATE_DENIED: i32 = 2;
pub const STATE_TIMED_OUT: i32 = 3;
pub const STATE_DISCONNECTED: i32 = 4;
pub const STATE_ERROR: i32 = -1;
pub const FLAG_AIRBORNE: u8 = 1;
pub const FLAG_DEAD: u8 = 2;
pub const FLAG_ADS: u8 = 4;
pub const FLAG_CROUCHED: u8 = 8;
pub const FLAG_RELOADING: u8 = 16;
pub const EVENT_HIT: u8 = 1;
pub const EVENT_HURT: u8 = 2;
pub const EVENT_KILL: u8 = 3;
pub const EVENT_HEAD: u8 = 1;
pub const EVENT_KILLED: u8 = 2;
pub const FX_SHOT: u8 = 1;
pub const FX_IMPACT: u8 = 2;
pub const SURFACE_TERRAIN: u8 = 255;
pub const NOBODY: u16 = 0xFFFF;
pub const CONFIG_NET_THREAD: u32 = 1;

impl From<[f32; 3]> for LtVec3 {
    fn from(v: [f32; 3]) -> Self {
        Self { x: v[0], y: v[1], z: v[2] }
    }
}

impl From<LtVec3> for [f32; 3] {
    fn from(v: LtVec3) -> Self {
        [v.x, v.y, v.z]
    }
}

/// A 64-bit FNV-1a of every exported struct's layout and every function's
/// signature: the plugin and its generated bindings must agree on it.
pub fn layout_hash(fns: &[crate::api::FnInfo]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    let mut eat = |s: &str| {
        for b in s.bytes().chain([0xFF]) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    for s in STRUCTS {
        eat(s.name);
        eat(&s.size.to_string());
        for f in s.fields {
            eat(f.name);
            eat(&normalize(f.ty));
            eat(&f.offset.to_string());
        }
    }
    for f in fns {
        eat(f.name);
        for (a, t) in f.args {
            eat(a);
            eat(&normalize(t));
        }
        eat(&normalize(f.ret));
    }
    h
}

/// A type as `stringify!` wrote it, without spaces (`* mut u8` -> `*mut u8`
/// can come out either way).
pub fn normalize(ty: &str) -> String {
    let mut out = String::with_capacity(ty.len());
    let mut prev_word = false;
    for part in ty.split_whitespace() {
        let word = part.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        if prev_word && word {
            out.push(' ');
        }
        out.push_str(part);
        prev_word = part.ends_with(|c: char| c.is_alphanumeric() || c == '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_what_csharp_will_be_told() {
        // Sizes C#'s explicit layouts depend on: any change here is a change
        // to the bindings (and the layout hash).
        let size = |n: &str| STRUCTS.iter().find(|s| s.name == n).unwrap().size;
        assert_eq!(size("LtVec3"), 12);
        assert_eq!(size("LtIntent"), 20);
        assert_eq!(size("LtEntity"), 40);
        assert_eq!(size("LtEvent"), 8);
        assert_eq!(size("LtTracer"), 32);
        // Every field lies inside its struct, after the one before it.
        for s in STRUCTS {
            let mut end = 0;
            for f in s.fields {
                assert!(f.offset >= end, "{}.{} overlaps", s.name, f.name);
                end = f.offset + 1;
            }
            assert!(end <= s.size, "{}", s.name);
        }
    }

    #[test]
    fn types_normalize() {
        assert_eq!(normalize("* mut LtSession"), "*mut LtSession");
        assert_eq!(normalize("*const u8"), "*const u8");
        assert_eq!(normalize("[u64 ; 12]"), "[u64;12]");
    }
}
