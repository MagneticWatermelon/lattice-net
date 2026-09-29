//! The M1 authoritative server: movement only, sans-IO like the transport.
//!
//! `tick()` runs the phase pipeline from CLAUDE.md and times every phase:
//!
//! | phase     | work                                                            |
//! |-----------|-----------------------------------------------------------------|
//! | ingress   | each transport shard decodes its datagrams, acks, handshakes, timeouts (parallel) |
//! | events    | spawn/despawn, queue inputs into per-entity queues (serial) |
//! | movement  | one input seq per entity per tick, real or stand-in (parallel)  |
//! | grid      | rebuild the shared spatial grid                                 |
//! | history   | store positions for lag compensation (unused until M3)          |
//! | serialize | encode each entity once into an 11-byte blob (parallel)         |
//! | assembly  | per client: K nearest via grid, memcpy blobs into a snapshot (parallel by shard) |
//! | transport | each shard queues its clients' snapshots and builds packets (parallel) |
//!
//! Shots and event application (phases 3 and 4) come with M3. Egress (the socket
//! writes) happens in the binary. Datagrams move in per-shard buckets both ways:
//! route inbound ones with `router()`.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lattice_net::wire::Writer;
use lattice_net::{Channel, ClientId, Config, Router, Server, ServerEvent};
use rayon::prelude::*;

use crate::grid::Grid;
use crate::movement::{step, Input, MoveState, TICK_HZ, WORLD_SIZE};
use crate::msg::{self, Blob, SnapshotHeader, Welcome, ENTITY_BLOB, SNAPSHOT_HEADER, WAIT_STAND_IN};
use crate::rng::Rng;
use crate::stats::Histogram;

pub const PHASES: [&str; 8] = ["ingress", "events", "movement", "grid", "history", "serialize", "assembly", "transport"];
pub type Datagram = (SocketAddr, Vec<u8>);
/// An inbound datagram with its arrival time, as the receive thread saw it.
pub type InDatagram = (SocketAddr, Instant, Vec<u8>);
pub type PhaseTimes = [Duration; PHASES.len()];

/// Lag-compensation window: 200 ms.
const HISTORY_TICKS: usize = (TICK_HZ as usize) / 5;
/// Starved ticks that repeat the last input before movement freezes.
pub const GRACE_TICKS: u32 = 2;
/// A client can't queue more inputs than this (~0.5 s); beyond it the oldest
/// are discarded unapplied rather than letting latency grow.
const MAX_QUEUED_INPUTS: usize = 16;
const GRID_CELL: f32 = 32.0;
/// Input waits above 1 s land in the histogram's last bucket (0.1 ms units).
const INPUT_WAIT_CAP: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    /// Everyone spread over the whole continent.
    Uniform,
    /// The first 2,400 players split across 3 hotspots (~800 each), the rest uniform.
    Hotspots,
    /// The first 3,000 players in a 200 m disk, the rest uniform.
    Blob,
}

impl std::str::FromStr for SpawnMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "uniform" => Ok(Self::Uniform),
            "hotspots" => Ok(Self::Hotspots),
            "blob" => Ok(Self::Blob),
            _ => Err(format!("unknown spawn mode {s:?} (uniform|hotspots|blob)")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub net: Config,
    pub max_clients: usize,
    pub spawn: SpawnMode,
    /// Stand-in for M2's interest management: every client gets the `near_max`
    /// nearest entities within `near_radius`, every tick.
    pub near_radius: f32,
    pub near_max: usize,
    /// Transport shards. More than the thread count lets rayon balance them.
    pub shards: usize,
    /// Allocate `max_clients` connections at startup so accepts reuse them.
    pub preallocate: bool,
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            net: Config::default(),
            max_clients: 10_000,
            spawn: SpawnMode::Uniform,
            near_radius: 150.0,
            near_max: 64,
            shards: 64,
            preallocate: false,
            seed: 1,
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
    /// Real inputs that arrived for a seq a stand-in had already consumed.
    pub late_inputs: u64,
    /// Inputs dropped unapplied because the client queued too many.
    pub discarded_inputs: u64,
    pub bad_messages: u64,
    pub snapshots: u64,
    pub snapshot_entities: u64,
}

#[derive(Clone, Copy, Default)]
struct Body {
    alive: bool,
    state: MoveState,
    yaw: u16,
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
    /// Sorted by seq, all > last_seq, each with its first arrival time.
    pending: VecDeque<(u32, Input, Instant)>,
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
    fn push(&mut self, seq: u32, input: Input, arrived: Instant) -> Push {
        if seq <= self.last_seq {
            let age = self.last_seq - seq;
            if age < 32 && self.stand_ins & (1 << age) != 0 {
                self.stand_ins &= !(1 << age); // count each late seq once
                return Push::Late;
            }
            return Push::Duplicate;
        }
        let at = self.pending.partition_point(|&(s, _, _)| s < seq);
        if self.pending.get(at).is_some_and(|&(s, _, _)| s == seq) {
            return Push::Duplicate; // keep the first arrival
        }
        self.pending.insert(at, (seq, input, arrived));
        if self.pending.len() > MAX_QUEUED_INPUTS {
            let (s, _, _) = self.pending.pop_front().unwrap();
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

    /// Advance one tick, consuming seq `last_seq + 1`. `now` is the tick's time.
    fn advance(&mut self, body: &mut Body, now: Instant) -> Step {
        self.depth = self.pending.len().min(u8::MAX as usize) as u8;
        let next = self.last_seq + 1;
        let (input, kind) = if self.pending.front().is_some_and(|&(s, _, _)| s == next) {
            let (_, input, arrived) = self.pending.pop_front().unwrap();
            let waited = now.saturating_duration_since(arrived).as_micros() / 100;
            self.wait = waited.min(WAIT_STAND_IN as u128 - 1) as u16;
            self.last = input;
            self.starved_run = 0;
            (input, Step::Applied)
        } else if self.last_seq == 0 {
            body.state = step(body.state, Input::default());
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
        body.state = step(body.state, input);
        body.yaw = input.yaw;
        self.consume(next, kind != Step::Applied);
        kind
    }
}

pub struct SimServer {
    cfg: SimConfig,
    net: Server,
    tick: u32,
    bodies: Vec<Body>,
    inputs: Vec<InputQueue>,
    blobs: Vec<Blob>,
    history: Vec<Vec<[f32; 2]>>,
    free: Vec<u16>,
    by_client: HashMap<ClientId, u16>,
    /// Clients (and their entities) grouped by transport shard.
    shard_clients: Vec<Vec<(ClientId, u16)>>,
    /// Per-shard snapshot buffers, reused every tick.
    snapshots: Vec<Vec<(ClientId, Vec<u8>)>>,
    /// Per-shard transport events, each with the arrival time of the datagram
    /// that caused it.
    shard_events: Vec<Vec<(Instant, ServerEvent)>>,
    /// Input waits (arrival -> applied) since the last `take_input_wait`, in 0.1 ms.
    input_wait: Histogram,
    grid: Grid,
    rng: Rng,
    counters: Counters,
}

impl SimServer {
    pub fn new(mut cfg: SimConfig, now: Instant) -> Self {
        let max_fit = (cfg.net.max_message_size() - SNAPSHOT_HEADER) / ENTITY_BLOB;
        cfg.near_max = cfg.near_max.min(max_fit).min(u8::MAX as usize);
        assert!(cfg.max_clients <= u16::MAX as usize, "entity ids are u16");
        let mut net = Server::with_shards(cfg.net.clone(), cfg.max_clients, cfg.shards, now);
        if cfg.preallocate {
            net.preallocate(cfg.max_clients);
        }
        Self {
            net,
            shard_clients: vec![Vec::new(); cfg.shards],
            snapshots: vec![Vec::new(); cfg.shards],
            shard_events: (0..cfg.shards).map(|_| Vec::new()).collect(),
            input_wait: Histogram::new(INPUT_WAIT_CAP),
            rng: Rng::new(cfg.seed),
            cfg,
            tick: 0,
            bodies: Vec::new(),
            inputs: Vec::new(),
            blobs: Vec::new(),
            history: vec![Vec::new(); HISTORY_TICKS],
            free: Vec::new(),
            by_client: HashMap::new(),
            grid: Grid::new(GRID_CELL),
            counters: Counters::default(),
        }
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

    pub fn shard_count(&self) -> usize {
        self.shard_clients.len()
    }

    /// Input waits recorded since the last call, arrival -> applied, in 0.1 ms.
    pub fn take_input_wait(&mut self) -> Histogram {
        std::mem::replace(&mut self.input_wait, Histogram::new(INPUT_WAIT_CAP))
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

        // 1. ingress: the per-packet transport work, one task per shard
        self.net
            .shards_mut()
            .par_iter_mut()
            .zip(inbound.par_iter_mut())
            .zip(self.shard_events.par_iter_mut())
            .for_each(|((shard, bucket), events)| {
                for (from, arrived, data) in bucket.drain(..) {
                    shard.receive(from, &data, arrived);
                    while let Some(ev) = shard.poll_event() {
                        events.push((arrived, ev));
                    }
                }
                shard.update(now);
                while let Some(ev) = shard.poll_event() {
                    events.push((now, ev));
                }
            });
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
        let [applied, repeated, frozen] = self
            .bodies
            .par_iter_mut()
            .zip(self.inputs.par_iter_mut())
            .with_min_len(256)
            .filter(|(b, _)| b.alive)
            .map(|(b, q)| match q.advance(b, now) {
                Step::Applied => [1, 0, 0],
                Step::Repeated => [0, 1, 0],
                Step::Frozen => [0, 0, 1],
                Step::Waiting => [0, 0, 0],
            })
            .reduce(|| [0u64; 3], |a, b| [a[0] + b[0], a[1] + b[1], a[2] + b[2]]);
        self.counters.inputs_applied += applied;
        self.counters.repeated += repeated;
        self.counters.frozen += frozen;
        for (b, q) in self.bodies.iter().zip(&self.inputs) {
            if b.alive && q.last_seq > 0 && q.wait != WAIT_STAND_IN {
                self.input_wait.record(q.wait as u32);
            }
        }
        lap(2);

        // 5. spatial grid
        let bodies = &self.bodies;
        self.grid.rebuild(
            bodies.iter().enumerate().filter(|(_, b)| b.alive).map(|(i, b)| (i as u32, b.state.pos)),
        );
        lap(3);

        // 6. lag-comp history
        let slot = &mut self.history[self.tick as usize % HISTORY_TICKS];
        slot.clear();
        slot.extend(self.bodies.iter().map(|b| b.state.pos));
        lap(4);

        // 7. serialize each entity once
        self.blobs
            .par_iter_mut()
            .zip(self.bodies.par_iter())
            .enumerate()
            .with_min_len(512)
            .filter(|(_, (_, b))| b.alive)
            .for_each(|(i, (blob, b))| *blob = msg::encode_blob(i as u16, b.state.pos, b.yaw));
        lap(5);

        // 8. per-client assembly, grouped by shard so each shard's snapshots
        // are ready for its transport task
        let (radius, k, tick) = (self.cfg.near_radius, self.cfg.near_max, self.tick);
        let (bodies, inputs, blobs, grid) = (&self.bodies, &self.inputs, &self.blobs, &self.grid);
        self.shard_clients.par_iter().zip(self.snapshots.par_iter_mut()).for_each(|(clients, snaps)| {
            let mut near: Vec<(f32, u32)> = Vec::new();
            for &(client, e) in clients {
                let me = bodies[e as usize].state;
                near.clear();
                grid.for_each_near(me.pos, radius, |j| {
                    if j != e as u32 {
                        let p = bodies[j as usize].state.pos;
                        let d2 = (p[0] - me.pos[0]).powi(2) + (p[1] - me.pos[1]).powi(2);
                        if d2 <= radius * radius {
                            near.push((d2, j));
                        }
                    }
                });
                if near.len() > k {
                    near.select_nth_unstable_by(k, |a, b| a.0.total_cmp(&b.0));
                    near.truncate(k);
                }
                let mut w = Writer::with_capacity(SNAPSHOT_HEADER + near.len() * ENTITY_BLOB);
                let header = SnapshotHeader {
                    server_tick: tick,
                    ack_seq: inputs[e as usize].last_seq,
                    buffered: inputs[e as usize].depth,
                    wait: inputs[e as usize].wait,
                    own: me,
                    count: near.len() as u8,
                };
                msg::write_snapshot_header(&mut w, &header);
                for &(_, j) in near.iter() {
                    w.bytes(&blobs[j as usize]);
                }
                snaps.push((client, w.into_inner()));
            }
        });
        for snaps in &self.snapshots {
            self.counters.snapshots += snaps.len() as u64;
            self.counters.snapshot_entities +=
                snaps.iter().map(|(_, s)| ((s.len() - SNAPSHOT_HEADER) / ENTITY_BLOB) as u64).sum::<u64>();
        }
        lap(6);

        // 8b. transport: queue, frame, ack and checksum, one task per shard
        self.net
            .shards_mut()
            .par_iter_mut()
            .zip(self.snapshots.par_iter_mut())
            .zip(out.par_iter_mut())
            .for_each(|((shard, snaps), out)| {
                for (client, snap) in snaps.drain(..) {
                    let _ = shard.send(client, Channel::Unreliable, snap);
                }
                shard.flush(now);
                out.extend(shard.drain_outgoing());
            });
        lap(7);

        self.tick = self.tick.wrapping_add(1);
        self.counters.ticks += 1;
        times
    }

    fn spawn(&mut self, client: ClientId) {
        let (spawn, anchor, radius) = self.pick_spawn();
        let e = match self.free.pop() {
            Some(e) => e,
            None => {
                self.bodies.push(Body::default());
                self.inputs.push(InputQueue::default());
                self.blobs.push([0; ENTITY_BLOB]);
                (self.bodies.len() - 1) as u16
            }
        };
        let i = e as usize;
        self.bodies[i] = Body { alive: true, state: MoveState { pos: spawn, vel: [0.0; 2] }, yaw: 0 };
        self.inputs[i] = InputQueue::default();
        self.by_client.insert(client, e);
        self.shard_clients[self.net.shard_of_client(client)].push((client, e));
        self.counters.spawns += 1;
        let welcome = msg::encode_welcome(&Welcome { entity: e, spawn, anchor, radius });
        let _ = self.net.send(client, Channel::Reliable, welcome);
    }

    fn despawn(&mut self, client: ClientId) {
        if let Some(e) = self.by_client.remove(&client) {
            let list = &mut self.shard_clients[self.net.shard_of_client(client)];
            if let Some(i) = list.iter().position(|&(c, _)| c == client) {
                list.swap_remove(i);
            }
            self.bodies[e as usize].alive = false;
            self.free.push(e);
            self.counters.despawns += 1;
        }
    }

    fn on_input(&mut self, client: ClientId, data: &[u8], arrived: Instant) {
        let Some(&e) = self.by_client.get(&client) else { return };
        let (q, c) = (&mut self.inputs[e as usize], &mut self.counters);
        let ok = msg::decode_inputs(data, |seq, input| match q.push(seq, input, arrived) {
            Push::Late => c.late_inputs += 1,
            Push::Discarded => c.discarded_inputs += 1,
            Push::Queued | Push::Duplicate => {}
        });
        if ok.is_err() {
            self.counters.bad_messages += 1;
        }
    }

    /// Returns (spawn point, wander anchor, wander radius).
    fn pick_spawn(&mut self) -> ([f32; 2], [f32; 2], f32) {
        const HOTSPOTS: [[f32; 2]; 3] = [[2048.0, 2048.0], [6144.0, 2048.0], [4096.0, 6144.0]];
        const CENTER: [f32; 2] = [WORLD_SIZE / 2.0, WORLD_SIZE / 2.0];
        let k = self.counters.spawns;
        let (anchor, radius) = match self.cfg.spawn {
            SpawnMode::Hotspots if k < 2400 => (HOTSPOTS[k as usize % 3], 150.0),
            SpawnMode::Blob if k < 3000 => (CENTER, 200.0),
            _ => {
                let margin = 500.0;
                let p = [self.rng.range(margin, WORLD_SIZE - margin), self.rng.range(margin, WORLD_SIZE - margin)];
                (p, 400.0)
            }
        };
        (self.rng.in_disk(anchor, radius), anchor, radius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fwd() -> Input {
        Input { move_x: 127, yaw: 777, ..Default::default() }
    }

    #[test]
    fn wait_runs_from_first_arrival_to_applied() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        q.push(1, fwd(), ms(0));
        q.push(1, fwd(), ms(20)); // a redundant copy doesn't reset the clock
        q.push(2, fwd(), ms(20));
        assert_eq!(q.advance(&mut b, ms(33)), Step::Applied);
        assert_eq!(q.wait, 330, "33 ms in 0.1 ms units");
        assert_eq!(q.advance(&mut b, ms(66)), Step::Applied);
        assert_eq!(q.wait, 460);
        assert_eq!(q.advance(&mut b, ms(99)), Step::Repeated);
        assert_eq!(q.wait, WAIT_STAND_IN);
    }

    #[test]
    fn input_queue_orders_and_dedups() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };

        assert_eq!(q.advance(&mut b, t), Step::Waiting, "no input yet consumes nothing");
        assert_eq!(q.last_seq, 0);
        assert_eq!(q.push(2, fwd(), t), Push::Queued);
        assert_eq!(q.push(1, fwd(), t), Push::Queued);
        assert_eq!(q.push(2, fwd(), t), Push::Duplicate);
        assert_eq!(q.advance(&mut b, t), Step::Applied);
        assert_eq!((q.last_seq, q.depth), (1, 2));
        assert_eq!(q.push(1, fwd(), t), Push::Duplicate, "already applied");
        assert_eq!(q.advance(&mut b, t), Step::Applied);
        assert_eq!(q.last_seq, 2);

        // A backlog drains one per tick; overflow discards the oldest unapplied.
        for s in 3..=3 + MAX_QUEUED_INPUTS as u32 {
            q.push(s, fwd(), t);
        }
        assert_eq!(q.pending.len(), MAX_QUEUED_INPUTS);
        assert_eq!(q.last_seq, 3, "seq 3 was discarded");
        assert_eq!(q.advance(&mut b, t), Step::Applied);
        assert_eq!(q.last_seq, 4);
    }

    #[test]
    fn starvation_repeats_then_freezes_and_drops_late_inputs() {
        let t = Instant::now();
        let mut q = InputQueue::default();
        let mut b = Body { alive: true, ..Default::default() };
        q.push(1, fwd(), t);
        assert_eq!(q.advance(&mut b, t), Step::Applied);

        // Lag switch: nothing arrives for 10 ticks.
        let kinds: Vec<Step> = (0..10).map(|_| q.advance(&mut b, t)).collect();
        assert_eq!(&kinds[..2], &[Step::Repeated; 2]);
        assert!(kinds[2..].iter().all(|&k| k == Step::Frozen));
        assert_eq!(q.last_seq, 11, "every stand-in consumes a seq");
        assert_eq!(b.yaw, 777, "frozen keeps facing");
        let frozen_at = b.state.pos;

        // The held-back burst arrives: all of it is too late to move anyone.
        for s in 2..=11 {
            assert_eq!(q.push(s, fwd(), t), Push::Late);
            assert_eq!(q.push(s, fwd(), t), Push::Duplicate, "a late seq counts once");
        }
        assert!(q.pending.is_empty());
        for _ in 0..30 {
            q.advance(&mut b, t);
        }
        assert!(b.state.vel == [0.0, 0.0] && b.state.pos[0] - frozen_at[0] < 0.5, "{:?}", b.state);

        // Fresh input for the next seq resumes movement and resets the grace.
        assert_eq!(q.push(q.last_seq + 1, fwd(), t), Push::Queued);
        assert_eq!(q.advance(&mut b, t), Step::Applied);
        assert_eq!(q.starved_run, 0);
        assert!(b.state.vel[0] > 0.0);
    }
}
