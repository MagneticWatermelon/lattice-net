//! M1 headless server: `SimServer` on a real UDP socket at 30 Hz, reporting
//! per-phase tick times, bandwidth and pps.
//!
//! A dedicated thread blocks on `recv_from` and queues datagrams, so arrivals
//! spread across the tick don't have to fit in the kernel buffer. Egress is
//! plain `send_to` spread over the rayon pool (`sendmmsg`/GSO come later).

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Write};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_sim::cli::Args;
use lattice_sim::movement::TICK_HZ;
use lattice_sim::server::{Counters, SimConfig, SimServer, SpawnMode, PHASES};
use lattice_sim::stats::summarize;
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
  --duration S         stop after S seconds (0 = run forever) [0]
  --until-empty        stop once clients connected and then all left
  --report S           report interval, seconds [5]
  --warmup S           ignore the first S seconds after the first client in the summary [3]
  --csv PATH           append one row per report window
  --seed N             spawn RNG seed [1]";

/// Columns of per-tick timing samples: the sim phases, then egress and total.
const COLS: usize = PHASES.len() + 2;
type Row = [u32; COLS];
type Datagrams = Vec<(SocketAddr, Vec<u8>)>;

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
        seed: a.get("seed", 1),
        ..Default::default()
    };
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

    println!(
        "listening on {bind} | spawn {:?}, near {} within {} m, {} rayon threads | socket buffers rcv {} KiB snd {} KiB",
        cfg.spawn,
        cfg.near_max,
        cfg.near_radius,
        rayon::current_num_threads(),
        rcvbuf >> 10,
        sndbuf >> 10
    );

    let net = Arc::new(NetCounters::default());
    let inbox: Arc<Mutex<Datagrams>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let receiver = {
        let (sock, net, inbox, stop) = (sock.clone(), net.clone(), inbox.clone(), stop.clone());
        std::thread::Builder::new().name("ingress".into()).spawn(move || {
            let mut buf = [0u8; 1500];
            while !stop.load(Relaxed) {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        net.in_pkts.fetch_add(1, Relaxed);
                        net.in_bytes.fetch_add(n as u64, Relaxed);
                        inbox.lock().unwrap().push((from, buf[..n].to_vec()));
                    }
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => {
                        net.recv_errors.fetch_add(1, Relaxed);
                    }
                }
            }
        })?
    };

    let start = Instant::now();
    let period = Duration::from_secs(1) / TICK_HZ;
    let mut sim = SimServer::new(cfg, start);
    let mut csv = csv_path.map(open_csv).transpose()?;

    let mut inbound = Vec::new();
    let mut out = Vec::new();
    let mut window = Window::new(start, sim.counters(), &net);
    let mut kept: Vec<Row> = Vec::new();
    let mut first_client: Option<Instant> = None;
    let mut peak_clients = 0;
    let mut next_tick = start;

    loop {
        let now = Instant::now();
        std::mem::swap(&mut inbound, &mut *inbox.lock().unwrap());

        let times = sim.tick(&mut inbound, now, &mut out);

        let t_egress = Instant::now();
        let bytes: usize = out.iter().map(|(_, p)| p.len()).sum();
        out.par_chunks(128).for_each(|chunk| {
            for (addr, pkt) in chunk {
                if sock.send_to(pkt, addr).is_err() {
                    net.send_errors.fetch_add(1, Relaxed);
                }
            }
        });
        window.out_pkts += out.len() as u64;
        window.out_bytes += bytes as u64;
        out.clear();

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
        if clients > 0 {
            let first = *first_client.get_or_insert(now);
            if now - first >= warmup {
                kept.push(row);
            }
        }

        if done - window.start >= report {
            window.report(done - start, &sim, &net, csv.as_mut())?;
            window = Window::new(done, sim.counters(), &net);
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
        window.report(Instant::now() - start, &sim, &net, csv.as_mut())?;
    }
    stop.store(true, Relaxed);
    let _ = receiver.join();
    print_summary(&mut kept, peak_clients, sim.counters(), &net);
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
}

impl Window {
    fn new(start: Instant, counters: &Counters, net: &NetCounters) -> Self {
        Self {
            start,
            rows: Vec::new(),
            overruns: 0,
            out_pkts: 0,
            out_bytes: 0,
            client_ticks: 0,
            counters: counters.clone(),
            in_pkts: net.in_pkts.load(Relaxed),
            in_bytes: net.in_bytes.load(Relaxed),
            kernel: kernel_udp_drops(),
        }
    }

    fn report(&mut self, t: Duration, sim: &SimServer, net: &NetCounters, csv: Option<&mut BufWriter<File>>) -> std::io::Result<()> {
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
        let (rcv_drops, snd_drops) = (kernel[0] - self.kernel[0], kernel[1] - self.kernel[1]);
        let down_kbps = self.out_bytes as f64 * 8.0 / 1000.0 / per_client_secs;
        let up_kbps = in_bytes as f64 * 8.0 / 1000.0 / per_client_secs;

        let sums: Vec<_> = (0..COLS)
            .map(|i| summarize(&mut self.rows.iter().map(|r| r[i]).collect::<Vec<_>>()))
            .collect();
        let tick = sums[COLS - 1];
        println!(
            "[{:>5.0}s] clients {} | tick p50 {} p99 {} max {} ms, {} overruns | out {:.1}k pps {:.0} kbps/client, {:.0} Mbps | in {:.1}k pps {:.0} kbps/client | stand-ins: repeated {:.2}% frozen {:.2}%, {} late inputs | {:.1} entities/snapshot | kernel drops rcv {} snd {}",
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
                ",{:.0},{:.0},{:.1},{:.1},{:.1},{:.3},{:.3},{},{:.1},{},{}",
                self.out_pkts as f64 / secs,
                in_pkts as f64 / secs,
                down_kbps,
                up_kbps,
                self.out_bytes as f64 * 8.0 / 1e6 / secs,
                repeated_pct,
                frozen_pct,
                late,
                ents_avg,
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
        h += ",out_pps,in_pps,down_kbps_per_client,up_kbps_per_client,egress_mbps,repeated_pct,frozen_pct,late_inputs,entities_per_snapshot,kernel_rcvbuf_drops,kernel_sndbuf_drops";
        writeln!(w, "{h}")?;
    }
    Ok(w)
}

fn print_summary(kept: &mut [Row], peak: usize, c: &Counters, net: &NetCounters) {
    println!("\n== summary: {} ticks after warmup, peak {peak} clients ==", kept.len());
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
        "  overruns {over} | spawns {} despawns {} | stand-ins: repeated {} frozen {} | late inputs {} discarded {} | bad messages {} | recv errors {} send errors {}",
        c.spawns,
        c.despawns,
        c.repeated,
        c.frozen,
        c.late_inputs,
        c.discarded_inputs,
        c.bad_messages,
        net.recv_errors.load(Relaxed),
        net.send_errors.load(Relaxed)
    );
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
