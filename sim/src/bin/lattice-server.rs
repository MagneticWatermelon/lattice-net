//! M1 headless server: `SimServer` on a real UDP socket at 30 Hz, reporting
//! per-phase tick times, bandwidth and pps.
//!
//! A dedicated thread per socket receives in `recvmmsg` batches on Linux (each
//! datagram stamped by the kernel on arrival) and queues them into per-shard
//! buckets, so arrivals spread across the tick don't have to fit in the kernel
//! buffer and routing costs no tick time. Egress is one rayon task per shard,
//! batched with `sendmmsg` on Linux, optionally with GSO (`--egress gso`): each
//! client's datagrams for the tick go out as one `UDP_SEGMENT` send, from
//! the shard's own assembly task as soon as its clients' messages are framed
//! (`--send-during-assembly`). The tick and its egress run on a rayon worker
//! with the others kept awake until they finish (`--keep-awake`), so a
//! parallel pass doesn't wait for them to wake.

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Write};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_net::Config;
use lattice_sim::cli::{Args, HexKey};
use lattice_sim::movement::TICK_HZ;
use lattice_sim::interest::InterestConfig;
use lattice_sim::ladder::{LadderConfig, RUNGS};
use lattice_sim::pool;
use lattice_sim::server::{Counters, Datagram, InDatagram, SimConfig, SimServer, SpawnMode, PHASES};
use lattice_sim::stats::{summarize, Histogram, KeyValues};
use lattice_sim::udp::{enable_rx_timestamps, Clocks, RecvBatch};
use rayon::prelude::*;
use socket2::{Domain, Protocol, Socket, Type};

const USAGE: &str = "\
lattice-server: M1 movement-only authoritative server

  --bind ADDR          [0.0.0.0:40000]
  --spawn MODE         uniform | hotspots | blob | line:<meters> | disk:<meters>   [uniform]
                       disk: everyone in one disk of that radius (the density limit)
  --max-clients N      [10000]
  --near-radius M      near tier: 30 Hz, accumulator [150]
  --near-per-tick N    near entities sent per client per tick [64]
  --mid-radius M       mid tier: 10 Hz, staggered by id [500]
  --far-radius M       far tier: 2 Hz, staggered by id [1500]
  --budget-kbps K      snapshot budget per client [1500]
  --squad-size N       squadmates are near-tier at any distance (0 = none) [4]
  --ladder on|off      degrade under load: radii, rates, 20 Hz, dilation [on]
  --ladder-high F      step down when the p90 of work/period exceeds this [0.85]
  --ladder-low F       step up after 3 s with work/period below this [0.6]
  --threads N          rayon threads [one per physical core: SMT siblings only add
                       scheduler overhead (on 64 cores, 128 threads ran slower than 64)]
  --keep-awake on|off  during a tick, idle rayon workers keep looking for work instead of
                       sleeping between phases, so a parallel pass doesn't wait for them
                       to wake (they still yield to the OS); between ticks they sleep.
                       Assumes one server per machine: two would starve each other [on]
  --send-during-assembly on|off
                       each shard sends its datagrams as soon as its clients' messages
                       are framed, while other shards still assemble, instead of all
                       shards after the last; the assembly phase's time then includes
                       framing and sending [on]
  --worker-cpus LIST   pin the rayon workers to these CPUs (`0-59`; one each when the list has
                       one per thread); --threads defaults to the list's length [unpinned]
  --rx-cpus LIST       pin the receive threads (and the main thread) to these CPUs, apart
                       from the workers; steer the NIC's interrupts there too
                       (scripts/irq-affinity.sh) [unpinned]
  --shards N           transport shards [64]
  --sockets N          receiving sockets on the port (SO_REUSEPORT), each with its own receive
                       thread and an equal run of the shards; each shard sends from its
                       group's socket. N must divide --shards (Linux) [1]
  --accepts-per-tick N new connections accepted per tick, server-wide (0 = no limit) [256]
  --no-prealloc        allocate connections on accept instead of pooling max-clients at startup
  --egress MODE        gso | sendmmsg | sendto   [sendmmsg on Linux, else sendto]
                       gso: sendmmsg with one UDP_SEGMENT send per client (pads
                       all but a client's last packet to full size; Linux 4.18+)
  --ingress MODE       recvmmsg | recvfrom   [recvmmsg on Linux, else recvfrom]
                       recvmmsg: up to 64 datagrams per syscall, stamped by the kernel on
                       arrival; recvfrom: one syscall per datagram, stamped when read
  --rx-gather-us U     recvmmsg: after a batch that wasn't full, wait up to U microseconds so
                       the next call finds a batch (a receive thread waking per datagram or
                       three costs more than the receiving), but never past just before the
                       next tick starts, so no datagram misses a tick for it; 0 = receive
                       again at once [1000]
  --duration S         stop after S seconds (0 = run forever) [0]
  --until-empty        stop once clients connected and then all left
  --report S           report interval, seconds [5]
  --warmup S           ignore the first S seconds after the first client in the summary [3];
                       the summary also stops once clients drain below 90% of peak
  --csv PATH           append one row per report window
  --summary PATH       write the end-of-run results as key=value lines (scripts/baseline.sh)
  --session-log PATH   append a JSON line per client session: when a judgment of its shots
                       first flags it, when it leaves, and for those still here at stop.
                       Keyed by the connect token's user id; its inputs, stand-ins, shots,
                       holds, trims, hits, deaths, RTT, and how its shots dipped under the
                       render floor without its allowances (judged every 100 shots)
  --debug-http ADDR    serve the debug map (what one client receives) on ADDR, e.g. 0.0.0.0:8080
  --token-key KEY      64 hex digits shared by server and bots, or @FILE to read them from
                       a file (the bots mint their own connect tokens, standing in for a
                       login service) [the public dev key]
  --server-id N        the server id tokens are minted for [1]
  --seed N             spawn RNG seed [1]
  --world-seed N       the world's terrain and cover (clients build it from the Welcome) [1]
  --no-separation      don't push overlapping players apart (for comparisons)
  --deaths-per-sec R   kill R random players a second, to exercise death and respawn
                       until there are weapons (they respawn after 5 s) [0]
  --immortal           hits land (and are confirmed) but deal no damage: hit rates then
                       measure aim and lag compensation, not who died first
  --no-cone            shots go exactly where aimed: no cone of fire or bloom (to compare
                       with fight baselines from before it)
  --no-render-floor    don't hold shots' claimed render steps to each client's render clock
                       (only the backtrack bound applies): to measure what it stops";

/// Columns of per-tick timing samples: the sim phases, then egress and total.
const COLS: usize = PHASES.len() + 2;
type Row = [u32; COLS];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Egress {
    /// One `send_to` syscall per datagram.
    SendTo,
    /// Up to `MMSG_BATCH` datagrams per `sendmmsg` syscall (Linux only).
    SendMmsg,
    /// `sendmmsg` where each entry is one client's datagrams as a single
    /// `UDP_SEGMENT` (GSO) send (Linux only).
    Gso,
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
            "gso" if cfg!(target_os = "linux") => Ok(Egress::Gso),
            "sendmmsg" | "gso" => Err(format!("{s} needs Linux")),
            _ => Err(format!("unknown egress mode {s:?} (gso|sendmmsg|sendto)")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ingress {
    /// One `recv_from` syscall per datagram, stamped when it's read.
    RecvFrom,
    /// `recvmmsg` batches of up to `RX_BATCH`, stamped by the kernel when they
    /// arrived (`SO_TIMESTAMPNS`), so time queued in the socket counts as
    /// waiting (Linux only).
    RecvMmsg,
}

impl Default for Ingress {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Ingress::RecvMmsg
        } else {
            Ingress::RecvFrom
        }
    }
}

impl std::str::FromStr for Ingress {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "recvfrom" => Ok(Ingress::RecvFrom),
            "recvmmsg" if cfg!(target_os = "linux") => Ok(Ingress::RecvMmsg),
            "recvmmsg" => Err("recvmmsg needs Linux".into()),
            _ => Err(format!("unknown ingress mode {s:?} (recvmmsg|recvfrom)")),
        }
    }
}

/// Datagrams per `recvmmsg`. At 10k clients one socket takes ~300 a
/// millisecond, so a call finds a full batch after ~0.2 ms of gathering.
const RX_BATCH: usize = 64;

/// How long before a tick starts the receive threads stop gathering and take
/// whatever arrives at once: covers a sleep's overshoot (timer slack is 50 us).
const RX_DRAIN_BEFORE_TICK: Duration = Duration::from_micros(200);

/// When the next tick starts, published by the main loop for the receive
/// threads (nanoseconds since `base`).
struct TickClock {
    base: Instant,
    next: AtomicU64,
}

impl TickClock {
    fn new(base: Instant) -> Self {
        Self { base, next: AtomicU64::new(0) }
    }

    fn publish(&self, next_tick: Instant) {
        self.next.store(next_tick.saturating_duration_since(self.base).as_nanos() as u64, Relaxed);
    }

    /// How long a receive thread may gather at `now`: up to `gather`, but it
    /// wakes `RX_DRAIN_BEFORE_TICK` before the next tick (zero once inside that).
    fn gather_for(&self, now: Instant, gather: Duration) -> Duration {
        let until = self.base + Duration::from_nanos(self.next.load(Relaxed));
        until.saturating_duration_since(now).saturating_sub(RX_DRAIN_BEFORE_TICK).min(gather)
    }
}

/// What a receive thread needs: its socket, and where its group's datagrams go.
struct Receiver {
    sock: Arc<UdpSocket>,
    group: usize,
    per_group: usize,
    router: lattice_net::Router,
    inbox: Arc<Mutex<Vec<Vec<InDatagram>>>>,
    net: Arc<NetCounters>,
    stop: Arc<AtomicBool>,
    /// recvmmsg: after a batch that wasn't full, sleep up to this long so the
    /// next one finds more queued (zero: receive again at once). Waking is what
    /// a receive thread pays for: on WSL at 150k pps, receiving whatever was
    /// there (3 a call) kept it 40% busy, the same as `recv_from`; gathering for
    /// 250 us (42 a call), 16%. The kernel's arrival stamps keep the waits exact,
    /// and `tick` keeps a gather from making a datagram miss its tick.
    gather: Duration,
    tick: Arc<TickClock>,
}

impl Receiver {
    fn run(&self, mode: Ingress) {
        match mode {
            Ingress::RecvFrom => self.each(),
            Ingress::RecvMmsg => self.batched(),
        }
    }

    /// One `recv_from`, one lock and one count per datagram.
    fn each(&self) {
        let mut buf = [0u8; 1500];
        while !self.stop.load(Relaxed) {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    // Arrival time: the start of the input's server-side wait and
                    // of the transport's ack_delay.
                    let arrived = Instant::now();
                    self.net.in_pkts.fetch_add(1, Relaxed);
                    self.net.in_bytes.fetch_add(n as u64, Relaxed);
                    self.net.recv_calls.fetch_add(1, Relaxed);
                    let shard = self.router.shard_in(self.group, &from) - self.group * self.per_group;
                    self.inbox.lock().unwrap()[shard].push((from, arrived, buf[..n].to_vec()));
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(_) => {
                    self.net.recv_errors.fetch_add(1, Relaxed);
                }
            }
        }
    }

    /// Whatever is queued, up to `RX_BATCH`, per syscall; routed, then queued
    /// under one lock and counted once.
    fn batched(&self) {
        let mut rx = RecvBatch::new(RX_BATCH);
        let mut routed: Vec<(usize, InDatagram)> = Vec::with_capacity(RX_BATCH);
        // When a call last left the socket empty: nothing after it arrived before.
        let mut drained = None;
        while !self.stop.load(Relaxed) {
            let n = match rx.recv(&self.sock, true) {
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => continue,
                Err(_) => {
                    self.net.recv_errors.fetch_add(1, Relaxed);
                    continue;
                }
            };
            let clocks = Clocks::now();
            let mut bytes = 0;
            for i in 0..n {
                let (data, from, stamp) = rx.get(i);
                let Some(from) = from else { continue };
                bytes += data.len();
                // The kernel's arrival time: the input's server-side wait and the
                // transport's ack_delay include time queued in the socket.
                let arrived = clocks.arrival(stamp, drained);
                let shard = self.router.shard_in(self.group, &from) - self.group * self.per_group;
                routed.push((shard, (from, arrived, data.to_vec())));
            }
            self.net.in_pkts.fetch_add(n as u64, Relaxed);
            self.net.in_bytes.fetch_add(bytes as u64, Relaxed);
            self.net.recv_calls.fetch_add(1, Relaxed);
            {
                let mut inbox = self.inbox.lock().unwrap();
                for (shard, d) in routed.drain(..) {
                    inbox[shard].push(d);
                }
            }
            if n < RX_BATCH {
                drained = Some(clocks.instant);
                let wait = self.tick.gather_for(clocks.instant, self.gather);
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
            }
        }
    }
}

/// CPU time each receive thread has used so far (Linux; empty elsewhere).
fn thread_cpu(threads: &[std::thread::JoinHandle<()>]) -> Vec<Duration> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::thread::JoinHandleExt;
        threads
            .iter()
            .filter_map(|h| {
                let mut clock: libc::clockid_t = 0;
                // SAFETY: the thread is alive (joined only after the last call) and
                // both out-pointers are valid.
                let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
                let ok = unsafe {
                    libc::pthread_getcpuclockid(h.as_pthread_t(), &mut clock) == 0 && libc::clock_gettime(clock, &mut ts) == 0
                };
                ok.then(|| Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
            })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = threads;
        Vec::new()
    }
}

/// The busiest and the mean receive thread's share of one core between two
/// `thread_cpu` readings `secs` apart, in percent.
fn busy_pct(from: &[Duration], to: &[Duration], secs: f64) -> Option<(f64, f64)> {
    if from.is_empty() || from.len() != to.len() || secs <= 0.0 {
        return None;
    }
    let pct: Vec<f64> = from.iter().zip(to).map(|(a, b)| 100.0 * b.saturating_sub(*a).as_secs_f64() / secs).collect();
    Some((pct.iter().cloned().fold(0.0, f64::max), pct.iter().sum::<f64>() / pct.len() as f64))
}

/// What one bucket's egress took.
#[derive(Debug, Default, Clone, Copy)]
struct Sent {
    /// Datagrams the kernel rejected.
    errors: usize,
    /// Sends handed to the kernel: one per datagram, or per GSO run.
    sends: usize,
    syscalls: usize,
}

impl std::ops::Add for Sent {
    type Output = Sent;
    fn add(self, o: Sent) -> Sent {
        Sent { errors: self.errors + o.errors, sends: self.sends + o.sends, syscalls: self.syscalls + o.syscalls }
    }
}

/// Sends every datagram.
fn send_all(sock: &UdpSocket, batch: &[Datagram], mode: Egress) -> Sent {
    match mode {
        #[cfg(target_os = "linux")]
        Egress::SendMmsg => send_mmsg(sock, batch),
        #[cfg(target_os = "linux")]
        Egress::Gso => send_gso(sock, batch),
        _ => Sent {
            errors: batch.iter().filter(|(addr, pkt)| sock.send_to(pkt, addr).is_err()).count(),
            sends: batch.len(),
            syscalls: batch.len(),
        },
    }
}

#[cfg(target_os = "linux")]
const MMSG_BATCH: usize = 256;

/// `sendmmsg` in batches. A datagram the kernel rejects is counted and
/// skipped; the rest of the batch is retried from the next one.
#[cfg(target_os = "linux")]
fn send_mmsg(sock: &UdpSocket, batch: &[Datagram]) -> Sent {
    use std::os::fd::AsRawFd;
    let fd = sock.as_raw_fd();
    let mut sent = Sent { sends: batch.len(), ..Sent::default() };
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
            sent.syscalls += 1;
            if n > 0 {
                off += n as usize;
            } else if n < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            } else {
                sent.errors += 1; // the datagram at `off` was rejected
                off += 1;
            }
        }
    }
    sent
}

/// Most segments per GSO send: the kernel's `UDP_MAX_SEGMENTS` (64), and
/// the whole send must stay under 64 KB.
#[cfg(target_os = "linux")]
fn max_segments(seg: usize) -> usize {
    (65_000 / seg.max(1)).min(64)
}

/// Splits `batch` into GSO runs: consecutive datagrams to one address where
/// all but the last have the same size and the last is no bigger (what
/// `UDP_SEGMENT` requires). The transport emits a client's datagrams back to
/// back, padded (`Config::pad_packets`), so a run is normally one client's tick.
#[cfg(target_os = "linux")]
fn gso_runs(batch: &[Datagram]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < batch.len() {
        let seg = batch[i].1.len();
        let mut j = i + 1;
        while j < batch.len()
            && j - i < max_segments(seg)
            && batch[j].0 == batch[i].0
            && batch[j - 1].1.len() == seg
            && batch[j].1.len() <= seg
        {
            j += 1;
        }
        runs.push((i, j - i));
        i = j;
    }
    runs
}

/// Whether the kernel knows `UDP_SEGMENT` (Linux 4.18+).
#[cfg(target_os = "linux")]
fn gso_supported(sock: &UdpSocket) -> bool {
    use std::os::fd::AsRawFd;
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: v and len are valid for writes of the sizes given.
    let r = unsafe {
        libc::getsockopt(sock.as_raw_fd(), libc::SOL_UDP, libc::UDP_SEGMENT, &mut v as *mut _ as *mut libc::c_void, &mut len)
    };
    r == 0
}

/// `sendmmsg` where each entry is a GSO run (`gso_runs`): one datagram per
/// segment on the wire, but one trip through the stack per run. A run the
/// kernel rejects counts all its datagrams as errors.
#[cfg(target_os = "linux")]
fn send_gso(sock: &UdpSocket, batch: &[Datagram]) -> Sent {
    use std::os::fd::AsRawFd;
    let fd = sock.as_raw_fd();
    // SAFETY: CMSG_SPACE is a pure size computation.
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<u16>() as u32) } as usize;
    assert!(space <= std::mem::size_of::<[u64; 4]>());
    let runs = gso_runs(batch);
    let mut sent = Sent { sends: runs.len(), ..Sent::default() };
    let mut iovs: Vec<libc::iovec> = Vec::new();
    for chunk in runs.chunks(MMSG_BATCH) {
        let addrs: Vec<socket2::SockAddr> = chunk.iter().map(|&(i, _)| socket2::SockAddr::from(batch[i].0)).collect();
        iovs.clear();
        iovs.extend(chunk.iter().flat_map(|&(i, n)| &batch[i..i + n]).map(|(_, p)| libc::iovec {
            iov_base: p.as_ptr() as *mut libc::c_void,
            iov_len: p.len(),
        }));
        let mut cmsgs = vec![[0u64; 4]; chunk.len()];
        let (iov_base, cmsg_base) = (iovs.as_mut_ptr(), cmsgs.as_mut_ptr());
        let mut first = 0;
        let mut msgs: Vec<libc::mmsghdr> = chunk
            .iter()
            .zip(&addrs)
            .enumerate()
            .map(|(k, (&(i, n), addr))| {
                // SAFETY: msghdr is plain data; all-zero is a valid empty header.
                let mut h: libc::msghdr = unsafe { std::mem::zeroed() };
                h.msg_name = addr.as_ptr() as *mut libc::c_void;
                h.msg_namelen = addr.len();
                // SAFETY: first + n <= iovs.len(): the runs of this chunk, in order.
                h.msg_iov = unsafe { iov_base.add(first) };
                h.msg_iovlen = n;
                first += n;
                if n > 1 {
                    // SAFETY: k < cmsgs.len(); each buffer is 8-aligned and at least
                    // CMSG_SPACE(2) bytes, so the header and its u16 fit.
                    unsafe {
                        h.msg_control = cmsg_base.add(k) as *mut libc::c_void;
                        h.msg_controllen = space;
                        let c = libc::CMSG_FIRSTHDR(&h);
                        (*c).cmsg_level = libc::SOL_UDP;
                        (*c).cmsg_type = libc::UDP_SEGMENT;
                        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<u16>() as u32) as _;
                        std::ptr::write_unaligned(libc::CMSG_DATA(c) as *mut u16, batch[i].1.len() as u16);
                    }
                }
                libc::mmsghdr { msg_hdr: h, msg_len: 0 }
            })
            .collect();
        let mut off = 0;
        while off < msgs.len() {
            // SAFETY: every header points into `addrs`, `iovs`, `cmsgs` and the
            // datagrams in `batch`, all alive and unmoved for the duration of the call.
            let n = unsafe { libc::sendmmsg(fd, msgs.as_mut_ptr().add(off), (msgs.len() - off) as libc::c_uint, 0) };
            sent.syscalls += 1;
            if n > 0 {
                off += n as usize;
            } else if n < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            } else {
                sent.errors += chunk[off].1; // the run at `off` was rejected
                off += 1;
            }
        }
    }
    sent
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
    /// Receive syscalls that returned datagrams.
    recv_calls: AtomicU64,
    send_errors: AtomicU64,
    /// Datagrams sent, and the sends (a GSO run counts once) and syscalls it took.
    out_pkts: AtomicU64,
    sends: AtomicU64,
    send_syscalls: AtomicU64,
}

fn main() -> std::io::Result<()> {
    let mut a = Args::parse(USAGE);
    let bind: SocketAddr = a.get("bind", "0.0.0.0:40000".parse().unwrap());
    let egress: Egress = a.get("egress", Egress::default());
    let ingress: Ingress = a.get("ingress", Ingress::default());
    let gather = Duration::from_micros(a.get("rx-gather-us", 1000));
    let cfg = SimConfig {
        spawn: a.get("spawn", SpawnMode::Uniform),
        max_clients: a.get("max-clients", 10_000),
        interest: {
            let d = InterestConfig::default();
            InterestConfig {
                near_radius: a.get("near-radius", d.near_radius),
                near_per_tick: a.get("near-per-tick", d.near_per_tick),
                mid_radius: a.get("mid-radius", d.mid_radius),
                far_radius: a.get("far-radius", d.far_radius),
                budget_bytes: a.get("budget-kbps", 1500) * 1000 / 8 / TICK_HZ as usize,
                squad_size: a.get("squad-size", d.squad_size),
                ..d
            }
        },
        shards: a.get("shards", 64),
        socket_groups: a.get("sockets", 1),
        preallocate: !a.flag("no-prealloc"),
        ladder: {
            let d = LadderConfig::default();
            let enabled = match a.get("ladder", "on".to_string()).as_str() {
                "on" => true,
                "off" => false,
                other => panic!("--ladder {other:?}: expected on or off"),
            };
            LadderConfig { enabled, high: a.get("ladder-high", d.high), low: a.get("ladder-low", d.low) }
        },
        seed: a.get("seed", 1),
        world_seed: a.get("world-seed", 1),
        separation: !a.flag("no-separation"),
        deaths_per_sec: a.get("deaths-per-sec", 0.0),
        immortal: a.flag("immortal"),
        cone_of_fire: !a.flag("no-cone"),
        // Where shots go in their cones: a fresh secret from the OS.
        spread_secret: None,
        render_floor: !a.flag("no-render-floor"),
        // Ticks take real time here: stamp sends when they're flushed.
        real_time: true,
        identity: lattice_net::ServerIdentity {
            token_key: a.get("token-key", HexKey::default()).0,
            server_id: a.get("server-id", 1),
        },
        net: Config {
            max_accepts_per_tick: a.get("accepts-per-tick", 256),
            pad_packets: egress == Egress::Gso,
            ..SimConfig::default().net
        },
    };
    let cpu_list = |list: Option<String>, flag: &str| {
        list.map(|l| lattice_sim::cpus::parse_list(&l).unwrap_or_else(|e| panic!("--{flag}: {e}")))
    };
    let worker_cpus = cpu_list(a.opt("worker-cpus"), "worker-cpus");
    let rx_cpus = cpu_list(a.opt("rx-cpus"), "rx-cpus");
    let threads: usize = a.get("threads", worker_cpus.as_ref().map_or_else(physical_cores, |c| c.len()));
    let mut on_off = |flag: &str| match a.get(flag, "on".to_string()).as_str() {
        "on" => true,
        "off" => false,
        other => panic!("--{flag} {other:?}: expected on or off"),
    };
    let keep_awake = on_off("keep-awake");
    let send_early = on_off("send-during-assembly");
    let duration = Duration::from_secs_f64(a.get("duration", 0.0));
    let until_empty = a.flag("until-empty");
    let report = Duration::from_secs_f64(a.get("report", 5.0));
    let warmup = Duration::from_secs_f64(a.get("warmup", 3.0));
    let csv_path: Option<String> = a.opt("csv");
    let summary_path: Option<String> = a.opt("summary");
    let session_path: Option<String> = a.opt("session-log");
    let debug_http: Option<SocketAddr> = a.opt("debug-http");
    a.finish();
    let mut session_log = match &session_path {
        Some(path) => Some(BufWriter::new(std::fs::OpenOptions::new().create(true).append(true).open(path)?)),
        None => None,
    };

    let mut pool = rayon::ThreadPoolBuilder::new().num_threads(threads);
    if let Some(cpus) = worker_cpus.clone() {
        pool = pool.start_handler(move |i| {
            lattice_sim::cpus::pin(lattice_sim::cpus::for_thread(&cpus, i, threads)).expect("--worker-cpus: pinning a worker");
        });
    }
    pool.build_global().expect("rayon pool");
    // The main thread runs no tick work (that's on a worker): it goes with the
    // receive threads, off the workers' cores.
    if let Some(cpus) = &rx_cpus {
        lattice_sim::cpus::pin(cpus).expect("--rx-cpus: pinning the main thread");
    }

    let groups = cfg.socket_groups;
    if groups == 0 || !cfg.shards.is_multiple_of(groups) {
        return Err(std::io::Error::other(format!("--sockets {groups} must divide --shards {}", cfg.shards)));
    }
    let mut socks = Vec::with_capacity(groups);
    let (mut rcvbuf, mut sndbuf) = (0, 0);
    for _ in 0..groups {
        let sock = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
        // Capped by net.core.{r,w}mem_max; raise those for big runs.
        sock.set_recv_buffer_size(16 << 20)?;
        sock.set_send_buffer_size(16 << 20)?;
        if groups > 1 {
            #[cfg(unix)]
            sock.set_reuse_port(true)?;
            #[cfg(not(unix))]
            return Err(std::io::Error::other("--sockets above 1 needs SO_REUSEPORT (Unix)"));
        }
        sock.bind(&bind.into())?;
        (rcvbuf, sndbuf) = (sock.recv_buffer_size()?, sock.send_buffer_size()?);
        let sock: UdpSocket = sock.into();
        #[cfg(target_os = "linux")]
        if egress == Egress::Gso && !gso_supported(&sock) {
            return Err(std::io::Error::other("--egress gso: this kernel has no UDP_SEGMENT (needs Linux 4.18+)"));
        }
        if ingress == Ingress::RecvMmsg {
            enable_rx_timestamps(&sock)?;
        }
        sock.set_read_timeout(Some(Duration::from_millis(50)))?;
        socks.push(Arc::new(sock));
    }

    let t0 = Instant::now();
    let mut sim = SimServer::new(cfg.clone(), t0);
    let shards = sim.shard_count();
    if cfg.preallocate {
        println!("preallocated connections for {} clients in {:?}", cfg.max_clients, t0.elapsed());
    }
    let start = Instant::now();

    println!(
        "listening on {bind} | spawn {:?} | tiers near {}@{} m, mid {} m, far {} m, budget {} B/tick, squads of {} | {} rayon threads (keep awake {}), {shards} shards (send during assembly {}), {groups} socket(s), {} accepts/tick, egress {egress:?}, ingress {ingress:?} | socket buffers rcv {} KiB snd {} KiB",
        cfg.spawn,
        cfg.interest.near_per_tick,
        cfg.interest.near_radius,
        cfg.interest.mid_radius,
        cfg.interest.far_radius,
        cfg.interest.budget_bytes,
        cfg.interest.squad_size,
        rayon::current_num_threads(),
        if keep_awake { "on" } else { "off" },
        if send_early { "on" } else { "off" },
        cfg.net.max_accepts_per_tick,
        rcvbuf >> 10,
        sndbuf >> 10
    );
    if cfg.identity.token_key == lattice_net::token::DEV_TOKEN_KEY {
        println!("WARNING: token key is the public dev key; anyone can mint tokens for this server (--token-key)");
    }

    let net = Arc::new(NetCounters::default());
    // One receive thread and inbox per socket: a socket's datagrams only ever
    // go to its own group of shards, so the threads share no lock.
    let per_group = shards / groups;
    let inboxes: Vec<Arc<Mutex<Vec<Vec<InDatagram>>>>> =
        (0..groups).map(|_| Arc::new(Mutex::new(vec![Vec::new(); per_group]))).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let tick_clock = Arc::new(TickClock::new(start));
    let mut receivers = Vec::with_capacity(groups);
    // Each receive thread's kernel id, for its scheduler stats.
    let rx_tids: Arc<Vec<std::sync::atomic::AtomicI32>> = Arc::new((0..groups).map(|_| Default::default()).collect());
    for (group, sock) in socks.iter().enumerate() {
        let rx = Receiver {
            sock: sock.clone(),
            group,
            per_group,
            router: sim.router(),
            inbox: inboxes[group].clone(),
            net: net.clone(),
            stop: stop.clone(),
            gather,
            tick: tick_clock.clone(),
        };
        let (cpus, tids) = (rx_cpus.clone(), rx_tids.clone());
        receivers.push(std::thread::Builder::new().name(format!("ingress-{group}")).spawn(move || {
            if let Some(cpus) = cpus {
                lattice_sim::cpus::pin(lattice_sim::cpus::for_thread(&cpus, group, groups)).expect("--rx-cpus: pinning a receive thread");
            }
            tids[group].store(lattice_sim::cpus::tid(), Relaxed);
            rx.run(ingress)
        })?);
    }

    let mut csv = csv_path.map(open_csv).transpose()?;
    let debug_map = match debug_http {
        Some(addr) => {
            println!("debug map on http://{addr}/ (from Windows, use the WSL IP from `hostname -I`)");
            Some(lattice_sim::debugmap::DebugMap::start(addr)?)
        }
        None => None,
    };

    let mut inbound: Vec<Vec<InDatagram>> = vec![Vec::new(); shards];
    // Server-side input waits after warmup, 0.1 ms units.
    let mut kept_wait = Histogram::new(10_000);
    // What lag compensation would rewind near and mid/far targets by for
    // each applied input, in ms.
    let mut kept_rewind = [Histogram::new(1000), Histogram::new(1000)];
    let mut kept_overruns = 0u64;
    let mut out: Vec<Vec<Datagram>> = vec![Vec::new(); shards];
    // With --send-during-assembly: each shard's datagrams, bytes and send time
    // this tick, written by its own task.
    let early_sent: Vec<(AtomicU64, AtomicU64, AtomicU64)> = (0..shards).map(|_| Default::default()).collect();
    let mut window = Window::new(start, &sim, &net, thread_cpu(&receivers));
    let mut kept: Vec<Row> = Vec::new();
    // Per steady tick, for ingress, assembly, transport and egress: the longest
    // shard task and the total of all tasks, in microseconds.
    let mut kept_spans: Vec<[[u32; 2]; 4]> = Vec::new();
    let mut first_client: Option<Instant> = None;
    // Counters as of the end of warmup, so the summary can separate the join burst.
    let mut warm: Option<Counters> = None;
    // ... and as of when clients start draining (a mass disconnect can lose
    // Disconnect packets and leave entities frozen until they time out).
    let mut cool: Option<Counters> = None;
    let mut peak_clients = 0;
    let mut steady_state = Steady::default();
    let mut next_tick = start;

    loop {
        let now = Instant::now();
        for (group, inbox) in inboxes.iter().enumerate() {
            let mut inbox = inbox.lock().unwrap();
            for (bucket, held) in inbound[group * per_group..].iter_mut().zip(inbox.iter_mut()) {
                std::mem::swap(bucket, held);
            }
        }
        // What's received from here on is for the next tick.
        tick_clock.publish(next_tick + sim.tick_period());

        if let Some(map) = &debug_map {
            // Fall back when the chosen client left (or nobody was chosen).
            let watch = map.watch().filter(|&e| sim.is_client_entity(e)).or_else(|| sim.any_entity());
            if watch.is_none() {
                map.clear();
            }
            sim.set_watch(watch);
        }
        let level = sim.level();
        // The tick and its egress, the pool's workers kept awake between their
        // phases (`--keep-awake`).
        // Sends one shard's bucket; returns its datagrams, bytes and time.
        let send_bucket = |shard: usize, bucket: &mut Vec<Datagram>| {
            let t0 = Instant::now();
            let sock = &socks[shard / per_group];
            let (n, bytes) = (bucket.len(), bucket.iter().map(|(_, p)| p.len()).sum::<usize>());
            let sent = send_all(sock, bucket, egress);
            if sent.errors > 0 {
                net.send_errors.fetch_add(sent.errors as u64, Relaxed);
            }
            net.sends.fetch_add(sent.sends as u64, Relaxed);
            net.send_syscalls.fetch_add(sent.syscalls as u64, Relaxed);
            bucket.clear();
            (n, bytes, t0.elapsed())
        };
        let mut run = || {
            if send_early {
                // Each shard's task sends its own bucket once it's framed; its
                // totals are gathered here after.
                let early = &early_sent;
                let sender = |shard: usize, bucket: &mut Vec<Datagram>| {
                    let (n, bytes, d) = send_bucket(shard, bucket);
                    let e = &early[shard];
                    e.0.store(n as u64, Relaxed);
                    e.1.store(bytes as u64, Relaxed);
                    e.2.store(d.as_nanos() as u64, Relaxed);
                };
                let times = sim.tick_sending(&mut inbound, now, &mut out, Some(&sender));
                let t_egress = Instant::now();
                let egress = early.iter().fold((0, 0, (Duration::ZERO, Duration::ZERO)), |a, e| {
                    let (n, bytes, d) = (e.0.swap(0, Relaxed) as usize, e.1.swap(0, Relaxed) as usize, Duration::from_nanos(e.2.swap(0, Relaxed)));
                    (a.0 + n, a.1 + bytes, (a.2 .0.max(d), a.2 .1 + d))
                });
                return (times, t_egress, egress);
            }
            let times = sim.tick(&mut inbound, now, &mut out);
            let t_egress = Instant::now();
            let egress = out
                .par_iter_mut()
                .enumerate()
                .map(|(shard, bucket)| {
                    let (n, bytes, d) = send_bucket(shard, bucket);
                    (n, bytes, (d, d))
                })
                .reduce(
                    || (0, 0, (Duration::ZERO, Duration::ZERO)),
                    |a, b| (a.0 + b.0, a.1 + b.1, (a.2 .0.max(b.2 .0), a.2 .1 + b.2 .1)),
                );
            (times, t_egress, egress)
        };
        let (times, t_egress, (pkts, bytes, egress_span)) = if keep_awake { pool::awake(run) } else { run() };
        if let (Some(map), Some(frame)) = (&debug_map, sim.take_debug_frame()) {
            map.publish(frame);
        }
        net.out_pkts.fetch_add(pkts as u64, Relaxed);
        window.out_pkts += pkts as u64;
        window.out_bytes += bytes as u64;

        let done = Instant::now();
        // The ladder judges this tick against the period it ran at.
        let period = sim.tick_period();
        sim.observe_tick(done - now);
        write_sessions(&mut sim, session_log.as_mut())?;
        window.level_min = window.level_min.min(sim.level());
        window.level_max = window.level_max.max(sim.level());
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
            steady_state.net_end = Some(net_snapshot(&net, &receivers, &rx_tids));
        }
        let steady = |warm: &Option<Counters>, cool: &Option<Counters>| warm.is_some() && cool.is_none();
        if clients > 0 {
            let first = *first_client.get_or_insert(now);
            if now - first >= warmup && cool.is_none() {
                warm.get_or_insert_with(|| sim.counters().clone());
                steady_state.net_start.get_or_insert_with(|| net_snapshot(&net, &receivers, &rx_tids));
                steady_state.first.get_or_insert(now);
                steady_state.last = Some(done);
                steady_state.client_ticks += clients as u64;
                steady_state.out_pkts += pkts as u64;
                steady_state.out_bytes += bytes as u64;
                steady_state.level_ticks[level as usize] += 1;
                kept.push(row);
                let t = sim.tasks();
                let us = |s: (Duration, Duration)| [s.0.as_micros() as u32, s.1.as_micros() as u32];
                // ingress, assembly, transport (PHASES order) + egress
                kept_spans.push([us(t[0]), us(t[8]), us(t[9]), us(egress_span)]);
                kept_overruns += (done - now > period) as u64;
            }
        }

        if done - window.start >= report {
            let wait = sim.take_input_wait();
            let rewind = sim.take_rewind();
            if steady(&warm, &cool) {
                kept_wait.merge(&wait);
                kept_rewind.iter_mut().zip(&rewind).for_each(|(k, r)| k.merge(r));
            }
            let cpu = thread_cpu(&receivers);
            window.report(done - start, &sim, &wait, &net, &cpu, csv.as_mut())?;
            window = Window::new(done, &sim, &net, cpu);
        }

        let finished = (!duration.is_zero() && done - start >= duration) || (until_empty && peak_clients > 0 && clients == 0);
        if finished {
            break;
        }

        next_tick += sim.tick_period();
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now;
        }
    }

    if window.rows.len() > 1 {
        let wait = sim.take_input_wait();
        let rewind = sim.take_rewind();
        if warm.is_some() && cool.is_none() {
            kept_wait.merge(&wait);
            kept_rewind.iter_mut().zip(&rewind).for_each(|(k, r)| k.merge(r));
        }
        window.report(Instant::now() - start, &sim, &wait, &net, &thread_cpu(&receivers), csv.as_mut())?;
    }
    // Before the receive threads end: their CPU clocks go with them.
    steady_state.net_end.get_or_insert_with(|| net_snapshot(&net, &receivers, &rx_tids));
    sim.close_sessions(Instant::now());
    write_sessions(&mut sim, session_log.as_mut())?;
    stop.store(true, Relaxed);
    for r in receivers {
        let _ = r.join();
    }
    if let Some(path) = summary_path {
        let cpus = |c: &Option<Vec<usize>>| c.as_ref().map_or("-".to_string(), |c| format!("{}-{}x{}", c[0], c[c.len() - 1], c.len()));
        let (worker_cpus, rx_cpus) = (cpus(&worker_cpus), cpus(&rx_cpus));
        let run = RunInfo { egress, keep_awake, send_early, worker_cpus, rx_cpus, ingress, gather, peak_clients, overruns: kept_overruns };
        let mut kv = summary_values(&kept, &run, &sim, warm.as_ref(), cool.as_ref(), &kept_wait, &net, &steady_state);
        for (tier, h) in ["near", "mid"].iter().zip(&kept_rewind) {
            let r = h.summary();
            kv.put(format!("rewind_{tier}_p50_ms"), r.p50);
            kv.put(format!("rewind_{tier}_p99_ms"), r.p99);
            kv.put(format!("rewind_{tier}_mean_ms"), format!("{:.1}", h.mean()));
        }
        // Where a split phase's time goes: wall = longest task + dispatch and
        // waiting; longest - work / threads = imbalance.
        for (i, name) in ["ingress", "assembly", "transport", "egress"].iter().enumerate() {
            // Sending during assembly, assembly's tasks frame and send too:
            // transport and egress have no phase of their own to judge.
            if send_early && i >= 2 {
                kv.put(format!("{name}_longest_p50_ms"), "-");
                kv.put(format!("{name}_work_p50_ms"), "-");
                continue;
            }
            let mut longest: Vec<u32> = kept_spans.iter().map(|s| s[i][0]).collect();
            let mut work: Vec<u32> = kept_spans.iter().map(|s| s[i][1]).collect();
            kv.put_ms(format!("{name}_longest_p50_ms"), summarize(&mut longest).p50);
            kv.put_ms(format!("{name}_work_p50_ms"), summarize(&mut work).p50);
        }
        kv.write(&path)?;
    }
    print_summary(&mut kept, kept_overruns, peak_clients, &sim, warm.as_ref(), cool.as_ref(), &kept_wait, &net);
    for (tier, h) in ["near", "mid/far"].iter().zip(&kept_rewind) {
        let r = h.summary();
        println!(
            "  rewind of {tier} targets (applied step - the input's render step for that tier): p50 {} p99 {} max {} mean {:.1} ms (n={})",
            r.p50,
            r.p99,
            r.max,
            h.mean(),
            h.len()
        );
    }
    println!("  render times ahead of the server (bogus) {}", sim.counters().render_ahead);
    let c = sim.counters();
    println!(
        "  shots {} (late {}, refused {}, rewinds capped {}) | hits head {} body {} (after cover {}, too late {}), kills {} | ground {} cover {} expired {} | segments {} candidates {}",
        c.shots, c.shots_late, c.shots_refused, c.rewinds_capped, c.hits_head, c.hits_body, c.hits_after_cover, c.hits_too_late, c.kills, c.hits_ground, c.hits_cover, c.expired, c.segments, c.candidates
    );
    Ok(())
}

/// Appends the session log's new lines (`SimServer::take_session_records`)
/// to `--session-log`, if there is one; without it they're dropped.
fn write_sessions(sim: &mut SimServer, log: Option<&mut BufWriter<std::fs::File>>) -> std::io::Result<()> {
    let records = sim.take_session_records();
    if let (Some(log), false) = (log, records.is_empty()) {
        for r in records {
            writeln!(log, "{}", r.to_json())?;
        }
        log.flush()?;
    }
    Ok(())
}

/// What the steady state (after warmup, before clients drain) sent and
/// received, for `--summary`.
#[derive(Default)]
struct Steady {
    first: Option<Instant>,
    last: Option<Instant>,
    client_ticks: u64,
    out_pkts: u64,
    out_bytes: u64,
    /// Ticks run at each ladder level.
    level_ticks: [u64; RUNGS.len()],
    /// `net_snapshot` when the steady state began and ended.
    net_start: Option<NetSnap>,
    net_end: Option<NetSnap>,
}

/// What had been received so far, and what receiving had cost.
struct NetSnap {
    at: Instant,
    /// Datagrams, bytes and receive syscalls, then the kernel's UDP receive and
    /// send buffer drops.
    counters: [u64; 5],
    /// CPU time of each receive thread.
    cpu: Vec<Duration>,
    /// How long each receive thread has waited for a core (schedstats).
    waited: Vec<Option<Duration>>,
    /// CPU time of the kernel's softirq threads (ksoftirqd).
    softirq: Option<Duration>,
}

fn net_snapshot(net: &NetCounters, receivers: &[std::thread::JoinHandle<()>], tids: &[std::sync::atomic::AtomicI32]) -> NetSnap {
    let [rcv, snd] = kernel_udp_drops();
    NetSnap {
        at: Instant::now(),
        counters: [net.in_pkts.load(Relaxed), net.in_bytes.load(Relaxed), net.recv_calls.load(Relaxed), rcv, snd],
        cpu: thread_cpu(receivers),
        waited: tids.iter().map(|t| lattice_sim::cpus::runq_wait(t.load(Relaxed))).collect(),
        softirq: lattice_sim::cpus::ksoftirqd_cpu(),
    }
}

struct RunInfo {
    egress: Egress,
    keep_awake: bool,
    send_early: bool,
    /// Pinning (`--worker-cpus`, `--rx-cpus`): first-last x count, or "-".
    worker_cpus: String,
    rx_cpus: String,
    ingress: Ingress,
    gather: Duration,
    peak_clients: usize,
    /// Steady-state ticks over their period.
    overruns: u64,
}

#[allow(clippy::too_many_arguments)]
fn summary_values(
    kept: &[Row],
    run: &RunInfo,
    sim: &SimServer,
    warm: Option<&Counters>,
    cool: Option<&Counters>,
    wait: &Histogram,
    net: &NetCounters,
    steady: &Steady,
) -> KeyValues {
    let mut kv = KeyValues::default();
    let cfg = sim.config();
    kv.put("spawn", format!("{:?}", cfg.spawn).to_lowercase());
    kv.put("egress", format!("{:?}", run.egress).to_lowercase());
    kv.put("ingress", format!("{:?}", run.ingress).to_lowercase());
    kv.put("rx_gather_us", run.gather.as_micros());
    kv.put("ladder", if cfg.ladder.enabled { "on" } else { "off" });
    kv.put("threads", rayon::current_num_threads());
    kv.put("keep_awake", if run.keep_awake { "on" } else { "off" });
    kv.put("send_during_assembly", if run.send_early { "on" } else { "off" });
    kv.put("worker_cpus", &run.worker_cpus);
    kv.put("rx_cpus", &run.rx_cpus);
    kv.put("shards", cfg.shards);
    kv.put("sockets", cfg.socket_groups);
    kv.put("peak_clients", run.peak_clients);
    kv.put("steady_ticks", kept.len());
    let secs = match (steady.first, steady.last) {
        (Some(a), Some(b)) => (b - a).as_secs_f64(),
        _ => 0.0,
    };
    kv.put("steady_secs", format!("{secs:.1}"));
    for i in 0..COLS {
        let s = summarize(&mut kept.iter().map(|r| r[i]).collect::<Vec<_>>());
        let n = col_name(i);
        kv.put_ms(format!("{n}_p50_ms"), s.p50);
        kv.put_ms(format!("{n}_p99_ms"), s.p99);
        kv.put_ms(format!("{n}_max_ms"), s.max);
    }
    kv.put("steady_overruns", run.overruns);

    // The level most steady ticks ran at, and all of them.
    let mode = (0..RUNGS.len()).max_by_key(|&l| steady.level_ticks[l]).unwrap_or(0);
    kv.put("level_mode", mode);
    kv.put("tick_hz", RUNGS[mode].tick_hz);
    kv.put("dilation", RUNGS[mode].dilation);
    let levels: Vec<String> = (0..RUNGS.len())
        .filter(|&l| steady.level_ticks[l] > 0)
        .map(|l| format!("L{l}:{}", steady.level_ticks[l]))
        .collect();
    kv.put("levels", if levels.is_empty() { "-".to_string() } else { levels.join(",") });

    let ticks = kept.len().max(1) as f64;
    let clients_avg = steady.client_ticks as f64 / ticks;
    let per_client_secs = (clients_avg * secs).max(1e-9);
    let counters = |s: &Option<NetSnap>| s.as_ref().map_or([0; 5], |s| s.counters);
    let [in0, inb0, calls0, rcv0, snd0] = counters(&steady.net_start);
    let [in1, inb1, calls1, rcv1, snd1] = counters(&steady.net_end);
    kv.put("clients_avg", format!("{clients_avg:.0}"));
    kv.put("out_pps", format!("{:.0}", steady.out_pkts as f64 / secs.max(1e-9)));
    kv.put("in_pps", format!("{:.0}", in1.saturating_sub(in0) as f64 / secs.max(1e-9)));
    kv.put("egress_mbps", format!("{:.1}", steady.out_bytes as f64 * 8.0 / 1e6 / secs.max(1e-9)));
    kv.put("packets_per_client_tick", format!("{:.2}", steady.out_pkts as f64 / steady.client_ticks.max(1) as f64));
    kv.put("wire_bytes_per_client_tick", format!("{:.0}", steady.out_bytes as f64 / steady.client_ticks.max(1) as f64));
    kv.put("down_kbps_per_client", format!("{:.1}", steady.out_bytes as f64 * 8.0 / 1000.0 / per_client_secs));
    kv.put("up_kbps_per_client", format!("{:.1}", inb1.saturating_sub(inb0) as f64 * 8.0 / 1000.0 / per_client_secs));
    kv.put("recv_per_call", format!("{:.2}", in1.saturating_sub(in0) as f64 / calls1.saturating_sub(calls0).max(1) as f64));
    // How much of a core each receive thread used: near 100% means one socket
    // can't keep up (more --sockets).
    if let (Some(a), Some(b)) = (&steady.net_start, &steady.net_end) {
        let secs = (b.at - a.at).as_secs_f64();
        if let Some((max, mean)) = busy_pct(&a.cpu, &b.cpu, secs) {
            kv.put("ingress_thread_busy_max_pct", format!("{max:.1}"));
            kv.put("ingress_thread_busy_mean_pct", format!("{mean:.1}"));
        }
        // How long a receive thread that had datagrams waited for a core:
        // more than a little means the workers (or the NIC's softirq work)
        // crowd it, and --rx-cpus with scripts/irq-affinity.sh is worth it.
        let waited: Option<Vec<(Duration, Duration)>> = a.waited.iter().zip(&b.waited).map(|(x, y)| x.zip(*y)).collect();
        if let Some((max, mean)) = waited.and_then(|w| {
            let (x, y): (Vec<_>, Vec<_>) = w.into_iter().unzip();
            busy_pct(&x, &y, secs)
        }) {
            kv.put("rx_runq_wait_max_pct", format!("{max:.2}"));
            kv.put("rx_runq_wait_mean_pct", format!("{mean:.2}"));
        }
        if let (Some(x), Some(y)) = (a.softirq, b.softirq) {
            kv.put("ksoftirqd_cpu_pct", format!("{:.1}", y.saturating_sub(x).as_secs_f64() / secs.max(1e-9) * 100.0));
        }
    }
    kv.put("kernel_rcvbuf_drops", rcv1.saturating_sub(rcv0));
    kv.put("kernel_sndbuf_drops", snd1.saturating_sub(snd0));

    let c = sim.counters();
    if let Some(w) = warm {
        let e = cool.unwrap_or(c);
        let snaps = e.snapshots.saturating_sub(w.snapshots).max(1) as f64;
        kv.put("snapshot_bytes_per_client_tick", format!("{:.0}", (e.snapshot_bytes - w.snapshot_bytes) as f64 / snaps));
        kv.put("near_bytes_per_client_tick", format!("{:.0}", (e.near_bytes - w.near_bytes) as f64 / snaps));
        kv.put("activity_bytes_per_client_tick", format!("{:.1}", (e.activity_bytes - w.activity_bytes) as f64 / snaps));
        for (i, tier) in ["near", "mid", "far"].iter().enumerate() {
            kv.put(format!("{tier}_per_client_tick"), format!("{:.1}", (e.tier_sent[i] - w.tier_sent[i]) as f64 / snaps));
        }
        kv.put("near_scanned_per_query", format!("{:.0}", (e.near_scanned - w.near_scanned) as f64 / snaps));
        kv.put("mid_scanned_per_query", format!("{:.0}", (e.mid_scanned - w.mid_scanned) as f64 / snaps));
        kv.put("far_skipped", e.far_skipped - w.far_skipped);
        kv.put("far_starved", e.far_starved - w.far_starved);
        kv.put("repeated", e.repeated - w.repeated);
        kv.put("frozen", e.frozen - w.frozen);
        kv.put("late_inputs", e.late_inputs - w.late_inputs);
        kv.put("discarded_inputs", e.discarded_inputs - w.discarded_inputs);
    }
    let ws = wait.summary();
    // p10 and p90 too: arrivals spread over the tick make the wait roughly
    // uniform over one period above the spare.
    kv.put("input_wait_p10_ms", format!("{:.1}", wait.quantile(0.10) as f64 / 10.0));
    kv.put("input_wait_p50_ms", format!("{:.1}", ws.p50 as f64 / 10.0));
    kv.put("input_wait_p90_ms", format!("{:.1}", wait.quantile(0.90) as f64 / 10.0));
    kv.put("input_wait_p99_ms", format!("{:.1}", ws.p99 as f64 / 10.0));

    // Whole run.
    kv.put("spawns", c.spawns);
    kv.put("despawns", c.despawns);
    kv.put("deaths", c.deaths);
    kv.put("respawns", c.respawns);
    for (k, v) in [
        ("shots", c.shots),
        ("shots_late", c.shots_late),
        ("shots_refused", c.shots_refused),
        ("rewinds_capped", c.rewinds_capped),
        ("rewinds_trimmed", c.rewinds_trimmed),
        ("rewinds_trimmed_mid", c.rewinds_trimmed_mid),
        ("renders_held", c.renders_held),
        ("shot_dip_clients", c.dip_clients),
        ("shot_dip_flagged", c.dip_flagged),
        ("shot_dip_windows", c.dip_windows),
        ("shot_dip_windows_flagged", c.dip_windows_flagged),
        ("hits_head", c.hits_head),
        ("hits_body", c.hits_body),
        ("hits_ground", c.hits_ground),
        ("hits_cover", c.hits_cover),
        ("projectiles_expired", c.expired),
        ("kills", c.kills),
        ("hits_after_cover", c.hits_after_cover),
        ("hits_too_late", c.hits_too_late),
        ("events", c.events),
        ("tracer_bytes", c.tracer_bytes),
        ("activity_cells", c.activity_cells),
        ("activity_sent", c.activity_sent),
        ("activity_bytes", c.activity_bytes),
        ("activity_cut", c.activity_cut),
        ("projectile_segments", c.segments),
        ("hit_candidates", c.candidates),
    ] {
        kv.put(k, v);
    }
    let te = sim.trim_excess();
    kv.put("trim_excess_p50_steps", format!("{:.1}", te.quantile(0.5) as f64 / 10.0));
    kv.put("trim_excess_p99_steps", format!("{:.1}", te.quantile(0.99) as f64 / 10.0));
    kv.put("trim_excess_max_steps", format!("{:.1}", te.summary().max as f64 / 10.0));
    let de = sim.dip_excess();
    kv.put("shot_dip_excess_p50_steps", format!("{:.1}", de.quantile(0.5) as f64 / 10.0));
    kv.put("shot_dip_excess_p99_steps", format!("{:.1}", de.quantile(0.99) as f64 / 10.0));
    kv.put("shot_dip_excess_max_steps", format!("{:.1}", de.summary().max as f64 / 10.0));
    let hb = sim.held_by();
    kv.put("held_by_p50_steps", format!("{:.1}", hb.quantile(0.5) as f64 / 10.0));
    kv.put("held_by_p99_steps", format!("{:.1}", hb.quantile(0.99) as f64 / 10.0));
    kv.put("held_by_max_steps", format!("{:.1}", hb.summary().max as f64 / 10.0));
    kv.put("joins_deferred", sim.net().deferred_accepts());
    kv.put("bad_messages", c.bad_messages);
    kv.put("recv_errors", net.recv_errors.load(Relaxed));
    kv.put("send_errors", net.send_errors.load(Relaxed));
    kv.put("datagrams", net.out_pkts.load(Relaxed));
    kv.put("sends", net.sends.load(Relaxed));
    kv.put("send_syscalls", net.send_syscalls.load(Relaxed));
    kv
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
    recv_calls: u64,
    /// Receive threads' CPU time at the start.
    cpu: Vec<Duration>,
    kernel: [u64; 2],
    deferred: u64,
    level_min: u8,
    level_max: u8,
}

impl Window {
    fn new(start: Instant, sim: &SimServer, net: &NetCounters, cpu: Vec<Duration>) -> Self {
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
            recv_calls: net.recv_calls.load(Relaxed),
            cpu,
            kernel: kernel_udp_drops(),
            deferred: sim.net().deferred_accepts(),
            level_min: sim.level(),
            level_max: sim.level(),
        }
    }

    fn report(
        &mut self,
        t: Duration,
        sim: &SimServer,
        wait: &Histogram,
        net: &NetCounters,
        cpu: &[Duration],
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
        let per_call = in_pkts as f64 / (net.recv_calls.load(Relaxed) - self.recv_calls).max(1) as f64;
        let busy = busy_pct(&self.cpu, cpu, secs);
        let (repeated, frozen) = (c.repeated - self.counters.repeated, c.frozen - self.counters.frozen);
        let entity_ticks = ((c.inputs_applied - self.counters.inputs_applied) + repeated + frozen).max(1) as f64;
        let (repeated_pct, frozen_pct) = (100.0 * repeated as f64 / entity_ticks, 100.0 * frozen as f64 / entity_ticks);
        let late = c.late_inputs - self.counters.late_inputs;
        let snaps = (c.snapshots - self.counters.snapshots).max(1) as f64;
        let tier = |i: usize| (c.tier_sent[i] - self.counters.tier_sent[i]) as f64 / snaps;
        let snap_bytes = (c.snapshot_bytes - self.counters.snapshot_bytes) as f64 / snaps;
        let far_skipped = c.far_skipped - self.counters.far_skipped;
        let far_starved = c.far_starved - self.counters.far_starved;
        let degraded = (c.degraded_clients - self.counters.degraded_clients) as f64 / snaps;
        let near_bytes = (c.near_bytes - self.counters.near_bytes) as f64 / snaps;
        let (nd, nf) = (c.near_deltas - self.counters.near_deltas, c.near_full - self.counters.near_full);
        let delta_share = 100.0 * nd as f64 / (nd + nf).max(1) as f64;
        let rung = sim.rung();
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
            "[{:>5.0}s] clients {} | level {} ({}-{} in window: {} Hz, dilation {:.1}), pace {:.2}, {:.1}% clients bandwidth-degraded | tick p50 {} p99 {} max {} ms, {} overruns | out {:.1}k pps {:.0} kbps/client, {:.0} Mbps | in {:.1}k pps {:.0} kbps/client, {:.1} per recv, receive thread busy {} | stand-ins: repeated {:.2}% frozen {:.2}%, {} late inputs | per client-tick: {:.0} B (near {:.0} B, {:.0}% deltas), near {:.1} mid {:.1} far {:.1}, far skipped {} starved {} | input wait p50 {:.1} p99 {:.1} ms | {} joins deferred | kernel drops rcv {} snd {}",
            t.as_secs_f64(),
            sim.client_count(),
            sim.level(),
            self.level_min,
            self.level_max,
            rung.tick_hz,
            rung.dilation,
            sim.pace(),
            degraded * 100.0,
            ms(tick.p50),
            ms(tick.p99),
            ms(tick.max),
            self.overruns,
            self.out_pkts as f64 / secs / 1000.0,
            down_kbps,
            self.out_bytes as f64 * 8.0 / 1e6 / secs,
            in_pkts as f64 / secs / 1000.0,
            up_kbps,
            per_call,
            busy.map_or("?".to_string(), |(max, _)| format!("{max:.0}%")),
            repeated_pct,
            frozen_pct,
            late,
            snap_bytes,
            near_bytes,
            delta_share,
            tier(0),
            tier(1),
            tier(2),
            far_skipped,
            far_starved,
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
                ",{:.0},{:.0},{:.1},{:.1},{:.1},{:.3},{:.3},{},{:.0},{:.1},{:.1},{:.1},{},{},{:.1},{:.1},{},{},{},{},{},{:.3},{:.4},{:.2},{:.1}",
                self.out_pkts as f64 / secs,
                in_pkts as f64 / secs,
                down_kbps,
                up_kbps,
                self.out_bytes as f64 * 8.0 / 1e6 / secs,
                repeated_pct,
                frozen_pct,
                late,
                snap_bytes,
                tier(0),
                tier(1),
                tier(2),
                far_skipped,
                far_starved,
                tenth(ws.p50),
                tenth(ws.p99),
                deferred,
                rcv_drops,
                snd_drops,
                sim.level(),
                rung.tick_hz,
                sim.pace(),
                degraded,
                per_call,
                busy.map_or(0.0, |(max, _)| max)
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
        h += ",out_pps,in_pps,down_kbps_per_client,up_kbps_per_client,egress_mbps,repeated_pct,frozen_pct,late_inputs,snapshot_bytes_per_tick,near_per_tick,mid_per_tick,far_per_tick,far_skipped,far_starved,input_wait_p50_ms,input_wait_p99_ms,deferred_accepts,kernel_rcvbuf_drops,kernel_sndbuf_drops,level,tick_hz,pace,degraded_client_share,recv_per_call,receive_thread_busy_max_pct";
        writeln!(w, "{h}")?;
    }
    Ok(w)
}

#[allow(clippy::too_many_arguments)]
fn print_summary(
    kept: &mut [Row],
    over: u64,
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
    println!(
        "  overruns {over} | spawns {} despawns {} ({} joins deferred) | deaths {} respawns {} | stand-ins: repeated {} frozen {} | late inputs {} discarded {} | bad messages {} | recv errors {} send errors {}",
        c.spawns,
        c.despawns,
        sim.net().deferred_accepts(),
        c.deaths,
        c.respawns,
        c.repeated,
        c.frozen,
        c.late_inputs,
        c.discarded_inputs,
        c.bad_messages,
        net.recv_errors.load(Relaxed),
        net.send_errors.load(Relaxed)
    );
    let (pkts, sends, calls) = (net.out_pkts.load(Relaxed), net.sends.load(Relaxed), net.send_syscalls.load(Relaxed));
    println!(
        "  egress (whole run): {pkts} datagrams in {sends} sends ({:.2} per send), {calls} syscalls",
        pkts as f64 / sends.max(1) as f64
    );
    let (got, calls) = (net.in_pkts.load(Relaxed), net.recv_calls.load(Relaxed));
    println!("  ingress (whole run): {got} datagrams in {calls} receive calls ({:.2} per call)", got as f64 / calls.max(1) as f64);
    let levels: Vec<String> = c
        .level_ticks
        .iter()
        .enumerate()
        .filter(|(_, &n)| n > 0)
        .map(|(l, n)| format!("L{l}: {n}"))
        .collect();
    println!("  ticks per ladder level (whole run): {}", levels.join(", "));
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn gathering_stops_before_the_next_tick() {
        let base = Instant::now();
        let clock = TickClock::new(base);
        let ms = Duration::from_millis;
        clock.publish(base + ms(33));
        // Mid-tick: the full gather.
        assert_eq!(clock.gather_for(base + ms(10), ms(1)), ms(1));
        // Near the tick: only until the drain window before it.
        assert_eq!(clock.gather_for(base + ms(32), ms(1)), ms(1) - RX_DRAIN_BEFORE_TICK);
        // Inside the drain window, or past the tick: none.
        assert_eq!(clock.gather_for(base + ms(33) - Duration::from_micros(100), ms(1)), Duration::ZERO);
        assert_eq!(clock.gather_for(base + ms(40), ms(1)), Duration::ZERO);
    }

    #[test]
    fn gso_runs_follow_udp_segment_rules() {
        let a: SocketAddr = "10.0.0.1:1".parse().unwrap();
        let b: SocketAddr = "10.0.0.2:1".parse().unwrap();
        let d = |to, len| (to, vec![0u8; len]);
        let batch = vec![
            d(a, 1200), d(a, 1200), d(a, 300), // one client, padded: one run
            d(a, 500),                          // same client again after its short last one
            d(b, 900), d(b, 1000),              // bigger after smaller: can't share a run
            d(b, 40),
        ];
        assert_eq!(gso_runs(&batch), [(0, 3), (3, 1), (4, 1), (5, 2)]);
        // Never more than the kernel's segment limit.
        let many: Vec<Datagram> = (0..70).map(|_| d(a, 1200)).collect();
        assert_eq!(gso_runs(&many), [(0, 54), (54, 16)]);
    }
}

/// Physical cores: CPUs that share a core (SMT siblings) count once. Falls
/// back to all CPUs where the topology isn't readable.
fn physical_cores() -> usize {
    let all = std::thread::available_parallelism().map_or(1, |n| n.get());
    let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") else { return all };
    let mut cores = std::collections::HashSet::new();
    for e in dir.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.strip_prefix("cpu").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            if let Ok(s) = std::fs::read_to_string(e.path().join("topology/core_cpus_list")) {
                cores.insert(s.trim().to_string());
            }
        }
    }
    if cores.is_empty() { all } else { cores.len() }
}
