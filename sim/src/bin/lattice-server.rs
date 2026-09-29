//! M1 headless server: `SimServer` on a real UDP socket at 30 Hz, reporting
//! per-phase tick times, bandwidth and pps.
//!
//! A dedicated thread blocks on `recv_from` and queues datagrams into per-shard
//! buckets, so arrivals spread across the tick don't have to fit in the kernel
//! buffer and routing costs no tick time. Egress is one rayon task per shard,
//! batched with `sendmmsg` on Linux (GSO waits for M2, when clients get several
//! packets per tick).

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Write};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_net::Config;
use lattice_sim::cli::Args;
use lattice_sim::movement::TICK_HZ;
use lattice_sim::server::{Counters, Datagram, InDatagram, SimConfig, SimServer, SpawnMode, PHASES};
use lattice_sim::stats::{summarize, Histogram};
use rayon::prelude::*;
use socket2::{Domain, Protocol, Socket, Type};

const USAGE: &str = "\
lattice-server: M1 movement-only authoritative server

  --bind ADDR          [0.0.0.0:40000]
  --spawn MODE         uniform | hotspots | blob   [uniform]
  --max-clients N      [10000]
  --near-radius M      interest stand-in radius, meters [150]
  --near-max N         max entities per snapshot [64]
  --threads N          rayon threads [all cores]
  --shards N           transport shards [64]
  --accepts-per-tick N new connections accepted per tick, server-wide (0 = no limit) [256]
  --no-prealloc        allocate connections on accept instead of pooling max-clients at startup
  --egress MODE        sendmmsg | sendto   [sendmmsg on Linux, else sendto]
  --duration S         stop after S seconds (0 = run forever) [0]
  --until-empty        stop once clients connected and then all left
  --report S           report interval, seconds [5]
  --warmup S           ignore the first S seconds after the first client in the summary [3];
                       the summary also stops once clients drain below 90% of peak
  --csv PATH           append one row per report window
  --seed N             spawn RNG seed [1]";

/// Columns of per-tick timing samples: the sim phases, then egress and total.
const COLS: usize = PHASES.len() + 2;
type Row = [u32; COLS];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Egress {
    /// One `send_to` syscall per datagram.
    SendTo,
    /// Up to `MMSG_BATCH` datagrams per `sendmmsg` syscall (Linux only).
    SendMmsg,
}

impl Default for Egress {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Egress::SendMmsg
        } else {
            Egress::SendTo
        }
    }
}

impl std::str::FromStr for Egress {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "sendto" => Ok(Egress::SendTo),
            "sendmmsg" if cfg!(target_os = "linux") => Ok(Egress::SendMmsg),
            "sendmmsg" => Err("sendmmsg needs Linux".into()),
            _ => Err(format!("unknown egress mode {s:?} (sendmmsg|sendto)")),
        }
    }
}

/// Sends every datagram; returns how many failed.
fn send_all(sock: &UdpSocket, batch: &[Datagram], mode: Egress) -> usize {
    match mode {
        #[cfg(target_os = "linux")]
        Egress::SendMmsg => send_mmsg(sock, batch),
        _ => batch.iter().filter(|(addr, pkt)| sock.send_to(pkt, addr).is_err()).count(),
    }
}

#[cfg(target_os = "linux")]
const MMSG_BATCH: usize = 256;

/// `sendmmsg` in batches. A datagram the kernel rejects is counted and
/// skipped; the rest of the batch is retried from the next one.
#[cfg(target_os = "linux")]
fn send_mmsg(sock: &UdpSocket, batch: &[Datagram]) -> usize {
    use std::os::fd::AsRawFd;
    let fd = sock.as_raw_fd();
    let mut errors = 0;
    for chunk in batch.chunks(MMSG_BATCH) {
        let addrs: Vec<socket2::SockAddr> = chunk.iter().map(|(a, _)| socket2::SockAddr::from(*a)).collect();
        let mut iovs: Vec<libc::iovec> = chunk
            .iter()
            .map(|(_, p)| libc::iovec { iov_base: p.as_ptr() as *mut libc::c_void, iov_len: p.len() })
            .collect();
        let iov_base = iovs.as_mut_ptr();
        let mut msgs: Vec<libc::mmsghdr> = addrs
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                // SAFETY: msghdr is plain data; all-zero is a valid empty header.
                let mut h: libc::msghdr = unsafe { std::mem::zeroed() };
                h.msg_name = addr.as_ptr() as *mut libc::c_void;
                h.msg_namelen = addr.len();
                // SAFETY: i < iovs.len(), and iovs outlives the sendmmsg calls below.
                h.msg_iov = unsafe { iov_base.add(i) };
                h.msg_iovlen = 1;
                libc::mmsghdr { msg_hdr: h, msg_len: 0 }
            })
            .collect();
        let mut off = 0;
        while off < msgs.len() {
            // SAFETY: every header points into `addrs`, `iovs` and the datagrams in
            // `chunk`, all alive and unmoved for the duration of the call.
            let n = unsafe { libc::sendmmsg(fd, msgs.as_mut_ptr().add(off), (msgs.len() - off) as libc::c_uint, 0) };
            if n > 0 {
                off += n as usize;
            } else if n < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            } else {
                errors += 1; // the datagram at `off` was rejected
                off += 1;
            }
        }
    }
    errors
}

fn col_name(i: usize) -> &'static str {
    match i {
        i if i < PHASES.len() => PHASES[i],
        i if i == PHASES.len() => "egress",
        _ => "tick",
    }
}

#[derive(Default)]
struct NetCounters {
    in_pkts: AtomicU64,
    in_bytes: AtomicU64,
    recv_errors: AtomicU64,
    send_errors: AtomicU64,
}

fn main() -> std::io::Result<()> {
    let mut a = Args::parse(USAGE);
    let bind: SocketAddr = a.get("bind", "0.0.0.0:40000".parse().unwrap());
    let cfg = SimConfig {
        spawn: a.get("spawn", SpawnMode::Uniform),
        max_clients: a.get("max-clients", 10_000),
        near_radius: a.get("near-radius", 150.0),
        near_max: a.get("near-max", 64),
        shards: a.get("shards", 64),
        preallocate: !a.flag("no-prealloc"),
        seed: a.get("seed", 1),
        net: Config { max_accepts_per_tick: a.get("accepts-per-tick", 256), ..Config::default() },
    };
    let egress: Egress = a.get("egress", Egress::default());
    let threads: Option<usize> = a.opt("threads");
    let duration = Duration::from_secs_f64(a.get("duration", 0.0));
    let until_empty = a.flag("until-empty");
    let report = Duration::from_secs_f64(a.get("report", 5.0));
    let warmup = Duration::from_secs_f64(a.get("warmup", 3.0));
    let csv_path: Option<String> = a.opt("csv");
    a.finish();

    if let Some(n) = threads {
        rayon::ThreadPoolBuilder::new().num_threads(n).build_global().expect("rayon pool");
    }

    let sock = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
    // Capped by net.core.{r,w}mem_max; raise those for big runs.
    sock.set_recv_buffer_size(16 << 20)?;
    sock.set_send_buffer_size(16 << 20)?;
    sock.bind(&bind.into())?;
    let (rcvbuf, sndbuf) = (sock.recv_buffer_size()?, sock.send_buffer_size()?);
    let sock: UdpSocket = sock.into();
    sock.set_read_timeout(Some(Duration::from_millis(50)))?;
    let sock = Arc::new(sock);

    let t0 = Instant::now();
    let mut sim = SimServer::new(cfg.clone(), t0);
    let shards = sim.shard_count();
    if cfg.preallocate {
        println!("preallocated connections for {} clients in {:?}", cfg.max_clients, t0.elapsed());
    }
    let start = Instant::now();
    let period = Duration::from_secs(1) / TICK_HZ;

    println!(
        "listening on {bind} | spawn {:?}, near {} within {} m, {} rayon threads, {shards} shards, {} accepts/tick, egress {egress:?} | socket buffers rcv {} KiB snd {} KiB",
        cfg.spawn,
        cfg.near_max,
        cfg.near_radius,
        rayon::current_num_threads(),
        cfg.net.max_accepts_per_tick,
        rcvbuf >> 10,
        sndbuf >> 10
    );

    let net = Arc::new(NetCounters::default());
    let inbox: Arc<Mutex<Vec<Vec<InDatagram>>>> = Arc::new(Mutex::new(vec![Vec::new(); shards]));
    let stop = Arc::new(AtomicBool::new(false));
    let receiver = {
        let (sock, net, inbox, stop) = (sock.clone(), net.clone(), inbox.clone(), stop.clone());
        let router = sim.router();
        std::thread::Builder::new().name("ingress".into()).spawn(move || {
            let mut buf = [0u8; 1500];
            while !stop.load(Relaxed) {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        // Arrival time: the start of the input's server-side wait and
                        // of the transport's ack_delay.
                        let arrived = Instant::now();
                        net.in_pkts.fetch_add(1, Relaxed);
                        net.in_bytes.fetch_add(n as u64, Relaxed);
                        let shard = router.shard(&from);
                        inbox.lock().unwrap()[shard].push((from, arrived, buf[..n].to_vec()));
                    }
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => {
                        net.recv_errors.fetch_add(1, Relaxed);
                    }
                }
            }
        })?
    };

    let mut csv = csv_path.map(open_csv).transpose()?;

    let mut inbound: Vec<Vec<InDatagram>> = vec![Vec::new(); shards];
    // Server-side input waits after warmup, 0.1 ms units.
    let mut kept_wait = Histogram::new(10_000);
    let mut out: Vec<Vec<Datagram>> = vec![Vec::new(); shards];
    let mut window = Window::new(start, &sim, &net);
    let mut kept: Vec<Row> = Vec::new();
    let mut first_client: Option<Instant> = None;
    // Counters as of the end of warmup, so the summary can separate the join burst.
    let mut warm: Option<Counters> = None;
    // ... and as of when clients start draining (a mass disconnect can lose
    // Disconnect packets and leave entities frozen until they time out).
    let mut cool: Option<Counters> = None;
    let mut peak_clients = 0;
    let mut next_tick = start;

    loop {
        let now = Instant::now();
        std::mem::swap(&mut inbound, &mut *inbox.lock().unwrap());

        let times = sim.tick(&mut inbound, now, &mut out);

        let t_egress = Instant::now();
        let (pkts, bytes) = out
            .par_iter_mut()
            .map(|bucket| {
                let (n, bytes) = (bucket.len(), bucket.iter().map(|(_, p)| p.len()).sum::<usize>());
                let errors = send_all(&sock, bucket, egress);
                if errors > 0 {
                    net.send_errors.fetch_add(errors as u64, Relaxed);
                }
                bucket.clear();
                (n, bytes)
            })
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        window.out_pkts += pkts as u64;
        window.out_bytes += bytes as u64;

        let done = Instant::now();
        let mut row = [0u32; COLS];
        for (r, t) in row.iter_mut().zip(times) {
            *r = t.as_micros() as u32;
        }
        row[COLS - 2] = (done - t_egress).as_micros() as u32;
        row[COLS - 1] = (done - now).as_micros() as u32;
        if done - now > period {
            window.overruns += 1;
        }
        window.rows.push(row);

        let clients = sim.client_count();
        peak_clients = peak_clients.max(clients);
        window.client_ticks += clients as u64;
        if warm.is_some() && cool.is_none() && clients < peak_clients * 9 / 10 {
            cool = Some(sim.counters().clone());
        }
        let steady = |warm: &Option<Counters>, cool: &Option<Counters>| warm.is_some() && cool.is_none();
        if clients > 0 {
            let first = *first_client.get_or_insert(now);
            if now - first >= warmup && cool.is_none() {
                warm.get_or_insert_with(|| sim.counters().clone());
                kept.push(row);
            }
        }

        if done - window.start >= report {
            let wait = sim.take_input_wait();
            if steady(&warm, &cool) {
                kept_wait.merge(&wait);
            }
            window.report(done - start, &sim, &wait, &net, csv.as_mut())?;
            window = Window::new(done, &sim, &net);
        }

        let finished = (!duration.is_zero() && done - start >= duration) || (until_empty && peak_clients > 0 && clients == 0);
        if finished {
            break;
        }

        next_tick += period;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now;
        }
    }

    if window.rows.len() > 1 {
        let wait = sim.take_input_wait();
        if warm.is_some() && cool.is_none() {
            kept_wait.merge(&wait);
        }
        window.report(Instant::now() - start, &sim, &wait, &net, csv.as_mut())?;
    }
    stop.store(true, Relaxed);
    let _ = receiver.join();
    print_summary(&mut kept, peak_clients, &sim, warm.as_ref(), cool.as_ref(), &kept_wait, &net);
    Ok(())
}

struct Window {
    start: Instant,
    rows: Vec<Row>,
    overruns: u64,
    out_pkts: u64,
    out_bytes: u64,
    client_ticks: u64,
    counters: Counters,
    in_pkts: u64,
    in_bytes: u64,
    kernel: [u64; 2],
    deferred: u64,
}

impl Window {
    fn new(start: Instant, sim: &SimServer, net: &NetCounters) -> Self {
        Self {
            start,
            rows: Vec::new(),
            overruns: 0,
            out_pkts: 0,
            out_bytes: 0,
            client_ticks: 0,
            counters: sim.counters().clone(),
            in_pkts: net.in_pkts.load(Relaxed),
            in_bytes: net.in_bytes.load(Relaxed),
            kernel: kernel_udp_drops(),
            deferred: sim.net().deferred_accepts(),
        }
    }

    fn report(
        &mut self,
        t: Duration,
        sim: &SimServer,
        wait: &Histogram,
        net: &NetCounters,
        csv: Option<&mut BufWriter<File>>,
    ) -> std::io::Result<()> {
        let ws = wait.summary();
        let tenth = |v: u32| v as f64 / 10.0;
        let secs = self.start.elapsed().as_secs_f64();
        let ticks = self.rows.len() as u64;
        let clients_avg = self.client_ticks as f64 / ticks.max(1) as f64;
        let per_client_secs = (clients_avg * secs).max(1e-9);
        let c = sim.counters();
        let in_pkts = net.in_pkts.load(Relaxed) - self.in_pkts;
        let in_bytes = net.in_bytes.load(Relaxed) - self.in_bytes;
        let (repeated, frozen) = (c.repeated - self.counters.repeated, c.frozen - self.counters.frozen);
        let entity_ticks = ((c.inputs_applied - self.counters.inputs_applied) + repeated + frozen).max(1) as f64;
        let (repeated_pct, frozen_pct) = (100.0 * repeated as f64 / entity_ticks, 100.0 * frozen as f64 / entity_ticks);
        let late = c.late_inputs - self.counters.late_inputs;
        let snaps = c.snapshots - self.counters.snapshots;
        let ents_avg = (c.snapshot_entities - self.counters.snapshot_entities) as f64 / snaps.max(1) as f64;
        let kernel = kernel_udp_drops();
        let deferred = sim.net().deferred_accepts() - self.deferred;
        let (rcv_drops, snd_drops) = (kernel[0] - self.kernel[0], kernel[1] - self.kernel[1]);
        let down_kbps = self.out_bytes as f64 * 8.0 / 1000.0 / per_client_secs;
        let up_kbps = in_bytes as f64 * 8.0 / 1000.0 / per_client_secs;

        let sums: Vec<_> = (0..COLS)
            .map(|i| summarize(&mut self.rows.iter().map(|r| r[i]).collect::<Vec<_>>()))
            .collect();
        let tick = sums[COLS - 1];
        println!(
            "[{:>5.0}s] clients {} | tick p50 {} p99 {} max {} ms, {} overruns | out {:.1}k pps {:.0} kbps/client, {:.0} Mbps | in {:.1}k pps {:.0} kbps/client | stand-ins: repeated {:.2}% frozen {:.2}%, {} late inputs | {:.1} entities/snapshot | input wait p50 {:.1} p99 {:.1} ms | {} joins deferred | kernel drops rcv {} snd {}",
            t.as_secs_f64(),
            sim.client_count(),
            ms(tick.p50),
            ms(tick.p99),
            ms(tick.max),
            self.overruns,
            self.out_pkts as f64 / secs / 1000.0,
            down_kbps,
            self.out_bytes as f64 * 8.0 / 1e6 / secs,
            in_pkts as f64 / secs / 1000.0,
            up_kbps,
            repeated_pct,
            frozen_pct,
            late,
            ents_avg,
            tenth(ws.p50),
            tenth(ws.p99),
            deferred,
            rcv_drops,
            snd_drops,
        );
        let phases: Vec<String> =
            (0..COLS - 1).map(|i| format!("{} {}/{}", col_name(i), ms(sums[i].p50), ms(sums[i].p99))).collect();
        println!("         p50/p99 ms: {}", phases.join("  "));

        if let Some(w) = csv {
            let mut line = format!("{:.1},{},{},{}", t.as_secs_f64(), sim.client_count(), ticks, self.overruns);
            for s in &sums {
                line += &format!(",{},{},{}", s.p50, s.p99, s.max);
            }
            line += &format!(
                ",{:.0},{:.0},{:.1},{:.1},{:.1},{:.3},{:.3},{},{:.1},{:.1},{:.1},{},{},{}",
                self.out_pkts as f64 / secs,
                in_pkts as f64 / secs,
                down_kbps,
                up_kbps,
                self.out_bytes as f64 * 8.0 / 1e6 / secs,
                repeated_pct,
                frozen_pct,
                late,
                ents_avg,
                tenth(ws.p50),
                tenth(ws.p99),
                deferred,
                rcv_drops,
                snd_drops
            );
            writeln!(w, "{line}")?;
            w.flush()?;
        }
        Ok(())
    }
}

fn open_csv(path: String) -> std::io::Result<BufWriter<File>> {
    let fresh = std::fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
    let mut w = BufWriter::new(File::options().create(true).append(true).open(&path)?);
    if fresh {
        let mut h = String::from("t_s,clients,ticks,overruns");
        for i in 0..COLS {
            let n = col_name(i);
            h += &format!(",{n}_p50_us,{n}_p99_us,{n}_max_us");
        }
        h += ",out_pps,in_pps,down_kbps_per_client,up_kbps_per_client,egress_mbps,repeated_pct,frozen_pct,late_inputs,entities_per_snapshot,input_wait_p50_ms,input_wait_p99_ms,deferred_accepts,kernel_rcvbuf_drops,kernel_sndbuf_drops";
        writeln!(w, "{h}")?;
    }
    Ok(w)
}

fn print_summary(
    kept: &mut [Row],
    peak: usize,
    sim: &SimServer,
    warm: Option<&Counters>,
    cool: Option<&Counters>,
    wait: &Histogram,
    net: &NetCounters,
) {
    let c = sim.counters();
    println!("\n== summary: {} steady-state ticks (after warmup, before clients drain), peak {peak} clients ==", kept.len());
    if kept.is_empty() {
        return;
    }
    println!("  {:<10} {:>8} {:>8} {:>8}", "phase", "p50 ms", "p99 ms", "max ms");
    for i in 0..COLS {
        let s = summarize(&mut kept.iter().map(|r| r[i]).collect::<Vec<_>>());
        println!("  {:<10} {:>8} {:>8} {:>8}", col_name(i), ms(s.p50), ms(s.p99), ms(s.max));
    }
    let over = kept.iter().filter(|r| r[COLS - 1] as u128 > (Duration::from_secs(1) / TICK_HZ).as_micros()).count();
    println!(
        "  overruns {over} | spawns {} despawns {} ({} joins deferred) | stand-ins: repeated {} frozen {} | late inputs {} discarded {} | bad messages {} | recv errors {} send errors {}",
        c.spawns,
        c.despawns,
        sim.net().deferred_accepts(),
        c.repeated,
        c.frozen,
        c.late_inputs,
        c.discarded_inputs,
        c.bad_messages,
        net.recv_errors.load(Relaxed),
        net.send_errors.load(Relaxed)
    );
    let ws = wait.summary();
    println!(
        "  input wait on the server (datagram arrival -> applied): p50 {:.1} p99 {:.1} max {:.1} mean {:.1} ms",
        ws.p50 as f64 / 10.0,
        ws.p99 as f64 / 10.0,
        ws.max as f64 / 10.0,
        wait.mean() / 10.0
    );
    if let Some(w) = warm {
        let e = cool.unwrap_or(c);
        println!(
            "  steady state: stand-ins repeated {} frozen {} | late inputs {} discarded {}",
            e.repeated - w.repeated,
            e.frozen - w.frozen,
            e.late_inputs - w.late_inputs,
            e.discarded_inputs - w.discarded_inputs
        );
    }
}

fn ms(us: u32) -> String {
    format!("{:.2}", us as f64 / 1000.0)
}

/// System-wide UDP RcvbufErrors / SndbufErrors (includes the bots when they share the box).
#[cfg(target_os = "linux")]
fn kernel_udp_drops() -> [u64; 2] {
    let Ok(snmp) = std::fs::read_to_string("/proc/net/snmp") else { return [0; 2] };
    let mut lines = snmp.lines().filter(|l| l.starts_with("Udp:"));
    let (Some(names), Some(values)) = (lines.next(), lines.next()) else { return [0; 2] };
    let get = |key: &str| {
        let i = names.split_whitespace().position(|n| n == key)?;
        values.split_whitespace().nth(i)?.parse().ok()
    };
    [get("RcvbufErrors").unwrap_or(0), get("SndbufErrors").unwrap_or(0)]
}

#[cfg(not(target_os = "linux"))]
fn kernel_udp_drops() -> [u64; 2] {
    [0; 2]
}
