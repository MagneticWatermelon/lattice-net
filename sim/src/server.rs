//! The M1 authoritative server: movement only, sans-IO like the transport.
//!
//! `tick()` runs the phase pipeline from CLAUDE.md and times every phase:
//!
//! | phase     | work                                                            |
//! |-----------|-----------------------------------------------------------------|
//! | ingress   | each transport shard decodes its datagrams, acks, handshakes, timeouts (parallel) |
//! | events    | spawn/despawn, queue inputs into per-entity queues (serial) |
//! | movement  | one input seq per entity per tick, real or stand-in (parallel)  |
//! | grid      | rebuild the shared spatial grid, plus the mid/far due-set views |
//! | history   | store positions for lag compensation (unused until M3)          |
//! | serialize | encode each entity once per tier: near blob for all, mid/far blob for due ones (parallel) |
//! | assembly  | per client: pick near/mid/far per `interest.rs`, fit the byte budget, memcpy blobs into messages (parallel by shard) |
//! | transport | each shard queues its clients' snapshots and builds packets (parallel) |
//!
//! Shots and event application (phases 3 and 4) come with M3. Egress (the socket
//! writes) happens in the binary. Datagrams move in per-shard buckets both ways:
//! route inbound ones with `router()`.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_net::wire::Writer;
use lattice_net::{Channel, ClientId, Config, Router, Server, ServerEvent, ServerIdentity};
use rayon::prelude::*;

use crate::grid::{Grid, Knn};
use lattice_game::world::World;
use crate::interest::{self, due, near_base, InterestConfig, NearCandidate, NearState, SelectScratch, Tier};
use crate::ladder::{self, ClientLadder, Ladder, LadderConfig, PaceMeter, Rung, MAX_LEVEL};
use crate::movement::{self, step, Input, MoveState, HEIGHT, RADIUS, TICK_HZ, WORLD_SIZE};
use crate::delta::{self, NearEntry, NearQ, MAX_BASE_AGE, NEAR_HISTORY};
use crate::msg::{self, Blob, PacketFill, RenderTime, SnapshotHeader, Welcome, FAR_BLOB, SNAPSHOT_LEN, WAIT_STAND_IN};
use crate::rng::Rng;
use crate::stats::Histogram;

pub const PHASES: [&str; 9] = ["ingress", "events", "movement", "grid", "separate", "history", "serialize", "assembly", "transport"];
pub type Datagram = (SocketAddr, Vec<u8>);
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

/// Lag-compensation window: 200 ms.
const HISTORY_TICKS: usize = (TICK_HZ as usize) / 5;
/// Starved ticks that repeat the last input before movement freezes.
pub const GRACE_TICKS: u32 = 2;
/// A client can't queue more inputs than this (~0.5 s); beyond it the oldest
/// are discarded unapplied rather than letting latency grow.
const MAX_QUEUED_INPUTS: usize = 16;
const GRID_CELL: f32 = 32.0;
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
    pub inputs_applied: u64,
    /// Entity-ticks where the next input hadn't arrived. Each consumes that seq
    /// with a stand-in: the last input for `GRACE_TICKS`, then a frozen one.
    pub repeated: u64,
    pub frozen: u64,
    /// Inputs whose render time was ahead of the server's step (bogus).
    pub render_ahead: u64,
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
    alive: bool,
    state: MoveState,
    yaw: u16,
    pitch: i16,
    /// Ticks this player was pushed apart from a crowd (wrapping), for the snapshot.
    pushes: u8,
    squad: u32,
    /// Tick this entity (slot) spawned: an older baseline belongs to a previous occupant.
    spawned: u32,
}

impl Default for Body {
    fn default() -> Self {
        Self { alive: false, state: MoveState::default(), yaw: 0, pitch: 0, pushes: 0, squad: NO_SQUAD, spawned: 0 }
    }
}

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
}

/// Per-entity input stream. Every tick consumes exactly one input seq, so each
/// server step matches exactly one client step and replays stay consistent.
#[derive(Default)]
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
    /// Sorted by seq, all > last_seq, each with its render time and first arrival.
    pending: VecDeque<(u32, Input, Option<RenderTime>, Instant)>,
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
    fn push(&mut self, seq: u32, input: Input, render: Option<RenderTime>, arrived: Instant) -> Push {
        if seq <= self.last_seq {
            let age = self.last_seq - seq;
            if age < 32 && self.stand_ins & (1 << age) != 0 {
                self.stand_ins &= !(1 << age); // count each late seq once
                return Push::Late;
            }
            return Push::Duplicate;
        }
        let at = self.pending.partition_point(|&(s, ..)| s < seq);
        if self.pending.get(at).is_some_and(|&(s, ..)| s == seq) {
            return Push::Duplicate; // keep the first arrival
        }
        self.pending.insert(at, (seq, input, render, arrived));
        if self.pending.len() > MAX_QUEUED_INPUTS {
            let (s, ..) = self.pending.pop_front().unwrap();
            self.consume(s, false);
            return Push::Discarded;
        }
        Push::Queued
    }

    fn consume(&mut self, seq: u32, stand_in: bool) {
        let shift = seq - self.last_seq;
        self.stand_ins = if shift >= 32 { 0 } else { self.stand_ins << shift } | stand_in as u32;
        self.last_seq = seq;
    }

    /// Advance one movement step, consuming seq `last_seq + 1`. `now` is the
    /// tick's time, `step` the game step the result is at.
    fn advance(&mut self, body: &mut Body, now: Instant, step_no: u32, world: &World) -> Step {
        self.depth = self.pending.len().min(u8::MAX as usize) as u8;
        self.rewind = None;
        let next = self.last_seq + 1;
        let (input, kind) = if self.pending.front().is_some_and(|&(s, ..)| s == next) {
            let (_, input, render, arrived) = self.pending.pop_front().unwrap();
            self.rewind = render.map(|r| r.ages(step_no));
            let waited = now.saturating_duration_since(arrived).as_micros() / 100;
            self.wait = waited.min(WAIT_STAND_IN as u128 - 1) as u16;
            self.last = input;
            self.starved_run = 0;
            (input, Step::Applied)
        } else if self.last_seq == 0 {
            body.state = step(world, body.state, Input::default());
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
        body.state = step(world, body.state, input);
        body.yaw = input.yaw;
        body.pitch = input.pitch;
        self.consume(next, kind != Step::Applied);
        kind
    }
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
    inputs: Vec<InputQueue>,
    /// Serialized once per tick: every entity's quantized near state, kept
    /// for `NEAR_HISTORY` ticks as delta baselines, and mid/far blobs for the
    /// entities due this tick (and last tick's far-due, for carries). Near
    /// history is tick-major: baselines are mostly 2-4 ticks back, so the
    /// arrays in use stay in cache.
    near_hist: Vec<Vec<NearQ>>,
    near_hist_tick: [u32; NEAR_HISTORY],
    far_blobs: Vec<Blob>,
    history: Vec<Vec<[f32; 2]>>,
    free: Vec<u16>,
    by_client: HashMap<ClientId, u16>,
    /// Clients grouped by transport shard, with their interest state.
    shard_clients: Vec<Vec<ClientSlot>>,
    scratch: Vec<Scratch>,
    squads: HashMap<u32, Vec<u16>>,
    squad_anchor: HashMap<u32, [f32; 2]>,
    /// Per-shard snapshot buffers, reused every tick.
    snapshots: Vec<Vec<Snap>>,
    /// Per-shard transport events, each with the arrival time of the datagram
    /// that caused it.
    shard_events: Vec<Vec<(Instant, ServerEvent)>>,
    /// Input waits (arrival -> applied) since the last `take_input_wait`, in 0.1 ms.
    input_wait: Histogram,
    /// Rewinds (see `InputQueue::rewind`) for near and for mid/far targets
    /// since the last `take_rewind`, in ms of game time.
    rewind: [Histogram; 2],
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
}

impl SimServer {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        assert!(cfg.max_clients <= u16::MAX as usize, "entity ids are u16");
        let mut net =
            Server::with_socket_groups(cfg.net.clone(), &cfg.identity, cfg.max_clients, cfg.shards, cfg.socket_groups, now);
        if cfg.preallocate {
            net.preallocate(cfg.max_clients);
        }
        let (ladder, interest) = (Ladder::new(cfg.ladder.clone()), cfg.interest.clone());
        Self {
            world: World::shared(cfg.world_seed),
            pushes: Vec::new(),
            net,
            shard_clients: (0..cfg.shards).map(|_| Vec::new()).collect(),
            scratch: (0..cfg.shards).map(|_| Scratch::default()).collect(),
            squads: HashMap::new(),
            squad_anchor: HashMap::new(),
            snapshots: vec![Vec::new(); cfg.shards],
            shard_events: (0..cfg.shards).map(|_| Vec::new()).collect(),
            input_wait: Histogram::new(INPUT_WAIT_CAP),
            rewind: [Histogram::new(REWIND_CAP_MS), Histogram::new(REWIND_CAP_MS)],
            rng: Rng::new(cfg.seed),
            cfg,
            tick: 0,
            step: 0,
            bodies: Vec::new(),
            inputs: Vec::new(),
            near_hist: vec![Vec::new(); NEAR_HISTORY],
            near_hist_tick: [u32::MAX; NEAR_HISTORY],
            far_blobs: Vec::new(),
            history: vec![Vec::new(); HISTORY_TICKS],
            free: Vec::new(),
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
        }
    }

    /// Record what `entity`'s client receives, for the debug map (`None` stops).
    /// Captures happen every 6th tick (5 Hz at 30 Hz).
    pub fn set_watch(&mut self, entity: Option<u16>) {
        self.watch = entity;
    }

    /// The latest capture, if one was taken since the last call.
    /// For the last tick's phases split by shard (ingress, assembly, transport):
    /// the longest shard task and the total of all of them. With the phase's
    /// wall time this separates serial work, imbalance and dispatch overhead.
    pub fn tasks(&self) -> &PhaseSpans {
        &self.spans
    }

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
        self.step_acc += rung.steps_per_tick();
        let steps = self.step_acc.floor() as u32;
        self.step_acc -= steps as f64;
        self.counters.level_ticks[self.ladder.level() as usize] += 1;

        // 1. ingress: the per-packet transport work, one task per shard
        // (wall-clock time only matters for connect-token expiry)
        let unix_now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.spans = [NO_SPAN; PHASES.len()];
        self.spans[0] = self
            .net
            .shards_mut()
            .par_iter_mut()
            .zip(inbound.par_iter_mut())
            .zip(self.shard_events.par_iter_mut())
            .map(|((shard, bucket), events)| {
                let t0 = Instant::now();
                for (from, arrived, data) in bucket.drain(..) {
                    shard.receive(from, &data, arrived);
                    while let Some(ev) = shard.poll_event() {
                        events.push((arrived, ev));
                    }
                }
                shard.update(now, unix_now);
                while let Some(ev) = shard.poll_event() {
                    events.push((now, ev));
                }
                span(t0.elapsed())
            })
            .reduce(|| NO_SPAN, join_spans);
        lap(0);

        // 1b. events: these touch the world, so they're applied on one thread
        for k in 0..self.shard_count() {
            let mut events = std::mem::take(&mut self.shard_events[k]);
            for (arrived, ev) in events.drain(..) {
                match ev {
                    ServerEvent::Connected { client, .. } => self.spawn(client),
                    ServerEvent::Disconnected { client, .. } => self.despawn(client),
                    ServerEvent::Message { client, channel: Channel::Unreliable, data } => {
                        self.on_input(client, &data, arrived)
                    }
                    ServerEvent::Message { .. } => self.counters.bad_messages += 1,
                }
            }
            self.shard_events[k] = events; // keep the allocation
        }
        lap(1);

        // 2. movement
        let world = &*self.world;
        let base_step = self.step;
        let [applied, repeated, frozen] = self
            .bodies
            .par_iter_mut()
            .zip(self.inputs.par_iter_mut())
            .with_min_len(256)
            .filter(|(b, _)| b.alive)
            .map(|(b, q)| {
                // A tick consumes `steps` 1/30 s movement steps (1 or 2 at 20 Hz).
                let queued = q.pending.len();
                let mut n = [0u64; 3];
                for k in 0..steps {
                    match q.advance(b, now, base_step + k + 1, world) {
                        Step::Applied => n[0] += 1,
                        Step::Repeated => n[1] += 1,
                        Step::Frozen => n[2] += 1,
                        Step::Waiting => {}
                    }
                }
                // Reported depth means "due now + spares", whatever the step count.
                q.depth = (queued + 1).saturating_sub(steps as usize).min(u8::MAX as usize) as u8;
                n
            })
            .reduce(|| [0u64; 3], |a, b| [a[0] + b[0], a[1] + b[1], a[2] + b[2]]);
        self.step = base_step + steps;
        self.counters.inputs_applied += applied;
        self.counters.repeated += repeated;
        self.counters.frozen += frozen;
        for (b, q) in self.bodies.iter().zip(&self.inputs) {
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
                if !b.alive || !on {
                    return 0;
                }
                let p = b.state.pos;
                grid.for_each_within(p, SEP_DIST, |j, x, y| {
                    if j as usize == i {
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
        lap(4);

        // 6. lag-comp history
        let slot = &mut self.history[self.tick as usize % HISTORY_TICKS];
        slot.clear();
        slot.extend(self.bodies.iter().map(|b| b.state.pos));
        lap(5);

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
                *near = NearQ::new(&b.state, b.yaw, b.pitch);
                let prev = tick.wrapping_sub(1);
                if due(e, tick, mid_period) || due(e, tick, far_period) || due(e, prev, far_period) {
                    *far = msg::encode_blob(e, &b.state, b.yaw, b.pitch);
                }
            });
        lap(6);

        // 8. per-client assembly, grouped by shard so each shard's snapshots
        // are ready for its transport task
        let max_message = self.cfg.net.max_message_size();
        let packet_body = self.cfg.net.packet_body_size();
        let view = View {
            cfg: &self.interest,
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
        self.spans[7] = self
            .shard_clients
            .par_iter_mut()
            .zip(self.snapshots.par_iter_mut())
            .zip(self.scratch.par_iter_mut())
            .zip(self.net.shards_mut().par_iter_mut())
            .map(|(((clients, snaps), scratch), shard)| {
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
                span(t0.elapsed())
            })
            .reduce(|| NO_SPAN, join_spans);
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
            c.mid_scanned += t.mid_scanned;
        }
        lap(7);

        // 8b. transport: queue, frame, ack and checksum, one task per shard
        self.spans[8] = self
            .net
            .shards_mut()
            .par_iter_mut()
            .zip(self.snapshots.par_iter_mut())
            .zip(out.par_iter_mut())
            .map(|((shard, snaps), out)| {
                let t0 = Instant::now();
                for (client, snap, tag) in snaps.drain(..) {
                    let _ = match tag {
                        Some(tag) => shard.send_tagged(client, snap, tag),
                        None => shard.send(client, Channel::Unreliable, snap),
                    };
                }
                shard.flush(now);
                out.extend(shard.drain_outgoing());
                span(t0.elapsed())
            })
            .reduce(|| NO_SPAN, join_spans);
        lap(8);

        self.tick = self.tick.wrapping_add(1);
        self.counters.ticks += 1;
        times
    }

    fn spawn(&mut self, client: ClientId) {
        let squad = match self.cfg.interest.squad_size {
            0 => NO_SQUAD,
            n => (self.counters.spawns / n as u64) as u32,
        };
        let (spawn, anchor, radius) = self.pick_spawn(squad);
        let e = match self.free.pop() {
            Some(e) => e,
            None => {
                self.bodies.push(Body::default());
                self.pushes.push([0.0; 2]);
                self.inputs.push(InputQueue::default());
                self.far_blobs.push([0; FAR_BLOB]);
                (self.bodies.len() - 1) as u16
            }
        };
        let i = e as usize;
        self.bodies[i] =
            Body { alive: true, state: MoveState::standing(&self.world, spawn), yaw: 0, pitch: 0, pushes: 0, squad, spawned: self.tick };
        self.inputs[i] = InputQueue::default();
        if squad != NO_SQUAD {
            self.squads.entry(squad).or_default().push(e);
        }
        self.by_client.insert(client, e);
        let slot = ClientSlot {
            client,
            entity: e,
            near: NearState::default(),
            far_carry: Vec::new(),
            ladder: ClientLadder::default(),
            sent_ring: (0..SENT_RING).map(|_| (u32::MAX, Vec::new())).collect(),
        };
        self.shard_clients[self.net.shard_of_client(client)].push(slot);
        self.counters.spawns += 1;
        let welcome = msg::encode_welcome(&Welcome { entity: e, spawn, anchor, radius, world_seed: self.cfg.world_seed });
        let _ = self.net.send(client, Channel::Reliable, welcome);
    }

    fn despawn(&mut self, client: ClientId) {
        if let Some(e) = self.by_client.remove(&client) {
            let list = &mut self.shard_clients[self.net.shard_of_client(client)];
            if let Some(i) = list.iter().position(|s| s.client == client) {
                list.swap_remove(i);
            }
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
            self.free.push(e);
            self.counters.despawns += 1;
        }
    }

    fn on_input(&mut self, client: ClientId, data: &[u8], arrived: Instant) {
        let Some(&e) = self.by_client.get(&client) else { return };
        let (q, c) = (&mut self.inputs[e as usize], &mut self.counters);
        let ok = msg::decode_inputs(data, |seq, input, render| match q.push(seq, input, render, arrived) {
            Push::Late => c.late_inputs += 1,
            Push::Discarded => c.discarded_inputs += 1,
            Push::Queued | Push::Duplicate => {}
        });
        if ok.is_err() {
            self.counters.bad_messages += 1;
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
    tick: u32,
    step: u32,
    bodies: &'a [Body],
    inputs: &'a [InputQueue],
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
        let cost = |fill: PacketFill, tier: Tier, n: usize| {
            fill.entities(n, msg::blob_size(tier), msg::blobs_per_message(tier, self.max_message))
        };
        let mut left = cfg.budget_bytes.saturating_sub(SNAPSHOT_LEN + near_bytes);
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
        let inp = &self.inputs[e as usize];
        let mut w = Writer::with_capacity(SNAPSHOT_LEN);
        msg::write_snapshot(
            &mut w,
            &SnapshotHeader {
                server_tick: tick,
                step: self.step,
                ack_seq: inp.last_seq,
                buffered: inp.depth,
                wait: inp.wait,
                pace: self.pace,
                level: self.level,
                client_level: slot.ladder.level(),
                own: me.state,
                pushes: me.pushes,
            },
        );
        let mut bytes = w.len() + near_bytes;
        snaps.push((slot.client, w.into_inner(), None));
        if let Some(m) = near_msg {
            snaps.push((slot.client, m, Some(tick))); // tagged: its ack sets baselines
        }
        let mid_blobs = sc.mid[..n_mid].iter().map(|&(_, j)| &self.far_blobs[j as usize][..]);
        bytes += self.write_tier(Tier::Mid, mid_blobs, &mut fill, slot.client, snaps);
        let far_blobs = sc.far[..n_far].iter().map(|&(_, j)| &self.far_blobs[j as usize][..]);
        bytes += self.write_tier(Tier::Far, far_blobs, &mut fill, slot.client, snaps);

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
        q.push(1, fwd(), None, ms(0));
        q.push(1, fwd(), None, ms(20)); // a redundant copy doesn't reset the clock
        q.push(2, fwd(), None, ms(20));
        assert_eq!(q.advance(&mut b, ms(33), 1, &tw()), Step::Applied);
        assert_eq!(q.wait, 330, "33 ms in 0.1 ms units");
        assert_eq!(q.advance(&mut b, ms(66), 1, &tw()), Step::Applied);
        assert_eq!(q.wait, 460);
        assert_eq!(q.advance(&mut b, ms(99), 1, &tw()), Step::Repeated);
        assert_eq!(q.wait, WAIT_STAND_IN);
    }

    #[test]
    fn input_queue_orders_and_dedups() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };

        assert_eq!(q.advance(&mut b, t, 1, &tw()), Step::Waiting, "no input yet consumes nothing");
        assert_eq!(q.last_seq, 0);
        assert_eq!(q.push(2, fwd(), None, t), Push::Queued);
        assert_eq!(q.push(1, fwd(), None, t), Push::Queued);
        assert_eq!(q.push(2, fwd(), None, t), Push::Duplicate);
        assert_eq!(q.advance(&mut b, t, 1, &tw()), Step::Applied);
        assert_eq!((q.last_seq, q.depth), (1, 2));
        assert_eq!(q.push(1, fwd(), None, t), Push::Duplicate, "already applied");
        assert_eq!(q.advance(&mut b, t, 1, &tw()), Step::Applied);
        assert_eq!(q.last_seq, 2);

        // A backlog drains one per tick; overflow discards the oldest unapplied.
        for s in 3..=3 + MAX_QUEUED_INPUTS as u32 {
            q.push(s, fwd(), None, t);
        }
        assert_eq!(q.pending.len(), MAX_QUEUED_INPUTS);
        assert_eq!(q.last_seq, 3, "seq 3 was discarded");
        assert_eq!(q.advance(&mut b, t, 1, &tw()), Step::Applied);
        assert_eq!(q.last_seq, 4);
    }

    #[test]
    fn starvation_repeats_then_freezes_and_drops_late_inputs() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        q.push(1, fwd(), None, t);
        assert_eq!(q.advance(&mut b, t, 1, &tw()), Step::Applied);

        // Lag switch: nothing arrives for 10 ticks.
        let kinds: Vec<Step> = (0..10).map(|_| q.advance(&mut b, t, 1, &tw())).collect();
        assert_eq!(&kinds[..2], &[Step::Repeated; 2]);
        assert!(kinds[2..].iter().all(|&k| k == Step::Frozen));
        assert_eq!(q.last_seq, 11, "every stand-in consumes a seq");
        assert_eq!(b.yaw, 777, "frozen keeps facing");
        let frozen_at = b.state.pos;

        // The held-back burst arrives: all of it is too late to move anyone.
        for s in 2..=11 {
            assert_eq!(q.push(s, fwd(), None, t), Push::Late);
            assert_eq!(q.push(s, fwd(), None, t), Push::Duplicate, "a late seq counts once");
        }
        assert!(q.pending.is_empty());
        for _ in 0..30 {
            q.advance(&mut b, t, 1, &tw());
        }
        assert!(b.state.vel == [0.0, 0.0] && b.state.pos[0] - frozen_at[0] < 0.5, "{:?}", b.state);

        // Fresh input for the next seq resumes movement and resets the grace.
        assert_eq!(q.push(q.last_seq + 1, fwd(), None, t), Push::Queued);
        assert_eq!(q.advance(&mut b, t, 1, &tw()), Step::Applied);
        assert_eq!(q.starved_run, 0);
        assert!(b.state.vel[0] > 0.0);
    }
}
