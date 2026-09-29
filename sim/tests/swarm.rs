//! Real `lattice_net::Client`s + `BotBrain`s against `SimServer`, in simulated
//! time, with no sockets. Lockstep: every bot ticks, then the server ticks.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lattice_net::{Channel, Client, ClientState, Config};
use lattice_sim::bot::BotBrain;
use lattice_sim::movement::TICK_HZ;
use lattice_sim::rng::Rng;
use lattice_sim::interest::{InterestConfig, Tier, FAR_PERIOD, MID_PERIOD};
use lattice_sim::server::{SimConfig, SimServer, SpawnMode};

const SERVER: &str = "10.0.0.1:40000";

struct Swarm {
    server: SimServer,
    bots: Vec<(SocketAddr, Client, BotBrain)>,
    now: Instant,
    rng: Rng,
    /// Probability that any datagram (either direction) is dropped.
    loss: f32,
    /// Datagrams on their way to the server, stamped with their arrival time.
    to_server: Vec<(SocketAddr, Instant, Vec<u8>)>,
    /// Probability that a bot->server datagram arrives one tick late (jitter).
    delay: f32,
    delayed: Vec<(SocketAddr, Vec<u8>)>,
    /// The server ticks on its own schedule (its period depends on its level).
    next_server: Instant,
    /// Stretch the server's period by this factor: it falls behind its own
    /// schedule, as when it can't finish ticks in time.
    stretch: f32,
    /// Report this fraction of the period as each tick's work, instead of the
    /// real (tiny) in-process time, to drive the degradation ladder.
    fake_load: Option<f32>,
    /// Bot 0 doesn't run at all (a client hitch).
    stall_bot0: bool,
    /// Bot 0's outgoing packets are held back instead of sent (a lag switch).
    hold_bot0: Option<Vec<(SocketAddr, Vec<u8>)>>,
}

impl Swarm {
    fn new(n: usize, spawn: SpawnMode) -> Self {
        Self::with_config(n, SimConfig { spawn, ..Default::default() })
    }

    fn with_config(n: usize, cfg: SimConfig) -> Self {
        let now = Instant::now();
        let server_addr: SocketAddr = SERVER.parse().unwrap();
        let bots = (0..n)
            .map(|i| {
                let addr: SocketAddr = format!("10.1.{}.{}:5000", i / 250, i % 250 + 1).parse().unwrap();
                (addr, Client::new(Config::default(), server_addr, now), BotBrain::new(i as u64))
            })
            .collect();
        Self {
            server: SimServer::new(cfg, now),
            bots,
            now,
            rng: Rng::new(7),
            loss: 0.0,
            to_server: Vec::new(),
            delay: 0.0,
            delayed: Vec::new(),
            next_server: now,
            stretch: 1.0,
            fake_load: None,
            stall_bot0: false,
            hold_bot0: None,
        }
    }

    fn step(&mut self) {
        // Held back last step: they reach the server this step, a tick late.
        let now = self.now;
        self.to_server.extend(self.delayed.drain(..).map(|(a, p)| (a, now, p)));
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
                    _ => self.to_server.push((*addr, self.now, pkt)),
                }
            }
        }

        if self.now < self.next_server {
            self.now += Duration::from_secs(1) / TICK_HZ;
            return; // the server isn't due: datagrams wait for it
        }
        let period = self.server.tick_period();
        self.next_server += period.mul_f32(self.stretch);
        let router = self.server.router();
        let mut inbound = vec![Vec::new(); router.shard_count()];
        for (from, at, pkt) in self.to_server.drain(..) {
            inbound[router.shard(&from)].push((from, at, pkt));
        }
        let mut out = vec![Vec::new(); router.shard_count()];
        let t = Instant::now();
        self.server.tick(&mut inbound, self.now, &mut out);
        let work = self.fake_load.map_or(t.elapsed(), |f| period.mul_f32(f));
        self.server.observe_tick(work);
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
        let near = st.tier_seen[Tier::Near as usize] as f64 / st.snapshots as f64;
        assert!(near > 2.0, "near entities per snapshot {near}");
        assert_eq!(st.tier_seen[Tier::Far as usize], 0, "nobody is 500 m away");
    }
    // All 40 spawn in a 200 m disk; a 150 m near radius covers a good part of it.
    let (seen, snaps) = s.bots.iter().fold((0, 0), |a, (_, _, b)| (a.0 + b.stats.tier_seen[0], a.1 + b.stats.snapshots));
    let avg = seen as f64 / snaps as f64;
    assert!(avg > 8.0, "swarm avg near entities per snapshot {avg}");
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
fn input_clock_keeps_one_spare_input() {
    let mut s = Swarm::new(20, SpawnMode::Uniform);
    for _ in 0..20 * TICK_HZ {
        s.step();
    }
    for (_, _, b) in &s.bots {
        let depth = b.server_buffer();
        assert!((1.75..=2.5).contains(&depth), "server queue depth {depth}");
        // Lockstep starts at depth 1: the clock runs ahead once, then holds steady.
        assert!(b.stats.clock_extra >= 1, "the clock must run ahead to build a spare");
        assert!(b.stats.clock_extra + b.stats.clock_skipped <= 3, "clock hunting: {:?}", b.stats);
    }
    assert_eq!(stand_ins(&s), 0);

    // Lockstep with one spare: an input sent on tick t arrives at once, waits a
    // tick on the server (the spare) and is applied on t+1; its snapshot is read
    // on t+2. So the server wait is exactly 1 tick and the round trip exactly 2.
    // The network takes no time here, so the transport RTT must read ~0: the
    // server's hold is reported as ack_delay and subtracted.
    for (_, client, b) in &mut s.bots {
        let mut samples = Vec::new();
        b.drain_latency(&mut samples);
        let steady = &samples[samples.len() - 100..];
        assert!(steady.iter().all(|t| t.seen_ms == 67 && t.server_wait == Some(333)), "{:?}", &steady[..3]);
        let rtt = client.stats().unwrap().rtt_ms;
        assert!(rtt < 0.5, "rtt {rtt} ms over a zero-latency link");
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
    let now = s.now;
    s.to_server.extend(burst.into_iter().map(|(a, p)| (a, now, p)));
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

#[test]
fn input_clock_drains_back_to_one_spare_after_jitter() {
    // Every input packet arrives a tick late for 3 s: the bots build an extra
    // tick of lead to keep their spare. When the delay goes away, that lead is a
    // second spare (depth 3, +33 ms of input latency) and must drain away.
    let mut s = Swarm::new(5, SpawnMode::Blob);
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    s.delay = 1.0;
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    s.delay = 0.0;
    for _ in 0..4 * TICK_HZ {
        s.step();
    }
    assert_eq!(stand_ins(&s), 0, "one spare absorbs a one-tick delay");
    for (_, _, b) in &mut s.bots {
        assert!(b.server_buffer() < 2.5, "still at depth {}", b.server_buffer());
        let mut samples = Vec::new();
        b.drain_latency(&mut samples);
        let last = samples.last().unwrap();
        assert_eq!(last.server_wait, Some(333), "server wait back to one tick");
    }
}

/// A line of still players 26 m apart: none within 6 m of a tier boundary.
fn line_swarm(n: usize, interest: InterestConfig) -> Swarm {
    let cfg = SimConfig { spawn: SpawnMode::Line(26.0), interest, ..Default::default() };
    let mut s = Swarm::with_config(n, cfg);
    s.bots[0].2.enable_tracking();
    s
}

/// A bot's entity and its index on the line (spawn order, not bot order).
fn line_index(s: &Swarm, bot: usize, spacing: f32) -> (u16, i32) {
    let entity = s.bots[bot].2.welcome().unwrap().entity;
    let x = s.server.entity_state(entity).unwrap().pos[0];
    (entity, ((x - 1000.0) / spacing).round() as i32)
}

fn expected_tier(d: f32, cfg: &InterestConfig) -> Option<Tier> {
    if d <= cfg.near_radius {
        Some(Tier::Near)
    } else if d <= cfg.mid_radius {
        Some(Tier::Mid)
    } else if d <= cfg.far_radius {
        Some(Tier::Far)
    } else {
        None
    }
}

#[test]
fn tiers_follow_distance_and_update_at_their_rates() {
    let cfg = InterestConfig::default();
    let mut s = line_swarm(62, cfg.clone());
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let mut warm = Default::default();
    s.bots[0].2.drain_intervals(&mut warm);
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let t = s.bots[0].2.tracker().unwrap();
    let (_, k0) = line_index(&s, 0, 26.0);
    for i in 1..s.bots.len() {
        let (entity, k) = line_index(&s, i, 26.0);
        let d = (k - k0).abs() as f32 * 26.0;
        let known = t.get(entity);
        assert_eq!(known.map(|k| k.tier), expected_tier(d, &cfg), "bot {i} at {d} m");
        if let Some(known) = known {
            let truth = s.server.entity_state(entity).unwrap().pos;
            let err = ((known.pos[0] - truth[0]).powi(2) + (known.pos[1] - truth[1]).powi(2)).sqrt();
            // Far entities are up to 15 ticks stale; a bot wanders ~2 m around its anchor.
            assert!(err < 5.0, "bot {i}: known position {err} m off");
        }
    }
    let mut iv: [Vec<u16>; 3] = Default::default();
    let mut me = s.bots.remove(0).2;
    me.drain_intervals(&mut iv);
    assert!(iv[0].iter().all(|&g| g == 1), "near every tick: {:?}", &iv[0][..10]);
    assert!(iv[1].iter().all(|&g| g == MID_PERIOD as u16), "mid every 3rd tick");
    assert!(iv[2].iter().all(|&g| g == FAR_PERIOD as u16), "far every 15th tick");
    assert!(!iv[2].is_empty());
    let c = s.server.counters();
    assert_eq!((c.far_skipped, c.far_starved, c.mid_truncated), (0, 0, 0), "the default budget doesn't bind");
}

#[test]
fn squadmates_are_near_tier_at_any_distance() {
    // Squads of 2 in a line 300 m apart: bot 1 is 300 m from bot 0 (mid band)
    // but in its squad; bot 2 is 600 m away (far band) and isn't.
    let interest = InterestConfig { squad_size: 2, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Line(300.0), interest, ..Default::default() };
    let mut s = Swarm::with_config(3, cfg);
    s.bots[0].2.enable_tracking();
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    // Squads are by spawn order: line index k is in squad k / 2.
    let t = s.bots[0].2.tracker().unwrap();
    let (_, k0) = line_index(&s, 0, 300.0);
    for i in 1..3 {
        let (entity, k) = line_index(&s, i, 300.0);
        let want = if k / 2 == k0 / 2 {
            Tier::Near // squadmate, 300 m away
        } else if (k - k0).abs() == 1 {
            Tier::Mid
        } else {
            Tier::Far
        };
        assert_eq!(t.get(entity).map(|k| k.tier), Some(want), "line index {k} vs {k0}");
    }
}

#[test]
fn a_tight_budget_never_cuts_the_near_tier() {
    // Room for own state and near (near deltas: ~110 B mid-line), but not for
    // all of mid and far (~275 B in all at level 0): far falls behind, the
    // server flags it, and the starving clients shrink their own mid/far radii
    // until it fits (~200 B at client level 3).
    const BUDGET: u64 = 250;
    let interest = InterestConfig { budget_bytes: BUDGET as usize, ..Default::default() };
    let mut s = line_swarm(62, interest);
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let mut iv: [Vec<u16>; 3] = Default::default();
    s.bots[0].2.drain_intervals(&mut iv);
    assert!(iv[0].iter().all(|&g| g == 1), "near still every tick");
    let c = s.server.counters().clone();
    assert!(c.far_skipped > 0 && c.far_starved > 0, "skips and starvation are counted: {c:?}");
    assert!(c.degraded_clients > 0, "starving clients shrink their own mid/far radii");
    // Once starving clients have shrunk their radii, starvation drops sharply.
    // (The budget fits on average but not every tick's peak, so it doesn't
    // reach zero; the ladder's own rules are unit-tested in ladder.rs.)
    let first = c.far_starved as f64 / 3.0;
    let mark = s.server.counters().far_starved;
    for _ in 0..10 * TICK_HZ {
        s.step();
    }
    let later = (s.server.counters().far_starved - mark) as f64 / 10.0;
    assert!(later * 3.0 < first, "starved {first:.1}/s before degrading, {later:.1}/s after");
    let c = s.server.counters();
    // Every client stayed within its budget (the client at the end of the line
    // sees less, so the average is below 190).
    assert!(c.snapshot_bytes <= BUDGET * c.snapshots, "{} B over {} client-ticks", c.snapshot_bytes, c.snapshots);
}

fn stand_ins_and_discards(s: &Swarm) -> (u64, u64) {
    let c = s.server.counters();
    (c.repeated + c.frozen, c.discarded_inputs)
}

#[test]
fn ladder_degrades_to_the_bottom_and_back_without_breaking_clients() {
    let mut s = Swarm::new(20, SpawnMode::Blob);
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    // Sustained overload: every rung, down to 20 Hz at dilation 0.8.
    s.fake_load = Some(0.95);
    for _ in 0..30 * TICK_HZ {
        s.step();
    }
    assert_eq!(s.server.level(), lattice_sim::ladder::MAX_LEVEL);
    assert_eq!(s.server.rung().tick_hz, 20);
    assert!((s.server.pace() - 0.8).abs() < 0.01, "pace {}", s.server.pace());
    for (_, _, b) in &s.bots {
        assert_eq!((b.stats.level, b.stats.pace), (lattice_sim::ladder::MAX_LEVEL, 800), "clients are told");
    }
    // Load gone: back to normal, one level per calm stretch.
    s.fake_load = Some(0.2);
    for _ in 0..60 * TICK_HZ {
        s.step();
    }
    assert_eq!((s.server.level(), s.server.rung().tick_hz), (0, 30));
    assert!((s.server.pace() - 1.0).abs() < 0.001, "pace {}", s.server.pace());
    let visited = s.server.counters().level_ticks.iter().filter(|&&t| t > 0).count();
    assert_eq!(visited, 9, "every level was used: {:?}", s.server.counters().level_ticks);
    // Through every rung, 20 Hz ticks and dilation included, clients kept pace:
    // no input overflowed a queue, none arrived late, prediction never broke.
    assert_eq!(stand_ins_and_discards(&s), (0, 0));
    assert_eq!(s.corrections(), 0);
}

#[test]
fn clients_follow_a_server_that_falls_behind() {
    // The server means to tick at 30 Hz but only manages 20 (and its ladder is
    // off, so it doesn't adapt). It advertises the pace it achieves, 2/3, and
    // the bots slow their inputs to match instead of overflowing its queues.
    let ladder = lattice_sim::ladder::LadderConfig { enabled: false, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Blob, ladder, ..Default::default() };
    let mut s = Swarm::with_config(10, cfg);
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    s.stretch = 1.5;
    for _ in 0..15 * TICK_HZ {
        s.step();
    }
    assert!((s.server.pace() - 2.0 / 3.0).abs() < 0.02, "pace {}", s.server.pace());
    assert_eq!(stand_ins_and_discards(&s), (0, 0));
    for (_, _, b) in &mut s.bots {
        let mut samples = Vec::new();
        b.drain_latency(&mut samples);
        // The backlog built before the bots heard is drained: about one
        // server tick (50 ms here) of wait again.
        let w = samples.last().unwrap().server_wait.unwrap();
        assert!(w <= 1000, "server wait {} ms", w / 10);
    }
    assert_eq!(s.corrections(), 0);
}


#[test]
fn sink_bots_keep_playing_and_only_count_what_they_get() {
    let mut s = Swarm::new(10, SpawnMode::Blob);
    for (i, (_, _, b)) in s.bots.iter_mut().enumerate() {
        b.set_sink(i % 2 == 1);
    }
    for _ in 0..5 * TICK_HZ {
        s.step();
    }
    assert_eq!((stand_ins_and_discards(&s), s.corrections()), ((0, 0), 0));
    for (i, (_, _, b)) in s.bots.iter_mut().enumerate() {
        let mut samples = Vec::new();
        b.drain_latency(&mut samples);
        assert!(b.stats.tier_seen[0] > 0, "bot {i} counts near entities");
        assert_eq!(b.stats.pace, 1000, "and follows the pace");
        assert_eq!(samples.is_empty(), i % 2 == 1, "only full bots keep latency samples");
    }
}

/// Every bot tracks; checks each one's decoded near states against the
/// server's quantized states for the same ticks.
fn assert_near_states_exact(s: &Swarm) -> usize {
    let mut checked = 0;
    for (_, _, b) in &s.bots {
        assert_eq!(b.stats.near_decode_errors, 0, "a delta referred to a baseline the client lacks");
        let t = b.tracker().unwrap();
        for (_, _, other) in &s.bots {
            let entity = other.welcome().unwrap().entity;
            let Some(k) = t.get(entity).filter(|k| k.tier == Tier::Near) else { continue };
            let theirs = s.server.near_state_at(entity, k.tick);
            if theirs.is_some() {
                assert_eq!(t.near_state(entity, k.tick), theirs, "entity {entity} at tick {}", k.tick);
                checked += 1;
            }
        }
    }
    checked
}

#[test]
fn near_deltas_decode_exactly_and_cut_near_bytes() {
    let mut s = Swarm::new(20, SpawnMode::Blob);
    s.bots.iter_mut().for_each(|(_, _, b)| b.enable_tracking());
    for _ in 0..5 * TICK_HZ {
        s.step();
    }
    assert!(assert_near_states_exact(&s) > 100);
    let c = s.server.counters();
    assert!(c.near_deltas > 10 * c.near_full, "deltas {} vs full {}", c.near_deltas, c.near_full);
    let per_entity = c.near_bytes as f64 / c.tier_sent[0] as f64;
    assert!(per_entity < 7.5, "{per_entity:.1} B per near entity (a full near blob was 15)");
}

#[test]
fn near_deltas_survive_loss() {
    // Baselines only advance on acks, so a lost message never leaves a client
    // holding a delta it can't resolve.
    let mut s = Swarm::new(20, SpawnMode::Blob);
    s.bots.iter_mut().for_each(|(_, _, b)| b.enable_tracking());
    for _ in 0..TICK_HZ {
        s.step();
    }
    s.loss = 0.3;
    for _ in 0..8 * TICK_HZ {
        s.step();
    }
    assert!(assert_near_states_exact(&s) > 100);
    let c = s.server.counters();
    assert!(c.near_deltas > c.near_full, "deltas {} vs full {}", c.near_deltas, c.near_full);
}
