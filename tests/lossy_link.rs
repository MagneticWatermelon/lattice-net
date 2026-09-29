//! End-to-end tests over a simulated network: loss, duplication, jitter (=> reordering).
//! Because the protocol is sans-IO, the whole thing runs in simulated time, deterministically.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lattice_net::{Channel, Client, ClientState, Config, Server, ServerEvent};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
    fn range(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Copy)]
struct LinkProfile {
    loss: f64,
    dup: f64,
    base_ms: u64,
    jitter_ms: u64,
}

struct Link {
    rng: Rng,
    p: LinkProfile,
    in_flight: Vec<(Instant, SocketAddr, SocketAddr, Vec<u8>)>,
    sent: u64,
}

impl Link {
    fn new(seed: u64, p: LinkProfile) -> Self {
        Self { rng: Rng(seed), p, in_flight: Vec::new(), sent: 0 }
    }
    fn send(&mut self, now: Instant, from: SocketAddr, to: SocketAddr, data: Vec<u8>) {
        self.sent += 1;
        if self.rng.chance(self.p.loss) {
            return;
        }
        let copies = if self.rng.chance(self.p.dup) { 2 } else { 1 };
        for _ in 0..copies {
            let delay = self.p.base_ms + self.rng.range(self.p.jitter_ms + 1);
            self.in_flight.push((now + Duration::from_millis(delay), from, to, data.clone()));
        }
    }
    fn due(&mut self, now: Instant) -> Vec<(SocketAddr, SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.in_flight.len() {
            if self.in_flight[i].0 <= now {
                let (_, f, t, d) = self.in_flight.swap_remove(i);
                out.push((f, t, d));
            } else {
                i += 1;
            }
        }
        out
    }
}

const SERVER: &str = "10.0.0.1:40000";
const TICK: Duration = Duration::from_millis(16);

fn client_addr(i: usize) -> SocketAddr {
    format!("10.1.{}.{}:{}", i / 250, i % 250 + 1, 50000 + i).parse().unwrap()
}

struct World {
    now: Instant,
    link: Link,
    server: Server,
    server_addr: SocketAddr,
    clients: Vec<(SocketAddr, Client)>,
}

impl World {
    fn new(n_clients: usize, profile: LinkProfile, seed: u64) -> Self {
        let now = Instant::now();
        let server_addr: SocketAddr = SERVER.parse().unwrap();
        let cfg = Config::default();
        let server = Server::new(cfg.clone(), 10_000, now);
        let clients = (0..n_clients)
            .map(|i| (client_addr(i), Client::new(cfg.clone(), server_addr, now)))
            .collect();
        Self { now, link: Link::new(seed, profile), server, server_addr, clients }
    }

    /// One tick: deliver due packets, update, flush, put new packets on the wire.
    fn step(&mut self) -> Vec<ServerEvent> {
        self.now += TICK;
        let now = self.now;
        for (from, to, data) in self.link.due(now) {
            if to == self.server_addr {
                self.server.receive(from, &data, now);
            } else if let Some((_, c)) = self.clients.iter_mut().find(|(a, _)| *a == to) {
                c.receive(from, &data, now);
            }
        }
        let mut events = Vec::new();
        while let Some(e) = self.server.poll_event() {
            events.push(e);
        }
        self.server.update(now);
        self.server.flush(now);
        for (to, pkt) in self.server.drain_outgoing() {
            self.link.send(now, self.server_addr, to, pkt);
        }
        for (addr, c) in &mut self.clients {
            c.update(now);
            c.flush(now);
            for pkt in c.drain_outgoing() {
                self.link.send(now, *addr, self.server_addr, pkt);
            }
        }
        events
    }

    fn connect_all(&mut self, max_ticks: usize) {
        for _ in 0..max_ticks {
            self.step();
            if self.clients.iter().all(|(_, c)| c.state() == ClientState::Connected) {
                return;
            }
        }
        panic!("not all clients connected");
    }
}

const NASTY: LinkProfile = LinkProfile { loss: 0.25, dup: 0.05, base_ms: 30, jitter_ms: 80 };

#[test]
fn reliable_is_ordered_exactly_once_under_25pct_loss_and_reordering() {
    let mut w = World::new(1, NASTY, 0xC0FFEE);
    w.connect_all(1000);

    const N: u32 = 5_000;
    let mut next_to_send = 0u32;
    let mut server_got: Vec<u32> = Vec::new();
    let mut client_got: Vec<u32> = Vec::new();

    for _tick in 0..20_000 {
        // client -> server: ~25 reliable msgs/tick; server echoes each back reliably
        for _ in 0..25 {
            if next_to_send < N {
                w.clients[0].1.send(Channel::Reliable, next_to_send.to_le_bytes().to_vec()).unwrap();
                next_to_send += 1;
            }
        }
        // also some unreliable traffic to share packet space
        w.clients[0].1.send(Channel::Unreliable, vec![0xAA; 40]).unwrap();

        for e in w.step() {
            if let ServerEvent::Message { client, channel: Channel::Reliable, data } = e {
                let v = u32::from_le_bytes(data[..4].try_into().unwrap());
                server_got.push(v);
                w.server.send(client, Channel::Reliable, data).unwrap();
            }
        }
        while let Some((ch, data)) = w.clients[0].1.recv() {
            if ch == Channel::Reliable {
                client_got.push(u32::from_le_bytes(data[..4].try_into().unwrap()));
            }
        }
        if client_got.len() == N as usize {
            break;
        }
    }

    let expected: Vec<u32> = (0..N).collect();
    assert_eq!(server_got, expected, "server: in order, exactly once");
    assert_eq!(client_got, expected, "client: echoes in order, exactly once");

    let s = w.clients[0].1.stats().unwrap();
    eprintln!(
        "client stats: sent={} recv={} acked={} lost={} dup={} rtt={:.1}ms loss={:.2}",
        s.packets_sent, s.packets_received, s.packets_acked, s.packets_lost, s.duplicate_packets, s.rtt_ms, s.loss
    );
    // RTT should be ~ 2 * (30 + 40 avg jitter) + tick quantization
    assert!(s.rtt_ms > 80.0 && s.rtt_ms < 250.0, "rtt {}", s.rtt_ms);
    assert!(s.loss > 0.12 && s.loss < 0.40, "loss estimate {}", s.loss);
    assert!(s.duplicate_packets > 0);
}

#[test]
fn sequence_numbers_survive_wraparound() {
    // 70k reliable messages => message ids wrap; >65536 packets => packet seqs wrap.
    let mut w = World::new(
        1,
        LinkProfile { loss: 0.10, dup: 0.02, base_ms: 20, jitter_ms: 30 },
        42,
    );
    w.connect_all(1000);
    const N: u32 = 70_000;
    let mut sent = 0u32;
    let mut got = Vec::with_capacity(N as usize);
    let mut ticks = 0;
    while got.len() < N as usize {
        ticks += 1;
        assert!(ticks < 200_000, "stalled at {}", got.len());
        for _ in 0..40 {
            if sent < N {
                w.clients[0].1.send(Channel::Reliable, sent.to_le_bytes().to_vec()).unwrap();
                sent += 1;
            }
        }
        for e in w.step() {
            if let ServerEvent::Message { channel: Channel::Reliable, data, .. } = e {
                got.push(u32::from_le_bytes(data[..4].try_into().unwrap()));
            }
        }
    }
    // Keep ticking with keepalives so the packet sequence wraps too.
    while w.clients[0].1.stats().unwrap().packets_sent < 70_000 {
        w.step();
        ticks += 1;
    }
    assert_eq!(got, (0..N).collect::<Vec<_>>());
    assert_eq!(w.clients[0].1.state(), ClientState::Connected);
    eprintln!("wraparound: {ticks} ticks, client packets sent {}", w.clients[0].1.stats().unwrap().packets_sent);
}

#[test]
fn many_clients_connect_through_lossy_link() {
    let mut w = World::new(300, NASTY, 7);
    w.connect_all(2000);
    assert_eq!(w.server.client_count(), 300);
    // Every client id is unique and matches the server's view
    let mut ids: Vec<_> = w.clients.iter().map(|(_, c)| c.client_id().unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 300);
}

#[test]
fn unreliable_is_latest_wins_and_never_blocks() {
    let mut w = World::new(1, NASTY, 99);
    w.connect_all(1000);
    let mut received = 0u32;
    let mut max_tick_seen = 0u32;
    for tick in 0..2000u32 {
        // "snapshot" tagged with tick number, 3x per tick => some get dropped at flush if too big
        for _ in 0..3 {
            let mut m = tick.to_le_bytes().to_vec();
            m.resize(500, 0);
            w.server.send(0, Channel::Unreliable, m).unwrap();
        }
        w.step();
        while let Some((ch, data)) = w.clients[0].1.recv() {
            assert_eq!(ch, Channel::Unreliable);
            assert_eq!(data.len(), 500);
            let t = u32::from_le_bytes(data[..4].try_into().unwrap());
            max_tick_seen = max_tick_seen.max(t);
            received += 1;
        }
    }
    eprintln!("unreliable: received {received} of 6000");
    assert!(received > 3500, "most should arrive with 25% loss");
    assert!(max_tick_seen > 1990, "stream keeps flowing, no head-of-line blocking");
}

#[test]
fn timeouts_on_both_sides() {
    let mut w = World::new(1, LinkProfile { loss: 0.0, dup: 0.0, base_ms: 10, jitter_ms: 0 }, 1);
    w.connect_all(100);
    // cut the cable
    w.link.p.loss = 1.0;
    let mut disconnected = false;
    for _ in 0..400 {
        for e in w.step() {
            if let ServerEvent::Disconnected { reason, .. } = e {
                assert_eq!(reason, lattice_net::DisconnectReason::TimedOut);
                disconnected = true;
            }
        }
    }
    // server event is emitted by update() and observed on the following step
    while let Some(e) = w.server.poll_event() {
        if matches!(e, ServerEvent::Disconnected { .. }) {
            disconnected = true;
        }
    }
    assert!(disconnected);
    assert_eq!(w.clients[0].1.state(), ClientState::TimedOut);
    assert_eq!(w.server.client_count(), 0);
}

#[test]
fn server_full_is_denied() {
    let now = Instant::now();
    let cfg = Config::default();
    let server_addr: SocketAddr = SERVER.parse().unwrap();
    let mut server = Server::new(cfg.clone(), 1, now);
    let mut a = Client::new(cfg.clone(), server_addr, now);
    let mut b = Client::new(cfg.clone(), server_addr, now);
    let (aa, ba) = (client_addr(0), client_addr(1));
    let mut t = now;
    for _ in 0..20 {
        t += TICK;
        for (addr, c) in [(aa, &mut a), (ba, &mut b)] {
            c.update(t);
            c.flush(t);
            for p in c.drain_outgoing() {
                server.receive(addr, &p, t);
            }
        }
        server.flush(t);
        let out: Vec<_> = server.drain_outgoing().collect();
        for (to, p) in out {
            if to == aa {
                a.receive(server_addr, &p, t);
            } else {
                b.receive(server_addr, &p, t);
            }
        }
    }
    assert_eq!(a.state(), ClientState::Connected);
    assert_eq!(b.state(), ClientState::Denied(lattice_net::DenyReason::ServerFull));
}

#[test]
fn spoofed_and_garbage_packets_are_ignored() {
    let mut w = World::new(1, LinkProfile { loss: 0.0, dup: 0.0, base_ms: 5, jitter_ms: 0 }, 3);
    w.connect_all(100);
    let before = w.server.dropped_packets;
    let attacker: SocketAddr = "66.66.66.66:6666".parse().unwrap();

    // 1) random garbage
    w.server.receive(attacker, &[0u8; 64], w.now);
    // 2) a ChallengeResponse with a forged cookie
    let forged = lattice_net::packet::encode(
        Config::default().protocol_id,
        &lattice_net::packet::Packet::ChallengeResponse { client_salt: 1, cookie: 12345 },
    );
    w.server.receive(attacker, &forged, w.now);
    // 3) a payload spoofing the real client's address but with a wrong session tag
    let spoof = lattice_net::packet::encode(
        Config::default().protocol_id,
        &lattice_net::packet::Packet::Payload {
            session: 0xDEAD_BEEF,
            header: lattice_net::packet::AckHeader { seq: 999, ack: 0, ack_bits: 0 },
            body: &[0, 3, b'b', b'a', b'd'],
        },
    );
    let victim = w.clients[0].0;
    w.server.receive(victim, &spoof, w.now);

    assert_eq!(w.server.dropped_packets - before, 3);
    assert_eq!(w.server.client_count(), 1);
    assert!(w.server.poll_event().is_none());
}
