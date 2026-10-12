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
    }

    /// Another player, as drawn this frame.
    pub struct LtEntity {
        pub id: u16,
        pub faction: u8,
        /// `LT_TIER_*` of its newest sample.
        pub tier: u8,
        /// `LT_HOW_*`: interpolated, extrapolated, held, new.
        pub how: u8,
        /// `LT_FLAG_*`: airborne, dead, aiming down sights.
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
    }

    /// The world's size and contents.
    pub struct LtWorldInfo {
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
    }

    /// A cover box: blocks movement and shots.
    pub struct LtBox {
        /// z: its bottom.
        pub min: LtVec3,
        /// z: its top.
        pub max: LtVec3,
        /// `LT_COVER_*`: what it's drawn as.
        pub kind: u8,
        /// Which way its front faces: 0 east, 1 north, 2 west, 3 south.
        pub facing: u8,
    }

    /// A base or an outpost: a square of flattened ground.
    pub struct LtSite {
        pub x: f32,
        pub y: f32,
        /// Half the side of its flat square.
        pub half: f32,
        /// The height it's flattened to.
        pub level: f32,
        /// A walled base (else an outpost).
        pub base: u8,
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
