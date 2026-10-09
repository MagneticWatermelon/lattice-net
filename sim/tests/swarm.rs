//! Real `lattice_net::Client`s + `BotBrain`s against `SimServer`, in simulated
//! time, with no sockets. Lockstep: every bot ticks, then the server ticks.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lattice_net::token::USER_DATA_BYTES;
use lattice_net::{Channel, Client, ClientState, Config, ConnectToken};
use lattice_sim::bot::BotBrain;
use lattice_sim::movement::TICK_HZ;
use lattice_sim::rng::Rng;
use lattice_sim::interest::{InterestConfig, Tier, FAR_PERIOD, MID_PERIOD};
use lattice_sim::server::{Datagram, SimConfig, SimServer, SpawnMode};

const SERVER: &str = "10.0.0.1:40000";

/// What a login service would give user `user` for this server.
fn token(cfg: &SimConfig, user: u64) -> ConnectToken {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let id = &cfg.identity;
    ConnectToken::mint(&id.token_key, cfg.net.protocol_id, id.server_id, unix + 60, user, &[0; USER_DATA_BYTES])
}

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
    /// Report this fraction of the period as each tick's work, to drive the
    /// degradation ladder. Not the real in-process time by default (`None`
    /// opts in): with the whole suite running in parallel, a real tick could
    /// stall long enough to step the ladder down mid-test, and a test's tiers
    /// and rates with it.
    fake_load: Option<f32>,
    /// Bot 0 doesn't run at all (a client hitch).
    stall_bot0: bool,
    /// Bot 0's outgoing packets are held back instead of sent (a lag switch).
    hold_bot0: Option<Vec<(SocketAddr, Vec<u8>)>>,
    /// The server's datagrams from its latest tick, in send order.
    last_out: Vec<(SocketAddr, usize)>,
    /// Server->bot datagrams are held back 0..=`down_jitter` bot steps, uniformly.
    down_jitter: u32,
    to_bots: Vec<(Instant, SocketAddr, Vec<u8>)>,
    /// Tracking bots draw a frame every step, checked against `truth`.
    render: bool,
    /// What tracking bots drew vs the server's states at the render step, in
    /// meters, by tier (from `render_from` on).
    render_err: [Vec<f32>; 3],
    render_from: Option<Instant>,
    /// Render steps went backwards (across all bots).
    render_backwards: u64,
    last_render: HashMap<usize, f64>,
    /// The server's states by game step (the last 3 s), for checking renders.
    truth: VecDeque<(u32, HashMap<u16, [f32; 3]>)>,
    /// One-way delay per bot, both directions, in steps (latency classes).
    lag: Vec<u32>,
    /// Bot -> server datagrams held back by `lag`, with when they arrive.
    up_delayed: Vec<(Instant, SocketAddr, Vec<u8>)>,
    /// A bot that aims at a target as it draws it, and fires; and each shot's
    /// aim and the direction its client drew the tracer along.
    gunner: Option<Gunner>,
    gunner_dirs: Vec<([f32; 3], [f32; 3])>,
    /// Tick with `tick_sending`: each shard sends from its own task.
    send_early: bool,
    /// Bot 0 runs like the game client: frames at this rate, each firing
    /// (if it's the gunner) and running the input clock by elapsed time
    /// (`tick_inputs`), instead of a step per tick (`step_inputs`). Its last
    /// frame's time.
    frames_bot0: Option<u32>,
    last_frame: Option<Instant>,
}

/// A test gunner: aims at `target` where its bot draws it, leading for the
/// flight time (and drop) at the drawn velocity, plus `extra_lead` steps.
#[derive(Debug, Clone, Copy)]
struct Gunner {
    bot: usize,
    target: u16,
    head: bool,
    extra_lead: f32,
    shots: u64,
    /// Aims down sights (its bot's inputs must say so too: `set_ads`).
    ads: bool,
    /// A cheat: aim where it drew the target this many steps earlier, and
    /// claim that render time ("backtrack").
    back: f32,
    /// Holds its aim where it first drew the target (`fixed`, set by the
    /// first shot) instead of tracking it.
    hold: bool,
    fixed: Option<(u16, i16)>,
    /// With `hold`, a cheat: each shot claims whichever render step up to
    /// this many steps back put the target on the held crosshair.
    pick_back: f32,
}

/// Aims `brain` at `g.target` as drawn at `now`, and pulls the trigger.
/// Returns the aim and the direction the client drew its tracer along.
fn aim_and_fire(brain: &mut BotBrain, g: &mut Gunner, now: Instant) -> Option<([f32; 3], [f32; 3])> {
    use lattice_game::weapon::aim;
    use lattice_sim::bot::aim_at;
    let core = brain.core_mut();
    let r = core.render_step(now)?;
    let (yaw, pitch, back) = if g.hold {
        let (y, p) = match g.fixed {
            Some(f) => f,
            None => *g.fixed.insert(aim_at(core, g.target, r, g.head, g.extra_lead)?),
        };
        // The cheat: of the render steps it may claim, the one whose target
        // lines up best with where it's aiming.
        let want = aim(y, p);
        let mut best = (f32::MAX, 0.0);
        for k in 0..=(g.pick_back * 8.0) as u32 {
            let b = k as f32 / 8.0;
            if let Some((by, bp)) = aim_at(core, g.target, r - b as f64, g.head, g.extra_lead) {
                let d = aim(by, bp);
                let miss = 1.0 - (want[0] * d[0] + want[1] * d[1] + want[2] * d[2]);
                if miss < best.0 {
                    best = (miss, b);
                }
            }
        }
        (y, p, best.1)
    } else {
        let (y, p) = aim_at(core, g.target, r - g.back as f64, g.head, g.extra_lead)?;
        (y, p, g.back)
    };
    let dir = core.fire_claiming(now, yaw, pitch, g.ads, back as f64)?;
    g.shots += 1;
    Some((aim(yaw, pitch), dir))
}

/// Where `entity` really was at (fractional) game step `r`: the server's
/// states either side, interpolated.
fn truth_at(truth: &VecDeque<(u32, HashMap<u16, [f32; 3]>)>, entity: u16, r: f64) -> Option<[f32; 3]> {
    let i = truth.partition_point(|(s, _)| (*s as f64) <= r);
    let (a, b) = (truth.get(i.checked_sub(1)?)?, truth.get(i)?);
    let (pa, pb) = (a.1.get(&entity)?, b.1.get(&entity)?);
    let t = ((r - a.0 as f64) / (b.0 - a.0) as f64) as f32;
    Some([0, 1, 2].map(|k| pa[k] + (pb[k] - pa[k]) * t))
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
                (addr, Client::new(Config::default(), server_addr, token(&cfg, i as u64), now), BotBrain::new(i as u64))
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
            fake_load: Some(0.1),
            stall_bot0: false,
            hold_bot0: None,
            last_out: Vec::new(),
            down_jitter: 0,
            to_bots: Vec::new(),
            render: false,
            render_err: Default::default(),
            render_from: None,
            render_backwards: 0,
            last_render: HashMap::new(),
            truth: VecDeque::new(),
            lag: vec![0; n],
            up_delayed: Vec::new(),
            gunner: None,
            gunner_dirs: Vec::new(),
            send_early: false,
            frames_bot0: None,
            last_frame: None,
        }
    }

    fn step(&mut self) {
        // Held back last step: they reach the server this step, a tick late.
        let now = self.now;
        self.to_server.extend(self.delayed.drain(..).map(|(a, p)| (a, now, p)));
        let server_addr: SocketAddr = SERVER.parse().unwrap();
        // Bot datagrams whose one-way delay is up.
        let mut k = 0;
        while k < self.up_delayed.len() {
            if self.up_delayed[k].0 <= now {
                let (at, from, pkt) = self.up_delayed.swap_remove(k);
                self.to_server.push((from, at, pkt));
            } else {
                k += 1;
            }
        }
        // Held-back server datagrams that are due.
        let mut k = 0;
        while k < self.to_bots.len() {
            if self.to_bots[k].0 <= now {
                let (_, to, pkt) = self.to_bots.swap_remove(k);
                if let Some((_, client, _)) = self.bots.iter_mut().find(|(a, _, _)| *a == to) {
                    client.receive(server_addr, &pkt, now);
                }
            } else {
                k += 1;
            }
        }
        let measuring = self.render_from.is_some_and(|t| now >= t);
        for (i, (addr, client, brain)) in self.bots.iter_mut().enumerate() {
            if i == 0 && self.stall_bot0 {
                continue;
            }
            client.update(self.now);
            if client.state() == ClientState::Connected {
                while let Some((_, data)) = client.recv() {
                    brain.on_message(&data, self.now);
                }
                if self.render && brain.entities().is_some() {
                    let core = brain.core_mut();
                    if let Some(r) = core.render_step(now) {
                        let last = self.last_render.insert(i, r);
                        self.render_backwards += last.is_some_and(|l| r < l) as u64;
                        let (truth, err) = (&self.truth, &mut self.render_err);
                        core.render(now, |e, st| {
                            // Each entity is drawn at its own step (mid and far lag near).
                            if let (true, Some(t)) = (measuring, truth_at(truth, e, st.at)) {
                                let d = [0, 1, 2].map(|k| st.pos[k] - t[k]);
                                err[st.tier as usize].push((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt());
                            }
                        });
                    }
                }
                match self.frames_bot0.filter(|_| i == 0) {
                    // The game client's loop: each frame fires, then runs the
                    // input clock by the time since the last frame.
                    Some(fps) => {
                        let frame = Duration::from_secs(1) / fps;
                        // After a stall (or at first), the next frame is now.
                        let mut t = match self.last_frame {
                            Some(l) if now.saturating_duration_since(l) <= 3 * frame => l + frame,
                            _ => now,
                        };
                        while t <= now {
                            if let Some(g) = self.gunner.as_mut().filter(|g| g.bot == i) {
                                if let Some(d) = aim_and_fire(brain, g, t) {
                                    self.gunner_dirs.push(d);
                                }
                            }
                            for batch in brain.core_mut().tick_inputs(t, |_, _| Default::default()) {
                                client.send(Channel::Unreliable, batch).unwrap();
                            }
                            self.last_frame = Some(t);
                            t += frame;
                        }
                    }
                    None => {
                        if let Some(g) = self.gunner.as_mut().filter(|g| g.bot == i) {
                            if let Some(d) = aim_and_fire(brain, g, now) {
                                self.gunner_dirs.push(d);
                            }
                        }
                        if let Some(batch) = brain.tick_inputs(self.now) {
                            client.send(Channel::Unreliable, batch).unwrap();
                        }
                    }
                }
            }
            client.flush(self.now);
            for pkt in client.drain_outgoing() {
                let step = Duration::from_secs(1) / TICK_HZ;
                match &mut self.hold_bot0 {
                    Some(held) if i == 0 => held.push((*addr, pkt)),
                    _ if self.rng.chance(self.loss) => {}
                    // Held back a step, on top of the bot's lag.
                    _ if self.rng.chance(self.delay) => match self.lag[i] {
                        0 => self.delayed.push((*addr, pkt)),
                        lag => self.up_delayed.push((self.now + step * (lag + 1), *addr, pkt)),
                    },
                    _ if self.lag[i] > 0 => self.up_delayed.push((self.now + step * self.lag[i], *addr, pkt)),
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
        if self.send_early {
            // Each shard's datagrams arrive through the sender, from its task.
            let sent: Vec<std::sync::Mutex<Vec<Datagram>>> = (0..out.len()).map(|_| Default::default()).collect();
            let sender = |k: usize, bucket: &mut Vec<Datagram>| sent[k].lock().unwrap().append(bucket);
            self.server.tick_sending(&mut inbound, self.now, &mut out, Some(&sender));
            assert!(out.iter().all(|b| b.is_empty()), "the sender empties every bucket");
            for (b, s) in out.iter_mut().zip(sent) {
                *b = s.into_inner().unwrap();
            }
        } else {
            self.server.tick(&mut inbound, self.now, &mut out);
        }
        let work = self.fake_load.map_or(t.elapsed(), |f| period.mul_f32(f));
        self.server.observe_tick(work);
        self.last_out.clear();
        for (to, pkt) in out.into_iter().flatten() {
            self.last_out.push((to, pkt.len()));
            if self.rng.chance(self.loss) {
                continue;
            }
            let lag = self.bots.iter().position(|(a, _, _)| *a == to).map_or(0, |i| self.lag[i]);
            if self.down_jitter > 0 || lag > 0 {
                let extra = (self.rng.next_u64() % (self.down_jitter as u64 + 1)) as u32 + lag;
                self.to_bots.push((self.now + Duration::from_secs(1) / TICK_HZ * extra, to, pkt));
            } else if let Some((_, client, _)) = self.bots.iter_mut().find(|(a, _, _)| *a == to) {
                client.receive(server_addr, &pkt, self.now);
            }
        }
        if self.render {
            let states = self
                .bots
                .iter()
                .filter_map(|(_, _, b)| b.welcome().map(|w| w.entity))
                .filter_map(|e| self.server.entity_state(e).map(|s| (e, [s.pos[0], s.pos[1], s.z])))
                .collect();
            self.truth.push_back((self.server.step_number(), states));
            if self.truth.len() > 3 * TICK_HZ as usize {
                self.truth.pop_front();
            }
        }
        self.now += Duration::from_secs(1) / TICK_HZ;
    }

    fn corrections(&self) -> u64 {
        self.bots.iter().map(|(_, _, b)| b.stats().corrections).sum()
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
        let st = b.stats();
        assert_eq!((st.unmatched_acks, st.bad_messages, st.stale_snapshots), (0, 0, 0));
        let near = st.tier_seen[Tier::Near as usize] as f64 / st.snapshots as f64;
        assert!(near > 2.0, "near entities per snapshot {near}");
        assert_eq!(st.tier_seen[Tier::Far as usize], 0, "nobody is 500 m away");
    }
    // All 40 spawn in a 200 m disk; a 150 m near radius covers a good part of it.
    let (seen, snaps) = s.bots.iter().fold((0, 0), |a, (_, _, b)| (a.0 + b.stats().tier_seen[0], a.1 + b.stats().snapshots));
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
    let max_err = s.bots.iter().map(|(_, _, b)| b.stats().correction_error_max).fold(0.0, f32::max);
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
        assert!(b.stats().clock_extra >= 1, "the clock must run ahead to build a spare");
        assert!(b.stats().clock_extra + b.stats().clock_skipped <= 3, "clock hunting: {:?}", b.stats());
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
    assert_eq!(s.bots[0].2.stats().resyncs, 1);

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
    assert!(s.bots[0].2.stats().corrections > 0);
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
        assert_eq!(b.stats().resyncs, 0);
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
    let t = s.bots[0].2.entities().unwrap();
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
    let t = s.bots[0].2.entities().unwrap();
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
        assert_eq!((b.stats().level, b.stats().pace), (lattice_sim::ladder::MAX_LEVEL, 800), "clients are told");
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
        assert!(b.stats().tier_seen[0] > 0, "bot {i} counts near entities");
        assert_eq!(b.stats().pace, 1000, "and follows the pace");
        assert_eq!(samples.is_empty(), i % 2 == 1, "only full bots keep latency samples");
    }
}

/// Every bot tracks; checks each one's decoded near states against the
/// server's quantized states for the same ticks.
fn assert_near_states_exact(s: &Swarm) -> usize {
    let mut checked = 0;
    for (_, _, b) in &s.bots {
        assert_eq!(b.stats().near_decode_errors, 0, "a delta referred to a baseline the client lacks");
        let t = b.entities().unwrap();
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

#[test]
fn debug_capture_reports_what_the_watched_client_got() {
    let mut s = line_swarm(62, InterestConfig::default());
    for _ in 0..TICK_HZ {
        s.step();
    }
    let entity = s.bots[0].2.welcome().unwrap().entity;
    s.server.set_watch(Some(entity));
    let mut frame = None;
    for _ in 0..12 {
        s.step();
        frame = frame.or(s.server.take_debug_frame());
    }
    let f = frame.expect("a capture every 6th tick");
    let w = f.watched.unwrap();
    assert_eq!(w.entity, entity);
    assert_eq!(f.entities.len(), 62);
    assert!(!w.near.is_empty() && !w.mid.is_empty(), "{w:?}");
    assert!(w.near.iter().all(|&(_, age, sent, _)| sent == (age == 0)));
    assert_eq!(w.radii, [150.0, 500.0, 1500.0]);
    assert!(w.bytes > w.near_bytes);
}

#[test]
fn packets_fill_up_so_gso_padding_is_small() {
    // A tight near radius in a blob: clients get big mid tiers, several packets each.
    let cfg = SimConfig {
        spawn: SpawnMode::Blob,
        net: Config { pad_packets: true, max_packets_per_flush: 6, ..Config::default() },
        interest: InterestConfig {
            near_radius: 15.0,
            squad_size: 0,
            mid_period: 1,
            mid_per_tick: 256,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut s = Swarm::with_config(150, cfg);
    let mut multi = 0;
    for step in 0..6 * TICK_HZ {
        s.step();
        if step < 3 * TICK_HZ {
            continue;
        }
        // A client's packets are consecutive: all but its last are full size.
        for run in s.last_out.chunk_by(|a, b| a.0 == b.0) {
            multi += (run.len() > 1) as usize;
            for &(_, len) in &run[..run.len() - 1] {
                assert_eq!(len, lattice_net::packet::MAX_PACKET_SIZE);
            }
        }
    }
    assert!(multi > 1000, "clients got several packets per tick ({multi} times)");
    let net = s.server.net();
    let (mut padding, mut bytes, mut dropped) = (0, 0, 0);
    for id in net.client_ids() {
        let st = net.client_stats(id).unwrap();
        (padding, bytes, dropped) = (padding + st.padding_bytes, bytes + st.bytes_sent, dropped + st.unreliable_dropped);
    }
    assert_eq!(dropped, 0);
    let share = padding as f64 / bytes as f64;
    assert!(share < 0.02, "padding is {:.1}% of bytes", share * 100.0);
    assert_eq!(s.corrections(), 0);
}

/// Pairs of players closer than `d`, by the server's positions.
fn close_pairs(s: &Swarm, d: f32) -> usize {
    let pos: Vec<[f32; 2]> =
        s.bots.iter().filter_map(|(_, _, b)| s.server.entity_state(b.welcome()?.entity)).map(|m| m.pos).collect();
    let mut n = 0;
    for i in 0..pos.len() {
        for j in i + 1..pos.len() {
            n += ((pos[i][0] - pos[j][0]).powi(2) + (pos[i][1] - pos[j][1]).powi(2) < d * d) as usize;
        }
    }
    n
}

#[test]
fn crowds_are_pushed_apart_and_clients_tell_pushes_from_mispredictions() {
    // 150 players walking around inside a 3 m disk, far denser than they fit,
    // with and without separation (the bots are identical).
    let run = |separation: bool| {
        let cfg = SimConfig { spawn: SpawnMode::Disk(3.0), separation, ..Default::default() };
        let mut s = Swarm::with_config(150, cfg);
        let mut pairs = 0;
        for t in 0..5 * TICK_HZ {
            s.step();
            if t >= 2 * TICK_HZ {
                pairs += close_pairs(&s, 0.4);
            }
        }
        (s, pairs)
    };
    let (_, without) = run(false);
    let (s, with) = run(true);
    // Soft by design (at most 3 m/s): it can't stop players who keep walking
    // into an over-full crowd at 6 m/s, but it cuts the overlap by ~2x.
    assert!(with * 3 < without * 2, "separation keeps players apart: {without} -> {with} overlapping pairs");
    let pushes: u64 = s.bots.iter().map(|(_, _, b)| b.stats().push_corrections).sum();
    let max_push = s.bots.iter().map(|(_, _, b)| b.stats().push_error_max).fold(0.0f32, f32::max);
    assert!(pushes > 0, "pushes reach the clients");
    assert!(max_push < 0.25, "a push correction is a couple of ticks of push at most: {max_push} m");
    assert_eq!(s.corrections(), 0, "and nothing else mispredicts");
}

/// Every bot tracks and draws a frame each step; measured from `warm` on.
fn render_swarm(n: usize, cfg: SimConfig, warm: Duration) -> Swarm {
    let mut s = Swarm::with_config(n, cfg);
    for (_, _, b) in &mut s.bots {
        b.enable_tracking();
    }
    s.render = true;
    s.render_from = Some(s.now + warm);
    s
}

/// Entity-frames by tier and how they were drawn, over all bots.
fn frames(s: &Swarm) -> [[u64; 4]; 3] {
    let mut f = [[0u64; 4]; 3];
    for (_, _, b) in &s.bots {
        for (t, row) in b.entities().unwrap().smooth.frames.iter().enumerate() {
            for (h, n) in row.iter().enumerate() {
                f[t][h] += n;
            }
        }
    }
    f
}

fn pct(v: &mut [f32], p: f64) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1]
}

struct RenderReport {
    /// Share of entity-frames interpolated, by tier.
    interpolated: [f64; 3],
    /// Render error vs the server's states, p50 and p99 by tier, in meters.
    err: [(f32, f32); 3],
    /// Pops (mm) p50 and p99 by tier.
    pops: [(f32, f32); 3],
    /// Mean actual render delay, in steps.
    delay: f64,
}

/// Runs `secs` of measurement after the warm-up, and reports.
fn measure_render(s: &mut Swarm, warm_steps: u32, secs: u32) -> RenderReport {
    for _ in 0..warm_steps {
        s.step();
    }
    let f0 = frames(s);
    let mut pops: [Vec<u16>; 3] = Default::default();
    let d0: (u64, f64) = s.bots.iter().fold((0, 0.0), |a, (_, _, b)| (a.0 + b.stats().render_frames, a.1 + b.stats().render_delay_sum));
    for (_, _, b) in &mut s.bots {
        b.core_mut().drain_pops(&mut Default::default());
    }
    for _ in 0..secs * TICK_HZ {
        s.step();
    }
    for (_, _, b) in &mut s.bots {
        b.core_mut().drain_pops(&mut pops);
    }
    let d1: (u64, f64) = s.bots.iter().fold((0, 0.0), |a, (_, _, b)| (a.0 + b.stats().render_frames, a.1 + b.stats().render_delay_sum));
    let f1 = frames(s);
    let mut r = RenderReport { interpolated: [0.0; 3], err: [(0.0, 0.0); 3], pops: [(0.0, 0.0); 3], delay: (d1.1 - d0.1) / (d1.0 - d0.0).max(1) as f64 };
    for t in 0..3 {
        let n: u64 = (0..4).map(|h| f1[t][h] - f0[t][h]).sum();
        r.interpolated[t] = (f1[t][0] - f0[t][0]) as f64 / n.max(1) as f64;
        let e = &mut s.render_err[t];
        r.err[t] = (pct(e, 0.5), pct(e, 0.99));
        let mut p: Vec<f32> = pops[t].iter().map(|&x| x as f32).collect();
        r.pops[t] = (pct(&mut p, 0.5), pct(&mut p, 0.99));
        eprintln!(
            "{:?}: {:.2}% interpolated of {n} frames, error p50 {:.3} p99 {:.3} m ({} samples), pops p50 {} p99 {} mm",
            [Tier::Near, Tier::Mid, Tier::Far][t],
            r.interpolated[t] * 100.0,
            r.err[t].0,
            r.err[t].1,
            e.len(),
            r.pops[t].0,
            r.pops[t].1
        );
    }
    eprintln!("render delay {:.2} steps", r.delay);
    r
}

#[test]
fn render_timeline_on_a_clean_link() {
    // 60 bots roaming a 500 m disk: every tier in play.
    let cfg = SimConfig { spawn: SpawnMode::Disk(500.0), ..Default::default() };
    let mut s = render_swarm(60, cfg, Duration::from_secs(3));
    let r = measure_render(&mut s, 3 * TICK_HZ, 8);
    assert_eq!(s.render_backwards, 0, "render time never goes backwards");
    assert_eq!(s.corrections(), 0);
    assert!((r.delay - 2.0).abs() < 0.05, "near: 67 ms behind the newest step: {}", r.delay);
    // Near and mid are drawn between samples, to their quantization (near
    // ~8 mm; mid 16 mm across and 16 cm in height).
    assert!(r.interpolated[0] >= 0.999 && r.interpolated[1] >= 0.998, "{:?}", r.interpolated);
    assert!(r.err[0].1 < 0.02 && r.err[1].1 < 0.2, "{:?}", r.err);
    assert_eq!((r.pops[0].1, r.pops[1].1), (0.0, 0.0), "nothing near or mid pops");
    // Far (2 Hz) on the mid timeline: interpolated about half the time,
    // extrapolated the rest; errors of a few meters at 500+ m.
    assert!(r.interpolated[2] >= 0.4, "{:?}", r.interpolated);
    assert!(r.err[2].0 < 0.5 && r.err[2].1 < 5.0, "{:?}", r.err[2]);
    // Lag compensation's rewind on a zero-latency link: near targets, the
    // render delay (67 ms) + the spare input (33 ms) + waiting for the next
    // tick (33 ms); mid and far targets 133 ms more.
    let [near_rw, mid_rw] = s.server.take_rewind().map(|h| h.summary());
    eprintln!("rewind near p50 {} p99 {} max {}, mid p50 {} p99 {} max {} ms", near_rw.p50, near_rw.p99, near_rw.max, mid_rw.p50, mid_rw.p99, mid_rw.max);
    assert_eq!((near_rw.p50, near_rw.max, mid_rw.p50, mid_rw.max), (133, 133, 267, 267));
    assert_eq!(s.server.counters().render_ahead, 0);
}

#[test]
fn render_timeline_rides_out_loss_and_jitter() {
    let cfg = SimConfig { spawn: SpawnMode::Disk(500.0), ..Default::default() };
    let mut s = render_swarm(60, cfg, Duration::from_secs(3));
    s.loss = 0.05;
    s.down_jitter = 1;
    let r = measure_render(&mut s, 3 * TICK_HZ, 8);
    assert_eq!(s.render_backwards, 0);
    let snaps: u64 = s.bots.iter().map(|(_, _, b)| b.core().render_clock().snaps).sum();
    assert_eq!(snaps, 0, "jitter and loss are slewed through, never jumped");
    // 5% loss with a step of jitter: a lost update is bridged by the next in
    // both tiers (each is drawn two updates behind), so both interpolate.
    assert!(r.interpolated[0] >= 0.999 && r.interpolated[1] >= 0.99, "{:?}", r.interpolated);
    assert!(r.err[0].1 < 0.05 && r.err[1].1 < 0.3, "{:?}", r.err);
    let [near_rw, mid_rw] = s.server.take_rewind().map(|h| h.summary());
    eprintln!("rewind near p50 {} p99 {} max {}, mid p50 {} p99 {} max {} ms", near_rw.p50, near_rw.p99, near_rw.max, mid_rw.p50, mid_rw.p99, mid_rw.max);
    // The near delay grows a little to cover lost updates (~2.1 steps).
    assert!(r.delay < 2.5, "{}", r.delay);
    assert!(near_rw.p50 == 133 && near_rw.p99 <= 200 && mid_rw.p50 == 267 && mid_rw.p99 <= 300, "{near_rw:?} {mid_rw:?}");
}

#[test]
#[ignore = "measurement: cargo test --release --test swarm render_delay_sweep -- --ignored --nocapture"]
fn render_delay_sweep() {
    for (near, mid) in [(33, 133), (67, 133), (67, 200), (100, 267)] {
        for (loss, jitter) in [(0.0, 0), (0.05, 1)] {
            let cfg = SimConfig { spawn: SpawnMode::Disk(500.0), ..Default::default() };
            let mut s = render_swarm(60, cfg, Duration::from_secs(3));
            let ms = Duration::from_millis;
            // Fixed delays, for the sweep.
            let client = lattice_sim::bot::ClientConfig { near_delay: ms(near), near_delay_max: ms(near), mid_delay: ms(mid), track_entities: true };
            for (i, (_, _, b)) in s.bots.iter_mut().enumerate() {
                *b = BotBrain::with_config(i as u64, client.clone());
            }
            (s.loss, s.down_jitter) = (loss, jitter);
            eprintln!("== near {near} ms, mid {mid} ms, loss {loss}, jitter {jitter} step");
            measure_render(&mut s, 3 * TICK_HZ, 8);
            let [n, m] = s.server.take_rewind().map(|h| h.summary());
            eprintln!("rewind near p50 {} p99 {}, mid p50 {} p99 {} ms", n.p50, n.p99, m.p50, m.p99);
        }
    }
}

#[test]
fn render_timeline_at_20_hz_and_dilation() {
    // The ladder's bottom: 20 Hz ticks of 1 or 2 steps, at pace 0.8.
    let cfg = SimConfig { spawn: SpawnMode::Disk(300.0), ..Default::default() };
    let mut s = render_swarm(30, cfg, Duration::from_secs(32));
    s.fake_load = Some(0.95);
    let r = measure_render(&mut s, 32 * TICK_HZ, 6);
    assert_eq!(s.server.level(), lattice_sim::ladder::MAX_LEVEL);
    assert_eq!(s.render_backwards, 0);
    // Rendering on steps, not ticks: near keeps interpolating, the near delay
    // growing to cover 20 Hz ticks of 1 or 2 steps. Mid and far update every
    // 5 and 30 ticks down here.
    assert!(r.delay > 2.0 && r.delay <= 4.0 + 1e-6, "{}", r.delay);
    assert!(r.interpolated[0] >= 0.99 && r.err[0].1 < 0.02, "{:?} {:?}", r.interpolated, r.err);
    assert!(r.err[1].1 < 3.0, "{:?}", r.err);
}

#[test]
fn the_near_delay_grows_when_near_updates_come_less_often() {
    // 60 bots in a 30 m disk, with room for only 20 near updates per client
    // per tick: each near entity updates every ~3 ticks, irregularly, like
    // the near tier's cap in a big crowd. 67 ms would run past them.
    let interest = InterestConfig { near_per_tick: 20, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Disk(30.0), interest, ..Default::default() };
    // First with the near delay fixed at 67 ms, for comparison.
    let mut fixed = render_swarm(60, cfg.clone(), Duration::from_secs(4));
    let client = lattice_sim::bot::ClientConfig { near_delay_max: Duration::from_secs(2) / TICK_HZ, ..Default::default() };
    for (i, (_, _, b)) in fixed.bots.iter_mut().enumerate() {
        *b = BotBrain::with_config(i as u64, client.clone());
    }
    let f = measure_render(&mut fixed, 4 * TICK_HZ, 6);
    let mut s = render_swarm(60, cfg, Duration::from_secs(4));
    let r = measure_render(&mut s, 4 * TICK_HZ, 6);
    assert!(f.interpolated[0] < 0.9, "fixed at 67 ms, near extrapolates: {:?}", f.interpolated);
    assert_eq!(s.render_backwards, 0);
    assert!(r.delay > 2.5 && r.delay <= 4.0 + 1e-6, "near delay grew from 2 steps: {}", r.delay);
    eprintln!("near interpolated {:.4}", r.interpolated[0]);
    // Runs differ (the router's key is random, so shards and entity ids do):
    // 97.9-98.5% over 50 runs, median 98.2%. Fixed at 67 ms it's under 90%.
    assert!(r.interpolated[0] >= 0.97, "{:?}", r.interpolated);
    let [near_rw, mid_rw] = s.server.take_rewind().map(|h| h.summary());
    eprintln!("rewind near p50 {} p99 {}, mid p50 {} p99 {} ms", near_rw.p50, near_rw.p99, mid_rw.p50, mid_rw.p99);
    // Mid stays at 200 ms whatever near does: its lag shrank instead.
    assert!((260..=270).contains(&mid_rw.p50), "{mid_rw:?}");
}

#[test]
fn factions_follow_squads_and_stay_balanced() {
    let interest = InterestConfig { squad_size: 4, ..Default::default() };
    let s = {
        // Uniform: each squad has its own anchor, so bots can tell squads apart.
        let mut s = Swarm::with_config(36, SimConfig { spawn: SpawnMode::Uniform, interest, ..Default::default() });
        for _ in 0..TICK_HZ {
            s.step();
        }
        s
    };
    let mut per = [0; 3];
    let mut squads: HashMap<[u32; 2], (u8, usize)> = HashMap::new();
    for (_, _, b) in &s.bots {
        let w = b.welcome().unwrap();
        let f = lattice_game::faction::faction(w.entity);
        per[f as usize] += 1;
        let sq = squads.entry(w.anchor.map(f32::to_bits)).or_insert((f, 0));
        assert_eq!(f, sq.0, "a squad is one faction");
        sq.1 += 1;
    }
    assert!(squads.values().all(|&(_, n)| n == 4), "{squads:?}");
    assert_eq!(per, [12, 12, 12], "squads take turns");
}

#[test]
fn death_and_respawn_keep_prediction_exact() {
    let mut s = Swarm::new(12, SpawnMode::Blob);
    for (_, _, b) in &mut s.bots {
        b.enable_tracking();
    }
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    let me = s.bots[0].2.welcome().unwrap().entity;
    s.server.damage(me, 30);
    s.step();
    assert_eq!(s.server.vitals(me), Some((70, false)));
    s.server.damage(me, 200);
    for _ in 0..TICK_HZ {
        s.step();
    }
    assert_eq!(s.server.vitals(me), Some((0, true)));
    let b0 = &s.bots[0].2;
    assert!(b0.core().is_dead(), "the client knows");
    // Its inputs move nothing now, on the server as in its prediction.
    let at = s.server.entity_state(me).unwrap().pos;
    for _ in 0..TICK_HZ {
        s.step();
    }
    let later = s.server.entity_state(me).unwrap();
    assert!(((later.pos[0] - at[0]).powi(2) + (later.pos[1] - at[1]).powi(2)).sqrt() < 0.05, "the dead don't walk");
    // Everyone tracking it sees it dead, lying still: its AI keeps turning
    // (a stopped bot picks new headings), but the corpse keeps its aim.
    let seen = |s: &Swarm| s.bots[1..].iter().filter_map(|(_, _, b)| b.entities().and_then(|e| e.newest(me))).find(|x| x.dead);
    let body = seen(&s).expect("others see the body");
    for _ in 0..TICK_HZ {
        s.step();
    }
    let later = seen(&s).unwrap();
    assert_eq!((later.yaw, later.pitch, later.pos), (body.yaw, body.pitch, body.pos), "a still corpse");
    // 5 s after death it's back, at full health, somewhere else.
    for _ in 0..4 * TICK_HZ {
        s.step();
    }
    assert_eq!(s.server.vitals(me), Some((100, false)));
    assert!(!s.bots[0].2.core().is_dead());
    let c = s.server.counters();
    assert_eq!((c.deaths, c.respawns), (1, 1));
    // Dying and respawning are life events, never mispredictions.
    for (i, (_, _, b)) in s.bots.iter().enumerate() {
        assert_eq!((b.stats().corrections, b.stats().resyncs), (0, 0), "bot {i}");
    }
    assert_eq!(s.bots[0].2.stats().life_events, 2);
    assert_eq!(stand_ins(&s), 0);
}

#[test]
fn random_deaths_keep_everyone_in_step() {
    // 3 deaths a second among 40 players for 15 s (~15 dead at a time).
    let cfg = SimConfig { spawn: SpawnMode::Disk(60.0), deaths_per_sec: 3.0, ..Default::default() };
    let mut s = Swarm::with_config(40, cfg);
    for _ in 0..15 * TICK_HZ {
        s.step();
    }
    let c = s.server.counters().clone();
    assert!((35..=46).contains(&c.deaths), "{} deaths", c.deaths);
    assert!(c.respawns + 16 >= c.deaths && c.respawns <= c.deaths, "respawned 5 s later: {} of {}", c.respawns, c.deaths);
    let life: u64 = s.bots.iter().map(|(_, _, b)| b.stats().life_events).sum();
    assert!(life >= c.deaths + c.respawns - 40, "clients saw them: {life}");
    assert_eq!(s.corrections(), 0, "no misprediction from any death or respawn");
    assert_eq!(stand_ins(&s), 0);
}

#[test]
#[ignore = "diagnostic: cargo test --release --test swarm jitter_diagnostic -- --ignored --nocapture"]
fn jitter_diagnostic() {
    // Like the Windows check: a dense 80 m disk with random deaths.
    for deaths in [0.0, 6.0] {
        let cfg = SimConfig { spawn: SpawnMode::Disk(60.0), deaths_per_sec: deaths, ..Default::default() };
        let mut s = render_swarm(60, cfg, Duration::from_secs(4));
        let r = measure_render(&mut s, 4 * TICK_HZ, 10);
        let big = s.render_err[0].iter().filter(|&&e| e > 1.0).count();
        let changes: u64 = s.bots.iter().map(|(_, _, b)| b.stats().delay_changes).sum();
        let own = s.bots.iter().map(|(_, _, b)| b.stats().own_offset_max).fold(0.0f32, f32::max);
        eprintln!(
            "deaths {deaths}/s: near frames drawn >1 m from truth {big} of {} | delay changes {:.1} per bot | delay {:.2} steps | own offset max {own:.2} m | corrections {}",
            s.render_err[0].len(),
            changes as f64 / 60.0,
            r.delay,
            s.corrections()
        );
    }
}

/// A clear 50 m range: a shooter's spot and a target's, no cover within 45 m
/// of the middle and nothing but air between the shooter's eye and the
/// target's chest anywhere within 6 m of the line.
fn range(world: &lattice_game::world::World) -> ([f32; 2], [f32; 2]) {
    use lattice_game::hit;
    let y = 4096.0;
    for k in 0..300 {
        let x = 1200.0 + k as f32 * 37.0;
        let (a, b) = ([x, y], [x + 50.0, y]);
        let mut boxes = 0;
        world.boxes_near(x + 25.0, y, 45.0, |_| boxes += 1);
        if boxes > 0 {
            continue;
        }
        let eye = [a[0], a[1], world.terrain(a[0], a[1]) + 1.6];
        let clear = (-6..=6).all(|dy| {
            let (tx, ty) = (b[0], b[1] + dy as f32);
            let chest = [tx, ty, world.terrain(tx, ty) + 0.75];
            hit::terrain(world, eye, chest).is_none()
        });
        if clear {
            return (a, b);
        }
    }
    panic!("no clear range");
}

/// Two bots, shooter (bot 0, one-way `lag` steps) and target (bot 1), on a
/// clear range; the target strafes (or holds), the gunner fires for `secs`.
/// Returns the swarm, the target entity and the gunner's hits on it.
/// The target is invulnerable unless `lethal` (hit rates then measure aim,
/// not respawn timing).
#[allow(clippy::too_many_arguments)]
fn shoot_at(lag: u32, strafe: u32, head: bool, extra_lead: f32, secs: u32, lethal: bool) -> (Swarm, u16, Vec<lattice_sim::server::HitRecord>, Gunner) {
    use lattice_sim::bot::Moves;
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Line(50.0), interest, cone_of_fire: false, ..Default::default() };
    let mut s = render_swarm(2, cfg, Duration::from_secs(3600));
    s.lag = vec![lag, 0];
    for _ in 0..TICK_HZ {
        s.step();
    }
    let (a, b) = range(s.server.world());
    let (shooter, target) = (s.bots[0].2.welcome().unwrap().entity, s.bots[1].2.welcome().unwrap().entity);
    assert_ne!(lattice_game::faction::faction(shooter), lattice_game::faction::faction(target));
    s.server.teleport(shooter, a);
    s.server.teleport(target, b);
    s.server.set_invulnerable(target, !lethal);
    s.bots[0].2.set_moves(Moves::Hold);
    // `strafe`: steps between turns (0: holds still).
    s.bots[1].2.set_moves(if strafe > 0 { Moves::Strafe { period: strafe } } else { Moves::Hold });
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    s.server.take_rewind();
    s.gunner = Some(Gunner { bot: 0, target, head, extra_lead, shots: 0, ads: false, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
    for _ in 0..secs * TICK_HZ {
        s.step();
    }
    let g = s.gunner.take().unwrap();
    for _ in 0..TICK_HZ / 3 {
        s.step(); // the last shots land
    }
    let hits: Vec<_> = s.server.take_hits().into_iter().filter(|h| h.shooter == shooter && h.target == target).collect();
    (s, target, hits, g)
}

fn shoot(lag: u32, strafe: u32, head: bool, extra_lead: f32, secs: u32) -> (Swarm, u16, Vec<lattice_sim::server::HitRecord>, Gunner) {
    shoot_at(lag, strafe, head, extra_lead, secs, false)
}

#[test]
fn what_you_see_is_what_you_hit() {
    // A target running side to side at 6 m/s, 50 m out, turning every second.
    // The gunner aims where its client draws it (leading for the flight):
    // lag compensation must make that a hit, at 0 and 33 ms one-way.
    for lag in [0, 1] {
        let (mut s, _, hits, g) = shoot(lag, 30, false, 0.0, 20);
        let [near, _] = s.server.take_rewind().map(|h| h.summary());
        let c = s.server.counters();
        let rate = hits.len() as f64 / g.shots as f64;
        eprintln!("lag {lag}: {} of {} shots hit ({:.1}%), near rewind p50 {} ms, kills {}", hits.len(), g.shots, 100.0 * rate, near.p50, c.kills);
        assert!(g.shots > 100, "it fired: {}", g.shots);
        assert!(rate >= 0.95, "lag {lag}: hit {:.1}%", 100.0 * rate);
        assert_eq!((c.shots, c.shots_refused), (g.shots, 0), "every shot fired once, none refused");
        assert!(hits.iter().all(|h| h.rewind <= lattice_sim::shots::NEAR_CAP));
        assert_eq!(c.rewinds_trimmed, 0, "an honest shooter is never trimmed");
        assert_eq!(c.renders_held, 0, "nor held to a floor");
        assert_eq!(s.corrections(), 0, "shooting doesn't disturb prediction");
    }
}

#[test]
fn headshots_hit_the_head() {
    // A standing target: this checks the head hitbox, not leading a juke (a
    // runner turning every second would spoil 20% of head-sized leads).
    let (_, _, hits, g) = shoot(1, 0, true, 0.0, 10);
    let heads = hits.iter().filter(|h| h.head).count();
    eprintln!("{heads} head and {} body hits of {} shots", hits.len() - heads, g.shots);
    assert!(heads as f64 >= 0.97 * g.shots as f64, "{heads} of {}", g.shots);
}

#[test]
fn beyond_the_cap_the_shooter_leads() {
    // 133 ms one-way: the shooter's view is ~13 steps behind, over the 9-step
    // near cap. Aiming at what it draws misses a runner...
    // (A runner turning every 3 s: leading a turn isn't what's tested.)
    let (s, _, hits, g) = shoot(4, 90, false, 0.0, 15);
    let rate = hits.len() as f64 / g.shots as f64;
    assert!(s.server.counters().rewinds_capped > 0);
    assert!(hits.iter().all(|h| h.rewind <= lattice_sim::shots::NEAR_CAP));
    eprintln!("past the cap: {:.1}% of {} shots hit", 100.0 * rate, g.shots);
    assert!(rate < 0.5, "{rate}");
    // ...and leading by what the cap clipped hits again. (The view is behind
    // by the render delay + RTT + the wait: ~4 steps past the cap here.)
    let best = (2..=6)
        .map(|lead| {
            let (_, _, hits, g) = shoot(4, 90, false, lead as f32, 10);
            let rate = hits.len() as f64 / g.shots as f64;
            eprintln!("  leading {lead} steps more: {:.1}%", 100.0 * rate);
            rate
        })
        .fold(0.0, f64::max);
    assert!(best >= 0.9, "leading by the clipped time hits: {best}");
}

#[test]
fn walls_stop_shots() {
    use lattice_sim::bot::Moves;
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Line(50.0), interest, cone_of_fire: false, ..Default::default() };
    let mut s = render_swarm(2, cfg, Duration::from_secs(3600));
    for _ in 0..TICK_HZ {
        s.step();
    }
    // A tall wall running north-south; the target stands right behind it.
    let w = s.server.world();
    let wall = *w
        .boxes()
        .iter()
        .find(|b| {
            let c = [(b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0];
            b.max[0] - b.min[0] < 1.0 && b.max[1] - b.min[1] > 6.0 && b.top - w.terrain(c[0] + 1.5, c[1]) > 2.6 && b.top - w.terrain(c[0] - 20.0, c[1]) > 0.5
        })
        .expect("a wall");
    let c = [(wall.min[0] + wall.max[0]) / 2.0, (wall.min[1] + wall.max[1]) / 2.0];
    let (shooter, target) = (s.bots[0].2.welcome().unwrap().entity, s.bots[1].2.welcome().unwrap().entity);
    s.server.teleport(shooter, [c[0] - 20.0, c[1]]);
    s.server.teleport(target, [wall.max[0] + 0.6, c[1]]);
    s.bots[0].2.set_moves(Moves::Hold);
    s.bots[1].2.set_moves(Moves::Hold);
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
    for _ in 0..5 * TICK_HZ {
        s.step();
    }
    let c = s.server.counters();
    let g = s.gunner.unwrap();
    assert!(g.shots > 30);
    assert_eq!(c.hits_body + c.hits_head, 0, "nothing goes through a wall");
    assert!(c.hits_cover + c.hits_ground >= g.shots - 2, "they stop on it: cover {} ground {}", c.hits_cover, c.hits_ground);
    assert!(c.hits_cover > 0);
}

#[test]
fn five_body_hits_kill() {
    let (s, target, hits, _) = shoot_at(0, 0, false, 0.0, 12, true);
    // Group hits by the target's lives: each kill took exactly 100 HP. Hits
    // that land after it died (the shooter hadn't seen the death) deal none.
    let mut taken = 0u32;
    let mut kills = 0;
    for h in hits.iter().filter(|h| h.damage > 0) {
        assert!(!h.head);
        taken += h.damage as u32;
        assert_eq!(h.killed, taken >= 100, "the {}th HP point kills, no sooner", taken);
        if h.killed {
            (taken, kills) = (0, kills + 1);
        }
    }
    assert!(kills >= 2, "{kills} kills in 12 s (respawning after 5 s)");
    let c = s.server.counters();
    assert_eq!(c.kills, kills);
    assert!(c.hits_too_late > 0 && c.hits_too_late as usize == hits.len() - hits.iter().filter(|h| h.damage > 0).count());
    let _ = target;
}

/// A shooter's spot and a target's `dist` m east of it with a clear line
/// from the shooter's eye to the target's chest (terrain and cover).
fn line_of_fire(world: &lattice_game::world::World, dist: f32) -> ([f32; 2], [f32; 2]) {
    use lattice_game::hit;
    for k in 0..2000 {
        let (x, y) = (1200.0 + (k % 40) as f32 * 130.0, 1200.0 + (k / 40) as f32 * 97.0);
        let (a, b) = ([x, y], [x + dist, y]);
        let eye = [a[0], a[1], world.terrain(a[0], a[1]) + 1.6];
        let chest = [b[0], b[1], world.terrain(b[0], b[1]) + 0.75];
        if hit::terrain(world, eye, chest).is_some() {
            continue;
        }
        let mut blocked = false;
        world.boxes_near(x + dist / 2.0, y, dist / 2.0 + 2.0, |c| {
            blocked |= hit::aabb(eye, chest, [c.min[0], c.min[1], c.bottom], [c.max[0], c.max[1], c.top]).is_some();
            // Also keep cover off both spots.
            blocked |= [a, b].iter().any(|p| p[0] > c.min[0] - 2.0 && p[0] < c.max[0] + 2.0 && p[1] > c.min[1] - 2.0 && p[1] < c.max[1] + 2.0);
        });
        if !blocked {
            return (a, b);
        }
    }
    panic!("no clear line of fire at {dist} m");
}

/// Three bots, no squads (three factions): bot 0 shoots bot 1 from `dist` m,
/// bot 2 stands 5 m behind the shooter. All keep their combat news. No cone
/// of fire: shots go exactly where aimed.
fn firing_line(dist: f32, lethal: bool) -> (Swarm, [u16; 3]) {
    firing_line_with(dist, lethal, false)
}

fn firing_line_with(dist: f32, lethal: bool, cone_of_fire: bool) -> (Swarm, [u16; 3]) {
    use lattice_sim::bot::Moves;
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    // A fixed secret for where shots go in their cones, so runs repeat.
    let cfg = SimConfig { spawn: SpawnMode::Line(50.0), interest, cone_of_fire, spread_secret: Some([7; 32]), ..Default::default() };
    let mut s = render_swarm(3, cfg, Duration::from_secs(3600));
    for _ in 0..TICK_HZ {
        s.step();
    }
    let (a, b) = line_of_fire(s.server.world(), dist);
    let e = [0, 1, 2].map(|i| s.bots[i].2.welcome().unwrap().entity);
    s.server.teleport(e[0], a);
    s.server.teleport(e[1], b);
    s.server.teleport(e[2], [a[0] - 5.0, a[1] + 3.0]);
    s.server.set_invulnerable(e[1], !lethal);
    for (_, _, bot) in &mut s.bots {
        bot.set_moves(Moves::Hold);
        bot.core_mut().keep_news(true);
    }
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    (s, e)
}

fn news(s: &mut Swarm, bot: usize) -> (Vec<lattice_game::events::Event>, Vec<lattice_game::events::SeenShot>) {
    let (mut ev, mut sh) = (Vec::new(), Vec::new());
    s.bots[bot].2.core_mut().drain_news(&mut ev, &mut sh);
    (ev, sh)
}

#[test]
fn combat_news_reaches_the_right_players() {
    use lattice_game::events::{direction, Event};
    let (mut s, [shooter, target, bystander]) = firing_line(50.0, true);
    news(&mut s, 0);
    news(&mut s, 1);
    news(&mut s, 2);
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
    for _ in 0..10 * TICK_HZ {
        s.step();
    }
    let g = s.gunner.take().unwrap();
    for _ in 0..TICK_HZ {
        s.step();
    }
    let hits: Vec<_> = s.server.take_hits().into_iter().filter(|h| h.damage > 0).collect();
    let kills = hits.iter().filter(|h| h.killed).count();
    assert!(hits.len() >= 10 && kills >= 1, "{} hits, {kills} kills", hits.len());
    // The shooter: a hit marker for every hit that dealt damage, kills marked.
    let (ev, _) = news(&mut s, 0);
    let marks: Vec<_> = ev.iter().filter_map(|e| match *e {
        Event::Hit { target: t, damage, killed, .. } => Some((t, damage, killed)),
        _ => None,
    }).collect();
    assert_eq!(marks.len(), hits.len());
    assert!(marks.iter().all(|&(t, d, _)| t == target && d == 20));
    assert_eq!(marks.iter().filter(|m| m.2).count(), kills);
    let kill_news = |ev: &[Event]| ev.iter().filter(|e| matches!(e, Event::Kill { killer, victim, .. } if *killer == shooter && *victim == target)).count();
    assert_eq!(kill_news(&ev), kills);
    // The target: hurt, from the shooter's direction (it stands due west).
    let (ev, _) = news(&mut s, 1);
    let hurts: Vec<_> = ev.iter().filter_map(|e| match *e {
        Event::Hurt { from, amount, dir } => Some((from, amount, dir)),
        _ => None,
    }).collect();
    assert_eq!(hurts.len(), hits.len());
    let west = direction([1.0, 0.0], [0.0, 0.0]);
    assert!(hurts.iter().all(|&(f, a, d)| f == shooter && a == 20 && (d as i32 - west as i32).abs() <= 2), "{hurts:?}");
    assert_eq!(kill_news(&ev), kills);
    // The bystander: no news of a fight it isn't in, but the shooter's
    // tracers (it's near-tier to it), each at its aim and time.
    let (ev, seen) = news(&mut s, 2);
    assert!(ev.is_empty(), "{ev:?}");
    assert!(seen.iter().all(|t| t.shooter == shooter));
    assert!(seen.len() as u64 >= g.shots - 2 && seen.len() as u64 <= g.shots, "{} of {} shots seen", seen.len(), g.shots);
    let _ = bystander;
    let c = s.server.counters();
    assert!(c.events > 0 && c.tracer_bytes > 0);
}

#[test]
fn a_shooter_joins_its_targets_near_tier() {
    // 200 m apart: mid tier to each other (near is 150 m). Hits make them
    // near-tier for 5 s ("interaction"), then they drop back.
    let (mut s, [shooter, target, _]) = firing_line(200.0, false);
    let tier = |s: &Swarm| s.bots[1].2.entities().and_then(|e| e.get(shooter)).map(|k| k.tier);
    assert_eq!(tier(&s), Some(Tier::Mid));
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
    for _ in 0..2 * TICK_HZ {
        s.step();
    }
    s.gunner = None;
    let hits = s.server.take_hits().len();
    assert!(hits > 5, "it hits at 200 m: {hits}");
    assert_eq!(tier(&s), Some(Tier::Near), "the shooter is near-tier to its target");
    for _ in 0..7 * TICK_HZ {
        s.step();
    }
    assert_eq!(tier(&s), Some(Tier::Mid), "and drops back 5 s after the last hit");
}

/// 9 bots per latency class (one-way `lags` steps), 3 factions, all fighting
/// with the same aim error in a 40 m disk; everyone invulnerable (hit rates
/// then measure aim and lag compensation, not respawns). Returns per class
/// (shots, hits, near rewind p50 ms) and the rewinds capped.
/// Per class (shots, hits, hits' near rewind p50 in ms), then rewinds capped
/// and shots trimmed (by the backtrack bound) and held (to their shooter's
/// render clock) during the fight.
fn fight(lags: &[u32], secs: u32) -> (Vec<(u64, u64, f64)>, u64, u64, u64) {
    fight_on(lags, secs, (0.0, 0.0, 0))
}

/// `fight_with`, the server ticking the usual way.
fn fight_on(lags: &[u32], secs: u32, link: (f32, f32, u32)) -> (Vec<(u64, u64, f64)>, u64, u64, u64) {
    fight_with(lags, secs, link, false)
}

/// `fight` on links that lose `link.0` of datagrams, hold `link.1` of the
/// bots' back a step, and hold the server's back up to `link.2` steps; with
/// `send_early`, the server sends each shard's datagrams from its own task.
fn fight_with(lags: &[u32], secs: u32, link: (f32, f32, u32), send_early: bool) -> (Vec<(u64, u64, f64)>, u64, u64, u64) {
    use lattice_sim::bot::FightConfig;
    let n = 9 * lags.len();
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Disk(40.0), interest, cone_of_fire: false, ..Default::default() };
    let mut s = Swarm::with_config(n, cfg);
    s.render = true;
    s.send_early = send_early;
    (s.loss, s.delay, s.down_jitter) = link;
    for (i, (_, _, b)) in s.bots.iter_mut().enumerate() {
        // Not down the sights: that slows them, which hides what lag costs.
        b.set_fight(Some(FightConfig { ads: false, ..FightConfig::default() }));
        s.lag[i] = lags[i % lags.len()];
    }
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let ents: Vec<u16> = s.bots.iter().map(|(_, _, b)| b.welcome().unwrap().entity).collect();
    for &e in &ents {
        s.server.set_invulnerable(e, true);
    }
    s.server.take_hits();
    let c = s.server.counters();
    let (capped0, trimmed0, held0) = (c.rewinds_capped, c.rewinds_trimmed, c.renders_held);
    let shots0: Vec<u64> = s.bots.iter().map(|(_, _, b)| b.stats().shots).collect();
    for _ in 0..secs * TICK_HZ {
        s.step();
    }
    let hits = s.server.take_hits();
    let mut per = vec![(0u64, 0u64, Vec::new()); lags.len()];
    for (i, (_, _, b)) in s.bots.iter().enumerate() {
        let c = i % lags.len();
        per[c].0 += b.stats().shots - shots0[i];
        per[c].1 += hits.iter().filter(|h| h.shooter == ents[i]).count() as u64;
        per[c].2.extend(hits.iter().filter(|h| h.shooter == ents[i]).map(|h| h.rewind));
    }
    if link == (0.0, 0.0, 0) {
        assert_eq!(s.corrections(), 0);
    }
    let out = per.into_iter().map(|(shots, hits, mut rw)| {
        let p50 = pct(&mut rw.iter().map(|&r| r as f32).collect::<Vec<_>>(), 0.5) as f64 * 1000.0 / 30.0;
        rw.clear();
        (shots, hits, p50)
    }).collect();
    let c = s.server.counters();
    (out, c.rewinds_capped - capped0, c.rewinds_trimmed - trimmed0, c.renders_held - held0)
}

#[test]
fn honest_fighters_on_bad_links_are_never_held_or_trimmed() {
    // 5% loss both ways, a fifth of the bots' datagrams a step late, the
    // server's up to a step late, at three latencies: what jitter and loss
    // do to the render clock and to arrival times stays inside the render
    // floor's allowance and the backtrack bound.
    let (per, _, trimmed, held) = fight_on(&[0, 1, 4], 15, (0.05, 0.2, 1));
    eprintln!("bad links: {per:?}, held {held}, trimmed {trimmed}");
    assert!(per.iter().all(|p| p.0 > 300), "{per:?}");
    assert_eq!(held, 0, "an honest fighter is never held to its render clock");
    // The backtrack bound's slack is nearly used up here: a mid/far claim
    // whose input came in late (its server wait gone) sometimes lands up to
    // a step past it, about 1 shot in 60,000 (1 in 30 runs of ~2,000).
    assert!(trimmed <= 2, "{trimmed} trimmed");
}

#[test]
fn spending_the_jitter_allowance_on_stale_shots_is_flagged() {
    // On a bad link the render floor allows for jitter, so in any one claim
    // a step of jitter and a step of lying look alike. Half these fighters
    // cheat: each shot, and the input made just before it (which bounds the
    // shot's view), claims to have drawn a step further back than it did.
    // An honest client's dips into the allowance come from its link,
    // whenever it shoots; the cheats' come right before shots.
    use lattice_sim::bot::FightConfig;
    use lattice_sim::shots::DIP_MIN_SHOTS;
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Disk(40.0), interest, cone_of_fire: false, ..Default::default() };
    let mut s = Swarm::with_config(18, cfg);
    s.render = true;
    (s.loss, s.delay, s.down_jitter) = (0.05, 0.2, 1);
    for (i, (_, _, b)) in s.bots.iter_mut().enumerate() {
        b.set_fight(Some(FightConfig { ads: false, ..FightConfig::default() }));
        if i % 2 == 1 {
            b.core_mut().claim_stale_shots(1.0);
        }
    }
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let ents: Vec<u16> = s.bots.iter().map(|(_, _, b)| b.welcome().unwrap().entity).collect();
    for &e in &ents {
        s.server.set_invulnerable(e, true);
    }
    for _ in 0..25 * TICK_HZ {
        s.step();
    }
    let dips = s.server.shot_dips();
    let (mut honest, mut cheats) = (Vec::new(), Vec::new());
    for (i, e) in ents.iter().enumerate() {
        if let Some(&(_, d)) = dips.iter().find(|d| d.0 == *e).filter(|d| d.1.judged()) {
            if i % 2 == 1 { &mut cheats } else { &mut honest }.push(d);
        }
    }
    let show = |v: &[lattice_sim::shots::Dips]| v.iter().map(|d| format!("{:.2} ({:.1} se)", d.excess, d.z)).collect::<Vec<_>>();
    eprintln!("pre-shot dip excess, steps: honest {:?}, cheats {:?}", show(&honest), show(&cheats));
    assert!(honest.len() >= 6 && cheats.len() >= 6, "enough of each fired {DIP_MIN_SHOTS}+ shots");
    assert!(honest.iter().all(|d| !d.flagged()), "no honest fighter flagged");
    assert!(cheats.iter().all(|d| d.flagged()), "every cheat flagged");
}

#[test]
fn sending_during_assembly_fights_the_same() {
    // The server hands each shard's datagrams over from the shard's own task
    // (`tick_sending`, lattice-server's default) instead of after the tick:
    // the same fight, no corrections, nobody trimmed or held, alike hit rates.
    let clean = (0.0, 0.0, 0);
    let (late, ..) = fight_with(&[0, 1], 10, clean, false);
    let (early, _, trimmed, held) = fight_with(&[0, 1], 10, clean, true);
    let rate = |p: &[(u64, u64, f64)]| p.iter().map(|&(s, h, _)| h as f64 / s as f64).collect::<Vec<_>>();
    eprintln!("hit rates: after the tick {:?}, during assembly {:?}", rate(&late), rate(&early));
    assert_eq!((trimmed, held), (0, 0));
    for (a, b) in rate(&late).iter().zip(rate(&early)) {
        assert!((a - b).abs() < 0.1, "{a} vs {b}");
    }
}

#[test]
#[ignore = "measurement: cargo test --release --test swarm fight_classes_sweep -- --ignored --nocapture"]
fn fight_classes_sweep() {
    let lags = [0, 1, 4, 6];
    let (per, capped, trimmed, held) = fight(&lags, 30);
    for (lag, (shots, hits, rw)) in lags.iter().zip(&per) {
        eprintln!("one-way {} ms: {hits} of {shots} shots hit ({:.1}%), hits' near rewind p50 {rw:.0} ms", lag * 33, 100.0 * *hits as f64 / *shots as f64);
    }
    eprintln!("rewinds capped: {capped}, trimmed: {trimmed}, held: {held}");
}

#[test]
fn latency_classes_hit_alike_within_the_cap() {
    // Same aim error, three latency classes: ~33 and ~100 ms RTT are fully
    // compensated (rewinds under the 300 ms near cap) and must hit alike;
    // ~300 ms RTT is ~100 ms past the cap: aiming at what it draws (not
    // leading by the clipped time), it hits measurably less.
    let (per, capped, trimmed, held) = fight(&[0, 1, 4], 20);
    let rate: Vec<f64> = per.iter().map(|&(shots, hits, _)| hits as f64 / shots as f64).collect();
    eprintln!("hit rates by class: {rate:?}, rewinds capped {capped}, trimmed {trimmed}, held {held}");
    // Honest shooters at every RTT: past the cap they lead, never trimmed
    // (the mid lag used to jump while the near clock slewed to a new delay,
    // claiming mid targets up to a step past their delay).
    assert_eq!((trimmed, held), (0, 0), "an honest fighter is never trimmed or held");
    assert!(per.iter().all(|p| p.0 > 600), "{per:?}");
    assert!((rate[0] - rate[1]).abs() <= 0.1 * rate[0], "within the cap, alike: {rate:?}");
    assert!(rate[2] < 0.8 * rate[0], "past the cap, measurably less: {rate:?}");
    assert!(capped > 0);
}

#[test]
fn backtrack_claims_are_trimmed() {
    // A cheat claims it was looking 10 steps further in the past than it
    // was, and aims where the target was then. The server holds the claim
    // to its render clock (its inputs claimed fresher views) or trims it to
    // what an honest client could have seen (RTT + wait + the longest render
    // delays + slack), so against a runner it mostly misses.
    let (mut s, target, _, _) = shoot(0, 30, false, 0.0, 1);
    s.server.take_hits();
    let shots0 = s.server.counters().shots;
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 10.0, hold: false, fixed: None, pick_back: 0.0 });
    for _ in 0..10 * TICK_HZ {
        s.step();
    }
    s.gunner = None;
    for _ in 0..TICK_HZ / 3 {
        s.step();
    }
    let c = s.server.counters().clone();
    let shots = c.shots - shots0;
    let hits = s.server.take_hits().len();
    let cut = c.rewinds_trimmed + c.renders_held;
    eprintln!("backtracking: {hits} of {shots} hit, {} held, {} trimmed", c.renders_held, c.rewinds_trimmed);
    assert!(cut as f64 >= 0.95 * shots as f64, "{cut} of {shots} cut");
    assert!((hits as f64) < 0.3 * shots as f64, "the cheat mostly misses: {hits} of {shots}");
}

/// A gunner that holds its aim on a spot its strafing target runs through
/// (50 m out, 6 m/s, turning every second) and fires as fast as the rifle
/// allows, for `secs`. With `pick_back` it's a cheat: each shot claims
/// whichever render step, up to that many steps back, lined the target up
/// with its crosshair; with `stale_inputs` too, its inputs all claim to have
/// drawn that many steps further back than they did. Returns hits, shots,
/// and how many shots were held to its render clock (`floor`: the render
/// floor on or off) or trimmed.
fn held_aim(pick_back: f32, stale_inputs: f64, floor: bool, secs: u32) -> (usize, u64, u64, u64) {
    use lattice_sim::bot::Moves;
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Line(50.0), interest, cone_of_fire: false, render_floor: floor, ..Default::default() };
    let mut s = render_swarm(2, cfg, Duration::from_secs(3600));
    for _ in 0..TICK_HZ {
        s.step();
    }
    let (a, b) = range(s.server.world());
    let (shooter, target) = (s.bots[0].2.welcome().unwrap().entity, s.bots[1].2.welcome().unwrap().entity);
    s.server.teleport(shooter, a);
    s.server.teleport(target, b);
    s.server.set_invulnerable(target, true);
    s.bots[0].2.set_moves(Moves::Hold);
    s.bots[0].2.core_mut().claim_stale_inputs(stale_inputs);
    s.bots[1].2.set_moves(Moves::Strafe { period: 30 });
    // Half a strafe in, so the held aim is mid-run: the target crosses it
    // once a second.
    for _ in 0..2 * TICK_HZ + 15 {
        s.step();
    }
    let c0 = s.server.counters().clone();
    s.server.take_hits();
    let hold = true;
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold, fixed: None, pick_back });
    for _ in 0..secs * TICK_HZ {
        s.step();
    }
    let g = s.gunner.take().unwrap();
    for _ in 0..TICK_HZ / 3 {
        s.step();
    }
    let hits = s.server.take_hits().into_iter().filter(|h| h.shooter == shooter && h.target == target).count();
    let c = s.server.counters();
    (hits, g.shots, c.renders_held - c0.renders_held, c.rewinds_trimmed - c0.rewinds_trimmed)
}

#[test]
fn picking_the_best_render_step_for_each_shot_is_floored() {
    // The backtrack bound allows claims up to the longest render delay
    // anyone may use, plus slack. A cheat that draws near players 2 steps
    // behind can claim up to ~4 more, and pick, shot by shot, whichever
    // moment of a runner's past lined up with a crosshair it doesn't move.
    // The render floor holds its claims to its own render clock: it can't
    // claim older than its inputs said it drew, and getting stale again
    // after a fresh claim takes time. So it hits about as an honest gunner
    // holding the same aim does, even when every input lies too.
    let (honest, shots_h, floored_h, _) = held_aim(0.0, 0.0, true, 20);
    let (free, shots_f, _, trimmed_f) = held_aim(4.0, 0.0, false, 20);
    let (cheat, shots_c, floored_c, trimmed_c) = held_aim(4.0, 0.0, true, 20);
    let (stale, shots_s, floored_s, trimmed_s) = held_aim(4.0, 4.0, true, 20);
    eprintln!("held aim: honest {honest} of {shots_h} hit; picking its render step, {free} of {shots_f} without the floor ({trimmed_f} trimmed), {cheat} of {shots_c} with it ({floored_c} held, {trimmed_c} trimmed), {stale} of {shots_s} with stale inputs too ({floored_s} held, {trimmed_s} trimmed)");
    assert_eq!(floored_h, 0, "an honest gunner is never held");
    assert!(shots_h > 150 && shots_f > 150 && shots_c > 150 && shots_s > 150);
    assert!(free as f64 > 1.5 * honest as f64, "without the floor the cheat works: {free} vs {honest}");
    for (name, hits) in [("picking", cheat), ("with stale inputs", stale)] {
        assert!((hits as f64) < 1.3 * honest as f64 + 3.0, "{name}: about as an honest gunner: {hits} vs {honest}");
    }
    assert!(floored_c > 0);
}

/// A gunner (bot 0) firing at a standing target 40 m away as fast as the
/// rifle allows, run like the game client (`frames`: frames a second, its
/// input clock by elapsed time) or like a bot (a step per tick); three
/// times it freezes completely (a shader compile, an alt-tab) for 100, 300
/// and 1,000 ms. Returns shots, hits, and shots held to its render clock,
/// trimmed, refused and dropped (a resync skipped their input), and how
/// often its render step went backwards.
fn stalled_gunner(frames: Option<u32>) -> [u64; 7] {
    let (mut s, [shooter, target, _]) = firing_line(40.0, false);
    s.frames_bot0 = frames;
    let hold = false;
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold, fixed: None, pick_back: 0.0 });
    for _ in 0..TICK_HZ {
        s.step();
    }
    let c0 = s.server.counters().clone();
    let (backwards0, dropped0) = (s.render_backwards, s.bots[0].2.stats().shots_dropped);
    s.server.take_hits();
    for stall_ms in [100u32, 300, 1000] {
        s.stall_bot0 = true;
        for _ in 0..(stall_ms * TICK_HZ).div_ceil(1000) {
            s.step();
        }
        s.stall_bot0 = false;
        for _ in 0..2 * TICK_HZ {
            s.step();
        }
    }
    let hits = s.server.take_hits().iter().filter(|h| h.shooter == shooter && h.target == target).count() as u64;
    let c = s.server.counters();
    [
        c.shots - c0.shots,
        hits,
        c.renders_held - c0.renders_held,
        c.rewinds_trimmed - c0.rewinds_trimmed,
        c.shots_refused - c0.shots_refused,
        s.bots[0].2.stats().shots_dropped - dropped0,
        s.render_backwards - backwards0,
    ]
}

#[test]
fn a_game_client_that_stalls_is_never_held() {
    // After a stall the client reads the snapshots that queued up all at
    // once, makes the inputs it missed at once (up to 300 ms of them; the
    // server fills the rest with stand-ins, and it resyncs), and fires
    // again. None of its shots may be held to its render clock or trimmed,
    // and its render step mustn't jump back. (Each stall also costs ~4 shots
    // of the 20 in the next 2 s: after a resync the input clock over-builds
    // its spare and drains it at 5%.)
    let [shots, hits, held, trimmed, refused, dropped, backwards] = stalled_gunner(Some(60));
    eprintln!("game client: {hits} hits of {shots} shots; held {held}, trimmed {trimmed}, refused {refused}, dropped {dropped}; render back {backwards}x");
    assert!(shots > 40, "it kept firing: {shots}");
    assert_eq!((held, trimmed), (0, 0), "never held or trimmed");
    assert_eq!(backwards, 0, "the render step never went back");
    // A bot (a step per tick) stalls the same way: never held either. Its
    // links here have no RTT at all, so the backtrack bound has no margin
    // while its near delay sits at the most after a stall: a shot can land
    // a hair past it (0.02 steps, 33 steps after a 1 s stall).
    let [shots, hits, held, trimmed, ..] = stalled_gunner(None);
    eprintln!("bot: {hits} hits of {shots} shots; held {held}, trimmed {trimmed}");
    assert_eq!(held, 0);
    assert!(trimmed <= 1, "{trimmed}");
}

#[test]
fn hits_from_an_earlier_life_deal_nothing() {
    // The target's life changes every 2 ticks (a teleport onto the same spot
    // is a life change, like a respawn): every hit lands on a life the
    // shooter saw that has since ended, so none deals damage.
    let (mut s, [_, target, _]) = firing_line(100.0, true);
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
    for k in 0..4 * TICK_HZ {
        if k % 2 == 0 {
            let p = s.server.entity_state(target).unwrap().pos;
            s.server.teleport(target, p);
        }
        s.step();
    }
    let hits = s.server.take_hits();
    eprintln!("{} hits", hits.len());
    assert!(hits.len() > 10, "it hits: {}", hits.len());
    assert!(hits.iter().all(|h| h.damage == 0 && !h.killed));
    assert_eq!(s.server.vitals(target), Some((100, false)));
    assert_eq!(s.server.counters().hits_too_late as usize, hits.len());
}

#[test]
fn distant_fights_are_seen() {
    use lattice_sim::bot::Moves;
    // Six shooters in a 15 m line fire east; watchers stand 40 m (near),
    // 400 m (mid), 1,000 m (far) and 2,000 m (past the far radius) south.
    let interest = InterestConfig { squad_size: 0, ..Default::default() };
    let cfg = SimConfig { spawn: SpawnMode::Line(50.0), interest, ..Default::default() };
    let mut s = render_swarm(10, cfg, Duration::from_secs(3600));
    for _ in 0..TICK_HZ {
        s.step();
    }
    let p = [3000.0, 4000.0];
    let e: Vec<u16> = (0..10).map(|i| s.bots[i].2.welcome().unwrap().entity).collect();
    for (k, &id) in e[..6].iter().enumerate() {
        s.server.teleport(id, [p[0], p[1] + 3.0 * k as f32]);
        s.server.set_invulnerable(id, true);
    }
    let watch = [40.0, 400.0, 1000.0, 2000.0];
    for (i, d) in watch.iter().enumerate() {
        s.server.teleport(e[6 + i], [p[0] - 20.0, p[1] - d]);
    }
    for (_, _, bot) in &mut s.bots {
        bot.set_moves(Moves::Hold);
        bot.set_heading(0.0);
        bot.core_mut().keep_news(true);
    }
    for _ in 0..3 * TICK_HZ {
        s.step();
    }
    let mut ambient = vec![Vec::new(); 4];
    let drain = |s: &mut Swarm, ambient: &mut Vec<Vec<lattice_client_core::AmbientShot>>| {
        for (w, out) in ambient.iter_mut().enumerate() {
            let now = s.now;
            s.bots[6 + w].2.core_mut().drain_ambient(now, out);
        }
    };
    drain(&mut s, &mut ambient);
    ambient.iter_mut().for_each(|a| a.clear());
    let before: Vec<_> = (6..10).map(|i| s.bots[i].2.core().stats.clone()).collect();
    for w in 6..10 {
        news(&mut s, w);
    }
    let shots0 = s.server.counters().shots;
    for k in 0..6 {
        s.bots[k].2.set_trigger(true);
    }
    for _ in 0..6 * TICK_HZ {
        s.step();
        drain(&mut s, &mut ambient);
    }
    for k in 0..6 {
        s.bots[k].2.set_trigger(false);
    }
    for _ in 0..2 * TICK_HZ {
        s.step();
        drain(&mut s, &mut ambient);
    }
    let fired = s.server.counters().shots - shots0;
    assert!(fired > 300, "{fired} shots");
    for (w, d) in watch.iter().enumerate() {
        let st = &s.bots[6 + w].2.core().stats;
        let (counted, made) = (st.activity_shots - before[w].activity_shots, st.ambient - before[w].ambient);
        let tracers = news(&mut s, 6 + w).1.len() as u64;
        let a = &ambient[w];
        let from_drawn = a.iter().filter(|x| x.shooter.is_some()).count();
        let placed = a.iter().filter(|x| (x.origin[0] - p[0]).abs() < 12.0 && x.origin[1] > p[1] - 12.0 && x.origin[1] < p[1] + 27.0).count();
        let east = a.iter().filter(|x| x.dir[0] > 0.95).count();
        eprintln!("{d} m: {counted} of {fired} counted, {tracers} tracers, {made} ambient ({} drawn: {from_drawn} from players, {placed} placed, {east} east)", a.len());
        if *d > 1600.0 {
            assert_eq!(counted, 0, "past the far radius: nothing");
            continue;
        }
        assert_eq!(counted, fired, "every shot is counted once");
        if *d < 150.0 {
            assert!(tracers + 6 >= fired, "near: exact tracers");
            assert!((made as f64) < 0.05 * fired as f64, "and they aren't drawn again: {made}");
        } else {
            assert_eq!(tracers, 0);
            assert!(made as f64 > 0.9 * fired as f64 && a.len() as u64 == made, "ambient for each shot: {made}");
            assert!(from_drawn as f64 > 0.9 * made as f64, "from the drawn shooters");
            assert!(placed == a.len() && east == a.len(), "where they are, aiming where they aim");
        }
    }
    let c = s.server.counters();
    assert!(c.activity_cells > 0 && c.activity_bytes > 0 && c.activity_cut == 0);
}

#[test]
fn the_shooter_learns_where_its_shots_went() {
    // Its tracers are drawn along picks of its own (the server's are
    // secret), so the server sends the shooter each of its shots back the
    // next tick with where it really went: its tracer can be re-aimed, and
    // land where the shot did. Revealed after the fact, a pick predicts
    // nothing about the next.
    let (mut s, [shooter, target, _]) = firing_line_with(40.0, false, true);
    news(&mut s, 0);
    s.server.take_fired();
    s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads: false, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
    let mut steps = Vec::new();
    for _ in 0..3 * TICK_HZ {
        let before = s.bots[0].2.core().last_shot_step();
        s.step();
        let after = s.bots[0].2.core().last_shot_step();
        if after != before {
            steps.extend(after);
        }
    }
    s.gunner = None;
    for _ in 0..TICK_HZ / 3 {
        s.step();
    }
    let fired: Vec<[f32; 3]> = s.server.take_fired().into_iter().filter(|f| f.0 == shooter).map(|f| f.2).collect();
    let (_, shots) = news(&mut s, 0);
    let own: Vec<_> = shots.iter().filter(|sh| sh.shooter == shooter).collect();
    eprintln!("{} shots fired, {} came back", fired.len(), own.len());
    assert!(fired.len() > 20);
    assert_eq!(own.len(), fired.len(), "every shot comes back once");
    for ((real, back), step) in fired.iter().zip(&own).zip(&steps) {
        // The angle between them, in f64 (acos near 1 in f32 is mostly noise).
        let (a, b) = (lattice_game::weapon::aim(back.yaw, back.pitch).map(f64::from), real.map(f64::from));
        let cross = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let sin = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
        let deg = sin.atan2(a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).to_degrees();
        // The wire's 16-bit yaw and pitch: under a hundredth of a degree.
        assert!(deg < 0.01, "the real direction: {deg} degrees off");
        assert!((back.step - step).abs() <= 1.0 / 16.0, "the step it fired at: {} vs {step}", back.step);
    }
}

#[test]
fn aiming_down_sights_hits_and_the_spread_is_the_servers_secret() {
    // A standing gunner 40 m from a standing target, with the cone of fire:
    // from the hip (2 degrees, blooming) most shots miss; down the sights
    // (0.1, blooming to 0.6) nearly all hit. Every shot stays in its cone,
    // but where in it is the server's secret: the shooter's client (which
    // a cheat would run) draws its tracer in the same cone, never along the
    // real shot, so it can't aim to cancel the spread.
    let mut rates = Vec::new();
    for ads in [false, true] {
        let (mut s, [shooter, target, _]) = firing_line_with(40.0, false, true);
        s.bots[0].2.set_ads(ads);
        for _ in 0..TICK_HZ {
            s.step();
        }
        s.server.take_hits();
        s.server.take_fired();
        s.gunner_dirs.clear();
        s.gunner = Some(Gunner { bot: 0, target, head: false, extra_lead: 0.0, shots: 0, ads, back: 0.0, hold: false, fixed: None, pick_back: 0.0 });
        for _ in 0..6 * TICK_HZ {
            s.step();
        }
        s.gunner = None;
        for _ in 0..TICK_HZ {
            s.step();
        }
        let fired: Vec<[f32; 3]> = s.server.take_fired().into_iter().filter(|f| f.0 == shooter).map(|f| f.2).collect();
        assert_eq!(fired.len(), s.gunner_dirs.len(), "every shot the client fired, the server fired");
        let degrees = |a: &[f32; 3], b: &[f32; 3]| (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0).acos().to_degrees();
        use lattice_game::weapon::{BLOOM_MAX_ADS, BLOOM_MAX_HIP, CONE_ADS, CONE_HIP};
        let widest = if ads { CONE_ADS + BLOOM_MAX_ADS } else { CONE_HIP + BLOOM_MAX_HIP } + 1e-3;
        let mut predicted = 0;
        for (shot, (aim, tracer)) in fired.iter().zip(&s.gunner_dirs) {
            assert!(degrees(shot, aim) <= widest && degrees(tracer, aim) <= widest, "in the cone");
            // Before the secret, client and server agreed to 1e-6.
            predicted += ((0..3).map(|k| (shot[k] - tracer[k]).abs()).fold(0.0f32, f32::max) < 1e-6) as u32;
        }
        assert_eq!(predicted, 0, "the client's tracer never knew where a shot would go");
        let hits = s.server.take_hits().len();
        eprintln!("{}: {hits} of {} hit", if ads { "down the sights" } else { "from the hip" }, fired.len());
        rates.push(hits as f64 / fired.len() as f64);
    }
    assert!(rates[0] < 0.4, "from the hip at 40 m, most miss: {rates:?}");
    assert!(rates[1] > 0.85, "down the sights, nearly all hit: {rates:?}");
}
