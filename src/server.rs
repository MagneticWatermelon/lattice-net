//! Sans-IO server: feed it datagrams, drain events and outgoing datagrams.
//!
//! Connections are partitioned into `Shard`s by a keyed hash of the peer
//! address. A shard owns everything about its connections (address map,
//! handshake, events, outgoing datagrams), so shards can run on different
//! threads with no locking. The only cross-shard state is read-only config and
//! keys plus an atomic client count that enforces `max_clients`.
//!
//! This crate spawns no threads: callers that want parallelism route datagrams
//! with `Router::shard`, then drive `shards_mut()` from their own thread pool.
//! `Server`'s own methods do the same work serially, routing internally.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::connection::{Channel, Config, Connection, SendError, Stats};
use crate::packet::{self, session_from_cookie, DenyReason, Packet};

/// Encodes its shard: `id % shard_count == shard index`.
pub type ClientId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    TimedOut,
    ClientDisconnected,
    Kicked,
}

#[derive(Debug)]
pub enum ServerEvent {
    Connected { client: ClientId, addr: SocketAddr },
    Disconnected { client: ClientId, reason: DisconnectReason },
    Message { client: ClientId, channel: Channel, data: Vec<u8> },
}

/// Cookies are valid for the current and previous bucket (10-20 s).
const COOKIE_BUCKET_SECS: u64 = 10;

/// Maps a peer address to its shard. Cheap to clone; hand one to the thread
/// that receives datagrams so it can bucket them per shard.
#[derive(Clone)]
pub struct Router {
    /// Keyed, so remote peers can't aim many addresses at one shard.
    key: RandomState,
    shards: u32,
}

impl Router {
    pub fn shard(&self, addr: &SocketAddr) -> usize {
        if self.shards == 1 {
            0
        } else {
            (self.key.hash_one(addr) % self.shards as u64) as usize
        }
    }

    pub fn shard_count(&self) -> usize {
        self.shards as usize
    }
}

struct Shared {
    cfg: Config,
    max_clients: usize,
    /// Randomly keyed SipHash: the server secret for handshake cookies.
    cookie_key: RandomState,
    epoch: Instant,
    router: Router,
    /// Connected clients across all shards.
    clients: AtomicUsize,
}

impl Shared {
    fn bucket(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.epoch).as_secs() / COOKIE_BUCKET_SECS
    }

    fn cookie(&self, addr: &SocketAddr, salt: u64, bucket: u64) -> u64 {
        let mut h = self.cookie_key.build_hasher();
        addr.hash(&mut h);
        salt.hash(&mut h);
        bucket.hash(&mut h);
        h.finish()
    }

    /// Takes a slot if one is free.
    fn reserve_slot(&self) -> bool {
        self.clients
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < self.max_clients).then_some(n + 1))
            .is_ok()
    }
}

struct Slot {
    addr: SocketAddr,
    salt: u64,
    conn: Connection,
}

/// The connections whose addresses route to one shard. `Send`; independent of
/// every other shard.
pub struct Shard {
    shared: Arc<Shared>,
    index: u32,
    by_addr: HashMap<SocketAddr, ClientId>,
    clients: HashMap<ClientId, Slot>,
    next_local: u32,
    /// This shard's share of `Config::max_accepts_per_tick`.
    accept_budget: usize,
    accepts_this_tick: usize,
    /// Recycled connections: an accept resets one of these instead of
    /// allocating (and page-faulting) fresh windows.
    pool: Vec<Connection>,
    events: VecDeque<ServerEvent>,
    outgoing: Vec<(SocketAddr, Vec<u8>)>,
    dropped_packets: u64,
    deferred_accepts: u64,
}

impl Shard {
    pub fn index(&self) -> usize {
        self.index as usize
    }

    /// `from` must route to this shard (`Router::shard`). Handshakes from an
    /// address routed elsewhere are dropped, so a routing bug can't create a
    /// connection in the wrong shard.
    pub fn receive(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        let Ok(pkt) = packet::decode(self.shared.cfg.protocol_id, data) else {
            self.dropped_packets += 1;
            return;
        };
        match pkt {
            Packet::ConnectionRequest { client_salt } => {
                if self.by_addr.contains_key(&from) {
                    return;
                }
                if !self.owns(&from) {
                    self.dropped_packets += 1;
                    return;
                }
                if self.shared.clients.load(Ordering::Relaxed) >= self.shared.max_clients {
                    self.push(from, Packet::Denied { client_salt, reason: DenyReason::ServerFull });
                    return;
                }
                // Stateless: nothing is allocated until the cookie comes back.
                let cookie = self.shared.cookie(&from, client_salt, self.shared.bucket(now));
                self.push(from, Packet::Challenge { client_salt, cookie });
            }

            Packet::ChallengeResponse { client_salt, cookie } => {
                if let Some(&id) = self.by_addr.get(&from) {
                    // Our Accepted was probably lost: resend it (idempotent).
                    if self.clients[&id].salt == client_salt {
                        self.push(from, Packet::Accepted { client_salt, client_id: id });
                    }
                    return;
                }
                let b = self.shared.bucket(now);
                let valid = cookie == self.shared.cookie(&from, client_salt, b)
                    || (b > 0 && cookie == self.shared.cookie(&from, client_salt, b - 1));
                if !valid || !self.owns(&from) {
                    self.dropped_packets += 1;
                    return;
                }
                if self.accepts_this_tick >= self.accept_budget {
                    // Admission control: no reply. The client resends and gets in on a later tick.
                    self.deferred_accepts += 1;
                    return;
                }
                if !self.shared.reserve_slot() {
                    self.push(from, Packet::Denied { client_salt, reason: DenyReason::ServerFull });
                    return;
                }
                let id = self.next_local.wrapping_mul(self.shared.router.shards).wrapping_add(self.index);
                self.next_local = self.next_local.wrapping_add(1);
                self.accepts_this_tick += 1;
                let session = session_from_cookie(cookie);
                let conn = match self.pool.pop() {
                    Some(mut c) => {
                        c.reset(session, now);
                        c
                    }
                    None => Connection::new(self.shared.cfg.clone(), session, now),
                };
                self.by_addr.insert(from, id);
                self.clients.insert(id, Slot { addr: from, salt: client_salt, conn });
                self.push(from, Packet::Accepted { client_salt, client_id: id });
                self.events.push_back(ServerEvent::Connected { client: id, addr: from });
            }

            Packet::Payload { session, header, body } => {
                let Some(&id) = self.by_addr.get(&from) else {
                    self.dropped_packets += 1;
                    return;
                };
                let slot = self.clients.get_mut(&id).expect("by_addr and clients in sync");
                if slot.conn.session() != session || slot.conn.on_payload(header, body, now).is_err() {
                    self.dropped_packets += 1;
                    return;
                }
                while let Some((channel, data)) = slot.conn.recv() {
                    self.events.push_back(ServerEvent::Message { client: id, channel, data });
                }
            }

            Packet::Disconnect { session } => {
                if let Some(&id) = self.by_addr.get(&from) {
                    if self.clients[&id].conn.session() == session {
                        self.remove(id, DisconnectReason::ClientDisconnected, false);
                    }
                }
            }

            _ => self.dropped_packets += 1,
        }
    }

    /// Detect timeouts and start a new accept budget. Call once per tick.
    pub fn update(&mut self, now: Instant) {
        self.accepts_this_tick = 0;
        let timed_out: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, s)| s.conn.timed_out(now))
            .map(|(&id, _)| id)
            .collect();
        for id in timed_out {
            self.remove(id, DisconnectReason::TimedOut, false);
        }
    }

    /// Build packets for every client in this shard. Call once per tick after queuing sends.
    pub fn flush(&mut self, now: Instant) {
        let mut pkts = Vec::new();
        for slot in self.clients.values_mut() {
            slot.conn.flush(now, &mut pkts);
            for p in pkts.drain(..) {
                self.outgoing.push((slot.addr, p));
            }
        }
    }

    pub fn send(&mut self, client: ClientId, channel: Channel, data: Vec<u8>) -> Result<(), SendError> {
        self.clients
            .get_mut(&client)
            .ok_or(SendError::UnknownClient)?
            .conn
            .send(channel, data)
    }

    pub fn disconnect(&mut self, client: ClientId) {
        self.remove(client, DisconnectReason::Kicked, true);
    }

    pub fn poll_event(&mut self) -> Option<ServerEvent> {
        self.events.pop_front()
    }

    pub fn drain_outgoing(&mut self) -> std::vec::Drain<'_, (SocketAddr, Vec<u8>)> {
        self.outgoing.drain(..)
    }

    /// Clients in this shard.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn client_ids(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.clients.keys().copied()
    }

    pub fn client_stats(&self, client: ClientId) -> Option<&Stats> {
        self.clients.get(&client).map(|s| s.conn.stats())
    }

    pub fn client_addr(&self, client: ClientId) -> Option<SocketAddr> {
        self.clients.get(&client).map(|s| s.addr)
    }

    /// Datagrams dropped for bad CRC, bad cookie, wrong session, misrouting, etc.
    pub fn dropped_packets(&self) -> u64 {
        self.dropped_packets
    }

    /// Valid challenge responses left unanswered because this tick's accept
    /// budget was spent (the client retries).
    pub fn deferred_accepts(&self) -> u64 {
        self.deferred_accepts
    }

    fn owns(&self, addr: &SocketAddr) -> bool {
        self.shared.router.shard(addr) == self.index as usize
    }

    fn remove(&mut self, id: ClientId, reason: DisconnectReason, notify: bool) {
        let Some(slot) = self.clients.remove(&id) else { return };
        self.by_addr.remove(&slot.addr);
        let session = slot.conn.session();
        self.shared.clients.fetch_sub(1, Ordering::AcqRel);
        if notify {
            // Redundant: this is fire-and-forget over UDP.
            for _ in 0..3 {
                self.push(slot.addr, Packet::Disconnect { session });
            }
        }
        self.pool.push(slot.conn);
        self.events.push_back(ServerEvent::Disconnected { client: id, reason });
    }

    fn push(&mut self, to: SocketAddr, p: Packet<'_>) {
        self.outgoing.push((to, packet::encode(self.shared.cfg.protocol_id, &p)));
    }
}

pub struct Server {
    shared: Arc<Shared>,
    shards: Vec<Shard>,
}

impl Server {
    /// A single-shard server.
    pub fn new(cfg: Config, max_clients: usize, now: Instant) -> Self {
        Self::with_shards(cfg, max_clients, 1, now)
    }

    /// `shards` independent partitions of the connections. More shards than
    /// threads lets a work-stealing pool balance uneven shards.
    pub fn with_shards(cfg: Config, max_clients: usize, shards: usize, now: Instant) -> Self {
        assert!((1..=u16::MAX as usize).contains(&shards), "shards must be 1..=65535");
        let accept_budget = match cfg.max_accepts_per_tick {
            0 => usize::MAX,
            n => n.div_ceil(shards),
        };
        let shared = Arc::new(Shared {
            cfg,
            max_clients,
            cookie_key: RandomState::new(),
            epoch: now,
            router: Router { key: RandomState::new(), shards: shards as u32 },
            clients: AtomicUsize::new(0),
        });
        let shards = (0..shards as u32)
            .map(|index| Shard {
                shared: shared.clone(),
                index,
                by_addr: HashMap::new(),
                clients: HashMap::new(),
                next_local: 0,
                accept_budget,
                accepts_this_tick: 0,
                pool: Vec::new(),
                events: VecDeque::new(),
                outgoing: Vec::new(),
                dropped_packets: 0,
                deferred_accepts: 0,
            })
            .collect();
        Self { shared, shards }
    }

    /// Allocates `connections` ready-to-use connections up front, spread over
    /// the shards with 25% headroom for uneven hashing, so accepts don't allocate
    /// or page-fault. Shards that run out fall back to allocating. Returns how
    /// many were created.
    pub fn preallocate(&mut self, connections: usize) -> usize {
        let per_shard = (connections.div_ceil(self.shards.len()) * 5).div_ceil(4);
        let cfg = &self.shared.cfg;
        let epoch = self.shared.epoch;
        for shard in &mut self.shards {
            shard.pool.extend((0..per_shard).map(|_| Connection::new(cfg.clone(), 0, epoch)));
        }
        per_shard * self.shards.len()
    }

    pub fn router(&self) -> Router {
        self.shared.router.clone()
    }

    pub fn shards(&self) -> &[Shard] {
        &self.shards
    }

    pub fn shards_mut(&mut self) -> &mut [Shard] {
        &mut self.shards
    }

    pub fn shard_of_client(&self, client: ClientId) -> usize {
        client as usize % self.shards.len()
    }

    pub fn receive(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        let s = self.shared.router.shard(&from);
        self.shards[s].receive(from, data, now);
    }

    /// Detect timeouts and reset accept budgets in every shard. Call once per tick.
    pub fn update(&mut self, now: Instant) {
        self.shards.iter_mut().for_each(|s| s.update(now));
    }

    /// Build packets for every client. Call once per tick after queuing sends.
    pub fn flush(&mut self, now: Instant) {
        self.shards.iter_mut().for_each(|s| s.flush(now));
    }

    pub fn send(&mut self, client: ClientId, channel: Channel, data: Vec<u8>) -> Result<(), SendError> {
        let s = self.shard_of_client(client);
        self.shards[s].send(client, channel, data)
    }

    pub fn disconnect(&mut self, client: ClientId) {
        let s = self.shard_of_client(client);
        self.shards[s].disconnect(client);
    }

    /// Events of one client stay in order; events of different shards don't
    /// interleave in any particular order.
    pub fn poll_event(&mut self) -> Option<ServerEvent> {
        self.shards.iter_mut().find_map(|s| s.poll_event())
    }

    pub fn drain_outgoing(&mut self) -> impl Iterator<Item = (SocketAddr, Vec<u8>)> + '_ {
        self.shards.iter_mut().flat_map(|s| s.drain_outgoing())
    }

    pub fn client_count(&self) -> usize {
        self.shared.clients.load(Ordering::Acquire)
    }

    pub fn client_ids(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.shards.iter().flat_map(|s| s.client_ids())
    }

    pub fn client_stats(&self, client: ClientId) -> Option<&Stats> {
        self.shards[self.shard_of_client(client)].client_stats(client)
    }

    pub fn client_addr(&self, client: ClientId) -> Option<SocketAddr> {
        self.shards[self.shard_of_client(client)].client_addr(client)
    }

    /// Datagrams dropped for bad CRC, bad cookie, wrong session, etc.
    pub fn dropped_packets(&self) -> u64 {
        self.shards.iter().map(|s| s.dropped_packets).sum()
    }

    /// Accepts deferred to a later tick by `Config::max_accepts_per_tick`.
    pub fn deferred_accepts(&self) -> u64 {
        self.shards.iter().map(|s| s.deferred_accepts).sum()
    }
}
