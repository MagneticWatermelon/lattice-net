//! M1 bot swarm: N `lattice_net::Client`s driven by `BotBrain`s, one UDP
//! socket per bot (the server identifies clients by address), sharded over a
//! few threads that each tick all their bots at 30 Hz.
//!
//! Reports prediction corrections, bytes and snapshot contents per bot, and join
//! latency (connect request to Welcome). If the swarm itself overruns its tick,
//! it says so: late inputs starve the server and show up as corrections.

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_net::{Channel, Client, ClientState, Config};
use lattice_sim::bot::BotBrain;
use lattice_sim::cli::Args;
use lattice_sim::movement::TICK_HZ;
use lattice_sim::stats::summarize;

const USAGE: &str = "\
lattice-bots: M1 bot swarm

  --server ADDR        [127.0.0.1:40000]
  --count N            bots [1000]
  --threads N          [min(8, cores / 2)]
  --ramp R             bots joining per second (0 = all at once) [0]
  --duration S         seconds from start until every bot disconnects [60]
  --report S           report interval, seconds [5]
  --seed N             [1]";

/// Cumulative per-thread totals; the main thread sums threads and diffs windows.
#[derive(Debug, Clone, Default)]
struct Totals {
    started: u64,
    connected: u64,
    welcomed: u64,
    failed: u64,
    snapshots: u64,
    stale_snapshots: u64,
    unmatched_acks: u64,
    resyncs: u64,
    clock_extra: u64,
    clock_skipped: u64,
    entities: u64,
    corrections: u64,
    correction_err_sum: f64,
    correction_err_max: f32,
    bytes_down: u64,
    bytes_up: u64,
    rtt_sum: f64,
    loss_sum: f64,
    tick_overruns: u64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.started += o.started;
        self.connected += o.connected;
        self.welcomed += o.welcomed;
        self.failed += o.failed;
        self.snapshots += o.snapshots;
        self.stale_snapshots += o.stale_snapshots;
        self.unmatched_acks += o.unmatched_acks;
        self.resyncs += o.resyncs;
        self.clock_extra += o.clock_extra;
        self.clock_skipped += o.clock_skipped;
        self.entities += o.entities;
        self.corrections += o.corrections;
        self.correction_err_sum += o.correction_err_sum;
        self.correction_err_max = self.correction_err_max.max(o.correction_err_max);
        self.bytes_down += o.bytes_down;
        self.bytes_up += o.bytes_up;
        self.rtt_sum += o.rtt_sum;
        self.loss_sum += o.loss_sum;
        self.tick_overruns += o.tick_overruns;
    }
}

#[derive(Default)]
struct Shared {
    threads: Vec<Totals>,
    /// Join latencies (ms) since the last report.
    joins: Vec<u32>,
}

struct Bot {
    start_at: Instant,
    seed: u64,
    /// Bound up front, before the clock starts: creating thousands of sockets
    /// inside the first tick overran the swarm and delivered its inputs late.
    sock: Option<UdpSocket>,
    net: Option<(UdpSocket, Client)>,
    brain: Option<BotBrain>,
    joined_ms: Option<u32>,
    failed: bool,
    /// Last transport stats seen, kept after the connection ends.
    bytes: (u64, u64),
}

impl Bot {
    fn tick(&mut self, server: SocketAddr, now: Instant, buf: &mut [u8]) -> std::io::Result<Option<u32>> {
        if self.failed || now < self.start_at {
            return Ok(None);
        }
        if self.net.is_none() {
            let sock = match self.sock.take() {
                Some(s) => s,
                None => bot_socket(server)?,
            };
            self.net = Some((sock, Client::new(Config::default(), server, now)));
            self.brain = Some(BotBrain::new(self.seed));
        }
        let (sock, client) = self.net.as_mut().unwrap();
        let brain = self.brain.as_mut().unwrap();
        loop {
            match sock.recv(buf) {
                Ok(n) => client.receive(server, &buf[..n], now),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // ICMP port unreachable (server not up yet) surfaces here; keep going.
                Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset) => {}
                Err(e) => return Err(e),
            }
        }
        client.update(now);

        let mut joined = None;
        match client.state() {
            ClientState::Connected => {
                while let Some((_, data)) = client.recv() {
                    brain.on_message(&data);
                }
                if self.joined_ms.is_none() && brain.welcome().is_some() {
                    let ms = (now - self.start_at).as_millis() as u32;
                    self.joined_ms = Some(ms);
                    joined = Some(ms);
                }
                if let Some(batch) = brain.tick_inputs() {
                    let _ = client.send(Channel::Unreliable, batch);
                }
            }
            ClientState::Connecting => {}
            ClientState::Denied(_) | ClientState::TimedOut | ClientState::Disconnected => self.failed = true,
        }
        client.flush(now);
        for pkt in client.drain_outgoing() {
            let _ = sock.send(&pkt);
        }
        if let Some(s) = client.stats() {
            self.bytes = (s.bytes_received, s.bytes_sent);
        }
        Ok(joined)
    }

    fn add_to(&self, t: &mut Totals) {
        let Some(brain) = &self.brain else { return };
        t.started += 1;
        t.failed += self.failed as u64;
        if let Some((_, client)) = &self.net {
            if let (ClientState::Connected, Some(s)) = (client.state(), client.stats()) {
                t.connected += 1;
                t.rtt_sum += s.rtt_ms as f64;
                t.loss_sum += s.loss as f64;
            }
        }
        t.welcomed += brain.welcome().is_some() as u64;
        let s = &brain.stats;
        t.snapshots += s.snapshots;
        t.stale_snapshots += s.stale_snapshots;
        t.unmatched_acks += s.unmatched_acks;
        t.resyncs += s.resyncs;
        t.clock_extra += s.clock_extra;
        t.clock_skipped += s.clock_skipped;
        t.entities += s.entities_seen;
        t.corrections += s.corrections;
        t.correction_err_sum += s.correction_error_sum;
        t.correction_err_max = t.correction_err_max.max(s.correction_error_max);
        t.bytes_down += self.bytes.0;
        t.bytes_up += self.bytes.1;
    }
}

fn bot_socket(server: SocketAddr) -> std::io::Result<UdpSocket> {
    let sock = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    sock.connect(server)?;
    sock.set_nonblocking(true)?;
    Ok(sock)
}

fn main() -> std::io::Result<()> {
    let mut a = Args::parse(USAGE);
    let server: SocketAddr = a.get("server", "127.0.0.1:40000".parse().unwrap());
    let count: usize = a.get("count", 1000);
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let threads: usize = a.get("threads", (cores / 2).clamp(1, 8));
    let ramp: f64 = a.get("ramp", 0.0);
    let duration = Duration::from_secs_f64(a.get("duration", 60.0));
    let report = Duration::from_secs_f64(a.get("report", 5.0));
    let seed: u64 = a.get("seed", 1);
    a.finish();

    println!("{count} bots -> {server} on {threads} threads, ramp {ramp}/s, {duration:?}");
    let sockets = (0..count).map(|_| bot_socket(server)).collect::<std::io::Result<Vec<_>>>()?;
    let mut sockets = sockets.into_iter().map(Some).collect::<Vec<_>>();
    let start = Instant::now();
    let end = start + duration;
    let shared = Arc::new(Mutex::new(Shared { threads: vec![Totals::default(); threads], joins: Vec::new() }));

    let mut handles = Vec::new();
    for t in 0..threads {
        let shared = shared.clone();
        let mut bots: Vec<Bot> = (t..count)
            .step_by(threads)
            .map(|i| Bot {
                start_at: start + if ramp > 0.0 { Duration::from_secs_f64(i as f64 / ramp) } else { Duration::ZERO },
                seed: seed.wrapping_mul(1_000_003).wrapping_add(i as u64),
                sock: sockets[i].take(),
                net: None,
                brain: None,
                joined_ms: None,
                failed: false,
                bytes: (0, 0),
            })
            .collect();
        handles.push(std::thread::Builder::new().name(format!("bots-{t}")).spawn(move || -> std::io::Result<()> {
            let period = Duration::from_secs(1) / TICK_HZ;
            // Spread the threads' ticks across the period, like real clients.
            let mut next = start + period * t as u32 / threads as u32;
            let mut buf = [0u8; 1500];
            let mut overruns = 0;
            let mut joins = Vec::new();
            let mut last_publish = start;
            loop {
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                }
                let now = Instant::now();
                if now >= end {
                    break;
                }
                for bot in &mut bots {
                    joins.extend(bot.tick(server, now, &mut buf)?);
                }
                next += period;
                if Instant::now() > next {
                    overruns += 1;
                    next = Instant::now();
                }
                if now - last_publish >= Duration::from_millis(500) {
                    last_publish = now;
                    let mut tot = Totals { tick_overruns: overruns, ..Default::default() };
                    bots.iter().for_each(|b| b.add_to(&mut tot));
                    let mut s = shared.lock().unwrap();
                    s.threads[t] = tot;
                    s.joins.append(&mut joins);
                }
            }
            let mut tot = Totals { tick_overruns: overruns, ..Default::default() };
            bots.iter().for_each(|b| b.add_to(&mut tot));
            for bot in &mut bots {
                if let Some((sock, client)) = &mut bot.net {
                    client.disconnect();
                    for pkt in client.drain_outgoing() {
                        let _ = sock.send(&pkt);
                    }
                }
            }
            tot.connected = 0;
            let mut s = shared.lock().unwrap();
            s.threads[t] = tot;
            s.joins.append(&mut joins);
            Ok(())
        })?);
    }

    let mut prev = Totals::default();
    let mut all_joins = Vec::new();
    let mut last = start;
    while Instant::now() < end {
        std::thread::sleep(report.min(end.saturating_duration_since(Instant::now())));
        let now = Instant::now();
        let (cur, mut joins) = snapshot(&shared);
        print_window(now - start, (now - last).as_secs_f64(), &cur, &prev, &mut joins);
        all_joins.extend(joins);
        prev = cur;
        last = now;
    }
    for h in handles {
        h.join().expect("bot thread panicked")?;
    }
    let (total, joins) = snapshot(&shared);
    all_joins.extend(joins);
    print_summary(&total, duration.as_secs_f64(), &mut all_joins);
    Ok(())
}

fn snapshot(shared: &Mutex<Shared>) -> (Totals, Vec<u32>) {
    let mut s = shared.lock().unwrap();
    let mut t = Totals::default();
    s.threads.iter().for_each(|x| t.add(x));
    (t, std::mem::take(&mut s.joins))
}

fn print_window(t: Duration, secs: f64, cur: &Totals, prev: &Totals, joins: &mut [u32]) {
    let bots = cur.connected.max(1) as f64;
    let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
    let snaps = d(cur.snapshots, prev.snapshots);
    let j = summarize(joins);
    println!(
        "[{:>5.0}s] bots {}/{} connected, {} welcomed, {} failed | {:.1} snaps/s/bot, {:.1} entities/snap | corrections {:.3}/s/bot | down {:.0} up {:.0} kbps/bot | rtt {:.1} ms loss {:.2}% | joins {} (p50 {} p99 {} ms) | swarm overruns {}",
        t.as_secs_f64(),
        cur.connected,
        cur.started,
        cur.welcomed,
        cur.failed,
        snaps / secs / bots,
        d(cur.entities, prev.entities) / snaps.max(1.0),
        d(cur.corrections, prev.corrections) / secs / bots,
        d(cur.bytes_down, prev.bytes_down) * 8.0 / 1000.0 / secs / bots,
        d(cur.bytes_up, prev.bytes_up) * 8.0 / 1000.0 / secs / bots,
        cur.rtt_sum / bots,
        100.0 * cur.loss_sum / bots,
        joins.len(),
        j.p50,
        j.p99,
        cur.tick_overruns - prev.tick_overruns,
    );
}

fn print_summary(t: &Totals, secs: f64, joins: &mut [u32]) {
    let j = summarize(joins);
    let bot_secs = (t.welcomed.max(1) as f64) * secs;
    println!("\n== bots summary ==");
    println!("  started {} welcomed {} failed {}", t.started, t.welcomed, t.failed);
    println!(
        "  join latency ms: p50 {} p99 {} max {} (n={})",
        j.p50,
        j.p99,
        j.max,
        joins.len()
    );
    println!(
        "  corrections {} ({:.2}/bot-minute), mean error {:.3} m, max {:.3} m",
        t.corrections,
        t.corrections as f64 / bot_secs * 60.0,
        t.correction_err_sum / t.corrections.max(1) as f64,
        t.correction_err_max
    );
    println!(
        "  snapshots {} ({:.1} entities avg), stale {}, unmatched acks {}, resyncs {}",
        t.snapshots,
        t.entities as f64 / t.snapshots.max(1) as f64,
        t.stale_snapshots,
        t.unmatched_acks,
        t.resyncs
    );
    println!("  input clock: {} extra inputs, {} skipped ticks", t.clock_extra, t.clock_skipped);
    println!(
        "  bytes down {:.1} MB up {:.1} MB | swarm tick overruns {}",
        t.bytes_down as f64 / 1e6,
        t.bytes_up as f64 / 1e6,
        t.tick_overruns
    );
}
