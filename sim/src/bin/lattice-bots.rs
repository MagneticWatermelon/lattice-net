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
use std::time::{Duration, Instant, SystemTime};

use lattice_net::token::USER_DATA_BYTES;
use lattice_net::{Channel, Client, ClientState, Config, ConnectToken};
use lattice_sim::bot::{BotBrain, InputTiming};
use lattice_sim::cli::{Args, HexKey};
use lattice_sim::movement::TICK_HZ;
use lattice_sim::stats::{summarize, Histogram, KeyValues};

const USAGE: &str = "\
lattice-bots: M1 bot swarm

  --server ADDR        [127.0.0.1:40000]
  --count N            bots [1000]
  --threads N          [min(8, cores / 2)]
  --ramp R             bots joining per second (0 = all at once) [0]
  --duration S         seconds from start until every bot disconnects [60]
  --report S           report interval, seconds [5]
  --track-every N      every Nth bot tracks entities to measure update intervals per tier [20]
  --full-every K       only every Kth bot measures (prediction, latency, tracking); the
                       rest are sink bots that play but only count what they're sent [1]
  --token-key HEX      64 hex digits shared by server and bots (the bots mint their own
                       connect tokens, standing in for a login service) [the public dev key]
  --server-id N        the server id tokens are minted for [1]
  --seed N             [1]
  --summary PATH       write the end-of-run results as key=value lines (scripts/baseline.sh)";

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
    /// Entity updates received per tier.
    tiers: [u64; 3],
    /// Gauges over connected bots: summed pace (per mille), deepest server
    /// level seen, bots with their own bandwidth level above 0.
    pace_sum: f64,
    level_max: u8,
    client_degraded: u64,
    backlog_skips: u64,
    near_decode_errors: u64,
    corrections: u64,
    correction_err_sum: f64,
    correction_err_max: f32,
    push_corrections: u64,
    push_err_max: f32,
    bytes_down: u64,
    bytes_up: u64,
    rtt_sum: f64,
    loss_sum: f64,
    tick_overruns: u64,
    /// Bot threads' time spent working vs. elapsed, summed over threads.
    busy_us: u64,
    wall_us: u64,
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
        for (a, b) in self.tiers.iter_mut().zip(o.tiers) {
            *a += b;
        }
        self.pace_sum += o.pace_sum;
        self.level_max = self.level_max.max(o.level_max);
        self.client_degraded += o.client_degraded;
        self.backlog_skips += o.backlog_skips;
        self.near_decode_errors += o.near_decode_errors;
        self.corrections += o.corrections;
        self.push_corrections += o.push_corrections;
        self.push_err_max = self.push_err_max.max(o.push_err_max);
        self.correction_err_sum += o.correction_err_sum;
        self.correction_err_max = self.correction_err_max.max(o.correction_err_max);
        self.bytes_down += o.bytes_down;
        self.bytes_up += o.bytes_up;
        self.rtt_sum += o.rtt_sum;
        self.loss_sum += o.loss_sum;
        self.tick_overruns += o.tick_overruns;
        self.busy_us += o.busy_us;
        self.wall_us += o.wall_us;
    }
}

struct Shared {
    threads: Vec<Totals>,
    /// Join latencies (ms) since the last report.
    joins: Vec<u32>,
    /// Input timings since the last report.
    latency: Latency,
}

/// Input timing histograms.
struct Latency {
    /// Input generated -> acked in a snapshot we read (round trip), ms.
    seen: Histogram,
    /// Server wait, arrival -> applied, 0.1 ms.
    wait: Histogram,
    /// Estimated input generated -> applied on the server: RTT / 2 + wait, ms.
    applied: Histogram,
    /// Update interval per entity, in ms, per tier (tracked bots only).
    intervals: [Histogram; 3],
}

impl Latency {
    fn new() -> Self {
        Self {
            seen: Histogram::new(LATENCY_CAP_MS),
            wait: Histogram::new(LATENCY_CAP_MS * 10),
            applied: Histogram::new(LATENCY_CAP_MS),
            intervals: std::array::from_fn(|_| Histogram::new(10_000)),
        }
    }

    fn record(&mut self, t: InputTiming, rtt_ms: f32) {
        self.seen.record(t.seen_ms as u32);
        if let Some(w) = t.server_wait {
            self.wait.record(w as u32);
            self.applied.record((rtt_ms / 2.0 + w as f32 / 10.0).round() as u32);
        }
    }

    fn merge(&mut self, o: &Latency) {
        self.seen.merge(&o.seen);
        self.wait.merge(&o.wait);
        self.applied.merge(&o.applied);
        for (a, b) in self.intervals.iter_mut().zip(&o.intervals) {
            a.merge(b);
        }
    }
}

/// Converts kernel receive timestamps (wall clock) to `Instant`s, using one
/// pair of clock readings per tick.
struct Clocks {
    instant: Instant,
    system: SystemTime,
}

impl Clocks {
    fn now() -> Self {
        Self { instant: Instant::now(), system: SystemTime::now() }
    }

    fn instant_of(&self, t: SystemTime) -> Instant {
        self.system.duration_since(t).map_or(self.instant, |ago| self.instant - ago)
    }
}

/// Stands in for the login service: mints each bot's connect token.
#[derive(Clone, Copy)]
struct Login {
    token_key: [u8; 32],
    server_id: u64,
}

impl Login {
    fn token(&self, user: u64, now: SystemTime) -> ConnectToken {
        let unix = now.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let cfg = Config::default();
        ConnectToken::mint(&self.token_key, cfg.protocol_id, self.server_id, unix + 60, user, &[0; USER_DATA_BYTES])
    }
}

struct Bot {
    start_at: Instant,
    seed: u64,
    track: bool,
    sink: bool,
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
    fn tick(&mut self, server: SocketAddr, login: &Login, clocks: &Clocks, rx: &mut RecvBatch) -> std::io::Result<Option<u32>> {
        let now = clocks.instant;
        if self.failed || now < self.start_at {
            return Ok(None);
        }
        if self.net.is_none() {
            let sock = match self.sock.take() {
                Some(s) => s,
                None => bot_socket(server)?,
            };
            let token = login.token(self.seed, clocks.system);
            self.net = Some((sock, Client::new(Config::default(), server, token, now)));
            let mut brain = BotBrain::new(self.seed);
            if self.track {
                brain.enable_tracking();
            }
            brain.set_sink(self.sink);
            self.brain = Some(brain);
        }
        let (sock, client) = self.net.as_mut().unwrap();
        let brain = self.brain.as_mut().unwrap();
        loop {
            match rx.recv(sock) {
                Ok(n) => {
                    for i in 0..n {
                        // Pass the arrival time, not the tick: the transport's RTT
                        // (and the input -> applied estimate built on it) must not
                        // include the up-to-a-tick wait for our own tick.
                        let (data, stamp) = rx.get(i);
                        client.receive(server, data, stamp.map_or(now, |t| clocks.instant_of(t)));
                    }
                    if n < RX_BATCH {
                        break;
                    }
                }
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
                    brain.on_message(&data, now);
                }
                if self.joined_ms.is_none() && brain.welcome().is_some() {
                    let ms = (now - self.start_at).as_millis() as u32;
                    self.joined_ms = Some(ms);
                    joined = Some(ms);
                }
                if let Some(batch) = brain.tick_inputs(now) {
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
                t.pace_sum += brain.stats().pace as f64;
                t.level_max = t.level_max.max(brain.stats().level);
                t.client_degraded += (brain.stats().client_level > 0) as u64;
                t.rtt_sum += s.rtt_ms as f64;
                t.loss_sum += s.loss as f64;
            }
        }
        t.welcomed += brain.welcome().is_some() as u64;
        let s = brain.stats();
        t.snapshots += s.snapshots;
        t.stale_snapshots += s.stale_snapshots;
        t.unmatched_acks += s.unmatched_acks;
        t.resyncs += s.resyncs;
        t.clock_extra += s.clock_extra;
        t.clock_skipped += s.clock_skipped;
        for (a, b) in t.tiers.iter_mut().zip(s.tier_seen) {
            *a += b;
        }
        t.backlog_skips += s.backlog_skips;
        t.near_decode_errors += s.near_decode_errors;
        t.corrections += s.corrections;
        t.correction_err_sum += s.correction_error_sum;
        t.correction_err_max = t.correction_err_max.max(s.correction_error_max);
        t.push_corrections += s.push_corrections;
        t.push_err_max = t.push_err_max.max(s.push_error_max);
        t.bytes_down += self.bytes.0;
        t.bytes_up += self.bytes.1;
    }
}

/// Input latencies above this land in the histogram's last bucket.
const LATENCY_CAP_MS: u32 = 2000;

fn bot_socket(server: SocketAddr) -> std::io::Result<UdpSocket> {
    let sock = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    sock.connect(server)?;
    sock.set_nonblocking(true)?;
    #[cfg(target_os = "linux")]
    enable_rx_timestamps(&sock)?;
    Ok(sock)
}

#[cfg(target_os = "linux")]
fn enable_rx_timestamps(sock: &UdpSocket) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let on: libc::c_int = 1;
    // SAFETY: a plain setsockopt with a valid fd and an int option value.
    let r = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPNS,
            &on as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Datagrams received per `recvmmsg` call. A bot gets 2-3 per tick, so one
/// call drains its socket without the extra empty `recv` that ends a loop.
const RX_BATCH: usize = 8;

/// Batched receive with the kernel's arrival timestamps (`SO_TIMESTAMPNS`).
/// One per bot thread, reused for every bot.
struct RecvBatch {
    bufs: Vec<[u8; 1500]>,
    lens: [usize; RX_BATCH],
    stamps: [Option<SystemTime>; RX_BATCH],
    #[cfg(target_os = "linux")]
    control: Vec<[u64; 8]>,
}

impl RecvBatch {
    fn new() -> Self {
        Self {
            bufs: vec![[0; 1500]; RX_BATCH],
            lens: [0; RX_BATCH],
            stamps: [None; RX_BATCH],
            #[cfg(target_os = "linux")]
            control: vec![[0; 8]; RX_BATCH],
        }
    }

    fn get(&self, i: usize) -> (&[u8], Option<SystemTime>) {
        (&self.bufs[i][..self.lens[i]], self.stamps[i])
    }

    /// Receives whatever is waiting, up to `RX_BATCH`, without blocking.
    #[cfg(target_os = "linux")]
    fn recv(&mut self, sock: &UdpSocket) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd;
        let mut iovs: [libc::iovec; RX_BATCH] = std::array::from_fn(|i| libc::iovec {
            iov_base: self.bufs[i].as_mut_ptr() as *mut libc::c_void,
            iov_len: self.bufs[i].len(),
        });
        let mut msgs: [libc::mmsghdr; RX_BATCH] = std::array::from_fn(|i| {
            // SAFETY: msghdr is plain data; all-zero is a valid empty header.
            let mut h: libc::msghdr = unsafe { std::mem::zeroed() };
            h.msg_iov = &mut iovs[i];
            h.msg_iovlen = 1;
            h.msg_control = self.control[i].as_mut_ptr() as *mut libc::c_void;
            h.msg_controllen = std::mem::size_of::<[u64; 8]>() as _;
            libc::mmsghdr { msg_hdr: h, msg_len: 0 }
        });
        // SAFETY: every header points into `iovs` (into `self.bufs`) and
        // `self.control`, all alive and unmoved for the duration of the call.
        let n = unsafe {
            libc::recvmmsg(sock.as_raw_fd(), msgs.as_mut_ptr(), RX_BATCH as u32, libc::MSG_DONTWAIT, std::ptr::null_mut())
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for (i, m) in msgs.iter().enumerate().take(n as usize) {
            self.lens[i] = m.msg_len as usize;
            self.stamps[i] = None;
            // SAFETY: walking the control messages the kernel just wrote.
            unsafe {
                let mut c = libc::CMSG_FIRSTHDR(&m.msg_hdr);
                while !c.is_null() {
                    if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_TIMESTAMPNS {
                        let ts: libc::timespec = std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const libc::timespec);
                        self.stamps[i] = Some(SystemTime::UNIX_EPOCH + Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32));
                    }
                    c = libc::CMSG_NXTHDR(&m.msg_hdr, c);
                }
            }
        }
        Ok(n as usize)
    }

    #[cfg(not(target_os = "linux"))]
    fn recv(&mut self, sock: &UdpSocket) -> std::io::Result<usize> {
        let n = sock.recv(&mut self.bufs[0])?;
        (self.lens[0], self.stamps[0]) = (n, None);
        Ok(1)
    }
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
    let track_every: usize = a.get("track-every", 20);
    let full_every: usize = a.get::<usize>("full-every", 1).max(1);
    let key: HexKey = a.get("token-key", HexKey::default());
    let summary_path: Option<String> = a.opt("summary");
    let login = Login { token_key: key.0, server_id: a.get("server-id", 1) };
    a.finish();
    if key.is_dev() {
        println!("minting tokens with the public dev key (--token-key to change)");
    }

    println!(
        "{count} bots -> {server} on {threads} threads, ramp {ramp}/s, {duration:?}, {} full + {} sink bots",
        count.div_ceil(full_every),
        count - count.div_ceil(full_every)
    );
    // One socket per bot, plus a few for the process itself.
    raise_open_file_limit(count as u64 + 64)?;
    let sockets = (0..count).map(|_| bot_socket(server)).collect::<std::io::Result<Vec<_>>>()?;
    let mut sockets = sockets.into_iter().map(Some).collect::<Vec<_>>();
    let start = Instant::now();
    let end = start + duration;
    let shared = Arc::new(Mutex::new(Shared {
        threads: vec![Totals::default(); threads],
        joins: Vec::new(),
        latency: Latency::new(),
    }));

    let mut handles = Vec::new();
    for t in 0..threads {
        let shared = shared.clone();
        let mut bots: Vec<Bot> = (t..count)
            .step_by(threads)
            .map(|i| Bot {
                start_at: start + if ramp > 0.0 { Duration::from_secs_f64(i as f64 / ramp) } else { Duration::ZERO },
                seed: seed.wrapping_mul(1_000_003).wrapping_add(i as u64),
                track: track_every > 0 && i % track_every == 0 && i % full_every == 0,
                sink: i % full_every != 0,
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
            let mut rx = RecvBatch::new();
            let (mut busy, thread_start) = (Duration::ZERO, Instant::now());
            let mut overruns = 0;
            let mut joins = Vec::new();
            let mut latency = Latency::new();
            let mut samples = Vec::new();
            let mut intervals: [Vec<u16>; 3] = Default::default();
            let mut last_publish = start;
            loop {
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                }
                let clocks = Clocks::now();
                let now = clocks.instant;
                if now >= end {
                    break;
                }
                let work = Instant::now();
                for bot in &mut bots {
                    joins.extend(bot.tick(server, &login, &clocks, &mut rx)?);
                    if bot.sink {
                        continue;
                    }
                    if let (Some(brain), Some((_, client))) = (&mut bot.brain, &bot.net) {
                        brain.drain_latency(&mut samples);
                        let rtt = client.stats().map_or(0.0, |s| s.rtt_ms);
                        samples.drain(..).for_each(|t| latency.record(t, rtt));
                        brain.drain_intervals(&mut intervals);
                        // Intervals come in server ticks; a tick's length depends on the level.
                        let hz = lattice_sim::ladder::RUNGS[(brain.stats().level as usize).min(lattice_sim::ladder::MAX_LEVEL as usize)].tick_hz;
                        for (h, v) in latency.intervals.iter_mut().zip(&mut intervals) {
                            v.drain(..).for_each(|g| h.record(g as u32 * 1000 / hz));
                        }
                    }
                }
                busy += work.elapsed();
                next += period;
                if Instant::now() > next {
                    overruns += 1;
                    next = Instant::now();
                }
                if now - last_publish >= Duration::from_millis(500) {
                    last_publish = now;
                    let mut tot = Totals {
                        tick_overruns: overruns,
                        busy_us: busy.as_micros() as u64,
                        wall_us: thread_start.elapsed().as_micros() as u64,
                        ..Default::default()
                    };
                    bots.iter().for_each(|b| b.add_to(&mut tot));
                    let mut s = shared.lock().unwrap();
                    s.threads[t] = tot;
                    s.joins.append(&mut joins);
                    s.latency.merge(&std::mem::replace(&mut latency, Latency::new()));
                }
            }
            let mut tot = Totals {
                        tick_overruns: overruns,
                        busy_us: busy.as_micros() as u64,
                        wall_us: thread_start.elapsed().as_micros() as u64,
                        ..Default::default()
                    };
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
            s.latency.merge(&latency);
            Ok(())
        })?);
    }

    let mut prev = Totals::default();
    let mut all_joins = Vec::new();
    let mut all_latency = Latency::new();
    let mut last = start;
    while Instant::now() < end {
        std::thread::sleep(report.min(end.saturating_duration_since(Instant::now())));
        let now = Instant::now();
        let (cur, mut joins, latency) = snapshot(&shared);
        print_window(now - start, (now - last).as_secs_f64(), &cur, &prev, &mut joins, &latency);
        all_joins.extend(joins);
        all_latency.merge(&latency);
        prev = cur;
        last = now;
    }
    for h in handles {
        h.join().expect("bot thread panicked")?;
    }
    let (total, joins, latency) = snapshot(&shared);
    all_joins.extend(joins);
    all_latency.merge(&latency);
    if let Some(path) = summary_path {
        summary_values(&total, duration.as_secs_f64(), &mut all_joins.clone(), &all_latency).write(&path)?;
    }
    print_summary(&total, duration.as_secs_f64(), &mut all_joins, &all_latency);
    Ok(())
}

/// The summary's numbers as key=value lines, for `--summary`.
fn summary_values(t: &Totals, secs: f64, joins: &mut [u32], latency: &Latency) -> KeyValues {
    let mut kv = KeyValues::default();
    let j = summarize(joins);
    let bot_secs = (t.welcomed.max(1) as f64) * secs;
    kv.put("started", t.started);
    kv.put("welcomed", t.welcomed);
    kv.put("failed", t.failed);
    kv.put("join_p50_ms", j.p50);
    kv.put("join_p99_ms", j.p99);
    kv.put("join_max_ms", j.max);
    kv.put("corrections", t.corrections);
    kv.put("corrections_per_bot_minute", format!("{:.3}", t.corrections as f64 / bot_secs * 60.0));
    kv.put("correction_max_m", format!("{:.3}", t.correction_err_max));
    kv.put("push_corrections", t.push_corrections);
    kv.put("push_corrections_per_bot_minute", format!("{:.3}", t.push_corrections as f64 / bot_secs * 60.0));
    kv.put("push_max_m", format!("{:.3}", t.push_err_max));
    let snaps = t.snapshots.max(1) as f64;
    kv.put("snapshots", t.snapshots);
    for (i, tier) in ["near", "mid", "far"].iter().enumerate() {
        kv.put(format!("{tier}_per_snapshot"), format!("{:.1}", t.tiers[i] as f64 / snaps));
    }
    kv.put("stale_snapshots", t.stale_snapshots);
    kv.put("resyncs", t.resyncs);
    let mut hist = |name: &str, h: &Histogram, scale: f64| {
        let s = h.summary();
        kv.put(format!("{name}_p50_ms"), format!("{:.1}", s.p50 as f64 * scale));
        kv.put(format!("{name}_p99_ms"), format!("{:.1}", s.p99 as f64 * scale));
        kv.put(format!("{name}_mean_ms"), format!("{:.1}", h.mean() * scale));
    };
    hist("input_applied", &latency.applied, 1.0);
    hist("server_wait", &latency.wait, 0.1);
    hist("round_trip", &latency.seen, 1.0);
    for (name, h) in ["near", "mid", "far"].iter().zip(&latency.intervals) {
        hist(&format!("{name}_interval"), h, 1.0);
    }
    kv.put("clock_extra", t.clock_extra);
    kv.put("clock_skipped", t.clock_skipped);
    kv.put("backlog_skips", t.backlog_skips);
    kv.put("near_decode_errors", t.near_decode_errors);
    kv.put("bytes_down_mb", format!("{:.1}", t.bytes_down as f64 / 1e6));
    kv.put("bytes_up_mb", format!("{:.1}", t.bytes_up as f64 / 1e6));
    kv.put("swarm_busy_pct", format!("{:.0}", 100.0 * t.busy_us as f64 / t.wall_us.max(1) as f64));
    kv.put("swarm_overruns", t.tick_overruns);
    kv
}

fn snapshot(shared: &Mutex<Shared>) -> (Totals, Vec<u32>, Latency) {
    let mut s = shared.lock().unwrap();
    let mut t = Totals::default();
    s.threads.iter().for_each(|x| t.add(x));
    let latency = std::mem::replace(&mut s.latency, Latency::new());
    (t, std::mem::take(&mut s.joins), latency)
}

fn print_window(t: Duration, secs: f64, cur: &Totals, prev: &Totals, joins: &mut [u32], latency: &Latency) {
    let bots = cur.connected.max(1) as f64;
    let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
    let snaps = d(cur.snapshots, prev.snapshots);
    let j = summarize(joins);
    let (applied, seen) = (latency.applied.summary(), latency.seen.summary());
    println!(
        "[{:>5.0}s] bots {}/{} connected, {} welcomed, {} failed | {:.1} snaps/s/bot, entities/snap near {:.1} mid {:.1} far {:.1} | corrections {:.3}/s/bot | down {:.0} up {:.0} kbps/bot | rtt {:.1} ms loss {:.2}% | input->applied ~ p50 {} p99 {} ms, round trip p50 {} p99 {} ms | joins {} (p50 {} p99 {} ms) | server pace {:.2} level {}, {} bots bandwidth-degraded | swarm busy {:.0}%, overruns {}",
        t.as_secs_f64(),
        cur.connected,
        cur.started,
        cur.welcomed,
        cur.failed,
        snaps / secs / bots,
        d(cur.tiers[0], prev.tiers[0]) / snaps.max(1.0),
        d(cur.tiers[1], prev.tiers[1]) / snaps.max(1.0),
        d(cur.tiers[2], prev.tiers[2]) / snaps.max(1.0),
        d(cur.corrections, prev.corrections) / secs / bots,
        d(cur.bytes_down, prev.bytes_down) * 8.0 / 1000.0 / secs / bots,
        d(cur.bytes_up, prev.bytes_up) * 8.0 / 1000.0 / secs / bots,
        cur.rtt_sum / bots,
        100.0 * cur.loss_sum / bots,
        applied.p50,
        applied.p99,
        seen.p50,
        seen.p99,
        joins.len(),
        j.p50,
        j.p99,
        cur.pace_sum / 1000.0 / bots,
        cur.level_max,
        cur.client_degraded,
        100.0 * d(cur.busy_us, prev.busy_us) / d(cur.wall_us, prev.wall_us).max(1.0),
        cur.tick_overruns - prev.tick_overruns,
    );
}

fn print_summary(t: &Totals, secs: f64, joins: &mut [u32], latency: &Latency) {
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
        "  push corrections (the server pushed us apart from a crowd) {} ({:.2}/bot-minute), max {:.3} m",
        t.push_corrections,
        t.push_corrections as f64 / bot_secs * 60.0,
        t.push_err_max
    );
    println!(
        "  snapshots {} (entities per snapshot: near {:.1} mid {:.1} far {:.1}), stale {}, unmatched acks {}, resyncs {}",
        t.snapshots,
        t.tiers[0] as f64 / t.snapshots.max(1) as f64,
        t.tiers[1] as f64 / t.snapshots.max(1) as f64,
        t.tiers[2] as f64 / t.snapshots.max(1) as f64,
        t.stale_snapshots,
        t.unmatched_acks,
        t.resyncs
    );
    let line = |name: &str, h: &Histogram, scale: f64| {
        let s = h.summary();
        println!(
            "  {name}: p50 {:.1} p99 {:.1} max {:.1} mean {:.1} ms (n={})",
            s.p50 as f64 * scale,
            s.p99 as f64 * scale,
            s.max as f64 * scale,
            h.mean() * scale,
            h.len()
        );
    };
    line("input -> applied on server (est. RTT/2 + server wait)", &latency.applied, 1.0);
    line("  of which server wait (arrival -> applied)", &latency.wait, 0.1);
    line("input -> seen acked (round trip)", &latency.seen, 1.0);
    for (name, h) in ["near", "mid", "far"].iter().zip(&latency.intervals) {
        line(&format!("{name} entity update interval (tracked bots)"), h, 1.0);
    }
    println!(
        "  input clock: {} extra inputs, {} skipped ticks, {} backlog skips | near decode errors (tracked bots) {}",
        t.clock_extra, t.clock_skipped, t.backlog_skips, t.near_decode_errors
    );
    println!(
        "  bytes down {:.1} MB up {:.1} MB | swarm busy {:.0}% of its threads' time, tick overruns {}",
        t.bytes_down as f64 / 1e6,
        t.bytes_up as f64 / 1e6,
        100.0 * t.busy_us as f64 / t.wall_us.max(1) as f64,
        t.tick_overruns
    );
}

/// Stock Linux allows 1,024 open files per process (soft limit), and each bot
/// holds a socket: lift the soft limit toward the hard one, or explain.
#[cfg(target_os = "linux")]
fn raise_open_file_limit(needed: u64) -> std::io::Result<()> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: lim is a valid rlimit to write into.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if lim.rlim_cur >= needed {
        return Ok(());
    }
    lim.rlim_cur = needed.min(lim.rlim_max);
    // SAFETY: lim holds a soft limit no higher than the hard one.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if lim.rlim_cur < needed {
        return Err(std::io::Error::other(format!(
            "{needed} open files needed (one socket per bot), but the hard limit is {}: raise it (ulimit -Hn, /etc/security/limits.conf)",
            lim.rlim_max
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn raise_open_file_limit(_needed: u64) -> std::io::Result<()> {
    Ok(())
}
