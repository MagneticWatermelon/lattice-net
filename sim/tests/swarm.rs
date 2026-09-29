//! Real `lattice_net::Client`s + `BotBrain`s against `SimServer`, in simulated
//! time, with no sockets. Lockstep: every bot ticks, then the server ticks.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lattice_net::{Channel, Client, ClientState, Config};
use lattice_sim::bot::BotBrain;
use lattice_sim::movement::TICK_HZ;
use lattice_sim::rng::Rng;
use lattice_sim::server::{SimConfig, SimServer, SpawnMode};

const SERVER: &str = "10.0.0.1:40000";

struct Swarm {
    server: SimServer,
    bots: Vec<(SocketAddr, Client, BotBrain)>,
    now: Instant,
    rng: Rng,
    /// Probability that any datagram (either direction) is dropped.
    loss: f32,
    to_server: Vec<(SocketAddr, Vec<u8>)>,
    /// Probability that a bot->server datagram arrives one tick late (jitter).
    delay: f32,
    delayed: Vec<(SocketAddr, Vec<u8>)>,
    /// Bot 0 doesn't run at all (a client hitch).
    stall_bot0: bool,
    /// Bot 0's outgoing packets are held back instead of sent (a lag switch).
    hold_bot0: Option<Vec<(SocketAddr, Vec<u8>)>>,
}

impl Swarm {
    fn new(n: usize, spawn: SpawnMode) -> Self {
        let now = Instant::now();
        let server_addr: SocketAddr = SERVER.parse().unwrap();
        let cfg = SimConfig { spawn, ..Default::default() };
        let bots = (0..n)
            .map(|i| {
                let addr: SocketAddr = format!("10.1.{}.{}:5000", i / 250, i % 250 + 1).parse().unwrap();
                (addr, Client::new(Config::default(), server_addr, now), BotBrain::new(i as u64))
            })
            .collect();
        Self { server: SimServer::new(cfg, now), bots, now, rng: Rng::new(7), loss: 0.0, to_server: Vec::new(), delay: 0.0, delayed: Vec::new(), stall_bot0: false, hold_bot0: None }
    }

    fn step(&mut self) {
        // Held back last step: they reach the server this step, a tick late.
        self.to_server.append(&mut self.delayed);
        let server_addr: SocketAddr = SERVER.parse().unwrap();
        for (i, (addr, client, brain)) in self.bots.iter_mut().enumerate() {
            if i == 0 && self.stall_bot0 {
                continue;
            }
            client.update(self.now);
            if client.state() == ClientState::Connected {
                while let Some((_, data)) = client.recv() {
                    brain.on_message(&data, self.now);
                }
                if let Some(batch) = brain.tick_inputs(self.now) {
                    client.send(Channel::Unreliable, batch).unwrap();
                }
            }
            client.flush(self.now);
            for pkt in client.drain_outgoing() {
                match &mut self.hold_bot0 {
                    Some(held) if i == 0 => held.push((*addr, pkt)),
                    _ if self.rng.chance(self.loss) => {}
                    _ if self.rng.chance(self.delay) => self.delayed.push((*addr, pkt)),
                    _ => self.to_server.push((*addr, pkt)),
                }
            }
        }

        let router = self.server.router();
        let mut inbound = vec![Vec::new(); router.shard_count()];
        for (from, pkt) in self.to_server.drain(..) {
            inbound[router.shard(&from)].push((from, pkt));
        }
        let mut out = vec![Vec::new(); router.shard_count()];
        self.server.tick(&mut inbound, self.now, &mut out);
        for (to, pkt) in out.into_iter().flatten() {
            if self.rng.chance(self.loss) {
                continue;
            }
            if let Some((_, client, _)) = self.bots.iter_mut().find(|(a, _, _)| *a == to) {
                client.receive(server_addr, &pkt, self.now);
            }
        }
        self.now += Duration::from_secs(1) / TICK_HZ;
    }

    fn corrections(&self) -> u64 {
        self.bots.iter().map(|(_, _, b)| b.stats.corrections).sum()
    }
}

fn stand_ins(s: &Swarm) -> u64 {
    let c = s.server.counters();
    c.repeated + c.frozen
}

#[test]
fn clean_link_predicts_bit_exactly() {
    let mut s = Swarm::new(40, SpawnMode::Blob);
    for _ in 0..10 * TICK_HZ {
        s.step();
    }
    assert_eq!(s.server.client_count(), 40);
    let c = s.server.counters();
    assert_eq!(c.spawns, 40);
    assert_eq!((c.repeated, c.frozen), (0, 0), "lockstep delivery never starves the server");
    assert!(c.inputs_applied > 40 * 9 * TICK_HZ as u64);
    assert_eq!(s.corrections(), 0, "prediction must match the server bit-for-bit");

    for (_, _, b) in &s.bots {
        assert!(b.welcome().is_some());
        let st = &b.stats;
        assert_eq!((st.unmatched_acks, st.bad_messages, st.stale_snapshots), (0, 0, 0));
        let avg = st.entities_seen as f64 / st.snapshots as f64;
        assert!(avg > 2.0, "avg entities per snapshot {avg}");
    }
    // All 40 spawn in a 200 m disk; a 150 m view covers a good part of it.
    let (seen, snaps) = s.bots.iter().fold((0, 0), |a, (_, _, b)| (a.0 + b.stats.entities_seen, a.1 + b.stats.snapshots));
    let avg = seen as f64 / snaps as f64;
    assert!(avg > 8.0, "swarm avg entities per snapshot {avg}");
}

#[test]
fn loss_causes_corrections_then_reconverges() {
    let mut s = Swarm::new(30, SpawnMode::Hotspots);
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    s.loss = 0.3;
    for _ in 0..10 * TICK_HZ {
        s.step();
    }
    assert!(stand_ins(&s) > 0);
    assert!(s.corrections() > 0, "30% loss must cause some mispredictions");

    // Back to a clean link: after the queues settle, no new corrections.
    s.loss = 0.0;
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    let settled = s.corrections();
    let starved = stand_ins(&s);
    for _ in 0..5 * TICK_HZ {
        s.step();
    }
    assert_eq!(stand_ins(&s), starved);
    assert_eq!(s.corrections(), settled);
    assert_eq!(s.server.client_count(), 30);

    // And the bots' predicted positions agree with what everyone else sees.
    let max_err = s.bots.iter().map(|(_, _, b)| b.stats.correction_error_max).fold(0.0, f32::max);
    assert!(max_err < 5.0, "a correction moved a bot {max_err} m");
}

#[test]
fn input_clock_keeps_one_to_two_spare_inputs() {
    let mut s = Swarm::new(20, SpawnMode::Uniform);
    for _ in 0..20 * TICK_HZ {
        s.step();
    }
    for (_, _, b) in &s.bots {
        let depth = b.server_buffer();
        assert!((1.75..=3.25).contains(&depth), "server queue depth {depth}");
        // Lockstep starts at depth 1: the clock runs ahead once, then holds steady.
        assert!(b.stats.clock_extra >= 1, "the clock must run ahead to build a spare");
        assert!(b.stats.clock_extra + b.stats.clock_skipped <= 3, "clock hunting: {:?}", b.stats);
    }
    assert_eq!(stand_ins(&s), 0);

    // Lockstep with one spare: an input sent on tick t is consumed on t+1, and
    // its snapshot is read on t+2. So input latency is exactly 2 ticks.
    for (_, _, b) in &mut s.bots {
        let mut samples = Vec::new();
        b.drain_latency(&mut samples);
        let steady = &samples[samples.len() - 100..];
        assert!(steady.iter().all(|&ms| ms == 67), "latency {:?}", &steady[..10]);
    }
}

#[test]
fn stalled_client_resyncs_instead_of_staying_late() {
    let mut s = Swarm::new(10, SpawnMode::Blob);
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    s.stall_bot0 = true;
    for _ in 0..20 {
        s.step();
    }
    s.stall_bot0 = false;
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    let c = s.server.counters();
    // The 1-2 inputs already buffered at the server cover the first stalled ticks.
    assert_eq!(c.repeated, 2, "2 ticks of grace");
    assert!((16..=18).contains(&c.frozen), "then frozen: {}", c.frozen);
    assert_eq!(s.bots[0].2.stats.resyncs, 1);

    // Recovered: no more stand-ins and no more corrections.
    let (standins, corrections) = (stand_ins(&s), s.corrections());
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    assert_eq!((stand_ins(&s), s.corrections()), (standins, corrections));
}

#[test]
fn lag_switch_buys_no_movement() {
    let mut s = Swarm::new(5, SpawnMode::Blob);
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let entity = s.bots[0].2.welcome().unwrap().entity;
    let before = s.server.entity_state(entity).unwrap();
    s.hold_bot0 = Some(Vec::new());
    for _ in 0..TICK_HZ {
        s.step();
    }
    let held = s.server.entity_state(entity).unwrap();
    let moved = ((held.pos[0] - before.pos[0]).powi(2) + (held.pos[1] - before.pos[1]).powi(2)).sqrt();
    // 2 grace ticks at up to sprint speed, then a stop: well under a meter.
    assert!(moved < 1.0, "moved {moved} m while holding packets");
    assert_eq!(held.vel, [0.0, 0.0]);

    // Release the burst: every held input is too late to apply.
    let burst = s.hold_bot0.take().unwrap();
    s.to_server.extend(burst);
    let late_before = s.server.counters().late_inputs;
    s.step();
    assert!(s.server.counters().late_inputs - late_before >= TICK_HZ as u64 - 3);
    let after = s.server.entity_state(entity).unwrap();
    let jump = ((after.pos[0] - held.pos[0]).powi(2) + (after.pos[1] - held.pos[1]).powi(2)).sqrt();
    assert!(jump < 0.5, "the burst moved the entity {jump} m");

    // The bot corrects once and carries on normally.
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    assert!(s.bots[0].2.stats.corrections > 0);
    let corrections = s.corrections();
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    assert_eq!(s.corrections(), corrections);
}


#[test]
fn one_tick_of_jitter_is_absorbed_from_the_start() {
    // Half of all input packets arrive a tick late. Bots start with a spare
    // input queued, so the server never has to stand in for one.
    let mut s = Swarm::new(20, SpawnMode::Blob);
    s.delay = 0.5;
    for _ in 0..10 * TICK_HZ {
        s.step();
    }
    assert_eq!(stand_ins(&s), 0);
    assert_eq!(s.corrections(), 0);
    for (_, _, b) in &s.bots {
        assert_eq!(b.stats.resyncs, 0);
    }
}
