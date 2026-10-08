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
use crate::movement::{self, step, Input, MoveState, BUTTON_ADS, HEIGHT, RADIUS, TICK_HZ, WORLD_SIZE};
use crate::shots::{self, ClockRate, Cut, Fire, FlyStats, History, Outcome, Projectile, RenderFloor, Sky, SpreadKey};
use lattice_game::weapon::{self, shot_time, Bloom, Shot, DAMAGE_BODY, DAMAGE_HEAD, FIRE_STEPS};
use lattice_game::msg::MID_LAG_UNITS;
use lattice_game::events::{self, Event};
use lattice_game::faction::{faction, FACTIONS, MAX_HEALTH, RESPAWN_STEPS};
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
/// Starved ticks that repeat the last input before movement freezes.
pub const GRACE_TICKS: u32 = 2;
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
}

impl std::str::FromStr for SpawnMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "uniform" => Ok(Self::Uniform),
            "hotspots" => Ok(Self::Hotspots),
            "blob" => Ok(Self::Blob),
            _ => {
                let arg = |prefix: &str| s.strip_prefix(prefix).and_then(|v| v.parse::<f32>().ok()).filter(|v| *v > 0.0);
                match (arg("line:"), arg("disk:")) {
                    (Some(spacing), _) => Ok(Self::Line(spacing)),
                    (_, Some(radius)) => Ok(Self::Disk(radius)),
                    _ => Err(format!("unknown spawn mode {s:?} (uniform|hotspots|blob|line:<meters>|disk:<meters>)")),
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
    pub world_seed: u64,
    /// Push overlapping players apart (soft separation). Off only for comparisons.
    pub separation: bool,
    /// Test aid until there are weapons: kill this many random living
    /// players a second (they respawn after `RESPAWN_STEPS`).
    pub deaths_per_sec: f32,
    /// Measurement aid: hits land (and are confirmed) but deal no damage, so
    /// hit rates measure aim and lag compensation, not who died first.
    pub immortal: bool,
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
            world_seed: 1,
            separation: true,
            deaths_per_sec: 0.0,
            immortal: false,
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
    pub spawns: u64,
    pub despawns: u64,
    pub deaths: u64,
    pub respawns: u64,
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
    /// Players it hit or was hit by recently: near-tier for each other until
    /// the tick given ("interaction"). `NO_CONTACT` is empty.
    contacts: [(u16, u32); CONTACTS],
}

impl Default for Body {
    fn default() -> Self {
        Self {
            alive: false,
            state: MoveState::default(),
            yaw: 0,
            pitch: 0,
            ads: false,
            pushes: 0,
            squad: NO_SQUAD,
            spawned: 0,
            health: MAX_HEALTH,
            respawn_at: None,
            life: 0,
            anchor: [0.0; 2],
            radius: 0.0,
            invulnerable: false,
            contacts: [(NO_CONTACT, 0); CONTACTS],
        }
    }
}

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
            spread: None,
            claims: RenderFloor::default(),
            batch: Vec::new(),
            inputs_seen: 0,
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
                    shot.map(|s| {
                        let near = place(s.render);
                        let h = self.claims.shot(seq, [near, near - lag], before);
                        (h, (near - h[0]).abs().max((near - lag - h[1]).abs()))
                    })
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
                Push::Late => n[0] += 1,
                Push::Discarded => n[1] += 1,
                Push::Queued | Push::Duplicate => {}
            }
        }
        self.batch = batch;
        n[2] += ok.is_err() as u64;
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
    /// soon after the last shot (or is a copy of one). It leaves somewhere in
    /// its cone of fire: aiming down sights (`ads`) or not, from the state
    /// before the seq, with the shooter's bloom. Its claimed view is the one
    /// the render floor `held` it to, if it did.
    #[allow(clippy::too_many_arguments)]
    fn fire(&mut self, e: u16, seq: u32, shot: Shot, held: Option<Held>, render: Option<RenderTime>, late: bool, ads: bool) {
        let time = shot_time(seq, shot.frac);
        if self.last_shot.is_some_and(|last| time < last + FIRE_STEPS as u64 * 256) {
            self.refused += 1;
            return;
        }
        let Some(&(_, step_no, before, after)) = self.steps.iter().find(|s| s.0 == seq) else {
            self.refused += 1;
            return;
        };
        self.last_shot = Some(time);
        let cone = if self.cone { self.bloom.fire(time, ads, &before) } else { 0.0 };
        let pick = self.spread.as_ref().map_or(0, |(key, spawn)| key.pick(*spawn, seq));
        let dir = weapon::spread(shot.yaw, shot.pitch, cone, pick);
        let (yaw, pitch) = weapon::angles(dir);
        let tau0 = step_no as f64 - 1.0 + shot.frac as f64 / 256.0;
        let mid_lag = render.map_or(0.0, |r| r.mid_lag as f64 / MID_LAG_UNITS);
        let near = msg::render_age(step_no, shot.render) - (1.0 - shot.frac as f64 / 256.0);
        let (behind, held) = match held {
            Some((h, moved)) => ([tau0 - h[0], tau0 - h[1]], moved),
            None => ([near, near + mid_lag], 0.0),
        };
        self.fires.push(Fire {
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
            settling: self.inputs_seen < SETTLE_INPUTS,
        });
    }

    fn consume(&mut self, seq: u32, stand_in: bool) {
        let shift = seq - self.last_seq;
        self.stand_ins = if shift >= 32 { 0 } else { self.stand_ins << shift } | stand_in as u32;
        self.last_seq = seq;
    }

    /// Advance one movement step, consuming seq `last_seq + 1`. `now` is the
    /// tick's time, `step` the game step the result is at.
    fn advance(&mut self, e: u16, body: &mut Body, now: Instant, step_no: u32, world: &World) -> Step {
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
            body.state = step(world, body.state, Input::default()); // (dead or not, it stands)
            return Step::Waiting;
        } else {
            // The input for `next` is late or lost: a stand-in takes its seq, and
            // the real one is dropped if it shows up. Freezing after the grace
            // means holding packets back (a lag switch) buys no movement.
            self.starved_run += 1;
            self.wait = WAIT_STAND_IN;
            if self.starved_run <= GRACE_TICKS {
                (self.last, Step::Repeated)
            } else {
                (Input { yaw: self.last.yaw, ..Default::default() }, Step::Frozen)
            }
        };
        let before = body.state;
        body.state = step(world, body.state, if body.dead() { movement::dead_input(input) } else { input });
        self.steps[next as usize % STEP_RING] = (next, step_no, before, body.state);
        // A body keeps the aim it died with: the dead player's own camera
        // still turns (on its client), but a corpse doesn't spin for others.
        if !body.dead() {
            body.yaw = input.yaw;
            body.pitch = input.pitch;
        }
        body.ads = !body.dead() && input.buttons & BUTTON_ADS != 0;
        self.consume(next, kind != Step::Applied);
        // Only real inputs fire: a stand-in repeats movement, never a shot.
        if let Some(((shot, held), render)) = fired {
            if body.dead() {
                self.refused += 1;
            } else {
                self.fire(e, next, shot, held, render, false, input.buttons & BUTTON_ADS != 0);
            }
        }
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

/// Whether `shooter`'s eye sees `target`'s chest now (no terrain or cover
/// between them).
fn in_sight(world: &World, history: &History, shooter: u16, target: u16) -> bool {
    let (Some(a), Some(b)) = (history.now(shooter), history.now(target)) else { return false };
    let (p0, p1) = ([a.pos[0], a.pos[1], a.pos[2] + weapon::EYE_HEIGHT], [b.pos[0], b.pos[1], b.pos[2] + 1.0]);
    lattice_game::hit::line_clear(world, p0, p1)
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
    /// Damage to apply after this tick's movement: (entity, amount).
    pending_damage: Vec<(u16, u8)>,
    /// Fractional random deaths carried between ticks (`deaths_per_sec`).
    death_acc: f64,
    by_client: HashMap<ClientId, u16>,
    /// Clients grouped by transport shard, with their interest state.
    shard_clients: Vec<Vec<ClientSlot>>,
    scratch: Vec<Scratch>,
    squads: HashMap<u32, Vec<u16>>,
    squad_anchor: HashMap<u32, [f32; 2]>,
    /// Per-shard snapshot buffers, reused every tick.
    snapshots: Vec<Vec<Snap>>,
    /// Per shard: connects (true) and disconnects (false), in the order the
    /// transport reported them.
    lifecycle: Vec<Vec<(ClientId, bool)>>,
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
}

/// Ticks of advertised pace the render floor looks back over (2 s at 30 Hz).
const PACE_TICKS: usize = 60;

impl SimServer {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        assert!(cfg.max_clients <= u16::MAX as usize, "entity ids are u16");
        let mut net =
            Server::with_socket_groups(cfg.net.clone(), &cfg.identity, cfg.max_clients, cfg.shards, cfg.socket_groups, now);
        if cfg.preallocate {
            net.preallocate(cfg.max_clients);
        }
        let (ladder, interest) = (Ladder::new(cfg.ladder.clone()), cfg.interest.clone());
        let spread_secret = cfg.spread_secret;
        Self {
            world: World::shared(cfg.world_seed),
            pushes: Vec::new(),
            net,
            shard_clients: (0..cfg.shards).map(|_| Vec::new()).collect(),
            scratch: (0..cfg.shards).map(|_| Scratch::default()).collect(),
            squads: HashMap::new(),
            squad_anchor: HashMap::new(),
            snapshots: vec![Vec::new(); cfg.shards],
            lifecycle: vec![Vec::new(); cfg.shards],
            input_wait: Histogram::new(INPUT_WAIT_CAP),
            rewind: [Histogram::new(REWIND_CAP_MS), Histogram::new(REWIND_CAP_MS)],
            trim_excess: Histogram::new(200),
            send_delays: [0.0; SEND_DELAY_TICKS],
            rng: Rng::new(cfg.seed),
            cfg,
            tick: 0,
            step: 0,
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
        let judge = Judge { step: self.step, clock, floor: self.cfg.render_floor };
        let (ingress, [late, discarded, bad]) = self
            .net
            .shards_mut()
            .par_iter_mut()
            .zip(inbound.par_iter_mut())
            .zip(self.lifecycle.par_iter_mut())
            .map(|((shard, bucket), lifecycle)| {
                let t0 = Instant::now();
                let mut n = [0u64; 3];
                let mut handle = |ev, arrived, jitter| match ev {
                    ServerEvent::Message { client, channel: Channel::Unreliable, data } => {
                        if let Some(&e) = by_client.get(&client) {
                            let dead = bodies[e as usize].dead();
                            let judge = Judge { clock: ClockRate { jitter, ..judge.clock }, ..judge };
                            inputs[e as usize].lock().unwrap().receive(e, &data, arrived, dead, judge, &mut n);
                        }
                    }
                    ServerEvent::Message { .. } => n[2] += 1,
                    ServerEvent::Connected { client, .. } => lifecycle.push((client, true)),
                    ServerEvent::Disconnected { client, .. } => lifecycle.push((client, false)),
                };
                // How much the sender's link delay varies (its RTTs' range
                // of late), in seconds: the render floor's allowance.
                let jitter = |shard: &lattice_net::Shard, ev: &ServerEvent| match ev {
                    ServerEvent::Message { client, .. } => {
                        shard.client_stats(*client).map_or(0.0, |s| (s.rtt_max_ms - s.rtt_min_ms) as f64 / 1000.0)
                    }
                    _ => 0.0,
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
            for &(client, joined) in &events {
                if joined {
                    self.spawn(client);
                } else {
                    self.despawn(client);
                }
            }
            events.clear();
            self.lifecycle[k] = events; // keep the allocation
        }
        lap(1);

        // 2. respawns due this tick (before its movement: the inputs it
        // applies are the new life's), then movement
        self.respawn_due(self.step + steps);
        let world = &*self.world;
        let base_step = self.step;
        // Movement also gathers the shots fired (here, or by late inputs at
        // ingress) for the shots phase, in entity order.
        let ([applied, repeated, frozen], fires, refused) = self
            .bodies
            .par_iter_mut()
            .zip(self.inputs.par_iter_mut())
            .enumerate()
            .with_min_len(256)
            .fold(
                || ([0u64; 3], Vec::new(), 0u64),
                |(mut n, mut fires, refused), (e, (b, q))| {
                    let q = q.get_mut().unwrap();
                    if b.alive {
                        // A tick consumes `steps` 1/30 s movement steps (1 or 2 at 20 Hz).
                        let queued = q.pending.len();
                        for k in 0..steps {
                            match q.advance(e as u16, b, now, base_step + k + 1, world) {
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
                    (n, fires, refused + std::mem::take(&mut q.refused) as u64)
                },
            )
            .reduce(
                || ([0; 3], Vec::new(), 0),
                |(a, mut fa, ra), (b, mut fb, rb)| {
                    fa.append(&mut fb);
                    ([a[0] + b[0], a[1] + b[1], a[2] + b[2]], fa, ra + rb)
                },
            );
        self.fires = fires;
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
        let alive = || bodies.iter().enumerate().filter(|(_, b)| b.alive).map(|(i, b)| (i as u32, b.state.pos));
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
            let world = &*self.world;
            self.bodies.par_iter_mut().zip(self.pushes.par_iter()).with_min_len(256).for_each(|(b, &push)| {
                if b.alive && push != [0.0; 2] {
                    b.state = movement::nudge(world, b.state, push);
                    b.pushes = b.pushes.wrapping_add(1);
                }
            });
        }
        // 5c. damage and deaths: after movement, so a death shows in this
        // tick's snapshots and the next tick's inputs move nothing.
        self.apply_damage(self.step);
        lap(4);

        // 6. lag-comp history
        let entries = self.bodies.iter().map(|b| shots::HistEntry { pos: [b.state.pos[0], b.state.pos[1], b.state.z], life: b.life, live: b.alive && !b.dead() });
        self.history.record(self.tick, self.step, entries);
        lap(5);

        // 6b. shots: new projectiles, then every projectile flies to now
        self.spans[6] = self.shots_phase();
        self.post_news();
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
                *near = NearQ::new(&b.state, b.yaw, b.pitch, b.health, b.ads);
                let prev = tick.wrapping_sub(1);
                if due(e, tick, mid_period) || due(e, tick, far_period) || due(e, prev, far_period) {
                    *far = msg::encode_blob(e, &b.state, b.yaw, b.pitch, b.health);
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
        self.tick_shots.extend(fires.iter().map(|f| (f.shooter, f.yaw, f.pitch, f.tau0)));
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
            self.activity.add(f.shooter, f.origin, f.yaw);
            if self.fired.len() == 4096 {
                self.fired.pop_front();
            }
            self.fired.push_back((f.shooter, f.seq, f.dir));
            self.projectiles.push(p);
            self.counters.shots += 1;
            self.counters.shots_late += f.late as u64;
            self.counters.rewinds_capped += cut.capped as u64;
            self.counters.rewinds_trimmed += cut.trimmed as u64;
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
        let (world, history) = (&*self.world, &self.history);
        let sky = Sky { world, grid: &self.grid, history, near: &near };
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
                        let after_cover = matches!(outcome, Outcome::Player { target, .. } if !in_sight(world, history, p.shooter, target));
                        ends.push(Ended { id: p.id, index: c * 64 + i, shooter: p.shooter, outcome, at, after_cover });
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
        for Ended { shooter, outcome, at, after_cover, .. } in ended {
            match outcome {
                Outcome::Ground => self.counters.hits_ground += 1,
                Outcome::Cover => self.counters.hits_cover += 1,
                Outcome::Expired => self.counters.expired += 1,
                Outcome::Player { target, head, rewind, life } => {
                    let full = if head { DAMAGE_HEAD } else { DAMAGE_BODY };
                    // The shooter saw it alive, in the life it hit; if it has
                    // died (or respawned) since, no damage.
                    let same_life = self.bodies[target as usize].life == life;
                    let (damage, killed) = match same_life.then(|| self.hurt(target, full)).flatten() {
                        Some(killed) => (full, killed),
                        None => (0, false),
                    };
                    let c = &mut self.counters;
                    (c.hits_head, c.hits_body) = (c.hits_head + head as u64, c.hits_body + !head as u64);
                    c.kills += killed as u64;
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

    /// The outbox as one reliable message per client, bucketed by shard.
    fn post_news(&mut self) {
        self.reliable_out.resize_with(self.shard_count(), Vec::new);
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

    /// Takes `amount` of health from `e` now: whether that killed it (it
    /// respawns `RESPAWN_STEPS` from now), or `None` if it wasn't alive to
    /// take it.
    fn hurt(&mut self, e: u16, amount: u8) -> Option<bool> {
        let step_no = self.step;
        let b = self.bodies.get_mut(e as usize).filter(|b| b.alive && !b.dead())?;
        if b.invulnerable {
            return Some(false);
        }
        b.health = b.health.saturating_sub(amount);
        if b.health > 0 {
            return Some(false);
        }
        b.respawn_at = Some(step_no + RESPAWN_STEPS);
        b.life = b.life.wrapping_add(1);
        self.counters.deaths += 1;
        Some(true)
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
        self.pending_damage.push((entity, amount));
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
                    self.pending_damage.push((e, MAX_HEALTH));
                    break;
                }
            }
        }
        let _ = step_no;
        for (e, amount) in std::mem::take(&mut self.pending_damage) {
            self.hurt(e, amount);
        }
    }

    /// Respawns the dead whose time has come by `step`: back where they were
    /// sent to play, on their faction's side of it (120 degrees apart),
    /// standing, at full health.
    fn respawn_due(&mut self, step_no: u32) {
        for (e, b) in self.bodies.iter_mut().enumerate() {
            if !b.alive || b.respawn_at.is_none_or(|at| at > step_no) {
                continue;
            }
            let side = faction(e as u16) as f32 * std::f32::consts::TAU / FACTIONS as f32;
            let (s, c) = side.sin_cos();
            let center = [b.anchor[0] + c * b.radius * 0.5, b.anchor[1] + s * b.radius * 0.5];
            let p = self.rng.in_disk(center, b.radius * 0.3);
            let p = [p[0].clamp(1.0, WORLD_SIZE - 1.0), p[1].clamp(1.0, WORLD_SIZE - 1.0)];
            b.state = MoveState::standing(&self.world, p);
            (b.health, b.respawn_at, b.life) = (MAX_HEALTH, None, b.life.wrapping_add(1));
            self.counters.respawns += 1;
        }
    }

    fn spawn(&mut self, client: ClientId) {
        let squad = match self.cfg.interest.squad_size {
            0 => NO_SQUAD,
            n => (self.counters.spawns / n as u64) as u32,
        };
        let (spawn, anchor, radius) = self.pick_spawn(squad);
        // Factions take turns: by squad, or by player without squads.
        let side = if squad == NO_SQUAD { self.counters.spawns } else { squad as u64 } % FACTIONS as u64;
        let e = match self.free[side as usize].pop() {
            Some(e) => e,
            None => loop {
                // New ids go to their faction's free list until one is ours.
                let id = self.bodies.len() as u16;
                self.bodies.push(Body::default());
                self.pushes.push([0.0; 2]);
                self.inputs.push(Mutex::new(InputQueue { cone: self.cfg.cone_of_fire, ..InputQueue::default() }));
                self.far_blobs.push([0; FAR_BLOB]);
                if faction(id) as u64 == side {
                    break id;
                }
                self.free[faction(id) as usize].insert(0, id);
            },
        };
        let i = e as usize;
        self.bodies[i] = Body {
            alive: true,
            state: MoveState::standing(&self.world, spawn),
            squad,
            spawned: self.tick,
            anchor,
            radius,
            invulnerable: self.cfg.immortal,
            ..Body::default()
        };
        let spread = Some((Arc::clone(&self.spread), self.counters.spawns));
        self.inputs[i] = Mutex::new(InputQueue { cone: self.cfg.cone_of_fire, spread, ..InputQueue::default() });
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
        let welcome = msg::encode_welcome(&Welcome { entity: e, spawn, anchor, radius, world_seed: self.cfg.world_seed });
        let _ = self.net.send(client, Channel::Reliable, welcome);
    }

    fn despawn(&mut self, client: ClientId) {
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

    /// Returns (spawn point, wander anchor, wander radius).
    fn pick_spawn(&mut self, squad: u32) -> ([f32; 2], [f32; 2], f32) {
        const HOTSPOTS: [[f32; 2]; 3] = [[2048.0, 2048.0], [6144.0, 2048.0], [4096.0, 6144.0]];
        const CENTER: [f32; 2] = [WORLD_SIZE / 2.0, WORLD_SIZE / 2.0];
        let k = self.counters.spawns;
        let (anchor, radius) = match self.cfg.spawn {
            SpawnMode::Hotspots if k < 2400 => (HOTSPOTS[k as usize % 3], 150.0),
            SpawnMode::Blob if k < 3000 => (CENTER, 200.0),
            SpawnMode::Disk(radius) => (CENTER, radius),
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
        self.grid.knn(
            me.state.pos,
            cfg.near_radius,
            k,
            |j| j != e as u32 && (my_squad == NO_SQUAD || bodies[j as usize].squad != my_squad),
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
                if j != e {
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
            if j != NO_CONTACT && until > tick && j != e && self.bodies[j as usize].alive && sc.stamp[j as usize] != epoch {
                let d = self.dist2(me.state.pos, j).sqrt();
                sc.near.push(NearCandidate { entity: j, base: near_base(d, true), seed_age: interest::seed_age(j, tick, d, cfg, fresh) });
                sc.stamp[j as usize] = epoch;
            }
        }
        sc.picked.clear();
        slot.near.select(&sc.near, tick, cfg.near_per_tick, &mut sc.select, &mut sc.picked);

        // Mid: due this tick, not near-tier, the nearest first.
        let stamp = &sc.stamp;
        self.mid_grid.knn(me.state.pos, mid_radius, cfg.mid_per_tick, |j| stamp[j as usize] != epoch, &mut sc.knn);
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
            if self.bodies[j as usize].alive && sc.stamp[j as usize] != epoch && d2 > r_mid2 && d2 <= r_far2 {
                sc.far.push((-1.0, j));
                sc.stamp[j as usize] = epoch;
            }
        }
        self.far_grid.for_each_near(me.state.pos, far_radius, |j| {
            if sc.stamp[j as usize] != epoch {
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
        // Tracers: this tick's shots by its near-tier players.
        let shots_msg = (!self.shots.is_empty()).then(|| {
            let mut seen: Vec<(u16, u16, i16, f64)> = Vec::new();
            for c in &sc.near {
                let at = self.shots.partition_point(|s| s.0 < c.entity);
                seen.extend(self.shots[at..].iter().take_while(|s| s.0 == c.entity));
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
        let (ack_seq, buffered, wait) = {
            let q = self.inputs[e as usize].lock().unwrap();
            (q.last_seq, q.depth, q.wait)
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
        assert_eq!(q.advance(0, &mut b, ms(33), 1, &tw()), Step::Applied);
        assert_eq!(q.wait, 330, "33 ms in 0.1 ms units");
        assert_eq!(q.advance(0, &mut b, ms(66), 1, &tw()), Step::Applied);
        assert_eq!(q.wait, 460);
        assert_eq!(q.advance(0, &mut b, ms(99), 1, &tw()), Step::Repeated);
        assert_eq!(q.wait, WAIT_STAND_IN);
    }

    #[test]
    fn input_queue_orders_and_dedups() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };

        assert_eq!(q.advance(0, &mut b, t, 1, &tw()), Step::Waiting, "no input yet consumes nothing");
        assert_eq!(q.last_seq, 0);
        assert_eq!(q.push(0, 2, fwd(), None, None, t, false), Push::Queued);
        assert_eq!(q.push(0, 1, fwd(), None, None, t, false), Push::Queued);
        assert_eq!(q.push(0, 2, fwd(), None, None, t, false), Push::Duplicate);
        assert_eq!(q.advance(0, &mut b, t, 1, &tw()), Step::Applied);
        assert_eq!((q.last_seq, q.depth), (1, 2));
        assert_eq!(q.push(0, 1, fwd(), None, None, t, false), Push::Duplicate, "already applied");
        assert_eq!(q.advance(0, &mut b, t, 1, &tw()), Step::Applied);
        assert_eq!(q.last_seq, 2);

        // A backlog drains one per tick; overflow discards the oldest unapplied.
        for s in 3..=3 + MAX_QUEUED_INPUTS as u32 {
            q.push(0, s, fwd(), None, None, t, false);
        }
        assert_eq!(q.pending.len(), MAX_QUEUED_INPUTS);
        assert_eq!(q.last_seq, 3, "seq 3 was discarded");
        assert_eq!(q.advance(0, &mut b, t, 1, &tw()), Step::Applied);
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
        q.advance(0, &mut b, t, 1, &w);
        q.push(0, 1, fwd(), None, shot(128), t, false);
        assert_eq!((q.fires.len(), q.refused), (1, 0));
        let f = q.fires[0];
        assert_eq!(f.tau0, 0.5, "half way through the step to 1");
        // Seq 2 is too soon (one step later); seq 4 (three) is fine.
        for seq in 2..=4 {
            q.push(0, seq, fwd(), None, shot(128), t, false);
            q.advance(0, &mut b, t, seq, &w);
        }
        assert_eq!((q.fires.len(), q.refused), (2, 2), "seqs 2 and 3 too soon");
        // Seq 5 never arrives: the stand-in repeats its movement, not a shot.
        assert_eq!(q.advance(0, &mut b, t, 5, &w), Step::Repeated);
        assert_eq!(q.fires.len(), 2);
        // ...and when seq 5 turns up late, its shot still fires (counted late),
        // from where the stand-in left the shooter.
        q.advance(0, &mut b, t, 6, &w); // seq 6: another stand-in (too soon anyway)
        q.advance(0, &mut b, t, 7, &w);
        // (frac 200: three steps after seq 4's shot at frac 128)
        q.push(0, 7, fwd(), None, shot(200), t, false);
        assert_eq!(q.push(0, 7, fwd(), None, shot(200), t, false), Push::Duplicate, "counted once");
        assert_eq!(q.fires.len(), 3);
        assert!(q.fires[2].late);
        let (_, _, before, after) = q.steps[7 % STEP_RING];
        assert_eq!(q.fires[2].origin, weapon::muzzle(&before, &after, 200), "from where the stand-in moved it");
        // A shot whose seq is more than 8 steps gone, or from the dead, is refused.
        for seq in 8..=20 {
            q.advance(0, &mut b, t, seq, &w);
        }
        q.push(0, 10, fwd(), None, shot(0), t, false);
        b.health = 0;
        q.push(0, 21, fwd(), None, shot(0), t, false);
        q.advance(0, &mut b, t, 21, &w);
        assert_eq!((q.fires.len(), q.refused), (3, 4));
    }

    #[test]
    fn starvation_repeats_then_freezes_and_drops_late_inputs() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        q.push(0, 1, fwd(), None, None, t, false);
        assert_eq!(q.advance(0, &mut b, t, 1, &tw()), Step::Applied);

        // Lag switch: nothing arrives for 10 ticks.
        let kinds: Vec<Step> = (0..10).map(|_| q.advance(0, &mut b, t, 1, &tw())).collect();
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
            q.advance(0, &mut b, t, 1, &tw());
        }
        assert!(b.state.vel == [0.0, 0.0] && b.state.pos[0] - frozen_at[0] < 0.5, "{:?}", b.state);

        // Fresh input for the next seq resumes movement and resets the grace.
        assert_eq!(q.push(0, q.last_seq + 1, fwd(), None, None, t, false), Push::Queued);
        assert_eq!(q.advance(0, &mut b, t, 1, &tw()), Step::Applied);
        assert_eq!(q.starved_run, 0);
        assert!(b.state.vel[0] > 0.0);
    }
}
