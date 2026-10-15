//! The M1 authoritative server: movement only, sans-IO like the transport.
//!
//! `tick()` runs the phase pipeline from CLAUDE.md and times every phase:
//!
//! | phase     | work                                                            |
//! |-----------|-----------------------------------------------------------------|
//! | ingress   | each transport shard decodes its datagrams, acks, handshakes, timeouts, and queues its clients' inputs (parallel) |
//! | events    | spawn/despawn (serial) |
//! | movement  | one input seq per entity per tick, real or stand-in, and gather the shots fired (parallel) |
//! | grid      | rebuild the shared spatial grid, plus the mid/far due-set views |
//! | separate  | push overlapping players apart (parallel), then apply damage     |
//! | history   | store positions for lag compensation                            |
//! | shots     | shots become projectiles and every projectile flies to now (parallel); hits apply in projectile order (serial) |
//! | serialize | encode each entity once per tier: near blob for all, mid/far blob for due ones (parallel) |
//! | assembly  | per client: pick near/mid/far per `interest.rs`, fit the byte budget, memcpy blobs into messages (parallel by shard) |
//! | transport | each shard queues its clients' snapshots and builds packets (parallel) |
//!
//! Egress (the socket writes) happens in the binary. Datagrams move in
//! per-shard buckets both ways: route inbound ones with `router()`.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_net::wire::Writer;
use lattice_net::{Channel, ClientId, Config, Router, Server, ServerEvent, ServerIdentity};
use rayon::prelude::*;

use crate::activity::{self, Activity};
use crate::grid::{Grid, Knn};
use lattice_game::world::World;
use crate::interest::{self, due, near_base, InterestConfig, NearCandidate, NearState, SelectScratch, Tier};
use crate::ladder::{self, ClientLadder, Ladder, LadderConfig, PaceMeter, Rung, MAX_LEVEL};
use crate::movement::{self, step, Input, MoveState, BUTTON_ADS, BUTTON_RELOAD, GRACE_TICKS, HEIGHT, RADIUS, TICK_HZ, WORLD_SIZE};
use lattice_game::{ability, class};
use crate::shots::{self, ClockRate, Cut, Dips, Fire, FlyStats, History, Judgment, Outcome, Projectile, RenderFloor, Sky, SpreadKey};
use lattice_game::weapon::{self, shot_time, Ammo, Bloom, Shot};
use lattice_game::msg::MID_LAG_UNITS;
use lattice_game::events::{self, Event};
use lattice_game::faction::{faction, FACTIONS, MAX_HEALTH, RESPAWN_STEPS};
use lattice_game::clock::{self, Live};

/// Debugging aid (`LATTICE_FALL_TRACE=1`): every fall that hurts, where it
/// landed and from how high, to stderr.
static FALL_TRACE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("LATTICE_FALL_TRACE").is_some());
/// Debugging aid (`LATTICE_KILL_TRACE=1`): every kill by a player, where the
/// two stood and the spawn room each was in, if any, on stderr.
static KILL_TRACE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("LATTICE_KILL_TRACE").is_some());
use lattice_game::territory::{self, Territory};
use crate::delta::{self, NearEntry, NearQ, MAX_BASE_AGE, NEAR_HISTORY};
use crate::msg::{self, Blob, PacketFill, RenderTime, SnapshotHeader, Welcome, FAR_BLOB, SNAPSHOT_LEN, WAIT_STAND_IN};
use crate::rng::Rng;
use crate::stats::Histogram;

pub const PHASES: [&str; 10] = ["ingress", "events", "movement", "grid", "separate", "history", "shots", "serialize", "assembly", "transport"];
pub type Datagram = (SocketAddr, Vec<u8>);
/// Sends one shard's datagrams (`SimServer::tick_sending`): given the shard
/// and its bucket, which it should leave empty. Called from that shard's
/// task, so it must be thread-safe.
pub type Sender<'a> = &'a (dyn Fn(usize, &mut Vec<Datagram>) + Sync);
/// An inbound datagram with its arrival time, as the receive thread saw it.
pub type InDatagram = (SocketAddr, Instant, Vec<u8>);
pub type PhaseTimes = [Duration; PHASES.len()];
/// For a phase split by shard: its longest single task (the critical path)
/// and the total of all its tasks. Zero for phases not split by shard.
pub type Span = (Duration, Duration);
pub type PhaseSpans = [Span; PHASES.len()];

fn span(d: Duration) -> Span {
    (d, d)
}

fn join_spans(a: Span, b: Span) -> Span {
    (a.0.max(b.0), a.1 + b.1)
}

const NO_SPAN: Span = (Duration::ZERO, Duration::ZERO);

/// Steps of a stand-in's seq a late shot may still fire for.
const LATE_SHOT_STEPS: u32 = 8;

/// Ticks of send delays kept for the backtrack bound: a shot's claim
/// reaches back a render delay plus a round trip, well under 16 ticks.
const SEND_DELAY_TICKS: usize = 16;
/// Consumed steps an input queue remembers (for late shots' origins).
const STEP_RING: usize = 16;
/// A client can't queue more inputs than this (~0.5 s); beyond it the oldest
/// are discarded unapplied rather than letting latency grow.
const MAX_QUEUED_INPUTS: usize = 16;
const GRID_CELL: f32 = 32.0;
/// A projectile that hit a player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitRecord {
    pub shooter: u16,
    pub target: u16,
    pub head: bool,
    pub damage: u8,
    pub killed: bool,
    /// How far back the target was taken, in steps.
    pub rewind: f64,
    /// The shooter couldn't see the target any more when it landed.
    pub after_cover: bool,
    pub at: [f32; 3],
    pub tick: u32,
}

/// Players closer than this (two radii) are pushed apart: by `SEP_RATE` of the
/// overlap per tick, split between them, at most `MAX_PUSH` a tick (3 m/s).
const SEP_DIST: f32 = 2.0 * RADIUS;
const SEP_RATE: f32 = 0.5;
const MAX_PUSH: f32 = 0.1;
/// Cells of the mid- and far-tier due-set grids, sized to their query radii.
/// Mid is a k-nearest ring walk: fine enough to stop early in a crowd, coarse
/// enough that a sparse 500 m query stays under ~300 cells.
const MID_GRID_CELL: f32 = 64.0;
const FAR_GRID_CELL: f32 = 512.0;
const NO_SQUAD: u32 = u32::MAX;
/// Ticks of sent near messages a client's acks can refer to. An ack for an
/// older message is ignored; the baseline just stays older.
const SENT_RING: usize = 16;

/// A message for one client, and its delivery tag if it wants acks.
type Snap = (ClientId, Vec<u8>, Option<u32>);

/// What one watched client got on one tick, for the debug map.
#[derive(Debug, Clone, Default)]
pub struct WatchedClient {
    pub client: ClientId,
    pub entity: u16,
    pub pos: [f32; 2],
    /// Near, mid and far radii for this client (its bandwidth level applied).
    pub radii: [f32; 3],
    pub client_level: u8,
    /// Near set: (entity, ticks since last sent before this tick, sent now,
    /// sent as a delta).
    pub near: Vec<(u16, u32, bool, bool)>,
    pub mid: Vec<u16>,
    pub far: Vec<u16>,
    /// Due far entities that didn't fit the budget (carried to next tick).
    pub far_skipped: Vec<u16>,
    pub far_starved: u32,
    pub bytes: usize,
    pub near_bytes: usize,
}

/// The debug map's view of one tick.
#[derive(Debug, Clone, Default)]
pub struct DebugFrame {
    pub tick: u32,
    pub level: u8,
    pub tick_hz: u32,
    pub dilation: f32,
    pub pace: f32,
    pub clients: usize,
    /// Every entity: (entity, position).
    pub entities: Vec<(u16, [f32; 2])>,
    pub watched: Option<WatchedClient>,
}
/// Input waits above 1 s land in the histogram's last bucket (0.1 ms units).
const INPUT_WAIT_CAP: u32 = 10_000;
/// Rewinds above 1 s land in the histogram's last bucket (ms).
const REWIND_CAP_MS: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpawnMode {
    /// Everyone spread over the whole continent.
    Uniform,
    /// The first 2,400 players split across 3 hotspots (~800 each), the rest uniform.
    Hotspots,
    /// The first 3,000 players in a 200 m disk, the rest uniform.
    Blob,
    /// Player k stands still at (1000 + k * spacing, 4096): known distances,
    /// for checking tiers.
    Line(f32),
    /// Everyone in one disk of this radius at the center: the density limit
    /// (a 25 m disk is everyone on one capture point).
    Disk(f32),
    /// Player k starts at an end of one of the world's lanes (lane k / 2,
    /// from its far end if k is odd), for bots that walk and time them
    /// (`lattice-bots --run-lanes`).
    Lanes,
    /// Where the lattice says (U5, ahead of U4's deploy screen): at its
    /// faction's Landing, and after a death at the site its faction holds
    /// that's nearest where it died (`territory::spawn_points`), for bots
    /// that take sites (`lattice-bots --capture`).
    Sites,
}

impl std::str::FromStr for SpawnMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "uniform" => Ok(Self::Uniform),
            "hotspots" => Ok(Self::Hotspots),
            "blob" => Ok(Self::Blob),
            "lanes" => Ok(Self::Lanes),
            "sites" => Ok(Self::Sites),
            _ => {
                let arg = |prefix: &str| s.strip_prefix(prefix).and_then(|v| v.parse::<f32>().ok()).filter(|v| *v > 0.0);
                match (arg("line:"), arg("disk:")) {
                    (Some(spacing), _) => Ok(Self::Line(spacing)),
                    (_, Some(radius)) => Ok(Self::Disk(radius)),
                    _ => Err(format!("unknown spawn mode {s:?} (uniform|hotspots|blob|lanes|sites|line:<meters>|disk:<meters>)")),
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub net: Config,
    pub max_clients: usize,
    pub spawn: SpawnMode,
    pub interest: InterestConfig,
    /// Transport shards. More than the thread count lets rayon balance them.
    pub shards: usize,
    /// Receiving sockets (`SO_REUSEPORT`), each owning an equal run of the
    /// shards (see `lattice_net::Router`). `shards` must divide evenly.
    pub socket_groups: usize,
    /// Allocate `max_clients` connections at startup so accepts reuse them.
    pub preallocate: bool,
    pub ladder: LadderConfig,
    pub seed: u64,
    /// The world (terrain and cover) everyone plays in; sent in the Welcome so
    /// clients build the same one.
    /// The world's id (`World::shared`): `world::KILN`, or a seed for Harrow.
    pub world: u64,
    /// Push overlapping players apart (soft separation). Off only for comparisons.
    pub separation: bool,
    /// Test aid until there are weapons: kill this many random living
    /// players a second (they respawn after `RESPAWN_STEPS`).
    pub deaths_per_sec: f32,
    /// Measurement aid: hits land (and are confirmed) but deal no damage, so
    /// hit rates measure aim and lag compensation, not who died first. Nobody
    /// dies to have their rounds refilled, so it implies `unlimited_rounds`.
    pub immortal: bool,
    /// Load-test aid: the rifle never runs dry (`msg::RULE_UNLIMITED_ROUNDS`,
    /// announced in the Welcome so clients predict it): held triggers fire at
    /// the rifle's rate for as long as a run lasts, as before rounds existed.
    pub unlimited_rounds: bool,
    /// Test aid: territory's meters move this many times faster (1: the
    /// GDD's minutes; `territory::Territory::with_speed`).
    pub capture_speed: u32,
    /// Players deploy (U4, GDD §17; `msg::RULE_DEPLOY`, a world with
    /// territory only): they join with no body and ask to deploy at a site
    /// their faction may spawn at, appearing in its spawn room after lattice
    /// transit; the dead stay dead (their bodies a while) until they deploy
    /// again. Off: the spawn mode places everyone and the dead respawn.
    pub deploy: bool,
    /// Where in Kiln's day the server starts (`clock`): its first game step
    /// is this many minutes into a cycle (0: a day's start). Clients need no
    /// telling: the clock is the step's.
    pub clock_minutes: f64,
    /// Shots leave within their cone of fire (`weapon::Bloom`, `spread`).
    /// Off only for tests that measure lag compensation's geometry.
    pub cone_of_fire: bool,
    /// The secret that picks where in its cone each shot goes
    /// (`shots::SpreadKey`). None draws one from the OS at startup; tests
    /// fix it so their runs repeat.
    pub spread_secret: Option<[u8; 32]>,
    /// Hold each shooter's claimed render steps to its render clock
    /// (`shots::RenderFloor`). Off only to measure what it stops.
    pub render_floor: bool,
    /// Ticks take real time (the `lattice-server` binary): the transport
    /// stamps each shard's sends when it flushes them, so RTTs and the hold
    /// times acks report leave out the tick's processing (stamped at the
    /// tick's start, a 10k tick read 15-20 ms too much RTT), and the backtrack
    /// bound adds that send delay back. Off for in-process swarms, whose
    /// ticks are instants of simulated time.
    pub real_time: bool,
    /// Server id and token key, shared with whatever mints the clients'
    /// tokens (the bots, standing in for a login service).
    pub identity: ServerIdentity,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            // A snapshot spans ~3 packets; room for 6 covers the default budget.
            net: Config { max_packets_per_flush: 6, ..Config::default() },
            max_clients: 10_000,
            spawn: SpawnMode::Uniform,
            interest: InterestConfig::default(),
            shards: 64,
            socket_groups: 1,
            preallocate: false,
            ladder: LadderConfig::default(),
            seed: 1,
            world: 1,
            separation: true,
            deaths_per_sec: 0.0,
            immortal: false,
            unlimited_rounds: false,
            capture_speed: 1,
            deploy: false,
            clock_minutes: 0.0,
            cone_of_fire: true,
            spread_secret: None,
            render_floor: true,
            real_time: false,
            identity: ServerIdentity { server_id: 1, token_key: lattice_net::token::DEV_TOKEN_KEY },
        }
    }
}

/// Cumulative counters.
#[derive(Debug, Clone, Default)]
pub struct Counters {
    pub ticks: u64,
    /// Sites that changed hands (U5).
    pub site_flips: u64,
    pub spawns: u64,
    pub despawns: u64,
    pub deaths: u64,
    /// Of them, from falls (`movement::fall_damage`).
    pub fall_deaths: u64,
    pub respawns: u64,
    /// Deploys (`SimConfig::deploy`): asked for and refused, made, and
    /// waits for the per-tick budget (`DEPLOYS_PER_TICK`).
    pub deploys_asked: u64,
    pub deploys_refused: u64,
    pub deploys: u64,
    pub deploys_waited: u64,
    /// Recaps sent, and XP rewarded.
    pub recaps: u64,
    pub xp: u64,
    pub inputs_applied: u64,
    /// Entity-ticks where the next input hadn't arrived. Each consumes that seq
    /// with a stand-in: the last input for `GRACE_TICKS`, then a frozen one.
    pub repeated: u64,
    pub frozen: u64,
    /// Inputs whose render time was ahead of the server's step (bogus).
    pub render_ahead: u64,
    /// Shots fired (late: their input was replaced by a stand-in, the shot
    /// still fired), refused (too fast, from the dead, too late), and the
    /// rewinds a cap clipped.
    pub shots: u64,
    pub shots_late: u64,
    pub shots_refused: u64,
    pub rewinds_capped: u64,
    /// How projectiles ended: in a player (head or body), the ground, cover,
    /// out of range. Kills by shots.
    pub hits_head: u64,
    pub hits_body: u64,
    pub hits_ground: u64,
    pub hits_cover: u64,
    pub expired: u64,
    pub kills: u64,
    /// Hits on a target its shooter couldn't see any more (cover or terrain
    /// between them at the present): lag compensation's cost to the target.
    pub hits_after_cover: u64,
    /// Hits on a target that had died (or respawned) since the shooter saw
    /// it: no damage.
    pub hits_too_late: u64,
    /// Shots whose claimed render time was older than the shooter could
    /// plausibly have seen (a "backtrack" cheat): trimmed to what it could.
    pub rewinds_trimmed: u64,
    /// Of those, the ones whose larger excess was the mid/far claim.
    pub rewinds_trimmed_mid: u64,
    /// Shots whose claimed view didn't fit the shooter's render clock (older
    /// than its inputs said it drew, or newer than its own input): held to
    /// it (`shots::RenderFloor`).
    pub renders_held: u64,
    /// Clients judged, as they left, by whether their shots dipped under the
    /// render floor without its allowances more than their inputs did
    /// (`RenderFloor::judge`, at least `DIP_MIN_SHOTS` shots), and of those,
    /// the ones flagged in any window or over the session: spending the
    /// allowances on stale shots.
    pub dip_clients: u64,
    pub dip_flagged: u64,
    /// Windows of `JUDGE_SHOTS` shots judged during sessions, and flagged.
    pub dip_windows: u64,
    pub dip_windows_flagged: u64,
    /// Projectile segments flown and player candidates tested.
    pub segments: u64,
    pub candidates: u64,
    /// Combat events sent (reliable), and tracer bytes (unreliable).
    pub events: u64,
    pub tracer_bytes: u64,
    /// Distant fights (`activity.rs`): cell entries made (per window), and
    /// sent to clients, their bytes, and entries cut by the byte budget.
    pub activity_cells: u64,
    pub activity_sent: u64,
    pub activity_bytes: u64,
    pub activity_cut: u64,
    /// Real inputs that arrived for a seq a stand-in had already consumed.
    pub late_inputs: u64,
    /// Inputs dropped unapplied because the client queued too many.
    pub discarded_inputs: u64,
    pub bad_messages: u64,
    /// Client-ticks served.
    pub snapshots: u64,
    /// Entities sent per tier (near, mid, far).
    pub tier_sent: [u64; 3],
    /// Bytes of snapshot messages sent (own state + entities).
    pub snapshot_bytes: u64,
    /// Client-ticks whose near candidates exceeded `near_candidates`.
    pub near_capped: u64,
    /// Mid entities cut because near + mid overran the byte budget.
    pub mid_truncated: u64,
    /// Due far entities that didn't fit the budget (carried to the next tick).
    pub far_skipped: u64,
    /// Carried far entities that didn't fit again: the degrade signal.
    pub far_starved: u64,
    /// Near-tier bytes (the whole near messages), and entities sent as a
    /// delta against an acked baseline vs. in full.
    pub near_bytes: u64,
    pub near_deltas: u64,
    pub near_full: u64,
    /// Candidates whose distance the near and mid searches computed (the
    /// k-nearest work), summed over client-ticks.
    pub near_scanned: u64,
    pub mid_scanned: u64,
    /// Ticks spent at each degradation level.
    pub level_ticks: [u64; MAX_LEVEL as usize + 1],
    /// Client-ticks with the client's own bandwidth level above 0.
    pub degraded_clients: u64,
}

#[derive(Clone, Copy)]
struct Body {
    /// The slot holds a connected player (dead or not).
    alive: bool,
    state: MoveState,
    yaw: u16,
    pitch: i16,
    /// Aiming down sights (its last input), for others to see.
    ads: bool,
    /// Reloading (its rifle), for others to see.
    reloading: bool,
    /// Its class's ability is on (`ability::Ability::on`): a Bulwark, a Mend
    /// field, the cloak, jets firing. Others see it; damage, healing and
    /// interest follow it.
    ability: bool,
    /// Ticks this player was pushed apart from a crowd (wrapping), for the snapshot.
    pushes: u8,
    squad: u32,
    /// Tick this entity (slot) spawned: an older baseline belongs to a previous occupant.
    spawned: u32,
    /// 0 is dead: inputs move nothing until the respawn.
    health: u8,
    /// While dead, the step it respawns at.
    respawn_at: Option<u32>,
    /// Deaths and respawns (wrapping), for the snapshot.
    life: u8,
    /// Where it was sent to play (respawns come back there).
    anchor: [f32; 2],
    radius: f32,
    /// Test aid: hits land but deal no damage.
    invulnerable: bool,
    /// Damage from falls (`movement::fall_damage`) its movement took this
    /// tick, applied with the rest (`apply_damage`).
    fallen: u16,
    /// Where it last left the ground (for `LATTICE_FALL_TRACE`).
    left_ground: [f32; 3],
    /// Players it hit or was hit by recently: near-tier for each other until
    /// the tick given ("interaction"). `NO_CONTACT` is empty.
    contacts: [(u16, u32); CONTACTS],
    /// No body (`SimConfig::deploy`): not yet deployed, or gone from its
    /// corpse. Dead too; in no grid, so no one sees it or counts it.
    away: bool,
    /// Deploying: at which site, the step it appears there, and as what (its kit).
    deploy: Option<(u16, u32, u8)>,
    /// Its class (`class`): its weapon, what others see.
    kit: u8,
    /// When it last died, and the site whose region it fell in (lattice
    /// transit counts lanes from there; none: never deployed).
    died_at: u32,
    died_in: Option<u16>,
}

impl Default for Body {
    fn default() -> Self {
        Self {
            alive: false,
            state: MoveState::default(),
            yaw: 0,
            pitch: 0,
            ads: false,
            reloading: false,
            ability: false,
            pushes: 0,
            squad: NO_SQUAD,
            spawned: 0,
            health: MAX_HEALTH,
            respawn_at: None,
            life: 0,
            anchor: [0.0; 2],
            radius: 0.0,
            invulnerable: false,
            fallen: 0,
            left_ground: [0.0; 3],
            contacts: [(NO_CONTACT, 0); CONTACTS],
            away: false,
            deploy: None,
            kit: 0,
            died_at: 0,
            died_in: None,
        }
    }
}

/// Damage a player took (`SimServer::damage_log`): from whom (`events::NOBODY`
/// for a fall or a test's), how much, a headshot, when.
#[derive(Debug, Clone, Copy)]
struct Taken {
    from: u16,
    amount: u8,
    head: bool,
    step: u32,
}

/// Deploys made in one tick at most: a mass redeploy waits a tick or two
/// instead of spiking it (GDD §3.4: spawn cost killed PlanetSide 2's fights).
const DEPLOYS_PER_TICK: u32 = 40;
/// A corpse no one revived goes after this long (30 s, GDD §4.4's revive
/// window): it has no body then, and isn't drawn.
const CORPSE_STEPS: u32 = 30 * 30;
/// The regions' lookup cells (`SimServer::regions`): 64 m.
const REGION_CELL: f32 = 64.0;

impl Body {
    fn dead(&self) -> bool {
        self.health == 0
    }

    /// Keeps `other` near-tier for this player until `until` (replacing the
    /// one that expires first, if all are taken).
    fn contact(&mut self, other: u16, until: u32) {
        let i = self.contacts.iter().position(|c| c.0 == other).unwrap_or_else(|| {
            (0..CONTACTS).min_by_key(|&i| if self.contacts[i].0 == NO_CONTACT { 0 } else { self.contacts[i].1 }).unwrap()
        });
        self.contacts[i] = (other, until);
    }
}

/// Recent combat contacts kept per player, and for how long (5 s).
const CONTACTS: usize = 4;
const CONTACT_TICKS: u32 = 150;
const NO_CONTACT: u16 = u16::MAX;

/// A connected client, kept in its transport shard's list with its interest state.
struct ClientSlot {
    client: ClientId,
    entity: u16,
    near: NearState,
    /// Far entities that were due but didn't fit the budget: sent first next tick.
    far_carry: Vec<u16>,
    /// Per-client bandwidth ladder.
    ladder: ClientLadder,
    /// Which entities each recent near message carried, by tick (its tag).
    sent_ring: Vec<(u32, Vec<u16>)>,
}

/// Per-shard scratch for assembly, reused every tick.
#[derive(Default)]
struct Scratch {
    /// `stamp[e] == epoch` marks entity e as already taken for this client.
    stamp: Vec<u32>,
    epoch: u32,
    /// The k-nearest search's selection state.
    knn: Knn,
    near: Vec<NearCandidate>,
    select: SelectScratch,
    /// This tick's near entities and their acked baselines.
    picked: Vec<(u16, u32)>,
    acked: Vec<u32>,
    ack_pairs: Vec<(u16, u32)>,
    near_entries: Vec<NearEntry>,
    /// Filled when this shard assembled the watched client.
    watched: Option<WatchedClient>,
    mid: Vec<(f32, u16)>,
    far: Vec<(f32, u16)>,
    /// This client's distant-fight entries, with their squared distance.
    activity: Vec<(f32, lattice_game::activity::Entry)>,
    tally: Tally,
}

#[derive(Default, Clone, Copy)]
struct Tally {
    snapshots: u64,
    tier_sent: [u64; 3],
    bytes: u64,
    near_capped: u64,
    mid_truncated: u64,
    far_skipped: u64,
    far_starved: u64,
    degraded: u64,
    near_bytes: u64,
    near_deltas: u64,
    near_full: u64,
    /// Candidates the near and mid searches computed distances for.
    near_scanned: u64,
    mid_scanned: u64,
    tracer_bytes: u64,
    activity_sent: u64,
    activity_bytes: u64,
    activity_cut: u64,
}

/// Per-entity input stream. Every tick consumes exactly one input seq, so each
/// server step matches exactly one client step and replays stay consistent.
/// A queued input: seq, input, render time, shot (with its claim as the
/// render floor held it: near and mid render steps, and how far it moved),
/// first arrival.
type Queued = (u32, Input, Option<RenderTime>, Option<(Shot, Option<Held>)>, Instant);
/// A shot's claimed render steps (near, mid/far; absolute) as the render
/// floor held them, and how far that moved them, in steps.
type Held = ([f64; 2], f64);

/// What a message's inputs are judged by: the server's step (to place
/// render steps), and for the render floor (when it's on) the client's
/// clock rate and link jitter.
#[derive(Debug, Clone, Copy)]
struct Judge {
    step: u32,
    clock: ClockRate,
    floor: bool,
    /// The sender's RTT now, in ms (0 before it's measured).
    rtt: f32,
}

/// One client's time on the server, for the session log
/// (`SimServer::take_session_records`): who it is (the connect token's user
/// id) and what it did.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub user: u64,
    /// When it connected: unix seconds, and here.
    pub joined_unix: u64,
    joined: Option<Instant>,
    /// Seconds connected, as of the record.
    pub secs: f64,
    /// Inputs applied; seqs a stand-in took (its input was late or lost);
    /// inputs that came after a stand-in took their seq.
    pub inputs: u64,
    pub stand_ins: u64,
    pub late: u64,
    /// Shots fired; of those, held to its render clock (`RenderFloor`) or
    /// trimmed (`shots::plausible`); and shots refused (too fast, from the
    /// dead, too late).
    pub shots: u64,
    pub held: u64,
    pub trimmed: u64,
    pub refused: u64,
    /// Its shots that hit a player, its kills, its deaths.
    pub hits: u64,
    pub kills: u64,
    pub deaths: u64,
    /// Its deploys, and the XP it earned (U4).
    pub deploys: u64,
    pub xp: u64,
    /// The transport's RTT: summed over its messages, and the highest, in ms.
    pub rtt_sum_ms: f64,
    pub rtt_samples: u64,
    pub rtt_max_ms: f32,
    /// Windows of its shots judged (`RenderFloor::judge`), and flagged; the
    /// first judgment that flagged it.
    pub windows: u32,
    pub windows_flagged: u32,
    pub first_flag: Option<Judgment>,
}

impl Session {
    /// A copy with `secs` as of `now`.
    fn as_of(&self, now: Instant) -> Session {
        let secs = self.joined.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        Session { secs, ..self.clone() }
    }
}

/// What a session record is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// A judgment of its shots flagged it, the first time this session.
    Flagged,
    /// It left (disconnected or timed out).
    Left,
    /// Still connected when the server stopped (`SimServer::close_sessions`).
    Open,
}

/// A line of the session log.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub event: SessionEvent,
    pub entity: u16,
    pub session: Session,
    /// Flagged: the judgment's window and session dips. Left and Open: the
    /// window not judged yet, and the whole session's.
    pub window: Option<Dips>,
    pub total: Option<Dips>,
}

impl SessionRecord {
    /// One JSON object, no newline.
    pub fn to_json(&self) -> String {
        let s = &self.session;
        let dips = |d: Option<Dips>| match d {
            Some(d) => format!("{{\"shots\":{},\"excess\":{:.3},\"z\":{:.2},\"flagged\":{}}}", d.shots, d.excess, d.z, d.flagged()),
            None => "null".to_string(),
        };
        let event = match self.event {
            SessionEvent::Flagged => "flagged",
            SessionEvent::Left => "left",
            SessionEvent::Open => "open",
        };
        let rtt_mean = if s.rtt_samples > 0 { s.rtt_sum_ms / s.rtt_samples as f64 } else { 0.0 };
        format!(
            "{{\"event\":\"{event}\",\"user\":{},\"entity\":{},\"joined_unix\":{},\"secs\":{:.1},\"inputs\":{},\"stand_ins\":{},\"late\":{},\"shots\":{},\"held\":{},\"trimmed\":{},\"refused\":{},\"hits\":{},\"kills\":{},\"deaths\":{},\"deploys\":{},\"xp\":{},\"rtt_mean_ms\":{:.1},\"rtt_max_ms\":{:.1},\"windows\":{},\"windows_flagged\":{},\"window\":{},\"total\":{}}}",
            s.user, self.entity, s.joined_unix, s.secs, s.inputs, s.stand_ins, s.late, s.shots, s.held, s.trimmed, s.refused, s.hits, s.kills, s.deaths, s.deploys, s.xp,
            rtt_mean, s.rtt_max_ms, s.windows, s.windows_flagged, dips(self.window), dips(self.total)
        )
    }
}

struct InputQueue {
    /// Newest seq consumed, by a real input or a stand-in. 0 = none yet.
    last_seq: u32,
    /// Newest real input applied.
    last: Input,
    /// Consecutive stand-in ticks.
    starved_run: u32,
    /// Bit i set: seq `last_seq - i` was consumed by a stand-in.
    stand_ins: u32,
    /// Real inputs queued when this tick started; steers the client's input clock.
    depth: u8,
    /// Wait of the newest consumed seq, arrival to applied, in 0.1 ms, or
    /// `WAIT_STAND_IN`. Set by `advance`; reported in the snapshot.
    wait: u16,
    /// How far behind its step the newest applied input's render steps were
    /// (near, mid/far), in steps: what lag compensation would rewind a target
    /// in that tier by. Set by `advance` when the input carried render times.
    rewind: Option<(f64, f64)>,
    /// Sorted by seq, all > last_seq, each with its render time, shot and first arrival.
    pending: VecDeque<Queued>,
    /// The last consumed seqs: (seq, the step it moved to, state before,
    /// state after), for shots' origins (late ones included).
    steps: [(u32, u32, MoveState, MoveState); STEP_RING],
    /// The newest shot fired (`weapon::shot_time`), for the rate and duplicates.
    last_shot: Option<u64>,
    /// Shots to fly this tick, and shots refused (too fast, from the dead,
    /// too late), since last taken.
    fires: Vec<Fire>,
    refused: u32,
    /// The shooter's bloom, and whether shots leave within their cone of
    /// fire (`SimConfig::cone_of_fire`).
    bloom: Bloom,
    cone: bool,
    /// The rifle's rounds, as of `last_seq` (and the late shots fired since).
    ammo: Ammo,
    /// The server's rules give unlimited rounds: the rifle is full after every step.
    unlimited: bool,
    /// The server's secret for where in the cone each shot goes, and this
    /// spawn's number under it. None before the first spawn.
    spread: Option<(Arc<SpreadKey>, u64)>,
    /// The render steps this client has claimed, for the render floor.
    claims: RenderFloor,
    /// A message's inputs while it's judged, oldest first.
    batch: Vec<(u32, Input, Option<RenderTime>, Option<Shot>)>,
    /// Inputs received (first arrivals): the first `SETTLE_INPUTS` are the
    /// client's first seconds, while its clocks settle.
    inputs_seen: u32,
    session: Session,
    /// A judgment flagged it for the first time since the last tick.
    flagged_now: bool,
}

/// A client's first two seconds of inputs (at 30 Hz): its RTT isn't
/// measured yet and its render clock is still catching its target, so the
/// backtrack bound allows it more (`shots::plausible`).
const SETTLE_INPUTS: u32 = 60;

impl Default for InputQueue {
    fn default() -> Self {
        Self {
            last_seq: 0,
            last: Input::default(),
            starved_run: 0,
            stand_ins: 0,
            depth: 0,
            wait: 0,
            rewind: None,
            pending: VecDeque::new(),
            steps: [(0, 0, MoveState::default(), MoveState::default()); STEP_RING],
            last_shot: None,
            fires: Vec::new(),
            refused: 0,
            bloom: Bloom::default(),
            cone: true,
            ammo: Ammo::FULL,
            unlimited: false,
            spread: None,
            claims: RenderFloor::default(),
            batch: Vec::new(),
            inputs_seen: 0,
            session: Session::default(),
            flagged_now: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Push {
    Queued,
    /// Redundant copy of something already queued or applied.
    Duplicate,
    /// Its seq was already consumed by a stand-in; dropped.
    Late,
    /// Queue overflow: the oldest input was dropped unapplied.
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Applied,
    Repeated,
    Frozen,
    /// No input ever received: stand still, consume nothing.
    Waiting,
}

impl InputQueue {
    /// Queues the inputs of one message. Counts into `n`: inputs late (a
    /// stand-in took their seq) and discarded (queue full), and bad messages.
    ///
    /// Each input's claimed render step is held to the client's render clock
    /// as it arrives, and each shot's to the inputs around it
    /// (`shots::RenderFloor`), so a shot carries the view it's allowed.
    fn receive(&mut self, e: u16, data: &[u8], arrived: Instant, dead: bool, judge: Judge, n: &mut [u64; 3]) {
        if judge.rtt > 0.0 {
            let s = &mut self.session;
            (s.rtt_sum_ms, s.rtt_samples, s.rtt_max_ms) = (s.rtt_sum_ms + judge.rtt as f64, s.rtt_samples + 1, s.rtt_max_ms.max(judge.rtt));
        }
        let mut batch = std::mem::take(&mut self.batch);
        batch.clear();
        let ok = msg::decode_inputs(data, |seq, input, render, shot| batch.push((seq, input, render, shot)));
        let newest = batch.first().map_or(0, |b| b.0);
        // Oldest first: a shot is held between the input before it and its own.
        for &(seq, input, render, mut shot) in batch.iter().rev() {
            let new = self.is_new(seq);
            self.inputs_seen += new as u32;
            let settling = self.inputs_seen < SETTLE_INPUTS;
            // How far a shot may claim older than the input before it: a
            // client's input clock runs ahead of what it has made in its
            // first seconds, and by a step after a stand-in filled a gap.
            let before = match (settling, self.stand_ins != 0) {
                (true, _) => None,
                (false, true) => Some(1.1),
                (false, false) => Some(0.0),
            };
            let place = |units| judge.step as f64 - msg::render_age(judge.step, units);
            let held = match (judge.floor && new, render, shot) {
                (true, Some(r), _) => {
                    let lag = r.mid_lag as f64 / MID_LAG_UNITS;
                    let near = place(r.near);
                    self.claims.input(seq, [near, near - lag], newest, arrived, judge.clock);
                    let held = shot.map(|s| {
                        let near = place(s.render);
                        // Reckoned with this message's mid lag: as a copy
                        // (newest - seq) steps late, it may have slewed since.
                        let drift = newest.saturating_sub(seq) as f64 * lattice_client_core::clock::SLEW;
                        let h = self.claims.shot(seq, [near, near - lag], before, drift);
                        let m = (near - h[0]).abs().max((near - lag - h[1]).abs());
                        (h, m)
                    });
                    if let Some(j) = self.claims.judge() {
                        self.judged(j);
                    }
                    held
                }
                // No render steps in the message: held to the earlier claims
                // alone; with none at all past its first seconds, a client
                // that never says what it drew doesn't get lag compensation.
                (true, None, Some(s)) => {
                    let near = place(s.render);
                    match self.claims.bare_shot(seq, [near, near], newest, arrived, judge.clock) {
                        Some(h) => Some((h, (near - h[0]).abs().max((near - h[1]).abs()))),
                        None if settling => None,
                        None => {
                            shot = None;
                            self.refused += 1;
                            None
                        }
                    }
                }
                _ => None,
            };
            match self.push(e, seq, input, render, shot.map(|s| (s, held)), arrived, dead) {
                Push::Late => {
                    n[0] += 1;
                    self.session.late += 1;
                }
                Push::Discarded => n[1] += 1,
                Push::Queued | Push::Duplicate => {}
            }
        }
        self.batch = batch;
        n[2] += ok.is_err() as u64;
    }

    /// A window of the client's shots was judged (`RenderFloor::judge`).
    fn judged(&mut self, j: Judgment) {
        let s = &mut self.session;
        s.windows += 1;
        if j.flagged() {
            s.windows_flagged += 1;
            if s.first_flag.is_none() {
                s.first_flag = Some(j);
                self.flagged_now = true;
            }
        }
    }

    /// Whether input `seq` hasn't arrived before: queued or applied, or late
    /// (a stand-in took its seq; still unseen).
    fn is_new(&self, seq: u32) -> bool {
        if seq <= self.last_seq {
            let age = self.last_seq - seq;
            age < 32 && self.stand_ins & (1 << age) != 0
        } else {
            self.pending.binary_search_by_key(&seq, |q| q.0).is_err()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(&mut self, e: u16, seq: u32, input: Input, render: Option<RenderTime>, shot: Option<(Shot, Option<Held>)>, arrived: Instant, dead: bool) -> Push {
        if seq <= self.last_seq {
            let age = self.last_seq - seq;
            if age < 32 && self.stand_ins & (1 << age) != 0 {
                self.stand_ins &= !(1 << age); // count each late seq once
                // Its movement is gone (a stand-in moved instead), but a shot
                // in it still fires, from where the stand-in put the shooter.
                if let Some((shot, held)) = shot {
                    if age < LATE_SHOT_STEPS && !dead {
                        self.fire(e, seq, shot, held, render, true, input.buttons & BUTTON_ADS != 0);
                    } else {
                        self.refused += 1;
                    }
                }
                return Push::Late;
            }
            return Push::Duplicate;
        }
        let at = self.pending.partition_point(|&(s, ..)| s < seq);
        if self.pending.get(at).is_some_and(|&(s, ..)| s == seq) {
            return Push::Duplicate; // keep the first arrival
        }
        self.pending.insert(at, (seq, input, render, shot, arrived));
        if self.pending.len() > MAX_QUEUED_INPUTS {
            let (s, ..) = self.pending.pop_front().unwrap();
            self.consume(s, false);
            return Push::Discarded;
        }
        Push::Queued
    }

    /// Fires `shot` from input `seq` (already consumed): from the shooter's
    /// eye between its states before and after that seq, unless it comes too
    /// soon after the last shot (or is a copy of one), or the rifle has no
    /// round ready (`Ammo`: its client predicts the same). It leaves somewhere in
    /// its cone of fire: aiming down sights (`ads`) or not, from the state
    /// before the seq, with the shooter's bloom. Its claimed view is the one
    /// the render floor `held` it to, if it did.
    #[allow(clippy::too_many_arguments)]
    fn fire(&mut self, e: u16, seq: u32, shot: Shot, held: Option<Held>, render: Option<RenderTime>, late: bool, ads: bool) {
        let time = shot_time(seq, shot.frac);
        let w = weapon::of(self.ammo.kit);
        if self.last_shot.is_some_and(|last| time < last + w.interval as u64) {
            self.refused += 1;
            return;
        }
        let Some(&(_, step_no, before, after)) = self.steps.iter().find(|s| s.0 == seq) else {
            self.refused += 1;
            return;
        };
        if !self.ammo.ready() {
            self.refused += 1;
            return;
        }
        self.ammo.fire();
        self.last_shot = Some(time);
        let cone = if self.cone { self.bloom.fire(time, ads, &before) } else { 0.0 };
        let picks = |round: u8| self.spread.as_ref().map_or([0; 4], |(key, spawn)| key.picks(*spawn, seq, round));
        let first = picks(0);
        let dir = weapon::spread(shot.yaw, shot.pitch, cone, first[0]);
        let (yaw, pitch) = weapon::angles(dir);
        let tau0 = step_no as f64 - 1.0 + shot.frac as f64 / 256.0;
        let mid_lag = render.map_or(0.0, |r| r.mid_lag as f64 / MID_LAG_UNITS);
        let near = msg::render_age(step_no, shot.render) - (1.0 - shot.frac as f64 / 256.0);
        let (behind, held) = match held {
            Some((h, moved)) => ([tau0 - h[0], tau0 - h[1]], moved),
            None => ([near, near + mid_lag], 0.0),
        };
        let fire = Fire {
            shooter: e,
            seq,
            origin: weapon::muzzle(&before, &after, shot.frac),
            dir,
            // Where it really went (others' tracers show the spread).
            yaw,
            pitch,
            tau0,
            behind,
            late,
            // This input's server wait (just set by `advance`); none if late.
            wait: if late { 0.0 } else { self.wait as f64 / 10.0 / 1000.0 * TICK_HZ as f64 },
            held,
            settling: if self.inputs_seen < SETTLE_INPUTS {
                shots::SETTLE_SLACK
            } else if self.stand_ins != 0 {
                shots::STAND_IN_SLACK // a stand-in in the last 32 steps
            } else {
                0.0
            },
            kit: self.ammo.kit,
            pellet: 0,
        };
        if w.pellets <= 1 {
            self.fires.push(fire);
            return;
        }
        // A shotgun: its pellets spread round the shot, each by a pick of its own.
        let more = picks(1);
        for k in 0..w.pellets {
            let pick = match k {
                0..=2 => first[k as usize + 1],
                3..=6 => more[k as usize - 3],
                _ => more[3] ^ first[0].rotate_left(17),
            };
            let d = w.pellet(dir, pick);
            let (yaw, pitch) = weapon::angles(d);
            self.fires.push(Fire { dir: d, yaw, pitch, pellet: k, ..fire });
        }
    }

    fn consume(&mut self, seq: u32, stand_in: bool) {
        let shift = seq - self.last_seq;
        self.stand_ins = if shift >= 32 { 0 } else { self.stand_ins << shift } | stand_in as u32;
        self.last_seq = seq;
    }

    /// Advance one movement step, consuming seq `last_seq + 1`. `now` is the
    /// tick's time, `step` the game step the result is at.
    fn advance(&mut self, e: u16, body: &mut Body, now: Instant, step_no: u32, world: &World, owners: &[u8]) -> Step {
        let who = movement::Mover { faction: faction(e), owners, live: Live::at(step_no) };
        self.depth = self.pending.len().min(u8::MAX as usize) as u8;
        self.rewind = None;
        let next = self.last_seq + 1;
        let mut fired = None;
        let (input, kind) = if self.pending.front().is_some_and(|&(s, ..)| s == next) {
            let (_, input, render, shot, arrived) = self.pending.pop_front().unwrap();
            fired = shot.map(|s| (s, render));
            self.rewind = render.map(|r| r.ages(step_no));
            let waited = now.saturating_duration_since(arrived).as_micros() / 100;
            self.wait = waited.min(WAIT_STAND_IN as u128 - 1) as u16;
            self.last = input;
            self.starved_run = 0;
            (input, Step::Applied)
        } else if self.last_seq == 0 {
            body.state = step(world, body.state, Input::default(), who); // (dead or not, it stands)
            return Step::Waiting;
        } else {
            // The input for `next` is late or lost: a stand-in takes its seq, and
            // the real one is dropped if it shows up. Freezing after the grace
            // means holding packets back (a lag switch) buys no movement.
            // (The rule is shared: a resyncing client predicts it.)
            self.starved_run += 1;
            self.wait = WAIT_STAND_IN;
            let kind = if self.starved_run <= GRACE_TICKS { Step::Repeated } else { Step::Frozen };
            (movement::stand_in(self.last, self.starved_run), kind)
        };
        // The rifle as its client predicts it (`Ammo::step`): the reload counts
        // down, then a shot in this input (in `fire`), then its reload button.
        // The ability's gear from its state before the step (`Ammo::gear`), as predicted.
        let applied = if body.dead() { movement::dead_input(input) } else { input };
        let gear = self.ammo.gear(applied.buttons);
        self.ammo.tick();
        let mag = self.ammo.mag;
        let before = body.state;
        body.state = movement::step_with(world, body.state, applied, who, gear);
        if before.grounded && !body.state.grounded {
            body.left_ground = [before.pos[0], before.pos[1], before.z];
        }
        if !body.dead() {
            let hurt = movement::fall_damage(&before, &body.state);
            body.fallen = body.fallen.saturating_add(hurt as u16);
            if hurt > 0 && *FALL_TRACE {
                let s = &body.state;
                let l = body.left_ground;
                eprintln!("fall {e} at ({:.0}, {:.0}, {:.1}) from {:.1} m: {hurt} damage, left the ground at ({:.1}, {:.1}, {:.1})", s.pos[0], s.pos[1], s.z, movement::fall_height(&before, s), l[0], l[1], l[2]);
            }
        }
        self.steps[next as usize % STEP_RING] = (next, step_no, before, body.state);
        // A body keeps the aim it died with: the dead player's own camera
        // still turns (on its client), but a corpse doesn't spin for others.
        if !body.dead() {
            body.yaw = input.yaw;
            body.pitch = input.pitch;
        }
        body.ads = !body.dead() && input.buttons & BUTTON_ADS != 0;
        self.consume(next, kind != Step::Applied);
        if kind == Step::Applied {
            self.session.inputs += 1;
        } else {
            self.session.stand_ins += 1;
        }
        // Only real inputs fire: a stand-in repeats movement, never a shot.
        if let Some(((shot, held), render)) = fired {
            if body.dead() {
                self.refused += 1;
            } else {
                self.fire(e, next, shot, held, render, false, input.buttons & BUTTON_ADS != 0);
            }
        }
        if input.buttons & BUTTON_RELOAD != 0 && !body.dead() {
            self.ammo.start_reload();
        }
        // Then the ability (a round fired drops the cloak).
        let fired = self.ammo.mag < mag;
        let kit = self.ammo.kit;
        self.ammo.ability.step(kit, applied.buttons, fired);
        if self.unlimited {
            self.ammo = self.ammo.refilled();
        }
        body.reloading = !body.dead() && self.ammo.reload > 0;
        body.ability = !body.dead() && self.ammo.ability.on();
        kind
    }
}

/// Hands one shard's reliable news and snapshots to its connections, flushes
/// them stamped `at`, and appends the datagrams to `out`. Returns `at`.
fn frame(shard: &mut lattice_net::Shard, snaps: &mut Vec<(ClientId, Vec<u8>, Option<u32>)>, news: &mut Vec<(ClientId, Vec<u8>)>, out: &mut Vec<Datagram>, at: Instant) -> Instant {
    for (client, msg) in news.drain(..) {
        let _ = shard.send(client, Channel::Reliable, msg);
    }
    for (client, snap, tag) in snaps.drain(..) {
        let _ = match tag {
            Some(tag) => shard.send_tagged(client, snap, tag),
            None => shard.send(client, Channel::Unreliable, snap),
        };
    }
    shard.flush(at);
    out.extend(shard.drain_outgoing());
    at
}

/// Spawn rooms' fields hurt this much (`SimServer::fields`) every
/// `FIELD_PERIOD` steps: from full health, dead in about 4 s.
const FIELD_DAMAGE: u8 = 12;
const FIELD_PERIOD: u32 = TICK_HZ / 2;

/// Whether a player standing at `s` is in spawn room `r` (its floor's
/// footprint, feet on or a little over its floor).
fn in_room(r: &lattice_game::world::SpawnRoom, s: &MoveState) -> bool {
    let (dx, dy) = (s.pos[0] - r.center[0], s.pos[1] - r.center[1]);
    let (u, v) = (dx * r.axis[0] + dy * r.axis[1], -dx * r.axis[1] + dy * r.axis[0]);
    u.abs() <= r.half[0] && v.abs() <= r.half[1] && s.z > r.floor - 0.5 && s.z < r.floor + 2.5
}

/// Whether `shooter`'s eye sees `target`'s chest now (no terrain or cover
/// between them).
fn in_sight(world: &World, owners: &[u8], live: Live, history: &History, shooter: u16, target: u16) -> bool {
    let (Some(a), Some(b)) = (history.now(shooter), history.now(target)) else { return false };
    let eye = if a.crouched { weapon::CROUCH_EYE_HEIGHT } else { weapon::EYE_HEIGHT };
    let chest = lattice_game::hit::chest(b.crouched);
    let (p0, p1) = ([a.pos[0], a.pos[1], a.pos[2] + eye], [b.pos[0], b.pos[1], b.pos[2] + chest]);
    lattice_game::hit::line_clear(world, p0, p1, movement::Mover { faction: faction(shooter), owners, live })
}

/// A projectile that ended this tick: where it was in the list, how, and
/// (for a hit) whether its shooter still sees the target.
struct Ended {
    id: u64,
    index: usize,
    shooter: u16,
    outcome: Outcome,
    at: [f32; 3],
    after_cover: bool,
    kit: u8,
    origin: [f32; 3],
}

/// How far player `i` at `p`, feet at `z`, is pushed this tick by player `j`
/// at `q`, feet at `qz` (before the `MAX_PUSH` cap). Only bodies that
/// overlap push: within `SEP_DIST` across, and less than a body height apart
/// vertically (one standing on a crate beside another still overlaps it; one
/// on a floor above doesn't).
fn separation(i: u32, p: [f32; 2], z: f32, j: u32, q: [f32; 2], qz: f32) -> [f32; 2] {
    let (dx, dy) = (p[0] - q[0], p[1] - q[1]);
    let d = (dx * dx + dy * dy).sqrt();
    if d >= SEP_DIST || (qz - z).abs() >= HEIGHT {
        return [0.0; 2];
    }
    let (nx, ny) = if d > 1e-4 {
        (dx / d, dy / d)
    } else {
        // Coincident: a direction from the pair, opposite for each.
        let (lo, hi) = (i.min(j), i.max(j));
        let a = (lo.wrapping_mul(0x9E37_79B9) ^ hi.wrapping_mul(0x85EB_CA6B)) as f32 * (std::f32::consts::TAU / 4_294_967_296.0);
        let sign = if i < j { 1.0 } else { -1.0 };
        (a.cos() * sign, a.sin() * sign)
    };
    let k = (SEP_DIST - d) * 0.5 * SEP_RATE;
    [nx * k, ny * k]
}

pub struct SimServer {
    cfg: SimConfig,
    net: Server,
    tick: u32,
    /// Game time in 1/30 s movement steps, as of the last tick's states.
    step: u32,
    bodies: Vec<Body>,
    /// Each entity's input queue. Locked so ingress tasks can queue their
    /// shards' inputs in parallel; never contended, since all of a client's
    /// messages come through its shard. Phases with `&mut` skip the lock.
    inputs: Vec<Mutex<InputQueue>>,
    /// Serialized once per tick: every entity's quantized near state, kept
    /// for `NEAR_HISTORY` ticks as delta baselines, and mid/far blobs for the
    /// entities due this tick (and last tick's far-due, for carries). Near
    /// history is tick-major: baselines are mostly 2-4 ticks back, so the
    /// arrays in use stay in cache.
    near_hist: Vec<Vec<NearQ>>,
    near_hist_tick: [u32; NEAR_HISTORY],
    far_blobs: Vec<Blob>,
    history: History,
    projectiles: Vec<Projectile>,
    /// Firing per cell, for distant fights.
    activity: Activity,
    next_projectile: u64,
    /// Hits since the last `take_hits`.
    hits: Vec<HitRecord>,
    /// The latest shots fired (shooter, seq, direction), for tests.
    fired: VecDeque<(u16, u32, [f32; 3])>,
    /// This tick's shots, sorted by shooter: (shooter, yaw, pitch, step fired),
    /// for the tracers of whoever has the shooter near-tier.
    tick_shots: Vec<(u16, u16, i16, f64)>,
    /// Combat news to send this tick: (recipient entity, event).
    outbox: Vec<(u16, Event)>,
    /// Reliable messages per shard, sent before this tick's snapshots.
    reliable_out: Vec<Vec<(ClientId, Vec<u8>)>>,
    /// Each entity's client, while connected.
    client_of: Vec<Option<ClientId>>,
    /// Each entity's client slot while connected: its shard and its index in
    /// that shard's list (shots look up a shooter's near set through it).
    slot_of: Vec<Option<(u16, u32)>>,
    /// This tick's shots, gathered in entity order by the movement phase.
    fires: Vec<Fire>,
    /// Free entity ids, per faction (`faction::faction(id)`).
    free: [Vec<u16>; FACTIONS as usize],
    /// Damage to apply after this tick's movement: (entity, amount, from).
    pending_damage: Vec<(u16, u8, u16)>,
    /// Fractional random deaths carried between ticks (`deaths_per_sec`).
    death_acc: f64,
    /// The last Mend field beat healed (`mend`): game steps / `MEND_PERIOD`.
    mend_beat: u32,
    /// The last spawn room field beat (`fields`).
    field_beat: u32,
    by_client: HashMap<ClientId, u16>,
    /// Clients grouped by transport shard, with their interest state.
    shard_clients: Vec<Vec<ClientSlot>>,
    scratch: Vec<Scratch>,
    squads: HashMap<u32, Vec<u16>>,
    squad_anchor: HashMap<u32, [f32; 2]>,
    /// Per-shard snapshot buffers, reused every tick.
    snapshots: Vec<Vec<Snap>>,
    /// Per shard: connects (with the connect token's user id) and
    /// disconnects (`None`), in the order the transport reported them.
    lifecycle: Vec<Vec<(ClientId, Option<u64>)>>,
    /// Per shard: requests on the reliable channel (Deploy), in order.
    requests: Vec<Vec<(ClientId, Vec<u8>)>>,
    /// Players deploy (`SimConfig::deploy`, on a world with territory).
    deploying: bool,
    /// What each player took in its last `RECAP_STEPS` (for its recap and
    /// its killer's assists).
    damage_log: Vec<Vec<Taken>>,
    /// Recaps to send with this tick's news: (victim, message).
    recaps: Vec<(u16, Vec<u8>)>,
    /// The site whose region each `REGION_CELL` cell is in (empty: no sites).
    regions: Vec<u16>,
    /// Shots fired in each site's region since the last Fights, and its step.
    site_shots: Vec<u32>,
    fights_step: u32,
    /// Session log lines since the last `take_session_records`.
    session_records: Vec<SessionRecord>,
    /// Input waits (arrival -> applied) since the last `take_input_wait`, in 0.1 ms.
    input_wait: Histogram,
    /// Rewinds (see `InputQueue::rewind`) for near and for mid/far targets
    /// since the last `take_rewind`, in ms of game time.
    rewind: [Histogram; 2],
    /// Trimmed shots: how far past the plausible bound they claimed, in 0.1 steps.
    trim_excess: Histogram,
    /// How long after each of the last `SEND_DELAY_TICKS` ticks' start its
    /// sends were stamped (0 unless `SimConfig::real_time`), in steps.
    send_delays: [f64; SEND_DELAY_TICKS],
    /// The shared spatial index (all entities).
    grid: Grid,
    /// Networking's views of it: entities due this tick for mid and for far.
    mid_grid: Grid,
    far_grid: Grid,
    rng: Rng,
    counters: Counters,
    ladder: Ladder,
    world: Arc<World>,
    /// Who holds what (U5), updated twice a second; who holds each site at
    /// this tick's steps (two at 20 Hz), for the shields.
    territory: Territory,
    /// Where at each site someone spawns (`SpawnMode::Sites`, deploys):
    /// spots in its spawn room, feet.
    spawn_points: Vec<Vec<[f32; 3]>>,
    /// A lane's start under the ground (a tunnel's, `SpawnMode::Lanes`): the
    /// height of its floor, for the spawn being made.
    spawn_floor: Option<f32>,
    owners: [Vec<u8>; 2],
    /// The step of the last territory update.
    territory_step: u32,
    /// This tick's separation push per entity.
    pushes: Vec<[f32; 2]>,
    pace: PaceMeter,
    /// Pace advertised in this tick's snapshots.
    pace_now: f32,
    /// Fractional 1/30 s movement steps carried between ticks (at 20 Hz: 1.5 per tick).
    step_acc: f64,
    /// `cfg.interest` at the current ladder level.
    interest: InterestConfig,
    /// Debug map: the entity whose client to watch, and the last capture.
    watch: Option<u16>,
    debug: Option<DebugFrame>,
    /// The last tick's per-shard task spans (see `tasks`).
    spans: PhaseSpans,
    /// Picks where in its cone each shot goes; shared with the input queues.
    spread: Arc<SpreadKey>,
    /// The pace advertised in each of the last `PACE_TICKS` ticks.
    paces: [f32; PACE_TICKS],
    /// How far the render floor moved shots' claimed views (0.1 steps).
    held_by: Histogram,
    /// Clients' pre-shot dip excess as they left (0.1 steps, from 0).
    dip_excess: Histogram,
}

/// Ticks of advertised pace the render floor looks back over (2 s at 30 Hz).
const PACE_TICKS: usize = 60;

impl SimServer {
    /// The rifle never runs dry here (`SimConfig::unlimited_rounds`, or immortal).
    fn unlimited_rounds(&self) -> bool {
        self.cfg.unlimited_rounds || self.cfg.immortal
    }

    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        assert!(cfg.max_clients <= u16::MAX as usize, "entity ids are u16");
        let first_step = clock::at_minute(cfg.clock_minutes);
        let mut net =
            Server::with_socket_groups(cfg.net.clone(), &cfg.identity, cfg.max_clients, cfg.shards, cfg.socket_groups, now);
        if cfg.preallocate {
            net.preallocate(cfg.max_clients);
        }
        let (ladder, interest) = (Ladder::new(cfg.ladder.clone()), cfg.interest.clone());
        let spread_secret = cfg.spread_secret;
        Self {
            world: World::shared(cfg.world),
            territory: Territory::with_speed(&World::shared(cfg.world), cfg.capture_speed.max(1)),
            spawn_points: territory::spawn_points(&World::shared(cfg.world)),
            spawn_floor: None,
            owners: [Vec::new(), Vec::new()],
            territory_step: first_step,
            pushes: Vec::new(),
            net,
            shard_clients: (0..cfg.shards).map(|_| Vec::new()).collect(),
            scratch: (0..cfg.shards).map(|_| Scratch::default()).collect(),
            squads: HashMap::new(),
            squad_anchor: HashMap::new(),
            snapshots: vec![Vec::new(); cfg.shards],
            lifecycle: vec![Vec::new(); cfg.shards],
            requests: vec![Vec::new(); cfg.shards],
            deploying: cfg.deploy && !World::shared(cfg.world).lanes().is_empty(),
            damage_log: Vec::new(),
            recaps: Vec::new(),
            regions: regions(&World::shared(cfg.world)),
            site_shots: vec![0; World::shared(cfg.world).sites().len()],
            fights_step: first_step,
            session_records: Vec::new(),
            input_wait: Histogram::new(INPUT_WAIT_CAP),
            rewind: [Histogram::new(REWIND_CAP_MS), Histogram::new(REWIND_CAP_MS)],
            trim_excess: Histogram::new(200),
            send_delays: [0.0; SEND_DELAY_TICKS],
            rng: Rng::new(cfg.seed),
            cfg,
            tick: 0,
            step: first_step,
            bodies: Vec::new(),
            inputs: Vec::new(),
            near_hist: vec![Vec::new(); NEAR_HISTORY],
            near_hist_tick: [u32::MAX; NEAR_HISTORY],
            far_blobs: Vec::new(),
            history: History::default(),
            projectiles: Vec::new(),
            fired: VecDeque::new(),
            activity: Activity::default(),
            next_projectile: 0,
            hits: Vec::new(),
            tick_shots: Vec::new(),
            outbox: Vec::new(),
            reliable_out: Vec::new(),
            client_of: Vec::new(),
            slot_of: Vec::new(),
            fires: Vec::new(),
            free: Default::default(),
            pending_damage: Vec::new(),
            death_acc: 0.0,
            mend_beat: 0,
            field_beat: 0,
            by_client: HashMap::new(),
            grid: Grid::new(GRID_CELL),
            mid_grid: Grid::new(MID_GRID_CELL),
            far_grid: Grid::new(FAR_GRID_CELL),
            counters: Counters::default(),
            ladder,
            pace: PaceMeter::default(),
            pace_now: 1.0,
            step_acc: 0.0,
            interest,
            watch: None,
            debug: None,
            spans: [NO_SPAN; PHASES.len()],
            spread: Arc::new(SpreadKey::new(spread_secret)),
            paces: [1.0; PACE_TICKS],
            held_by: Histogram::new(200),
            dip_excess: Histogram::new(200),
        }
    }

    /// Record what `entity`'s client receives, for the debug map (`None` stops).
    /// Captures happen every 6th tick (5 Hz at 30 Hz).
    pub fn set_watch(&mut self, entity: Option<u16>) {
        self.watch = entity;
    }

    /// For the last tick's phases split into parallel tasks (ingress, shots,
    /// assembly, transport): the longest task and the total of all of them.
    /// With the phase's wall time this separates serial work, imbalance and
    /// dispatch overhead.
    pub fn tasks(&self) -> &PhaseSpans {
        &self.spans
    }

    /// The latest capture, if one was taken since the last call.
    pub fn take_debug_frame(&mut self) -> Option<DebugFrame> {
        self.debug.take()
    }

    /// Whether a connected client controls `entity`.
    pub fn is_client_entity(&self, entity: u16) -> bool {
        self.by_client.values().any(|&e| e == entity)
    }

    /// Some connected client's entity (to watch when none was chosen).
    pub fn any_entity(&self) -> Option<u16> {
        self.by_client.values().min().copied()
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    pub fn client_count(&self) -> usize {
        self.by_client.len()
    }

    pub fn net(&self) -> &Server {
        &self.net
    }

    /// Routes an inbound datagram's source address to its bucket index.
    pub fn router(&self) -> Router {
        self.net.router()
    }

    /// Wall-clock time until the next tick should start, at the current level.
    pub fn tick_period(&self) -> Duration {
        self.ladder.rung().period()
    }

    /// The degradation level (0 = normal) and what it means.
    pub fn level(&self) -> u8 {
        self.ladder.level()
    }

    pub fn rung(&self) -> &Rung {
        self.ladder.rung()
    }

    /// Game-seconds per wall-second advertised to clients.
    pub fn pace(&self) -> f32 {
        self.pace_now
    }

    /// Feed the last tick's total work time (simulation plus egress), so the
    /// ladder can degrade or recover. Call once per tick, after egress.
    pub fn observe_tick(&mut self, work: Duration) {
        let period = self.tick_period();
        if self.ladder.observe(work, period).is_some() {
            self.interest = self.ladder.rung().apply(&self.cfg.interest);
        }
    }

    pub fn shard_count(&self) -> usize {
        self.shard_clients.len()
    }

    /// Input waits recorded since the last call, arrival -> applied, in 0.1 ms.
    pub fn take_input_wait(&mut self) -> Histogram {
        std::mem::replace(&mut self.input_wait, Histogram::new(INPUT_WAIT_CAP))
    }

    /// How far back each applied input's render steps were, for near and
    /// for mid/far targets (what lag compensation rewinds each by), in ms of
    /// game time, since the last call.
    /// Trimmed shots' excess over the plausible bound, in 0.1 steps (whole run).
    pub fn trim_excess(&self) -> &Histogram {
        &self.trim_excess
    }

    /// How far the render floor moved shots' claimed views, in 0.1 steps
    /// (`Counters::renders_held`).
    pub fn held_by(&self) -> &Histogram {
        &self.held_by
    }

    /// Clients' pre-shot dip excess (`RenderFloor::dips`) as they left, in
    /// 0.1 steps (`Counters::dip_clients`).
    pub fn dip_excess(&self) -> &Histogram {
        &self.dip_excess
    }

    /// Each connected client's shot dips so far, over its session
    /// (`RenderFloor::dips`).
    pub fn shot_dips(&self) -> Vec<(u16, shots::Dips)> {
        self.by_client.values().filter_map(|&e| self.inputs[e as usize].lock().unwrap().claims.dips().map(|d| (e, d))).collect()
    }

    /// The session log since the last call: a line when a client is first
    /// flagged (`RenderFloor::judge`), and one when it leaves. The server
    /// binary appends them to its `--session-log`.
    pub fn take_session_records(&mut self) -> Vec<SessionRecord> {
        std::mem::take(&mut self.session_records)
    }

    /// A last line for every client still connected (the server is
    /// stopping), for `take_session_records`.
    pub fn close_sessions(&mut self, now: Instant) {
        let mut open: Vec<u16> = self.by_client.values().copied().collect();
        open.sort_unstable();
        for e in open {
            let record = self.session_record(e, SessionEvent::Open, now);
            self.session_records.push(record);
        }
    }

    /// Entity `e`'s session as of `now`, with its shots' dips: the window
    /// not judged yet and the whole session.
    fn session_record(&mut self, e: u16, event: SessionEvent, now: Instant) -> SessionRecord {
        let q = self.inputs[e as usize].get_mut().unwrap();
        SessionRecord { event, entity: e, session: q.session.as_of(now), window: q.claims.window_dips(), total: q.claims.dips() }
    }

    pub fn take_rewind(&mut self) -> [Histogram; 2] {
        std::mem::replace(&mut self.rewind, [Histogram::new(REWIND_CAP_MS), Histogram::new(REWIND_CAP_MS)])
    }

    /// Game time of the last tick's states, in movement steps.
    pub fn step_number(&self) -> u32 {
        self.step
    }

    /// The quantized near state `entity` had at `tick`, if still in history.
    pub fn near_state_at(&self, entity: u16, tick: u32) -> Option<NearQ> {
        let slot = tick as usize % NEAR_HISTORY;
        let b = self.bodies.get(entity as usize)?;
        (self.near_hist_tick[slot] == tick && tick >= b.spawned).then(|| self.near_hist[slot][entity as usize])
    }

    pub fn tick_number(&self) -> u32 {
        self.tick
    }

    pub fn entity_state(&self, entity: u16) -> Option<MoveState> {
        self.bodies.get(entity as usize).filter(|b| b.alive).map(|b| b.state)
    }

    /// Run one tick. `inbound` and `out` have one bucket per shard: inbound
    /// datagrams must be bucketed by `router()`, and are consumed; outgoing ones
    /// are appended to their shard's bucket.
    pub fn tick(&mut self, inbound: &mut [Vec<InDatagram>], now: Instant, out: &mut [Vec<Datagram>]) -> PhaseTimes {
        self.tick_sending(inbound, now, out, None)
    }

    /// `tick`, handing each shard's datagrams to `send` as soon as they're
    /// framed, from the shard's own task: packets leave while other shards
    /// still assemble, instead of all after the last one (a burst that
    /// queued at AWS's bandwidth allowance), and two phase boundaries go.
    /// Assembly's phase time and task spans then include framing and
    /// sending; transport's spans are the framing alone.
    pub fn tick_sending(&mut self, inbound: &mut [Vec<InDatagram>], now: Instant, out: &mut [Vec<Datagram>], send: Option<Sender>) -> PhaseTimes {
        let started = Instant::now();
        assert_eq!(inbound.len(), self.shard_count(), "one inbound bucket per shard");
        assert_eq!(out.len(), self.shard_count(), "one outgoing bucket per shard");
        let mut times = PhaseTimes::default();
        let mut t = Instant::now();
        let mut lap = |i: usize| {
            let n = Instant::now();
            times[i] = n - t;
            t = n;
        };

        let rung = *self.ladder.rung();
        self.pace.record(now, rung.period());
        self.pace_now = ladder::advertised_pace(rung.dilation, self.pace.stretch());
        self.paces[self.tick as usize % PACE_TICKS] = self.pace_now;
        self.step_acc += rung.steps_per_tick();
        let steps = self.step_acc.floor() as u32;
        self.step_acc -= steps as f64;
        self.counters.level_ticks[self.ladder.level() as usize] += 1;

        // 1. ingress: the per-packet transport work, one task per shard
        // (wall-clock time only matters for connect-token expiry), which also
        // queues its clients' inputs: a client sends only once it's accepted,
        // and `Accepted` leaves at the end of the tick that spawned it, so its
        // entity is there. Connects and disconnects wait for the events phase.
        let unix_now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.spans = [NO_SPAN; PHASES.len()];
        let (by_client, bodies, inputs) = (&self.by_client, &self.bodies, &self.inputs);
        // Clients' render clocks run at the pace they were told (30 × pace
        // steps a second), which reaches them a round trip late: the render
        // floor goes by the lowest of the last two seconds'.
        let pace = self.paces.iter().copied().fold(f32::MAX, f32::min);
        let clock = ClockRate { rate: TICK_HZ as f64 * pace as f64, jitter: 0.0 };
        let judge = Judge { step: self.step, clock, floor: self.cfg.render_floor, rtt: 0.0 };
        let (ingress, [late, discarded, bad]) = self
            .net
            .shards_mut()
            .par_iter_mut()
            .zip(inbound.par_iter_mut())
            .zip(self.lifecycle.par_iter_mut())
            .zip(self.requests.par_iter_mut())
            .map(|(((shard, bucket), lifecycle), requests)| {
                let t0 = Instant::now();
                let mut n = [0u64; 3];
                let mut handle = |ev, arrived, (jitter, rtt)| match ev {
                    ServerEvent::Message { client, channel: Channel::Unreliable, data } => {
                        if let Some(&e) = by_client.get(&client) {
                            let dead = bodies[e as usize].dead();
                            let judge = Judge { clock: ClockRate { jitter, ..judge.clock }, rtt, ..judge };
                            inputs[e as usize].lock().unwrap().receive(e, &data, arrived, dead, judge, &mut n);
                        }
                    }
                    ServerEvent::Message { client, channel: Channel::Reliable, data } if data.first() == Some(&msg::MSG_DEPLOY) => requests.push((client, data.to_vec())),
                    ServerEvent::Message { .. } => n[2] += 1,
                    ServerEvent::Connected { client, user_id, .. } => lifecycle.push((client, Some(user_id))),
                    ServerEvent::Disconnected { client, .. } => lifecycle.push((client, None)),
                };
                // How much the sender's link delay varies (its RTTs' range
                // of late), in seconds: the render floor's allowance; and its
                // RTT, in ms.
                let jitter = |shard: &lattice_net::Shard, ev: &ServerEvent| match ev {
                    ServerEvent::Message { client, .. } => shard
                        .client_stats(*client)
                        .map_or((0.0, 0.0), |s| ((s.rtt_max_ms - s.rtt_min_ms) as f64 / 1000.0, s.rtt_ms)),
                    _ => (0.0, 0.0),
                };
                for (from, arrived, data) in bucket.drain(..) {
                    shard.receive(from, &data, arrived);
                    while let Some(ev) = shard.poll_event() {
                        let j = jitter(shard, &ev);
                        handle(ev, arrived, j);
                    }
                }
                shard.update(now, unix_now);
                while let Some(ev) = shard.poll_event() {
                    let j = jitter(shard, &ev);
                    handle(ev, now, j);
                }
                (span(t0.elapsed()), n)
            })
            .reduce(|| (NO_SPAN, [0; 3]), |a, b| (join_spans(a.0, b.0), [a.1[0] + b.1[0], a.1[1] + b.1[1], a.1[2] + b.1[2]]));
        self.spans[0] = ingress;
        self.counters.late_inputs += late;
        self.counters.discarded_inputs += discarded;
        self.counters.bad_messages += bad;
        lap(0);

        // 1b. events: connects and disconnects touch the world, so they're
        // applied on one thread, in shard order
        for k in 0..self.shard_count() {
            let mut events = std::mem::take(&mut self.lifecycle[k]);
            for &(client, user) in &events {
                match user {
                    Some(user) => self.spawn(client, user, now, unix_now),
                    None => self.despawn(client, now),
                }
            }
            events.clear();
            self.lifecycle[k] = events; // keep the allocation
            for (client, data) in std::mem::take(&mut self.requests[k]) {
                self.request_deploy(client, &data);
            }
        }
        lap(1);

        // 2. respawns due this tick (before its movement: the inputs it
        // applies are the new life's), then movement
        self.respawn_due(self.step + steps);
        let base_step = self.step;
        // Who holds each site at this tick's steps (shields follow them).
        for k in 0..2 {
            let mut o = std::mem::take(&mut self.owners[k]);
            self.territory.owners_at(base_step + k as u32 + 1, &mut o);
            self.owners[k] = o;
        }
        let (world, owners) = (&*self.world, &self.owners);
        // Movement also gathers the shots fired (here, or by late inputs at
        // ingress) for the shots phase, in entity order.
        let ([applied, repeated, frozen], fires, refused, flagged) = self
            .bodies
            .par_iter_mut()
            .zip(self.inputs.par_iter_mut())
            .enumerate()
            .with_min_len(256)
            .fold(
                || ([0u64; 3], Vec::new(), 0u64, Vec::new()),
                |(mut n, mut fires, refused, mut flagged), (e, (b, q))| {
                    let q = q.get_mut().unwrap();
                    if b.alive {
                        // A tick consumes `steps` 1/30 s movement steps (1 or 2 at 20 Hz).
                        let queued = q.pending.len();
                        for k in 0..steps {
                            match q.advance(e as u16, b, now, base_step + k + 1, world, &owners[(k as usize).min(1)]) {
                                Step::Applied => n[0] += 1,
                                Step::Repeated => n[1] += 1,
                                Step::Frozen => n[2] += 1,
                                Step::Waiting => {}
                            }
                        }
                        // Reported depth means "due now + spares", whatever the step count.
                        q.depth = (queued + 1).saturating_sub(steps as usize).min(u8::MAX as usize) as u8;
                    }
                    fires.append(&mut q.fires);
                    if std::mem::take(&mut q.flagged_now) {
                        flagged.push(e as u16);
                    }
                    q.session.refused += q.refused as u64;
                    (n, fires, refused + std::mem::take(&mut q.refused) as u64, flagged)
                },
            )
            .reduce(
                || ([0; 3], Vec::new(), 0, Vec::new()),
                |(a, mut fa, ra, mut ga), (b, mut fb, rb, mut gb)| {
                    fa.append(&mut fb);
                    ga.append(&mut gb);
                    ([a[0] + b[0], a[1] + b[1], a[2] + b[2]], fa, ra + rb, ga)
                },
            );
        self.fires = fires;
        for e in flagged {
            let q = self.inputs[e as usize].get_mut().unwrap();
            let j = q.session.first_flag.expect("flagged");
            self.session_records.push(SessionRecord {
                event: SessionEvent::Flagged,
                entity: e,
                session: q.session.as_of(now),
                window: Some(j.window),
                total: Some(j.total),
            });
        }
        self.step = base_step + steps;
        self.counters.inputs_applied += applied;
        self.counters.repeated += repeated;
        self.counters.frozen += frozen;
        self.counters.shots_refused += refused;
        for (b, q) in self.bodies.iter().zip(&mut self.inputs) {
            let q = q.get_mut().unwrap();
            if b.alive && q.last_seq > 0 && q.wait != WAIT_STAND_IN {
                self.input_wait.record(q.wait as u32);
            }
            if let (true, Some((near, mid))) = (b.alive, q.rewind) {
                // A render time ahead of the server is bogus (a client can't
                // see the future): counted, and treated as no rewind.
                self.counters.render_ahead += (near < 0.0) as u64;
                for (h, r) in self.rewind.iter_mut().zip([near, mid]) {
                    h.record((r.max(0.0) * 1000.0 / TICK_HZ as f64).round() as u32);
                }
            }
        }
        lap(2);

        // 5. spatial grid, and the due-set views networking queries
        let (bodies, tick) = (&self.bodies, self.tick);
        let alive = || bodies.iter().enumerate().filter(|(_, b)| b.alive && !b.away).map(|(i, b)| (i as u32, b.state.pos));
        self.grid.rebuild(alive());
        let (mid_period, far_period) = (self.interest.mid_period, self.interest.far_period);
        self.mid_grid.rebuild(alive().filter(|&(i, _)| due(i as u16, tick, mid_period)));
        self.far_grid.rebuild(alive().filter(|&(i, _)| due(i as u16, tick, far_period)));
        lap(3);

        // 5b. soft separation: players closer than SEP_DIST are pushed apart,
        // half each, a fraction of the overlap per tick. All pushes are
        // computed from this tick's positions first, then applied, so the
        // result doesn't depend on thread order. (The grid keeps the
        // pre-push positions: a few cm off, for this tick's interest only.)
        let (grid, bodies, on) = (&self.grid, &self.bodies, self.cfg.separation);
        let pushed: usize = self
            .pushes
            .par_iter_mut()
            .enumerate()
            .with_min_len(256)
            .map(|(i, push)| {
                *push = [0.0; 2];
                let b = &bodies[i];
                if !b.alive || !on || b.dead() {
                    return 0;
                }
                let p = b.state.pos;
                grid.for_each_within(p, SEP_DIST, |j, x, y| {
                    // The dead don't block (they're lying on the ground).
                    if j as usize == i || bodies[j as usize].dead() {
                        return;
                    }
                    let d = separation(i as u32, p, b.state.z, j, [x, y], bodies[j as usize].state.z);
                    push[0] += d[0];
                    push[1] += d[1];
                });
                let len = (push[0] * push[0] + push[1] * push[1]).sqrt();
                if len > MAX_PUSH {
                    *push = [push[0] / len * MAX_PUSH, push[1] / len * MAX_PUSH];
                }
                (*push != [0.0; 2]) as usize
            })
            .sum();
        // Most ticks of a spread-out crowd push nobody: skip the second pass.
        if pushed > 0 {
            let (world, owners, live) = (&*self.world, &self.owners[0][..], Live::at(self.step));
            self.bodies.par_iter_mut().zip(self.pushes.par_iter()).enumerate().with_min_len(256).for_each(|(e, (b, &push))| {
                if b.alive && push != [0.0; 2] {
                    b.state = movement::nudge(world, b.state, push, movement::Mover { faction: faction(e as u16), owners, live });
                    b.pushes = b.pushes.wrapping_add(1);
                }
            });
        }
        // 5c. damage and deaths: after movement, so a death shows in this
        // tick's snapshots and the next tick's inputs move nothing.
        self.apply_damage(self.step);
        lap(4);

        // 6. lag-comp history
        let entries = self.bodies.iter().map(|b| shots::HistEntry {
            pos: [b.state.pos[0], b.state.pos[1], b.state.z],
            life: b.life,
            live: b.alive && !b.dead(),
            crouched: b.state.crouched,
        });
        self.history.record(self.tick, self.step, entries);
        lap(5);

        // 6b. shots: new projectiles, then every projectile flies to now
        self.spans[6] = self.shots_phase();
        self.post_news();
        // 6c. territory, twice a second: who's on each point, the meters,
        // flips (announced now, in effect `FLIP_DELAY` steps on).
        if self.step >= self.territory_step + territory::UPDATE_STEPS && !self.territory.is_empty() {
            self.territory_phase();
        }
        if self.step >= self.fights_step + events::FIGHTS_STEPS && !self.territory.is_empty() {
            self.fights_phase();
        }
        if (tick + 1).is_multiple_of(activity::WINDOW) {
            self.counters.activity_cells += self.activity.finish(self.step) as u64;
        }
        lap(6);

        // 7. serialize each entity once per tier. Far blobs are needed for this
        // tick's due entities and last tick's far-due ones (budget carries).
        let hist_slot = tick as usize % NEAR_HISTORY;
        self.near_hist[hist_slot].resize(self.bodies.len(), NearQ::default());
        self.near_hist_tick[hist_slot] = tick;
        self.near_hist[hist_slot]
            .par_iter_mut()
            .zip(self.far_blobs.par_iter_mut())
            .zip(self.bodies.par_iter())
            .enumerate()
            .with_min_len(512)
            .filter(|(_, (_, b))| b.alive)
            .for_each(|(i, ((near, far), b))| {
                let e = i as u16;
                *near = NearQ::new(&b.state, b.yaw, b.pitch, b.health, b.ads, b.reloading, b.kit).with(delta::FLAG_ABILITY, b.ability);
                let prev = tick.wrapping_sub(1);
                if due(e, tick, mid_period) || due(e, tick, far_period) || due(e, prev, far_period) {
                    *far = msg::encode_blob(e, &b.state, b.yaw, b.pitch, b.health, b.kit);
                }
            });
        lap(7);

        // 8. per-client assembly, grouped by shard so each shard's snapshots
        // are ready for its transport task
        let max_message = self.cfg.net.max_message_size();
        let packet_body = self.cfg.net.packet_body_size();
        let view = View {
            cfg: &self.interest,
            shots: &self.tick_shots,
            activity: &self.activity,
            tick,
            step: self.step,
            pace: (self.pace_now * 1000.0).round() as u16,
            level: self.ladder.level(),
            watch: self.watch.filter(|_| tick % 6 == 0),
            bodies: &self.bodies,
            inputs: &self.inputs,
            near_hist: &self.near_hist,
            near_hist_tick: &self.near_hist_tick,
            far_blobs: &self.far_blobs,
            grid: &self.grid,
            mid_grid: &self.mid_grid,
            far_grid: &self.far_grid,
            squads: &self.squads,
            max_message,
            packet_body,
        };
        // With a sender, each shard's task frames its clients' messages and
        // sends them as soon as they're assembled (8b and egress folded in).
        let real = self.cfg.real_time;
        let stamp = move || if real { now + started.elapsed() } else { now };
        let (assembly, framing, sent) = self
            .shard_clients
            .par_iter_mut()
            .zip(self.snapshots.par_iter_mut())
            .zip(self.scratch.par_iter_mut())
            .zip(self.net.shards_mut().par_iter_mut())
            .zip(self.reliable_out.par_iter_mut())
            .zip(out.par_iter_mut())
            .enumerate()
            .map(|(k, (((((clients, snaps), scratch), shard), news), out))| {
                let t0 = Instant::now();
                scratch.tally = Tally::default();
                if scratch.stamp.len() < view.bodies.len() {
                    scratch.stamp.resize(view.bodies.len(), 0);
                }
                for slot in clients.iter_mut() {
                    // Which near messages this client has acked since last tick.
                    scratch.acked.clear();
                    shard.take_acked(slot.client, &mut scratch.acked);
                    view.assemble(slot, scratch, snaps);
                }
                let Some(send) = send else { return (span(t0.elapsed()), NO_SPAN, Duration::ZERO) };
                let t1 = Instant::now();
                let at = frame(shard, snaps, news, out, stamp());
                let framed = span(t1.elapsed());
                send(k, out);
                // The whole task: assembly, framing and sending.
                (span(t0.elapsed()), framed, at - now)
            })
            .reduce(|| (NO_SPAN, NO_SPAN, Duration::ZERO), |a, b| (join_spans(a.0, b.0), join_spans(a.1, b.1), a.2.max(b.2)));
        self.spans[8] = assembly;
        if let Some(watched) = self.scratch.iter_mut().find_map(|sc| sc.watched.take()) {
            let rung = self.ladder.rung();
            self.debug = Some(DebugFrame {
                tick,
                level: self.ladder.level(),
                tick_hz: rung.tick_hz,
                dilation: rung.dilation,
                pace: self.pace_now,
                clients: self.by_client.len(),
                entities: self.bodies.iter().enumerate().filter(|(_, b)| b.alive).map(|(i, b)| (i as u16, b.state.pos)).collect(),
                watched: Some(watched),
            });
        }
        for sc in &self.scratch {
            let (c, t) = (&mut self.counters, &sc.tally);
            c.snapshots += t.snapshots;
            for (a, b) in c.tier_sent.iter_mut().zip(t.tier_sent) {
                *a += b;
            }
            c.snapshot_bytes += t.bytes;
            c.near_capped += t.near_capped;
            c.mid_truncated += t.mid_truncated;
            c.far_skipped += t.far_skipped;
            c.far_starved += t.far_starved;
            c.degraded_clients += t.degraded;
            c.near_bytes += t.near_bytes;
            c.near_deltas += t.near_deltas;
            c.near_full += t.near_full;
            c.near_scanned += t.near_scanned;
            c.tracer_bytes += t.tracer_bytes;
            c.activity_sent += t.activity_sent;
            c.activity_bytes += t.activity_bytes;
            c.activity_cut += t.activity_cut;
            c.mid_scanned += t.mid_scanned;
        }
        lap(8);

        // 8b. transport: queue, frame, ack and checksum, one task per shard
        // (done already, with a sender). Sends are stamped when each shard
        // flushes (real time) or at the tick's instant (simulated), and the
        // latest stamp is kept.
        let (transport, sent) = match send {
            Some(_) => (framing, sent),
            None => self
                .net
                .shards_mut()
                .par_iter_mut()
                .zip(self.snapshots.par_iter_mut())
                .zip(self.reliable_out.par_iter_mut())
                .zip(out.par_iter_mut())
                .map(|(((shard, snaps), news), out)| {
                    let t0 = Instant::now();
                    let at = frame(shard, snaps, news, out, stamp());
                    (span(t0.elapsed()), at - now)
                })
                .reduce(|| (NO_SPAN, Duration::ZERO), |a, b| (join_spans(a.0, b.0), a.1.max(b.1))),
        };
        self.spans[9] = transport;
        self.send_delays[self.tick as usize % SEND_DELAY_TICKS] = sent.as_secs_f64() * TICK_HZ as f64;
        lap(9);

        self.tick = self.tick.wrapping_add(1);
        self.counters.ticks += 1;
        times
    }

    /// The shots phase: this tick's fires (gathered by movement) become
    /// projectiles; every projectile flies up to now (in parallel); hits are
    /// applied in projectile order. Returns the flight's task span.
    fn shots_phase(&mut self) -> Span {
        let fires = std::mem::take(&mut self.fires);
        self.tick_shots.clear();
        // (A shot once, however many pellets: others draw a shotgun's own.)
        self.tick_shots.extend(fires.iter().filter(|f| f.pellet == 0).map(|f| (f.shooter, f.yaw, f.pitch, f.tau0)));
        self.tick_shots.sort_unstable_by_key(|s| s.0);
        // The latest a recent tick sent: what a shooter drew from was up to
        // that much older than its RTT alone says.
        let send = self.send_delays.iter().copied().fold(0.0, f64::max);
        // What each shooter could plausibly have seen: from its RTT (once
        // measured; the highest of the last second or two, since each shot
        // saw the RTT of its moment and an average lags a rise) and its
        // input's wait. Older claims are trimmed. In parallel: the RTT
        // lookups are the work.
        let (net, client_of, first) = (&self.net, &self.client_of, self.next_projectile);
        let made: Vec<(Projectile, Cut)> = fires
            .par_iter()
            .enumerate()
            .map(|(i, f)| {
                let rtt = client_of
                    .get(f.shooter as usize)
                    .copied()
                    .flatten()
                    .and_then(|c| net.client_stats(c))
                    .map(|s| s.rtt_max_ms as f64 / 1000.0 * TICK_HZ as f64)
                    .filter(|&r| r > 0.0);
                Projectile::new(first + i as u64, f, shots::plausible(rtt, f.wait, send, f.settling))
            })
            .collect();
        self.next_projectile += fires.len() as u64;
        for (f, (p, cut)) in fires.iter().zip(made) {
            if f.pellet == 0 {
                self.activity.add(f.shooter, f.origin, f.yaw);
            }
            if let Some(i) = region_at(&self.regions, [f.origin[0], f.origin[1]]) {
                self.site_shots[i] += 1;
            }
            if self.fired.len() == 4096 {
                self.fired.pop_front();
            }
            self.fired.push_back((f.shooter, f.seq, f.dir));
            self.projectiles.push(p);
            if f.pellet > 0 {
                continue; // (a shot's pellets: counted once)
            }
            self.counters.shots += 1;
            self.counters.shots_late += f.late as u64;
            self.counters.rewinds_capped += cut.capped as u64;
            self.counters.rewinds_trimmed += cut.trimmed as u64;
            let session = &mut self.inputs[f.shooter as usize].get_mut().unwrap().session;
            (session.shots, session.trimmed) = (session.shots + 1, session.trimmed + cut.trimmed as u64);
            session.held += (f.held > 0.01) as u64;
            if cut.trimmed {
                self.counters.rewinds_trimmed_mid += cut.mid as u64;
                self.trim_excess.record((cut.excess * 10.0).round() as u32);
            }
            // Past a hundredth of a step: rounding isn't a claim.
            if f.held > 0.01 {
                self.counters.renders_held += 1;
                self.held_by.record((f.held * 10.0).round() as u32);
            }
        }
        if self.projectiles.is_empty() {
            return NO_SPAN;
        }
        // A shooter's near set is what its client was sent last tick: who it
        // drew at the near delay.
        let (clients, slot_of) = (&self.shard_clients, &self.slot_of);
        let near = |shooter: u16| match slot_of.get(shooter as usize) {
            Some(&Some((k, i))) => Some(&clients[k as usize][i as usize].near),
            _ => None,
        };
        let (world, history, owners, live) = (&*self.world, &self.history, &self.owners[0][..], Live::at(self.step));
        let sky = Sky { world, grid: &self.grid, history, near: &near, owners, live };
        let until = self.step as f64;
        // Fly in chunks of 64 projectiles; a chunk is a task (its time is
        // the span's "longest task"). A hit also learns whether the shooter
        // still sees the target now (lag compensation's cost, for counting).
        let flown: Vec<(Vec<Ended>, FlyStats, Duration)> = self
            .projectiles
            .par_chunks_mut(64)
            .enumerate()
            .map(|(c, chunk)| {
                let t = Instant::now();
                let (mut ends, mut st) = (Vec::new(), FlyStats::default());
                for (i, p) in chunk.iter_mut().enumerate() {
                    if let Some((outcome, at)) = shots::fly(p, until, &sky, &mut st) {
                        let after_cover = matches!(outcome, Outcome::Player { target, .. } if !in_sight(world, owners, live, history, p.shooter, target));
                        ends.push(Ended { id: p.id, index: c * 64 + i, shooter: p.shooter, outcome, at, after_cover, kit: p.kit, origin: p.origin });
                    }
                }
                (ends, st, t.elapsed())
            })
            .collect();
        let mut ended = Vec::new();
        let mut task = NO_SPAN;
        for (ends, st, t) in flown {
            ended.extend(ends);
            self.counters.segments += st.segments;
            self.counters.candidates += st.candidates;
            task = join_spans(task, span(t));
        }
        // Ended projectiles leave the list. Its order doesn't matter: each
        // flies alone, and hits apply in id order below.
        let mut gone: Vec<usize> = ended.iter().map(|e| e.index).collect();
        gone.sort_unstable_by(|a, b| b.cmp(a));
        for i in gone {
            self.projectiles.swap_remove(i);
        }
        // Apply in projectile order, so the outcome doesn't depend on threads.
        ended.sort_unstable_by_key(|e| e.id);
        for Ended { shooter, outcome, at, after_cover, kit, origin, .. } in ended {
            match outcome {
                Outcome::Ground => self.counters.hits_ground += 1,
                Outcome::Cover => self.counters.hits_cover += 1,
                Outcome::Expired => self.counters.expired += 1,
                Outcome::Player { target, head, rewind, life } => {
                    let dist = ((at[0] - origin[0]).powi(2) + (at[1] - origin[1]).powi(2) + (at[2] - origin[2]).powi(2)).sqrt();
                    let full = weapon::of(kit).damage_at(dist, head);
                    // The shooter saw it alive, in the life it hit; if it has
                    // died (or respawned) since, no damage.
                    let same_life = self.bodies[target as usize].life == life;
                    let (damage, killed) = match same_life.then(|| self.hurt(target, full, shooter, head, kit)).flatten() {
                        Some(killed) => (full, killed),
                        None => (0, false),
                    };
                    let c = &mut self.counters;
                    (c.hits_head, c.hits_body) = (c.hits_head + head as u64, c.hits_body + !head as u64);
                    c.kills += killed as u64;
                    let session = &mut self.inputs[shooter as usize].get_mut().unwrap().session;
                    (session.hits, session.kills) = (session.hits + 1, session.kills + killed as u64);
                    c.hits_after_cover += after_cover as u64;
                    c.hits_too_late += (damage == 0) as u64;
                    self.hits.push(HitRecord { shooter, target, head, damage, killed, rewind, after_cover, at, tick: self.tick });
                    if damage > 0 {
                        self.news(shooter, target, damage, head, killed);
                    }
                }
            }
        }
        task
    }

    /// Tells the shooter it hit, the target it was hit and from where, and
    /// (on a kill) both and their squads; makes them near-tier for each other.
    fn news(&mut self, shooter: u16, target: u16, damage: u8, head: bool, killed: bool) {
        let until = self.tick + CONTACT_TICKS;
        let (sp, tp) = (self.bodies[shooter as usize].state.pos, self.bodies[target as usize].state.pos);
        self.bodies[shooter as usize].contact(target, until);
        self.bodies[target as usize].contact(shooter, until);
        self.outbox.push((shooter, Event::Hit { target, damage, head, killed }));
        self.outbox.push((target, Event::Hurt { from: shooter, amount: damage, dir: events::direction(tp, sp) }));
        if killed {
            let kill = Event::Kill { killer: shooter, victim: target, head };
            let mut to: Vec<u16> = vec![shooter, target];
            for e in [shooter, target] {
                if let Some(m) = self.squads.get(&self.bodies[e as usize].squad) {
                    to.extend(m);
                }
            }
            to.sort_unstable();
            to.dedup();
            self.outbox.extend(to.into_iter().map(|e| (e, kill)));
        }
    }

    /// Territory's update: each faction's living players on each point (from
    /// this tick's grid), the meters and flips; what changed, to every client.
    fn territory_phase(&mut self) {
        let dt = self.step - self.territory_step;
        self.territory_step = self.step;
        let (grid, bodies, t) = (&self.grid, &self.bodies, &self.territory);
        let mut counts: Vec<Vec<[u16; 3]>> = Vec::with_capacity(t.len());
        for i in 0..t.len() {
            counts.push(
                t.points(i)
                    .iter()
                    .map(|q| {
                        let mut c = [0u16; 3];
                        grid.for_each_within([q[0], q[1]], territory::POINT_RADIUS, |j, _, _| {
                            let b = &bodies[j as usize];
                            if b.alive && !b.dead() && (b.state.z - q[2]).abs() <= territory::POINT_HEIGHT {
                                c[faction(j as u16) as usize] += 1;
                            }
                        });
                        c
                    })
                    .collect(),
            );
        }
        let mut changed = Vec::new();
        let attackers: Vec<u8> = self.territory.sites.iter().map(|s| s.attacker).collect();
        let flipped = self.territory.update(self.step, dt, &mut |i, p| counts[i][p], &mut changed);
        self.counters.site_flips += flipped.len() as u64;
        // XP: whoever's on a site's points when it falls to them; and whoever
        // of its holders is on them when an attack drains away.
        for &i in &changed {
            let i = i as usize;
            let s = self.territory.sites[i];
            let (to, xp, kind) = if flipped.contains(&(i as u16)) {
                let outpost = !territory::spawns_at(self.world.sites()[i].kind);
                (s.owner, if outpost { events::XP_CAPTURE_OUTPOST } else { events::XP_CAPTURE }, events::REWARD_CAPTURE)
            } else if attackers[i] != territory::NONE && s.attacker == territory::NONE && s.meter == 0 {
                (s.owner_at(self.step), events::XP_DEFEND, events::REWARD_DEFEND)
            } else {
                continue;
            };
            if to == territory::NONE {
                continue;
            }
            let mut on = Vec::new();
            for q in self.territory.points(i) {
                self.grid.for_each_within([q[0], q[1]], territory::POINT_RADIUS, |j, _, _| {
                    let b = &self.bodies[j as usize];
                    if faction(j as u16) == to && b.alive && !b.dead() && (b.state.z - q[2]).abs() <= territory::POINT_HEIGHT {
                        on.push(j as u16);
                    }
                });
            }
            on.sort_unstable();
            on.dedup();
            for j in on {
                self.reward(j, kind, xp, i as u16);
            }
        }
        let mut msgs = Vec::new();
        if !flipped.is_empty() {
            msgs.push(self.territory.encode_owners(self.step, Some(&flipped)));
        }
        if !changed.is_empty() {
            msgs.push(self.territory.encode_fronts(self.step, &changed));
        }
        if msgs.is_empty() {
            return;
        }
        self.reliable_out.resize_with(self.shard_count(), Vec::new);
        for client in self.client_of.iter().flatten() {
            for m in &msgs {
                self.reliable_out[self.net.shard_of_client(*client)].push((*client, m.clone()));
            }
        }
    }

    /// Who holds what (tests, the debug map).
    pub fn territory(&self) -> &Territory {
        &self.territory
    }

    /// The outbox as one reliable message per client, bucketed by shard;
    /// and the recaps.
    fn post_news(&mut self) {
        self.reliable_out.resize_with(self.shard_count(), Vec::new);
        for (e, m) in std::mem::take(&mut self.recaps) {
            if let Some(Some(client)) = self.client_of.get(e as usize).copied() {
                self.counters.recaps += 1;
                self.reliable_out[self.net.shard_of_client(client)].push((client, m));
            }
        }
        if self.outbox.is_empty() {
            return;
        }
        let mut outbox = std::mem::take(&mut self.outbox);
        outbox.sort_by_key(|&(e, _)| e);
        for run in outbox.chunk_by(|a, b| a.0 == b.0) {
            let Some(Some(client)) = self.client_of.get(run[0].0 as usize).copied() else { continue };
            for chunk in run.chunks(u8::MAX as usize) {
                let events: Vec<Event> = chunk.iter().map(|&(_, ev)| ev).collect();
                self.counters.events += events.len() as u64;
                self.reliable_out[self.net.shard_of_client(client)].push((client, events::encode_events(&events)));
            }
        }
        outbox.clear();
        self.outbox = outbox;
    }

    /// Takes `amount` of health from `e` now, dealt by `from` (`NOBODY`: a
    /// fall, a test) with a headshot or not, by `cause` (`events::CAUSE_*`):
    /// whether that killed it (it respawns `RESPAWN_STEPS` from now, or
    /// deploys again), or `None` if it wasn't alive to take it.
    fn hurt(&mut self, e: u16, amount: u8, from: u16, head: bool, cause: u8) -> Option<bool> {
        let step_no = self.step;
        let deploying = self.deploying;
        let pos = self.bodies.get(e as usize)?.state.pos;
        let b = self.bodies.get_mut(e as usize).filter(|b| b.alive && !b.dead())?;
        if b.invulnerable {
            return Some(false);
        }
        let amount = ability::taken(b.kit, b.ability, amount);
        // (What it lost: a kill's last hit counts only the health left.)
        let dealt = amount.min(b.health);
        b.health -= dealt;
        let log = &mut self.damage_log[e as usize];
        log.retain(|t| t.step + events::RECAP_STEPS > step_no);
        log.push(Taken { from, amount: dealt, head, step: step_no });
        if b.health > 0 {
            return Some(false);
        }
        b.respawn_at = (!deploying).then_some(step_no + RESPAWN_STEPS);
        b.life = b.life.wrapping_add(1);
        // (Exactly as its client finds it: the deploy screen's transit times are the server's.)
        (b.died_at, b.died_in) = (step_no, territory::region_of(&self.world, pos).map(|i| i as u16));
        self.counters.deaths += 1;
        self.inputs[e as usize].get_mut().unwrap().session.deaths += 1;
        if *KILL_TRACE && from != events::NOBODY {
            let room = |s: &MoveState| self.world.spawn_rooms().iter().position(|r| in_room(r, s));
            let (t, k) = (self.bodies[e as usize].state, self.bodies.get(from as usize).map(|b| b.state));
            if let Some(k) = k {
                eprintln!(
                    "kill {from} (faction {}) at ({:.1}, {:.1}, {:.1}) room {:?} -> {e} (faction {}) at ({:.1}, {:.1}, {:.1}) room {:?}, {:.1} m",
                    faction(from), k.pos[0], k.pos[1], k.z, room(&k), faction(e), t.pos[0], t.pos[1], t.z, room(&t),
                    ((k.pos[0] - t.pos[0]).powi(2) + (k.pos[1] - t.pos[1]).powi(2) + (k.z - t.z).powi(2)).sqrt()
                );
            }
        }
        self.died(e, from, head, cause);
        Some(true)
    }

    /// Spawn rooms' fields (U4/U5, PlanetSide's pain fields): every
    /// `FIELD_PERIOD` steps, everyone in a spawn room whose side doesn't hold
    /// its site takes `FIELD_DAMAGE`. Without it, attackers who got into a
    /// room while their side held it, shut in when it flipped back, camped
    /// whoever deployed there.
    fn fields(&mut self, step_no: u32) {
        let beat = step_no / FIELD_PERIOD;
        if beat == self.field_beat {
            return;
        }
        self.field_beat = beat;
        let mut hurt = Vec::new();
        for r in self.world.spawn_rooms() {
            let owner = self.owners[0].get(r.site as usize).copied().unwrap_or(territory::NONE);
            let reach = (r.half[0] * r.half[0] + r.half[1] * r.half[1]).sqrt();
            self.grid.for_each_within(r.center, reach, |j, _, _| {
                let b = &self.bodies[j as usize];
                if b.alive && !b.dead() && faction(j as u16) != owner && in_room(r, &b.state) {
                    hurt.push(j as u16);
                }
            });
        }
        for e in hurt {
            self.hurt(e, FIELD_DAMAGE, events::NOBODY, false, events::CAUSE_FIELD);
        }
    }

    /// Medics' Mend fields (U6): every `MEND_PERIOD` steps of game time, each
    /// medic whose field is on heals every living friend within
    /// `MEND_RADIUS` (and itself) by `MEND_HEAL`.
    fn mend(&mut self, step_no: u32) {
        let beat = step_no / ability::MEND_PERIOD as u32;
        if beat == self.mend_beat {
            return;
        }
        self.mend_beat = beat;
        let medics: Vec<(u8, [f32; 3])> = (0..self.bodies.len())
            .filter(|&m| {
                let b = &self.bodies[m];
                b.alive && !b.dead() && b.ability && b.kit == class::MEDIC
            })
            .map(|m| (faction(m as u16), [self.bodies[m].state.pos[0], self.bodies[m].state.pos[1], self.bodies[m].state.z]))
            .collect();
        if medics.is_empty() {
            return;
        }
        let r2 = ability::MEND_RADIUS * ability::MEND_RADIUS;
        let (grid, bodies) = (&self.grid, &mut self.bodies);
        for (side, at) in medics {
            grid.for_each_within([at[0], at[1]], ability::MEND_RADIUS, |j, _, _| {
                let b = &mut bodies[j as usize];
                let d2 = (b.state.pos[0] - at[0]).powi(2) + (b.state.pos[1] - at[1]).powi(2) + (b.state.z - at[2]).powi(2);
                if faction(j as u16) == side && b.alive && !b.dead() && d2 <= r2 {
                    b.health = (b.health + ability::MEND_HEAL).min(MAX_HEALTH);
                }
            });
        }
    }

    /// `e` was just killed by `from`: its recap (GDD §4.4), the killer's XP
    /// and the assists'.
    fn died(&mut self, e: u16, from: u16, head: bool, cause: u8) {
        let log = std::mem::take(&mut self.damage_log[e as usize]);
        let mut lines: Vec<(u16, u16, u8, u8)> = Vec::new();
        for t in &log {
            match lines.iter_mut().find(|l| l.0 == t.from) {
                Some(l) => {
                    l.1 = l.1.saturating_add(t.amount as u16);
                    l.2 = l.2.saturating_add(1);
                    l.3 = l.3.saturating_add(t.head as u8);
                }
                None => lines.push((t.from, t.amount as u16, 1, t.head as u8)),
            }
        }
        lines.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let me = self.bodies[e as usize].state;
        let killer = (from != events::NOBODY).then(|| self.bodies.get(from as usize)).flatten().filter(|b| b.alive);
        let distance = killer.map_or(0.0, |k| {
            let s = k.state;
            ((s.pos[0] - me.pos[0]).powi(2) + (s.pos[1] - me.pos[1]).powi(2) + (s.z - me.z).powi(2)).sqrt()
        });
        let recap = events::Recap {
            killer: from,
            cause,
            head,
            distance: (distance * 10.0).round().min(u16::MAX as f32) as u16,
            killer_health: killer.map_or(0, |k| k.health),
            lines: lines.iter().copied().take(events::RECAP_LINES).collect(),
        };
        self.recaps.push((e, events::encode_recap(&recap)));
        if from != events::NOBODY {
            self.reward(from, events::REWARD_KILL, events::XP_KILL + if head { events::XP_HEAD } else { 0 }, e);
        }
        for &(j, ..) in &lines {
            if j != from && j != events::NOBODY && j != e {
                self.reward(j, events::REWARD_ASSIST, events::XP_ASSIST, e);
            }
        }
    }

    /// XP for `e` (a `REWARD_*`, about an entity or a site): an event, and
    /// its session's total.
    fn reward(&mut self, e: u16, kind: u8, xp: u16, about: u16) {
        if !self.bodies.get(e as usize).is_some_and(|b| b.alive) {
            return;
        }
        self.outbox.push((e, Event::Reward { kind, xp, about }));
        self.inputs[e as usize].get_mut().unwrap().session.xp += xp as u64;
        self.counters.xp += xp as u64;
    }

    /// A client asks to deploy at a site (`RULE_DEPLOY`): if it's dead or
    /// has no body and its faction may spawn there, it goes (a corpse is
    /// released) and appears after lattice transit, counted in lanes from
    /// where it died; else it's told why not.
    fn request_deploy(&mut self, client: ClientId, data: &[u8]) {
        let Some(&e) = self.by_client.get(&client) else { return };
        let Ok((site, kit)) = msg::decode_deploy(data) else {
            self.counters.bad_messages += 1;
            return;
        };
        self.counters.deploys_asked += 1;
        let b = self.bodies[e as usize];
        let refused = if !self.deploying {
            Err(territory::REFUSED_INVALID)
        } else if !b.dead() {
            Err(territory::REFUSED_ALIVE)
        } else {
            self.territory.spawnable(site as usize, faction(e), self.step)
        };
        if let Err(reason) = refused {
            self.counters.deploys_refused += 1;
            self.outbox.push((e, Event::Refused { site, reason }));
            return;
        }
        let at = self.step + self.territory.transit_steps(b.died_in.map(|r| r as usize), site as usize);
        let b = &mut self.bodies[e as usize];
        b.deploy = Some((site, at, lattice_game::class::valid(kit)));
        b.away = true;
        self.outbox.push((e, Event::Deploying { site, at }));
    }

    /// Deploys due by `step` (`SimConfig::deploy`), up to the tick's budget:
    /// each appears at a spot in its site's spawn room, if its faction may
    /// still spawn there (else it's told it lost the spawn). And corpses no
    /// one revived go.
    fn deploys_due(&mut self, step_no: u32) {
        let mut budget = DEPLOYS_PER_TICK;
        for e in 0..self.bodies.len() {
            let b = &mut self.bodies[e];
            if !b.alive {
                continue;
            }
            if b.dead() && !b.away && step_no >= b.died_at + CORPSE_STEPS {
                b.away = true;
            }
            let Some((site, _, kit)) = b.deploy.filter(|&(_, at, _)| at <= step_no) else { continue };
            if budget == 0 {
                self.counters.deploys_waited += 1;
                continue;
            }
            let pts = &self.spawn_points[site as usize];
            if let Err(reason) = self.territory.spawnable(site as usize, faction(e as u16), step_no).and(if pts.is_empty() { Err(territory::REFUSED_NO_SPAWN) } else { Ok(()) }) {
                self.bodies[e].deploy = None;
                self.counters.deploys_refused += 1;
                self.outbox.push((e as u16, Event::Refused { site, reason }));
                continue;
            }
            budget -= 1;
            let spot = pts[(self.rng.next_u64() % pts.len() as u64) as usize];
            let p = self.rng.in_disk([spot[0], spot[1]], territory::SPAWN_SCATTER);
            let b = &mut self.bodies[e];
            b.state = MoveState::standing_at(&self.world, p, spot[2]);
            (b.health, b.respawn_at, b.life, b.reloading, b.away, b.deploy) = (MAX_HEALTH, None, b.life.wrapping_add(1), false, false, None);
            (b.anchor, b.radius, b.kit) = (p, 30.0, kit);
            self.damage_log[e].clear();
            let q = self.inputs[e].get_mut().unwrap();
            (q.ammo, q.bloom) = (Ammo::full(kit), Bloom::with_kit(kit));
            q.session.deploys += 1;
            self.counters.deploys += 1;
        }
    }

    /// Fights (every `FIGHTS_STEPS`, to everyone): at each site, who's there
    /// of each faction, and its region's shots a second since the last.
    fn fights_phase(&mut self) {
        let secs = (self.step - self.fights_step) as f32 / TICK_HZ as f32;
        self.fights_step = self.step;
        let mut fights = Vec::new();
        for (i, s) in self.world.sites().iter().enumerate() {
            let r = (s.half * 1.2).clamp(50.0, 250.0);
            let mut n = [0u32; 3];
            self.grid.for_each_within(s.center, r, |j, _, _| {
                let b = &self.bodies[j as usize];
                if b.alive && !b.dead() {
                    n[faction(j as u16) as usize] += 1;
                }
            });
            let shots = std::mem::take(&mut self.site_shots[i]);
            if n != [0; 3] || shots > 0 {
                let heat = (shots as f32 / secs.max(1.0)).round().min(255.0) as u8;
                fights.push(events::Fight { site: i as u16, players: n.map(|c| c.min(255) as u8), heat });
            }
        }
        let m = events::encode_fights(self.step, &fights);
        self.reliable_out.resize_with(self.shard_count(), Vec::new);
        for client in self.client_of.iter().flatten() {
            self.reliable_out[self.net.shard_of_client(*client)].push((*client, m.clone()));
        }
    }

    /// Test aid: `entity` carries kit `kit` (a class's weapon) from now, as
    /// if it had deployed so: a new life (its client rebases), its rounds full.
    pub fn set_kit(&mut self, entity: u16, kit: u8) {
        if let Some(b) = self.bodies.get_mut(entity as usize).filter(|b| b.alive) {
            b.kit = lattice_game::class::valid(kit);
            b.life = b.life.wrapping_add(1);
            let q = self.inputs[entity as usize].get_mut().unwrap();
            (q.ammo, q.bloom) = (Ammo::full(b.kit), Bloom::with_kit(b.kit));
        }
    }

    /// Test aid: hits on `entity` land but deal no damage.
    pub fn set_invulnerable(&mut self, entity: u16, on: bool) {
        if let Some(b) = self.bodies.get_mut(entity as usize) {
            b.invulnerable = on;
        }
    }

    /// Hits since the last call.
    /// The latest shots fired (up to 4096): shooter, seq and direction,
    /// cone of fire included.
    pub fn take_fired(&mut self) -> Vec<(u16, u32, [f32; 3])> {
        self.fired.drain(..).collect()
    }

    pub fn take_hits(&mut self) -> Vec<HitRecord> {
        std::mem::take(&mut self.hits)
    }

    /// Projectiles in flight.
    pub fn projectiles(&self) -> usize {
        self.projectiles.len()
    }

    /// Test aid: puts `entity` standing at `pos` before the next tick, and
    /// makes that where it respawns. Its client sees a life change (a cut),
    /// so prediction stays exact.
    /// Test aid: `teleport`, onto whatever's at about height `z` (a room's floor).
    pub fn teleport_at(&mut self, entity: u16, pos: [f32; 2], z: f32) {
        if let Some(b) = self.bodies.get_mut(entity as usize).filter(|b| b.alive) {
            b.state = MoveState::standing_at(&self.world, pos, z);
            b.life = b.life.wrapping_add(1);
            (b.anchor, b.radius) = (pos, 1.0);
        }
    }

    pub fn teleport(&mut self, entity: u16, pos: [f32; 2]) {
        if let Some(b) = self.bodies.get_mut(entity as usize).filter(|b| b.alive) {
            b.state = MoveState::standing(&self.world, pos);
            b.life = b.life.wrapping_add(1);
            (b.anchor, b.radius) = (pos, 1.0);
        }
    }

    /// The world everyone plays in.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Damages `entity` after the next tick's movement (0 = no effect, a
    /// dead player takes none). Weapons come in M3d.2; tests and
    /// `deaths_per_sec` use this.
    pub fn damage(&mut self, entity: u16, amount: u8) {
        self.pending_damage.push((entity, amount, events::NOBODY));
    }

    /// Test aid: `damage`, as if `from` dealt it with the rifle (its recap
    /// names it, and it earns the kill or the assist).
    pub fn damage_by(&mut self, entity: u16, amount: u8, from: u16) {
        self.pending_damage.push((entity, amount, from));
    }

    /// Whether `entity`'s ability is on (test aid).
    pub fn ability_on(&self, entity: u16) -> bool {
        self.bodies.get(entity as usize).is_some_and(|b| b.ability)
    }

    /// (health, dead) of a connected player.
    pub fn vitals(&self, entity: u16) -> Option<(u8, bool)> {
        self.bodies.get(entity as usize).filter(|b| b.alive).map(|b| (b.health, b.dead()))
    }

    /// Applies queued damage (and `deaths_per_sec`'s): a player brought to 0
    /// dies, to respawn `RESPAWN_STEPS` after `step`.
    fn apply_damage(&mut self, step_no: u32) {
        self.death_acc += self.cfg.deaths_per_sec as f64 * self.ladder.rung().steps_per_tick() / TICK_HZ as f64;
        while self.death_acc >= 1.0 && !self.by_client.is_empty() {
            self.death_acc -= 1.0;
            // A living player, at random (a few tries past free slots).
            let n = self.bodies.len() as u64;
            for _ in 0..16 {
                let e = (self.rng.next_u64() % n) as u16;
                if self.bodies[e as usize].alive && !self.bodies[e as usize].dead() {
                    self.pending_damage.push((e, MAX_HEALTH, events::NOBODY));
                    break;
                }
            }
        }
        self.mend(step_no);
        self.fields(step_no);
        for (e, amount, from) in std::mem::take(&mut self.pending_damage) {
            self.hurt(e, amount, from, false, events::CAUSE_RIFLE);
        }
        // Falls (movement took them; hits came first).
        for e in 0..self.bodies.len() {
            let fallen = std::mem::take(&mut self.bodies[e].fallen);
            if fallen > 0 && self.hurt(e as u16, fallen.min(255) as u8, events::NOBODY, false, events::CAUSE_FALL) == Some(true) {
                self.counters.fall_deaths += 1;
            }
        }
    }

    /// Respawns the dead whose time has come by `step`: back where they were
    /// sent to play, on their faction's side of it (120 degrees apart),
    /// standing, at full health.
    fn respawn_due(&mut self, step_no: u32) {
        if self.deploying {
            return self.deploys_due(step_no);
        }
        for e in 0..self.bodies.len() {
            let b = &self.bodies[e];
            if !b.alive || b.respawn_at.is_none_or(|at| at > step_no) {
                continue;
            }
            let at_site = (self.cfg.spawn == SpawnMode::Sites).then(|| self.site_spawn(faction(e as u16), Some(self.bodies[e].state.pos))).flatten();
            let b = &mut self.bodies[e];
            let p = match at_site {
                Some((_, p)) => [p[0], p[1]],
                None => {
                    let side = faction(e as u16) as f32 * std::f32::consts::TAU / FACTIONS as f32;
                    let (s, c) = side.sin_cos();
                    let center = [b.anchor[0] + c * b.radius * 0.5, b.anchor[1] + s * b.radius * 0.5];
                    self.rng.in_disk(center, b.radius * 0.3)
                }
            };
            let p = [p[0].clamp(1.0, WORLD_SIZE - 1.0), p[1].clamp(1.0, WORLD_SIZE - 1.0)];
            b.state = match at_site {
                Some((_, q)) => MoveState::standing_at(&self.world, p, q[2]),
                None => MoveState::standing(&self.world, p),
            };
            (b.health, b.respawn_at, b.life, b.reloading) = (MAX_HEALTH, None, b.life.wrapping_add(1), false);
            self.damage_log[e].clear();
            // A new life with its class's weapon, full.
            self.inputs[e].get_mut().unwrap().ammo = Ammo::full(b.kit);
            self.counters.respawns += 1;
        }
    }

    fn spawn(&mut self, client: ClientId, user: u64, now: Instant, unix_now: u64) {
        let squad = match self.cfg.interest.squad_size {
            0 => NO_SQUAD,
            n => (self.counters.spawns / n as u64) as u32,
        };
        // Factions take turns: by squad, or by player without squads.
        let side = if squad == NO_SQUAD { self.counters.spawns } else { squad as u64 } % FACTIONS as u64;
        // Deploying, it waits with no body at its Landing (as if at its
        // spawn room's door) until it deploys somewhere.
        let landing = self.deploying.then(|| self.territory.landing(side as u8)).flatten().and_then(|i| self.spawn_points[i].first().copied());
        let (spawn, anchor, radius) = match landing {
            Some(p) => ([p[0], p[1]], [p[0], p[1]], 1.0),
            None => match (self.cfg.spawn == SpawnMode::Sites).then(|| self.site_spawn(side as u8, None)).flatten() {
                Some((_, p)) => {
                    self.spawn_floor = Some(p[2]);
                    ([p[0], p[1]], [p[0], p[1]], 30.0)
                }
                None => self.pick_spawn(squad),
            },
        };
        let e = match self.free[side as usize].pop() {
            Some(e) => e,
            None => loop {
                // New ids go to their faction's free list until one is ours.
                let id = self.bodies.len() as u16;
                self.bodies.push(Body::default());
                self.pushes.push([0.0; 2]);
                self.inputs.push(Mutex::new(InputQueue { cone: self.cfg.cone_of_fire, unlimited: self.unlimited_rounds(), ..InputQueue::default() }));
                self.far_blobs.push([0; FAR_BLOB]);
                self.damage_log.push(Vec::new());
                if faction(id) as u64 == side {
                    break id;
                }
                self.free[faction(id) as usize].insert(0, id);
            },
        };
        let i = e as usize;
        let state = match self.spawn_floor.take() {
            Some(z) => MoveState::standing_at(&self.world, spawn, z),
            None => MoveState::standing(&self.world, spawn),
        };
        self.damage_log[i].clear();
        let away = landing.is_some();
        self.bodies[i] = Body {
            alive: true,
            state,
            away,
            health: if away { 0 } else { MAX_HEALTH },
            squad,
            spawned: self.tick,
            anchor,
            radius,
            invulnerable: self.cfg.immortal,
            fallen: 0,
            left_ground: [0.0; 3],
            ..Body::default()
        };
        let spread = Some((Arc::clone(&self.spread), self.counters.spawns));
        let session = Session { user, joined_unix: unix_now, joined: Some(now), ..Session::default() };
        self.inputs[i] = Mutex::new(InputQueue { cone: self.cfg.cone_of_fire, unlimited: self.unlimited_rounds(), spread, session, ..InputQueue::default() });
        if squad != NO_SQUAD {
            self.squads.entry(squad).or_default().push(e);
        }
        self.by_client.insert(client, e);
        if self.client_of.len() <= i {
            self.client_of.resize(i + 1, None);
        }
        self.client_of[i] = Some(client);
        let slot = ClientSlot {
            client,
            entity: e,
            near: NearState::default(),
            far_carry: Vec::new(),
            ladder: ClientLadder::default(),
            sent_ring: (0..SENT_RING).map(|_| (u32::MAX, Vec::new())).collect(),
        };
        let k = self.net.shard_of_client(client);
        self.shard_clients[k].push(slot);
        if self.slot_of.len() <= i {
            self.slot_of.resize(i + 1, None);
        }
        self.slot_of[i] = Some((k as u16, (self.shard_clients[k].len() - 1) as u32));
        self.counters.spawns += 1;
        let rules = if self.unlimited_rounds() { msg::RULE_UNLIMITED_ROUNDS } else { 0 } | if self.deploying { msg::RULE_DEPLOY } else { 0 };
        let s = self.bodies[i].state;
        let welcome = msg::encode_welcome(&Welcome { entity: e, spawn: s.pos, spawn_z: s.z, anchor, radius, world: self.cfg.world, world_digest: self.world.digest(), rules });
        let _ = self.net.send(client, Channel::Reliable, welcome);
        // Who holds what, and every site's fight.
        if !self.territory.is_empty() {
            let all: Vec<u16> = (0..self.territory.len() as u16).collect();
            let _ = self.net.send(client, Channel::Reliable, self.territory.encode_owners(self.step, None));
            let _ = self.net.send(client, Channel::Reliable, self.territory.encode_fronts(self.step, &all));
        }
    }

    fn despawn(&mut self, client: ClientId, now: Instant) {
        if let Some(e) = self.by_client.remove(&client) {
            let k = self.net.shard_of_client(client);
            let list = &mut self.shard_clients[k];
            if let Some((_, i)) = self.slot_of[e as usize].take() {
                debug_assert_eq!(list[i as usize].client, client);
                list.swap_remove(i as usize);
                // The last slot moved into its place.
                if let Some(moved) = list.get(i as usize) {
                    self.slot_of[moved.entity as usize] = Some((k as u16, i));
                }
            }
            self.client_of[e as usize] = None;
            // Did it spend the render floor's allowances on stale shots?
            let record = self.session_record(e, SessionEvent::Left, now);
            let c = &mut self.counters;
            let s = &record.session;
            (c.dip_windows, c.dip_windows_flagged) = (c.dip_windows + s.windows as u64, c.dip_windows_flagged + s.windows_flagged as u64);
            if let Some(d) = record.total.filter(|d| d.judged()) {
                c.dip_clients += 1;
                c.dip_flagged += (s.first_flag.is_some() || d.flagged() || record.window.is_some_and(|w| w.flagged())) as u64;
                self.dip_excess.record((d.excess.max(0.0) * 10.0).round() as u32);
            }
            self.session_records.push(record);
            let body = &mut self.bodies[e as usize];
            body.alive = false;
            if let Some(members) = self.squads.get_mut(&body.squad) {
                members.retain(|&m| m != e);
                if members.is_empty() {
                    self.squads.remove(&body.squad);
                    self.squad_anchor.remove(&body.squad);
                }
            }
            body.squad = NO_SQUAD;
            self.free[faction(e) as usize].push(e);
            self.counters.despawns += 1;
        }
    }

    /// Where faction `f` spawns (`SpawnMode::Sites`): at a site it holds
    /// with somewhere to stand, the nearest `near` (none: on a front, one
    /// linked to a site it doesn't hold, as a player deploying would; else
    /// its Landing); the site and the spot, a step or two from a route.
    fn site_spawn(&mut self, f: u8, near: Option<[f32; 2]>) -> Option<(usize, [f32; 3])> {
        let sites = self.world.sites();
        let held = |i: usize| self.territory.sites.get(i).is_some_and(|s| s.owner_at(self.step) == f) && !self.spawn_points[i].is_empty();
        let pick = match near {
            Some(p) => (0..sites.len()).filter(|&i| held(i)).min_by(|&a, &b| {
                let d = |i: usize| (sites[i].center[0] - p[0]).powi(2) + (sites[i].center[1] - p[1]).powi(2);
                d(a).total_cmp(&d(b))
            }),
            None => {
                let front: Vec<usize> = (0..sites.len()).filter(|&i| held(i) && self.territory.links(i).iter().any(|&j| self.territory.sites[j as usize].owner_at(self.step) != f)).collect();
                match front.len() {
                    0 => (0..sites.len()).find(|&i| held(i) && sites[i].kind == lattice_game::world::SiteKind::Landing && sites[i].sector == f).or_else(|| (0..sites.len()).find(|&i| held(i))),
                    n => Some(front[(self.rng.next_u64() % n as u64) as usize]),
                }
            }
        }?;
        let pts = &self.spawn_points[pick];
        let at = pts[(self.rng.next_u64() % pts.len() as u64) as usize];
        let p = self.rng.in_disk([at[0], at[1]], territory::SPAWN_SCATTER);
        Some((pick, [p[0], p[1], at[2]]))
    }

    /// Returns (spawn point, wander anchor, wander radius).
    fn pick_spawn(&mut self, squad: u32) -> ([f32; 2], [f32; 2], f32) {
        const HOTSPOTS: [[f32; 2]; 3] = [[2048.0, 2048.0], [6144.0, 2048.0], [4096.0, 6144.0]];
        const CENTER: [f32; 2] = [WORLD_SIZE / 2.0, WORLD_SIZE / 2.0];
        let k = self.counters.spawns;
        let (anchor, radius) = match self.cfg.spawn {
            SpawnMode::Hotspots if k < 2400 => (HOTSPOTS[k as usize % 3], 150.0),
            SpawnMode::Blob if k < 3000 => (CENTER, 200.0),
            SpawnMode::Disk(radius) => (CENTER, radius),
            SpawnMode::Lanes => {
                let lanes: Vec<_> = self.world.lanes().iter().filter(|l| l.a != l.b).collect();
                if let Some(l) = lanes.get((k / 2) as usize % lanes.len().max(1)) {
                    // The anchor is its other end: start and end name the lane.
                    let back = !k.is_multiple_of(2);
                    let (w, e) = (l.waypoint(0, back), l.waypoint(0, !back));
                    // A tunnel's start: on its floor, not the ground over it.
                    self.spawn_floor = (w[2] < self.world.terrain(w[0], w[1]) - 2.0).then_some(w[2]);
                    return ([w[0], w[1]], [e[0], e[1]], 1.0);
                }
                (CENTER, 200.0)
            }
            SpawnMode::Line(spacing) => {
                let p = [(1000.0 + k as f32 * spacing).min(WORLD_SIZE), WORLD_SIZE / 2.0];
                return (p, p, 1.0);
            }
            _ => {
                // A squad spawns and roams together around its first member's anchor.
                let margin = 500.0;
                let fresh = [self.rng.range(margin, WORLD_SIZE - margin), self.rng.range(margin, WORLD_SIZE - margin)];
                let p = if squad == NO_SQUAD { fresh } else { *self.squad_anchor.entry(squad).or_insert(fresh) };
                (p, 400.0)
            }
        };
        (self.rng.in_disk(anchor, radius), anchor, radius)
    }
}

/// The site whose region (`territory::region_of`) each `REGION_CELL`
/// cell's center is in, row by row (empty: a world without sites).
fn regions(w: &World) -> Vec<u16> {
    if w.sites().is_empty() {
        return Vec::new();
    }
    let n = (WORLD_SIZE / REGION_CELL) as usize;
    (0..n * n)
        .map(|c| {
            let p = [((c % n) as f32 + 0.5) * REGION_CELL, ((c / n) as f32 + 0.5) * REGION_CELL];
            territory::region_of(w, p).unwrap_or(0) as u16
        })
        .collect()
}

/// The site whose region `pos` is in, by `regions`' cells.
fn region_at(regions: &[u16], pos: [f32; 2]) -> Option<usize> {
    if regions.is_empty() {
        return None;
    }
    let n = (WORLD_SIZE / REGION_CELL) as usize;
    let c = |v: f32| ((v.max(0.0) / REGION_CELL) as usize).min(n - 1);
    Some(regions[c(pos[1]) * n + c(pos[0])] as usize)
}

/// Everything assembly reads, shared by all shard tasks.
struct View<'a> {
    cfg: &'a InterestConfig,
    /// This tick's shots, sorted by shooter (`SimServer::tick_shots`).
    shots: &'a [(u16, u16, i16, f64)],
    activity: &'a Activity,
    tick: u32,
    step: u32,
    bodies: &'a [Body],
    inputs: &'a [Mutex<InputQueue>],
    near_hist: &'a [Vec<NearQ>],
    near_hist_tick: &'a [u32; NEAR_HISTORY],
    far_blobs: &'a [Blob],
    grid: &'a Grid,
    mid_grid: &'a Grid,
    far_grid: &'a Grid,
    squads: &'a HashMap<u32, Vec<u16>>,
    max_message: usize,
    /// Message bytes per packet, for `PacketFill`.
    packet_body: usize,
    pace: u16,
    level: u8,
    /// Capture this entity's client this tick.
    watch: Option<u16>,
}

impl View<'_> {
    #[inline]
    fn dist2(&self, a: [f32; 2], j: u16) -> f32 {
        let p = self.bodies[j as usize].state.pos;
        (p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2)
    }

    /// Picks this tick's near, mid and far entities for one client (see
    /// `interest.rs`), fits them to the byte budget and appends the messages.
    /// `sc.acked` holds the tags (ticks) of this client's near messages that
    /// were acked since last tick.
    fn assemble(&self, slot: &mut ClientSlot, sc: &mut Scratch, snaps: &mut Vec<Snap>) {
        let (cfg, tick, e) = (self.cfg, self.tick, slot.entity);
        let me = self.bodies[e as usize];
        let fresh = !slot.near.started();

        // Acked near messages: the states they carried become baselines.
        sc.ack_pairs.clear();
        for &t in &sc.acked {
            let (sent_tick, entities) = &slot.sent_ring[t as usize % SENT_RING];
            if *sent_tick == t {
                sc.ack_pairs.extend(entities.iter().map(|&j| (j, t)));
            }
        }
        slot.near.apply_acks(&sc.ack_pairs, &mut sc.select);
        sc.epoch = sc.epoch.wrapping_add(1);
        if sc.epoch == 0 {
            sc.stamp.fill(0);
            sc.epoch = 1;
        }
        let epoch = sc.epoch;
        sc.stamp[e as usize] = epoch;

        // Near: distance or interaction (squad), ranked by the accumulator. The
        // scan only computes squared distances; priorities and seeded ages are
        // computed for the <= near_candidates that make the cut.
        let (mid_radius, far_radius) = (cfg.mid_radius * slot.ladder.mid_scale(), cfg.far_radius * slot.ladder.far_scale());
        let (r_mid2, r_far2) = (mid_radius.powi(2), far_radius.powi(2));
        // Non-squad candidates: the exact k nearest (see `Grid::knn`), so a
        // dense crowd costs a few hundred candidates instead of everyone in it.
        let k = cfg.near_candidates;
        let (bodies, my_squad) = (self.bodies, me.squad);
        // A cloaked enemy (U6) past `CLOAK_RANGE` isn't sent at all: what a
        // client never receives, it can't show through a wall.
        let side = faction(e);
        let hidden = |j: u32| {
            let b = &bodies[j as usize];
            b.ability && b.kit == class::RECON && faction(j as u16) != side && {
                let (dx, dy, dz) = (b.state.pos[0] - me.state.pos[0], b.state.pos[1] - me.state.pos[1], b.state.z - me.state.z);
                dx * dx + dy * dy + dz * dz > ability::CLOAK_RANGE * ability::CLOAK_RANGE
            }
        };
        self.grid.knn(
            me.state.pos,
            cfg.near_radius,
            k,
            |j| j != e as u32 && (my_squad == NO_SQUAD || bodies[j as usize].squad != my_squad) && !hidden(j),
            &mut sc.knn,
        );
        sc.tally.near_scanned += sc.knn.scanned;
        if k > 0 && sc.knn.keys().len() >= k {
            sc.tally.near_capped += 1;
        }
        sc.near.clear();
        for &key in sc.knn.keys() {
            let (j, d2) = Knn::split(key);
            let d = d2.sqrt();
            sc.near.push(NearCandidate {
                entity: j as u16,
                base: near_base(d, false),
                seed_age: interest::seed_age(j as u16, tick, d, cfg, fresh),
            });
            sc.stamp[j as usize] = epoch;
        }
        // Squadmates are near-tier at any distance.
        if let Some(members) = self.squads.get(&me.squad) {
            for &j in members {
                if j != e && !self.bodies[j as usize].away {
                    let d = self.dist2(me.state.pos, j).sqrt();
                    sc.near.push(NearCandidate {
                        entity: j,
                        base: near_base(d, true),
                        seed_age: interest::seed_age(j, tick, d, cfg, fresh),
                    });
                    sc.stamp[j as usize] = epoch;
                }
            }
        }
        // Combat contacts (who it hit, who hit it) too, for a few seconds.
        for &(j, until) in &me.contacts {
            if j != NO_CONTACT && until > tick && j != e && self.bodies[j as usize].alive && !self.bodies[j as usize].away && sc.stamp[j as usize] != epoch && !hidden(j as u32) {
                let d = self.dist2(me.state.pos, j).sqrt();
                sc.near.push(NearCandidate { entity: j, base: near_base(d, true), seed_age: interest::seed_age(j, tick, d, cfg, fresh) });
                sc.stamp[j as usize] = epoch;
            }
        }
        sc.picked.clear();
        slot.near.select(&sc.near, tick, cfg.near_per_tick, &mut sc.select, &mut sc.picked);

        // Mid: due this tick, not near-tier, the nearest first.
        let stamp = &sc.stamp;
        self.mid_grid.knn(me.state.pos, mid_radius, cfg.mid_per_tick, |j| stamp[j as usize] != epoch && !hidden(j), &mut sc.knn);
        sc.tally.mid_scanned += sc.knn.scanned;
        sc.mid.clear();
        for &key in sc.knn.keys() {
            let (j, d2) = Knn::split(key);
            sc.mid.push((d2, j as u16));
            sc.stamp[j as usize] = epoch;
        }

        // Far: last tick's carries first (key -1), then this tick's due ones.
        sc.far.clear();
        let carried = std::mem::take(&mut slot.far_carry);
        for &j in &carried {
            let d2 = self.dist2(me.state.pos, j);
            if self.bodies[j as usize].alive && sc.stamp[j as usize] != epoch && d2 > r_mid2 && d2 <= r_far2 && !hidden(j as u32) {
                sc.far.push((-1.0, j));
                sc.stamp[j as usize] = epoch;
            }
        }
        self.far_grid.for_each_near(me.state.pos, far_radius, |j| {
            if sc.stamp[j as usize] != epoch && !hidden(j) {
                let d2 = self.dist2(me.state.pos, j as u16);
                if d2 > r_mid2 && d2 <= r_far2 {
                    sc.far.push((d2, j as u16));
                }
            }
        });
        nearest(&mut sc.far, cfg.far_per_tick.max(carried.len()));
        slot.far_carry = carried; // reuse the allocation
        slot.far_carry.clear();

        // Near message: each entity as a delta against its acked baseline if
        // the server still has that state (and it's this entity's, not a
        // previous occupant's of the slot), else in full.
        sc.near_entries.clear();
        let (mut deltas, mut full) = (0, 0);
        let now = &self.near_hist[tick as usize % NEAR_HISTORY];
        // Sorting these small pairs is cheaper than sorting the entries later
        // (the encoder's own sort then finds them in order).
        sc.picked.sort_unstable_by_key(|p| p.0);
        for &(j, b) in &sc.picked {
            let state = now[j as usize];
            let bs = b as usize % NEAR_HISTORY;
            let usable = b > 0 && self.near_hist_tick[bs] == b && b >= self.bodies[j as usize].spawned && tick - b <= MAX_BASE_AGE;
            let base = usable.then(|| ((tick - b) as u8, self.near_hist[bs][j as usize]));
            if usable {
                deltas += 1;
            } else {
                full += 1;
            }
            sc.near_entries.push(NearEntry { entity: j, state, base });
        }
        let near_msg = (!sc.near_entries.is_empty()).then(|| {
            let mut w = Writer::with_capacity(delta::NEAR_HEADER + sc.near_entries.len() * 8);
            delta::encode_near(tick, &mut sc.near_entries, &mut w);
            w.into_inner()
        });
        let near_bytes = near_msg.as_ref().map_or(0, |m| m.len());
        // Tracers: this tick's shots by its near-tier players, and its own
        // (the cone of fire's pick is the server's: this is where they went).
        let shots_msg = (!self.shots.is_empty()).then(|| {
            let mut seen: Vec<(u16, u16, i16, f64)> = Vec::new();
            for e in sc.near.iter().map(|c| c.entity).chain([slot.entity]) {
                let at = self.shots.partition_point(|s| s.0 < e);
                seen.extend(self.shots[at..].iter().take_while(|s| s.0 == e));
            }
            (!seen.is_empty()).then(|| events::encode_shots(self.step, seen.into_iter()))
        });
        let shots_msg = shots_msg.flatten();
        let shots_bytes = shots_msg.as_ref().map_or(0, |m| m.len());
        sc.tally.tracer_bytes += shots_bytes as u64;
        let (ring_tick, ring) = &mut slot.sent_ring[tick as usize % SENT_RING];
        *ring_tick = tick;
        ring.clear();
        ring.extend(sc.picked.iter().map(|&(j, _)| j));

        // Budget: own state, then near, then mid, then far. What doesn't fit of
        // far is carried; a carried entity that doesn't fit again is starving.
        // Costs follow how the messages will pack (`PacketFill`).
        let mut fill = PacketFill::new(self.packet_body);
        fill.push(SNAPSHOT_LEN);
        if near_bytes > 0 {
            fill.push(near_bytes);
        }
        if shots_bytes > 0 {
            fill.push(shots_bytes);
        }
        let cost = |fill: PacketFill, tier: Tier, n: usize| {
            fill.entities(n, msg::blob_size(tier), msg::blobs_per_message(tier, self.max_message))
        };
        let mut left = cfg.budget_bytes.saturating_sub(SNAPSHOT_LEN + near_bytes + shots_bytes);
        let mut n_mid = sc.mid.len();
        while n_mid > 0 && cost(fill, Tier::Mid, n_mid).0 > left {
            n_mid -= 1;
        }
        sc.tally.mid_truncated += (sc.mid.len() - n_mid) as u64;
        let (mid_cost, after_mid) = cost(fill, Tier::Mid, n_mid);
        left -= mid_cost;
        let mut n_far = sc.far.len();
        while n_far > 0 && cost(after_mid, Tier::Far, n_far).0 > left {
            n_far -= 1;
        }
        let (far_cost, after_far) = cost(after_mid, Tier::Far, n_far);
        left -= far_cost;
        // Distant fights: once per window, on this client's tick of it, the
        // nearest cells that fit what's left.
        sc.activity.clear();
        if tick % activity::WINDOW == e as u32 % activity::WINDOW {
            self.activity.gather(me.state.pos, far_radius, &mut sc.activity);
        }
        let act_per = lattice_game::activity::per_message(self.max_message);
        let act_size = lattice_game::activity::ENTRY;
        let mut n_act = sc.activity.len();
        while n_act > 0 && after_far.entities(n_act, act_size, act_per).0 > left {
            n_act = n_act.saturating_sub(n_act.div_ceil(8).max(1));
        }
        if n_act < sc.activity.len() {
            sc.tally.activity_cut += (sc.activity.len() - n_act) as u64;
            sc.activity.select_nth_unstable_by(n_act, |a, b| a.0.total_cmp(&b.0));
        }
        let mut starved = 0;
        for &(key, j) in &sc.far[n_far..] {
            if key < 0.0 {
                starved += 1;
            } else {
                sc.tally.far_skipped += 1;
                slot.far_carry.push(j);
            }
        }
        sc.tally.far_starved += starved;

        // This client's bandwidth ladder: starving shrinks its mid/far radii.
        slot.ladder.update(starved);
        sc.tally.degraded += (slot.ladder.level() > 0) as u64;

        // Messages.
        let (ack_seq, buffered, wait, ammo) = {
            let q = self.inputs[e as usize].lock().unwrap();
            (q.last_seq, q.depth, q.wait, q.ammo)
        };
        let mut w = Writer::with_capacity(SNAPSHOT_LEN);
        msg::write_snapshot(
            &mut w,
            &SnapshotHeader {
                server_tick: tick,
                step: self.step,
                ack_seq,
                buffered,
                wait,
                pace: self.pace,
                level: self.level,
                client_level: slot.ladder.level(),
                own: me.state,
                pushes: me.pushes,
                health: me.health,
                life: me.life,
                ammo,
                away: me.away,
            },
        );
        let mut bytes = w.len() + near_bytes + shots_bytes;
        snaps.push((slot.client, w.into_inner(), None));
        if let Some(m) = near_msg {
            snaps.push((slot.client, m, Some(tick))); // tagged: its ack sets baselines
        }
        if let Some(m) = shots_msg {
            snaps.push((slot.client, m, None));
        }
        let mid_blobs = sc.mid[..n_mid].iter().map(|&(_, j)| &self.far_blobs[j as usize][..]);
        bytes += self.write_tier(Tier::Mid, mid_blobs, &mut fill, slot.client, snaps);
        let far_blobs = sc.far[..n_far].iter().map(|&(_, j)| &self.far_blobs[j as usize][..]);
        bytes += self.write_tier(Tier::Far, far_blobs, &mut fill, slot.client, snaps);
        let act_bytes = self.write_activity(&sc.activity[..n_act], &mut fill, slot.client, snaps);
        bytes += act_bytes;
        sc.tally.activity_sent += n_act as u64;
        sc.tally.activity_bytes += act_bytes as u64;

        if self.watch == Some(e) {
            let sent: std::collections::HashSet<u16> = sc.picked.iter().map(|p| p.0).collect();
            let near = slot
                .near
                .entries()
                .iter()
                .map(|&(j, last, base)| {
                    let was_sent = sent.contains(&j);
                    let usable = base > 0 && tick - base <= MAX_BASE_AGE && base >= self.bodies[j as usize].spawned;
                    // `last` is now this tick for the ones just sent.
                    let age = if was_sent { 0 } else { tick.saturating_sub(last) };
                    (j, age, was_sent, was_sent && usable)
                })
                .collect();
            sc.watched = Some(WatchedClient {
                client: slot.client,
                entity: e,
                pos: me.state.pos,
                radii: [cfg.near_radius, mid_radius, far_radius],
                client_level: slot.ladder.level(),
                near,
                mid: sc.mid[..n_mid].iter().map(|m| m.1).collect(),
                far: sc.far[..n_far].iter().map(|f| f.1).collect(),
                far_skipped: slot.far_carry.clone(),
                far_starved: starved as u32,
                bytes,
                near_bytes,
            });
        }

        let t = &mut sc.tally;
        t.snapshots += 1;
        t.tier_sent[0] += sc.picked.len() as u64;
        t.near_bytes += near_bytes as u64;
        t.near_deltas += deltas;
        t.near_full += full;
        t.tier_sent[1] += n_mid as u64;
        t.tier_sent[2] += n_far as u64;
        t.bytes += bytes as u64;
    }

    /// Writes distant-fight entries as Activity messages, packed like
    /// `write_tier`'s.
    fn write_activity(&self, entries: &[(f32, lattice_game::activity::Entry)], fill: &mut PacketFill, client: ClientId, snaps: &mut Vec<Snap>) -> usize {
        use lattice_game::activity::{per_message, write_header, ENTRY, HEADER};
        let (per, mut rest, mut bytes) = (per_message(self.max_message), entries, 0);
        while !rest.is_empty() {
            let n = fill.next_chunk(rest.len(), ENTRY, per);
            let mut w = Writer::with_capacity(HEADER + n * ENTRY);
            write_header(&mut w, self.activity.end_step, self.activity.len, n as u8);
            for (_, e) in &rest[..n] {
                w.bytes(e);
            }
            bytes += w.len();
            fill.push(w.len());
            snaps.push((client, w.into_inner(), None));
            rest = &rest[n..];
        }
        bytes
    }

    /// Writes `blobs` as Entities messages of at most one packet each, the
    /// first sized to fill the current packet.
    fn write_tier<'b>(
        &self,
        tier: Tier,
        blobs: impl ExactSizeIterator<Item = &'b [u8]>,
        fill: &mut PacketFill,
        client: ClientId,
        snaps: &mut Vec<Snap>,
    ) -> usize {
        let (per, total, size) = (msg::blobs_per_message(tier, self.max_message), blobs.len(), msg::blob_size(tier));
        let mut blobs = blobs;
        let (mut left, mut bytes) = (total, 0);
        while left > 0 {
            let n = fill.next_chunk(left, size, per);
            let mut w = Writer::with_capacity(msg::ENTITIES_HEADER + n * size);
            msg::write_entities_header(&mut w, self.tick, tier, n as u8);
            for b in blobs.by_ref().take(n) {
                w.bytes(b);
            }
            bytes += w.len();
            fill.push(w.len());
            snaps.push((client, w.into_inner(), None));
            left -= n;
        }
        bytes
    }
}

/// Keeps the `k` smallest keys (unordered), in place.
fn nearest(v: &mut Vec<(f32, u16)>, k: usize) {
    if v.len() > k {
        v.select_nth_unstable_by(k, |a, b| a.0.total_cmp(&b.0));
        v.truncate(k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separation_pushes_bodies_that_overlap_in_height_too() {
        let push = |dz: f32, dx: f32| separation(0, [100.0, 100.0], 10.0, 1, [100.0 + dx, 100.0], 10.0 + dz);
        // Side by side on the ground: pushed apart, away from the other.
        assert!(push(0.0, 0.5)[0] < 0.0);
        // One on a 1 m crate beside the other: their bodies still overlap.
        assert!(push(1.0, 0.5)[0] < 0.0);
        // One a storey up (a floor, or a tall wall's top): no contact.
        assert_eq!(push(HEIGHT, 0.5), [0.0; 2]);
        assert_eq!(push(-3.0, 0.0), [0.0; 2]);
        // Not touching across.
        assert_eq!(push(0.0, SEP_DIST), [0.0; 2]);
        // Coincident: opposite directions for each of the pair.
        let (a, b) = (separation(3, [5.0, 5.0], 0.0, 9, [5.0, 5.0], 0.0), separation(9, [5.0, 5.0], 0.0, 3, [5.0, 5.0], 0.0));
        assert!((a[0] + b[0]).abs() < 1e-6 && (a[1] + b[1]).abs() < 1e-6 && a != [0.0; 2]);
    }

    fn tw() -> Arc<World> {
        World::shared(1)
    }

    fn fwd() -> Input {
        Input { move_x: 127, yaw: 777, ..Default::default() }
    }

    #[test]
    fn spawn_modes_parse_and_disks_stay_inside() {
        assert!(matches!("disk:25".parse(), Ok(SpawnMode::Disk(r)) if r == 25.0));
        assert!(matches!("line:50".parse(), Ok(SpawnMode::Line(s)) if s == 50.0));
        for bad in ["disk:", "disk:0", "disk:x", "ring:5"] {
            assert!(bad.parse::<SpawnMode>().is_err(), "{bad}");
        }
        let mut sim = SimServer::new(SimConfig { spawn: SpawnMode::Disk(25.0), ..Default::default() }, Instant::now());
        let c = WORLD_SIZE / 2.0;
        for _ in 0..500 {
            let (p, anchor, radius) = sim.pick_spawn(NO_SQUAD);
            assert_eq!((anchor, radius), ([c, c], 25.0));
            assert!((p[0] - c).hypot(p[1] - c) <= 25.0);
            sim.counters.spawns += 1;
        }
    }

    #[test]
    fn wait_runs_from_first_arrival_to_applied() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        q.push(0, 1, fwd(), None, None, ms(0), false);
        q.push(0, 1, fwd(), None, None, ms(20), false); // a redundant copy doesn't reset the clock
        q.push(0, 2, fwd(), None, None, ms(20), false);
        assert_eq!(q.advance(0, &mut b, ms(33), 1, &tw(), &[]), Step::Applied);
        assert_eq!(q.wait, 330, "33 ms in 0.1 ms units");
        assert_eq!(q.advance(0, &mut b, ms(66), 1, &tw(), &[]), Step::Applied);
        assert_eq!(q.wait, 460);
        assert_eq!(q.advance(0, &mut b, ms(99), 1, &tw(), &[]), Step::Repeated);
        assert_eq!(q.wait, WAIT_STAND_IN);
    }

    #[test]
    fn input_queue_orders_and_dedups() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };

        assert_eq!(q.advance(0, &mut b, t, 1, &tw(), &[]), Step::Waiting, "no input yet consumes nothing");
        assert_eq!(q.last_seq, 0);
        assert_eq!(q.push(0, 2, fwd(), None, None, t, false), Push::Queued);
        assert_eq!(q.push(0, 1, fwd(), None, None, t, false), Push::Queued);
        assert_eq!(q.push(0, 2, fwd(), None, None, t, false), Push::Duplicate);
        assert_eq!(q.advance(0, &mut b, t, 1, &tw(), &[]), Step::Applied);
        assert_eq!((q.last_seq, q.depth), (1, 2));
        assert_eq!(q.push(0, 1, fwd(), None, None, t, false), Push::Duplicate, "already applied");
        assert_eq!(q.advance(0, &mut b, t, 1, &tw(), &[]), Step::Applied);
        assert_eq!(q.last_seq, 2);

        // A backlog drains one per tick; overflow discards the oldest unapplied.
        for s in 3..=3 + MAX_QUEUED_INPUTS as u32 {
            q.push(0, s, fwd(), None, None, t, false);
        }
        assert_eq!(q.pending.len(), MAX_QUEUED_INPUTS);
        assert_eq!(q.last_seq, 3, "seq 3 was discarded");
        assert_eq!(q.advance(0, &mut b, t, 1, &tw(), &[]), Step::Applied);
        assert_eq!(q.last_seq, 4);
    }

    #[test]
    fn shots_fire_once_at_the_rifles_rate_and_never_from_stand_ins() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        let shot = |frac| Some((Shot { frac, yaw: 0, pitch: 0, render: 0 }, None));
        let w = tw();
        // seq 1 fires; its redundant copy doesn't fire again.
        q.push(0, 1, fwd(), None, shot(128), t, false);
        q.advance(0, &mut b, t, 1, &w, &[]);
        q.push(0, 1, fwd(), None, shot(128), t, false);
        assert_eq!((q.fires.len(), q.refused), (1, 0));
        let f = q.fires[0];
        assert_eq!(f.tau0, 0.5, "half way through the step to 1");
        // Seq 2 is too soon (one step later); seq 4 (three) is fine.
        for seq in 2..=4 {
            q.push(0, seq, fwd(), None, shot(128), t, false);
            q.advance(0, &mut b, t, seq, &w, &[]);
        }
        assert_eq!((q.fires.len(), q.refused), (2, 2), "seqs 2 and 3 too soon");
        // Seq 5 never arrives: the stand-in repeats its movement, not a shot.
        assert_eq!(q.advance(0, &mut b, t, 5, &w, &[]), Step::Repeated);
        assert_eq!(q.fires.len(), 2);
        // ...and when seq 5 turns up late, its shot still fires (counted late),
        // from where the stand-in left the shooter.
        q.advance(0, &mut b, t, 6, &w, &[]); // seq 6: another stand-in (too soon anyway)
        q.advance(0, &mut b, t, 7, &w, &[]);
        // (frac 200: three steps after seq 4's shot at frac 128)
        q.push(0, 7, fwd(), None, shot(200), t, false);
        assert_eq!(q.push(0, 7, fwd(), None, shot(200), t, false), Push::Duplicate, "counted once");
        assert_eq!(q.fires.len(), 3);
        assert!(q.fires[2].late);
        let (_, _, before, after) = q.steps[7 % STEP_RING];
        assert_eq!(q.fires[2].origin, weapon::muzzle(&before, &after, 200), "from where the stand-in moved it");
        // A shot whose seq is more than 8 steps gone, or from the dead, is refused.
        for seq in 8..=20 {
            q.advance(0, &mut b, t, seq, &w, &[]);
        }
        q.push(0, 10, fwd(), None, shot(0), t, false);
        b.health = 0;
        q.push(0, 21, fwd(), None, shot(0), t, false);
        q.advance(0, &mut b, t, 21, &w, &[]);
        assert_eq!((q.fires.len(), q.refused), (3, 4));
    }

    #[test]
    fn starvation_repeats_then_freezes_and_drops_late_inputs() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        q.push(0, 1, fwd(), None, None, t, false);
        assert_eq!(q.advance(0, &mut b, t, 1, &tw(), &[]), Step::Applied);

        // Lag switch: nothing arrives for 10 ticks.
        let kinds: Vec<Step> = (0..10).map(|_| q.advance(0, &mut b, t, 1, &tw(), &[])).collect();
        assert_eq!(&kinds[..2], &[Step::Repeated; 2]);
        assert!(kinds[2..].iter().all(|&k| k == Step::Frozen));
        assert_eq!(q.last_seq, 11, "every stand-in consumes a seq");
        assert_eq!(b.yaw, 777, "frozen keeps facing");
        let frozen_at = b.state.pos;

        // The held-back burst arrives: all of it is too late to move anyone.
        for s in 2..=11 {
            assert_eq!(q.push(0, s, fwd(), None, None, t, false), Push::Late);
            assert_eq!(q.push(0, s, fwd(), None, None, t, false), Push::Duplicate, "a late seq counts once");
        }
        assert!(q.pending.is_empty());
        for _ in 0..30 {
            q.advance(0, &mut b, t, 1, &tw(), &[]);
        }
        assert!(b.state.vel == [0.0, 0.0] && b.state.pos[0] - frozen_at[0] < 0.5, "{:?}", b.state);

        // Fresh input for the next seq resumes movement and resets the grace.
        assert_eq!(q.push(0, q.last_seq + 1, fwd(), None, None, t, false), Push::Queued);
        assert_eq!(q.advance(0, &mut b, t, 1, &tw(), &[]), Step::Applied);
        assert_eq!(q.starved_run, 0);
        assert!(b.state.vel[0] > 0.0);
    }
}
