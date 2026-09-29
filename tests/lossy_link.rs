//! End-to-end tests over a simulated network: loss, duplication, jitter (=> reordering).
//! Because the protocol is sans-IO, the whole thing runs in simulated time, deterministically.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lattice_net::token::USER_DATA_BYTES;
use lattice_net::{
    Channel, Client, ClientState, Config, ConnectToken, DisconnectReason, Server, ServerEvent, ServerIdentity,
};

const SERVER_ID: u64 = 1;
const TOKEN_KEY: [u8; 32] = [7; 32];

fn identity() -> ServerIdentity {
    ServerIdentity { server_id: SERVER_ID, token_key: TOKEN_KEY }
}

fn unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// What the login service would hand user `user`.
fn token(cfg: &Config, user: u64) -> ConnectToken {
    ConnectToken::mint(&TOKEN_KEY, cfg.protocol_id, SERVER_ID, unix() + 60, user, &[user as u8; USER_DATA_BYTES])
}

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
        Self::sharded(n_clients, 1, 10_000, profile, seed)
    }

    fn sharded(n_clients: usize, shards: usize, max_clients: usize, profile: LinkProfile, seed: u64) -> Self {
        Self::with_config(Config::default(), n_clients, shards, max_clients, profile, seed)
    }

    fn with_config(cfg: Config, n_clients: usize, shards: usize, max_clients: usize, profile: LinkProfile, seed: u64) -> Self {
        let now = Instant::now();
        let server_addr: SocketAddr = SERVER.parse().unwrap();
        let server = Server::with_shards(cfg.clone(), &identity(), max_clients, shards, now);
        let clients = (0..n_clients)
            .map(|i| (client_addr(i), Client::new(cfg.clone(), server_addr, token(&cfg, i as u64), now)))
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
        self.server.update(now, unix());
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

    /// Steps until every client is past the handshake (connected or denied).
    fn settle(&mut self, max_ticks: usize) {
        for _ in 0..max_ticks {
            self.step();
            if self.clients.iter().all(|(_, c)| c.state() != ClientState::Connecting) {
                return;
            }
        }
        panic!("handshakes didn't settle");
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
    let mut server = Server::new(cfg.clone(), &identity(), 1, now);
    let mut a = Client::new(cfg.clone(), server_addr, token(&cfg, 0), now);
    let mut b = Client::new(cfg.clone(), server_addr, token(&cfg, 1), now);
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
    let before = w.server.dropped_packets();
    let attacker: SocketAddr = "66.66.66.66:6666".parse().unwrap();

    // 1) random garbage
    w.server.receive(attacker, &[0u8; 64], w.now);
    // 2) a padded request whose token is junk
    let mut forged = vec![1u8; 256];
    forged[10] = 99;
    w.server.receive(attacker, &forged, w.now);
    // 3) a payload spoofing the real client's address, not sealed with its key
    let spoof = [6u8, 0xE7, 0x03, 0, 3, b'b', b'a', b'd', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let victim = w.clients[0].0;
    w.server.receive(victim, &spoof, w.now);

    assert_eq!(w.server.dropped_packets() - before, 3);
    assert_eq!(w.server.client_count(), 1);
    assert!(w.server.poll_event().is_none());
}

#[test]
fn sharded_server_routes_ids_and_echoes() {
    let mut w = World::sharded(300, 8, 10_000, NASTY, 11);
    w.connect_all(2000);
    assert_eq!(w.server.client_count(), 300);
    let router = w.server.router();
    let mut ids = Vec::new();
    for (addr, c) in &w.clients {
        let id = c.client_id().unwrap();
        // The id encodes the shard its address routes to, and that shard owns it.
        let shard = router.shard(addr);
        assert_eq!(w.server.shard_of_client(id), shard);
        assert_eq!(w.server.shards()[shard].client_addr(id), Some(*addr));
        ids.push(id);
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 300);
    let per_shard: Vec<usize> = w.server.shards().iter().map(|s| s.client_count()).collect();
    assert!(per_shard.iter().all(|&n| n > 15), "keyed hash spreads clients: {per_shard:?}");

    // Reliable echo through every shard.
    for (_, c) in &mut w.clients {
        c.send(Channel::Reliable, b"ping".to_vec()).unwrap();
    }
    let mut echoed = 0;
    for _ in 0..500 {
        for e in w.step() {
            if let ServerEvent::Message { client, data, .. } = e {
                w.server.send(client, Channel::Reliable, data).unwrap();
            }
        }
        for (_, c) in &mut w.clients {
            while let Some((_, d)) = c.recv() {
                assert_eq!(d, b"ping");
                echoed += 1;
            }
        }
        if echoed == 300 {
            break;
        }
    }
    assert_eq!(echoed, 300);
}

#[test]
fn max_clients_holds_across_shards() {
    let clean = LinkProfile { loss: 0.0, dup: 0.0, base_ms: 5, jitter_ms: 0 };
    let mut w = World::sharded(40, 16, 25, clean, 5);
    w.settle(200);
    let connected = w.clients.iter().filter(|(_, c)| c.state() == ClientState::Connected).count();
    let denied = w
        .clients
        .iter()
        .filter(|(_, c)| c.state() == ClientState::Denied(lattice_net::DenyReason::ServerFull))
        .count();
    assert_eq!((connected, denied), (25, 15));
    assert_eq!(w.server.client_count(), 25);
    let in_shards: usize = w.server.shards().iter().map(|s| s.client_count()).sum();
    assert_eq!(in_shards, 25);
}

#[test]
fn shards_run_on_separate_threads() {
    let clean = LinkProfile { loss: 0.0, dup: 0.0, base_ms: 5, jitter_ms: 0 };
    let mut w = World::sharded(64, 4, 10_000, clean, 9);
    w.connect_all(200);
    let router = w.server.router();
    for (_, c) in &mut w.clients {
        c.send(Channel::Unreliable, vec![7; 32]).unwrap();
        c.flush(w.now);
    }
    // Bucket every client's datagram by shard, then process each shard on its own thread.
    let mut buckets: Vec<Vec<(SocketAddr, Vec<u8>)>> = vec![Vec::new(); router.shard_count()];
    for (addr, c) in &mut w.clients {
        for pkt in c.drain_outgoing() {
            buckets[router.shard(addr)].push((*addr, pkt));
        }
    }
    let now = w.now;
    let counts: Vec<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = w
            .server
            .shards_mut()
            .iter_mut()
            .zip(buckets)
            .map(|(shard, bucket)| {
                scope.spawn(move || {
                    for (from, data) in bucket {
                        shard.receive(from, &data, now);
                    }
                    let mut msgs = 0;
                    while let Some(e) = shard.poll_event() {
                        if let ServerEvent::Message { client, data, .. } = e {
                            shard.send(client, Channel::Unreliable, data).unwrap();
                            msgs += 1;
                        }
                    }
                    shard.flush(now);
                    msgs
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(counts.iter().sum::<usize>(), 64);
    assert_eq!(w.server.drain_outgoing().count(), 64);
    assert_eq!(w.server.dropped_packets(), 0);
}

#[test]
fn misrouted_handshake_is_dropped() {
    let now = Instant::now();
    let cfg = Config::default();
    let mut server = Server::with_shards(cfg.clone(), &identity(), 100, 4, now);
    let router = server.router();
    let addr = client_addr(3);
    let wrong = (router.shard(&addr) + 1) % 4;
    let mut c = Client::new(cfg.clone(), SERVER.parse().unwrap(), token(&cfg, 3), now);
    c.update(now);
    let req = c.drain_outgoing().next().unwrap();
    server.shards_mut()[wrong].receive(addr, &req, now);
    assert_eq!(server.shards()[wrong].dropped_packets(), 1);
    assert_eq!(server.drain_outgoing().count(), 0, "no challenge from the wrong shard");
    server.receive(addr, &req, now);
    assert_eq!(server.drain_outgoing().count(), 1, "routed correctly, it gets a challenge");
}

#[test]
fn accept_budget_spreads_a_mass_join_over_ticks() {
    let clean = LinkProfile { loss: 0.0, dup: 0.0, base_ms: 5, jitter_ms: 0 };
    // 8 per tick server-wide = 2 per shard per tick.
    let cfg = Config { max_accepts_per_tick: 8, ..Config::default() };
    let mut w = World::with_config(cfg, 200, 4, 10_000, clean, 13);
    let mut most_per_tick = 0;
    for _ in 0..2000 {
        let joined = w.step().iter().filter(|e| matches!(e, ServerEvent::Connected { .. })).count();
        most_per_tick = most_per_tick.max(joined);
        if w.clients.iter().all(|(_, c)| c.state() == ClientState::Connected) {
            break;
        }
    }
    assert!(w.clients.iter().all(|(_, c)| c.state() == ClientState::Connected), "everyone gets in eventually");
    assert!(most_per_tick <= 8, "{most_per_tick} accepts in one tick");
    assert!(w.server.deferred_accepts() > 0);
    assert_eq!(w.server.client_count(), 200);
}

/// One exchange over a perfect link: clients send, the server ticks, replies arrive.
fn pump(server: &mut Server, peers: &mut [(SocketAddr, &mut Client)], t: Instant) -> Vec<ServerEvent> {
    let server_addr: SocketAddr = SERVER.parse().unwrap();
    for (addr, c) in peers.iter_mut() {
        c.update(t);
        c.flush(t);
        for p in c.drain_outgoing() {
            server.receive(*addr, &p, t);
        }
    }
    let mut events = Vec::new();
    while let Some(e) = server.poll_event() {
        events.push(e);
    }
    server.update(t, unix());
    server.flush(t);
    let out: Vec<_> = server.drain_outgoing().collect();
    for (to, p) in out {
        if let Some((_, c)) = peers.iter_mut().find(|(a, _)| *a == to) {
            c.receive(server_addr, &p, t);
        }
    }
    events
}

#[test]
fn recycled_connection_starts_clean() {
    let cfg = Config::default();
    let server_addr: SocketAddr = SERVER.parse().unwrap();
    let mut t = Instant::now();
    let mut server = Server::with_shards(cfg.clone(), &identity(), 10, 1, t);
    assert_eq!(server.preallocate(1), 2, "25% headroom, rounded up");
    let (aa, ba) = (client_addr(0), client_addr(1));

    // A connects and leaves state behind in its connection: reliable messages
    // the server delivered (rx ids advanced) and ones A never got (tx in flight).
    let mut a = Client::new(cfg.clone(), server_addr, token(&cfg, 0), t);
    for _ in 0..10 {
        t += TICK;
        pump(&mut server, &mut [(aa, &mut a)], t);
    }
    let a_id = a.client_id().unwrap();
    for i in 0..5 {
        a.send(Channel::Reliable, vec![i]).unwrap();
    }
    for _ in 0..5 {
        t += TICK;
        pump(&mut server, &mut [(aa, &mut a)], t);
    }
    for m in [b"a0", b"a1", b"a2"] {
        server.send(a_id, Channel::Reliable, m.to_vec()).unwrap();
    }
    t += TICK;
    server.flush(t);
    server.drain_outgoing().for_each(drop); // A never gets these
    a.disconnect();
    t += TICK;
    pump(&mut server, &mut [(aa, &mut a)], t);
    assert_eq!(server.client_count(), 0);

    // B gets A's recycled connection (the pool is LIFO).
    let mut b = Client::new(cfg.clone(), server_addr, token(&cfg, 1), t);
    let mut server_got = Vec::new();
    let mut b_got = Vec::new();
    for step in 0..30 {
        t += TICK;
        for e in pump(&mut server, &mut [(ba, &mut b)], t) {
            if let ServerEvent::Message { data, .. } = e {
                server_got.push(data);
            }
        }
        if step == 10 {
            let b_id = b.client_id().unwrap();
            assert!(server.client_stats(b_id).unwrap().packets_received < 15, "stats reset");
            server.send(b_id, Channel::Reliable, b"b0".to_vec()).unwrap();
            server.send(b_id, Channel::Reliable, b"b1".to_vec()).unwrap();
            b.send(Channel::Reliable, b"hello".to_vec()).unwrap();
        }
        while let Some((_, d)) = b.recv() {
            b_got.push(d);
        }
    }
    assert_eq!(b_got, vec![b"b0".to_vec(), b"b1".to_vec()], "none of A's messages leak to B");
    assert_eq!(server_got, vec![b"hello".to_vec()], "B's message id 0 isn't mistaken for a duplicate");
}

/// Clients `peers` over a perfect link for `ticks` exchanges; the server's events.
fn run(server: &mut Server, peers: &mut [(SocketAddr, &mut Client)], t: &mut Instant, ticks: usize) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    for _ in 0..ticks {
        *t += TICK;
        events.extend(pump(server, peers, *t));
    }
    events
}

#[test]
fn a_token_copied_off_the_wire_is_useless_without_its_keys() {
    let cfg = Config::default();
    let mut t = Instant::now();
    let mut server = Server::new(cfg.clone(), &identity(), 10, t);
    let real = token(&cfg, 5);
    // The eavesdropper has every byte the client sends, not the keys the login
    // service gave the client over TLS.
    let mut stolen = real.clone();
    stolen.client_to_server_key = [0; 32];
    stolen.server_to_client_key = [0; 32];
    let mut thief = Client::new(cfg.clone(), SERVER.parse().unwrap(), stolen, t);
    run(&mut server, &mut [(client_addr(9), &mut thief)], &mut t, 30);
    assert_eq!(thief.state(), ClientState::Connecting, "gets a challenge, but can't answer it");
    assert_eq!(server.client_count(), 0);
    assert!(server.dropped_packets() > 0);

    // The real client still gets in with the token.
    let mut owner = Client::new(cfg.clone(), SERVER.parse().unwrap(), real, t);
    let events = run(&mut server, &mut [(client_addr(1), &mut owner)], &mut t, 10);
    assert_eq!(owner.state(), ClientState::Connected);
    assert!(events.iter().any(|e| matches!(e,
        ServerEvent::Connected { user_id: 5, user_data, .. } if *user_data == [5; USER_DATA_BYTES])));
}

#[test]
fn a_token_connects_once() {
    let cfg = Config::default();
    let mut t = Instant::now();
    let mut server = Server::with_shards(cfg.clone(), &identity(), 10, 4, t);
    let tok = token(&cfg, 5);
    let mut first = Client::new(cfg.clone(), SERVER.parse().unwrap(), tok.clone(), t);
    run(&mut server, &mut [(client_addr(1), &mut first)], &mut t, 10);
    assert_eq!(first.state(), ClientState::Connected);
    // Same token (so same keys) again, from elsewhere: its nonces would repeat.
    let mut again = Client::new(cfg.clone(), SERVER.parse().unwrap(), tok, t);
    run(&mut server, &mut [(client_addr(2), &mut again)], &mut t, 30);
    assert_eq!(again.state(), ClientState::Connecting);
    assert_eq!(server.client_count(), 1);
}

#[test]
fn a_user_connecting_again_replaces_the_old_connection() {
    let cfg = Config::default();
    let mut t = Instant::now();
    // Many shards, so the two addresses likely sit in different ones.
    let mut server = Server::with_shards(cfg.clone(), &identity(), 10, 8, t);
    let (a_addr, b_addr) = (client_addr(1), client_addr(2));
    let mut a = Client::new(cfg.clone(), SERVER.parse().unwrap(), token(&cfg, 5), t);
    run(&mut server, &mut [(a_addr, &mut a)], &mut t, 10);
    let old = a.client_id().unwrap();
    // The same user, with a new token from the login service, on another address.
    let mut b = Client::new(cfg.clone(), SERVER.parse().unwrap(), token(&cfg, 5), t);
    let events = run(&mut server, &mut [(a_addr, &mut a), (b_addr, &mut b)], &mut t, 10);
    assert_eq!(b.state(), ClientState::Connected);
    assert!(events.iter().any(|e| matches!(e,
        ServerEvent::Disconnected { client, reason: DisconnectReason::Replaced } if *client == old)));
    assert_eq!(a.state(), ClientState::Disconnected, "told by a sealed disconnect");
    assert_eq!(server.client_count(), 1);

    // A new client instance on the address of a connected one takes its place.
    let mut c = Client::new(cfg.clone(), SERVER.parse().unwrap(), token(&cfg, 6), t);
    let events = run(&mut server, &mut [(b_addr, &mut c)], &mut t, 10);
    assert_eq!(c.state(), ClientState::Connected);
    assert!(events.iter().any(|e| matches!(e, ServerEvent::Disconnected { reason: DisconnectReason::Replaced, .. })));
    assert_eq!(server.client_count(), 1);
}

#[test]
fn expired_and_foreign_tokens_get_no_reply() {
    let cfg = Config::default();
    let mut t = Instant::now();
    let mut server = Server::new(cfg.clone(), &identity(), 10, t);
    let blank = [0; USER_DATA_BYTES];
    let expired = ConnectToken::mint(&TOKEN_KEY, cfg.protocol_id, SERVER_ID, unix() - 1, 1, &blank);
    let other_server = ConnectToken::mint(&TOKEN_KEY, cfg.protocol_id, SERVER_ID + 1, unix() + 60, 2, &blank);
    let forged = ConnectToken::mint(&[8; 32], cfg.protocol_id, SERVER_ID, unix() + 60, 3, &blank);
    let old_protocol = ConnectToken::mint(&TOKEN_KEY, 1, SERVER_ID, unix() + 60, 4, &blank);
    for (i, tok) in [expired, other_server, forged, old_protocol].into_iter().enumerate() {
        let mut c = Client::new(cfg.clone(), SERVER.parse().unwrap(), tok, t);
        run(&mut server, &mut [(client_addr(i), &mut c)], &mut t, 5);
        assert_eq!(c.state(), ClientState::Connecting, "token {i}");
    }
    assert_eq!(server.drain_outgoing().count(), 0);
    assert_eq!(server.dropped_packets(), 4, "each request dropped (one each: resends are 100 ms apart)");
}

#[test]
fn forged_packets_from_a_clients_address_are_ignored() {
    let cfg = Config::default();
    let mut t = Instant::now();
    let mut server = Server::new(cfg.clone(), &identity(), 10, t);
    let addr = client_addr(1);
    let mut c = Client::new(cfg.clone(), SERVER.parse().unwrap(), token(&cfg, 1), t);
    run(&mut server, &mut [(addr, &mut c)], &mut t, 10);
    // An off-path attacker spoofing the client's address: a disconnect, and a
    // payload, for any sequence.
    for seq in 0..50u16 {
        let [a, b] = seq.to_le_bytes();
        server.receive(addr, &[7, a, b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], t);
        server.receive(addr, &[6, a, b, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19], t);
    }
    assert_eq!(server.dropped_packets(), 100);
    assert!(server.poll_event().is_none());
    run(&mut server, &mut [(addr, &mut c)], &mut t, 5);
    assert_eq!((c.state(), server.client_count()), (ClientState::Connected, 1));
}
