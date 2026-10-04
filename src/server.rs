//! Sans-IO server: feed it datagrams, drain events and outgoing datagrams.
//!
//! Connections are partitioned into `Shard`s by a keyed hash of the peer
//! address. A shard owns everything about its connections (address map,
//! handshake, events, outgoing datagrams), so shards can run on different
//! threads. The cross-shard state is read-only config and keys, an atomic
//! client count that enforces `max_clients`, and a small registry (one lock,
//! taken only when a connection is accepted or removed) of used connect tokens
//! and connected user ids.
//!
//! This crate spawns no threads: callers that want parallelism route datagrams
//! with `Router::shard`, then drive `shards_mut()` from their own thread pool.
//! `Server`'s own methods do the same work serially, routing internally.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::connection::{Channel, Config, Connection, Sealed, SendError, Stats};
use crate::crypto::Cipher;
use crate::packet::{self, DenyReason, Handshake, T_DISCONNECT, T_PAYLOAD};
use crate::token::{Key, TokenOpener, USER_DATA_BYTES};

/// Encodes its shard: `id % shard_count == shard index`.
pub type ClientId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    TimedOut,
    ClientDisconnected,
    Kicked,
    /// The same user connected again (with a new token), possibly from
    /// another address; the new connection won.
    Replaced,
}

/// Who this server is to the login service.
#[derive(Clone)]
pub struct ServerIdentity {
    /// Tokens name the server they're for; others are refused.
    pub server_id: u64,
    /// Shared with the login service, which seals tokens with it.
    pub token_key: Key,
}

impl std::fmt::Debug for ServerIdentity {
    // Never print the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerIdentity").field("server_id", &self.server_id).finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum ServerEvent {
    /// `user_id` and `user_data` come from the connect token, as the login
    /// service wrote them.
    Connected { client: ClientId, addr: SocketAddr, user_id: u64, user_data: [u8; USER_DATA_BYTES] },
    Disconnected { client: ClientId, reason: DisconnectReason },
    Message { client: ClientId, channel: Channel, data: Vec<u8> },
}

/// Cookies are valid for the current and previous bucket (10-20 s).
const COOKIE_BUCKET_SECS: u64 = 10;

/// Maps a peer address to its shard. Cheap to clone; hand one to each thread
/// that receives datagrams so it can bucket them per shard.
///
/// With several receiving sockets (`SO_REUSEPORT`), the kernel picks the
/// socket by hashing the 4-tuple, and that choice is stable while the socket
/// set is. So the shards are split into one equal run (group) per socket: the
/// socket that receives a peer's datagrams decides the group, and the keyed
/// hash picks the shard within it. No receive thread ever hands a datagram to
/// another socket's shards.
#[derive(Clone)]
pub struct Router {
    /// Keyed, so remote peers can't aim many addresses at one shard.
    key: RandomState,
    shards: u32,
    groups: u32,
}

impl Router {
    /// The shard for `addr` with a single receiving socket (group 0).
    pub fn shard(&self, addr: &SocketAddr) -> usize {
        self.shard_in(0, addr)
    }

    /// The shard for `addr` when its datagrams arrive on socket `group`.
    pub fn shard_in(&self, group: usize, addr: &SocketAddr) -> usize {
        let per = (self.shards / self.groups) as usize;
        let within = if per == 1 { 0 } else { (self.key.hash_one(addr) % per as u64) as usize };
        group * per + within
    }

    /// The socket group (receiving socket) a shard belongs to.
    pub fn group_of(&self, shard: usize) -> usize {
        shard / (self.shards / self.groups) as usize
    }

    pub fn shard_count(&self) -> usize {
        self.shards as usize
    }

    pub fn group_count(&self) -> usize {
        self.groups as usize
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Server-wide, behind one lock taken only on accept and removal.
#[derive(Default)]
struct Registry {
    /// Tokens that have connected (keyed by their tag), until they expire. A
    /// token connects at most once: its keys are the connection's, and a
    /// second connection would reuse their nonces.
    used_tokens: HashMap<[u8; 16], u64>,
    prune_at: usize,
    /// Connected users: user id -> client.
    users: HashMap<u64, ClientId>,
}

struct Shared {
    cfg: Config,
    tokens: TokenOpener,
    registry: Mutex<Registry>,
    /// Per shard: clients another shard's accept has replaced, to remove on
    /// that shard's next `update`.
    replaced: Vec<Mutex<Vec<ClientId>>>,
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
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < self.max_clients).then_some(n + 1))
            .is_ok()
    }
}

struct Slot {
    addr: SocketAddr,
    salt: u64,
    user_id: u64,
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
    /// Wall clock for token expiry, from `update`.
    unix_now: u64,
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
        if matches!(data.first(), Some(&(T_PAYLOAD | T_DISCONNECT))) {
            self.receive_sealed(from, data, now);
            return;
        }
        match packet::decode_handshake(data) {
            Ok(Handshake::Request { token, salt }) => {
                if self.by_addr.get(&from).is_some_and(|id| self.clients[id].salt == salt) {
                    return; // connected already; the client will see our payloads
                }
                if !self.owns(&from) {
                    self.dropped_packets += 1;
                    return;
                }
                // Stateless: the token is checked (so only holders of a valid
                // token get a reply), and nothing is allocated until the cookie
                // comes back.
                if self.shared.tokens.open(token.server_id, token.expires, token.private, self.unix_now).is_err() {
                    self.dropped_packets += 1;
                    return;
                }
                if self.shared.clients.load(Ordering::Relaxed) >= self.shared.max_clients {
                    self.outgoing.push((from, packet::encode_denied(salt, DenyReason::ServerFull)));
                    return;
                }
                let cookie = self.shared.cookie(&from, salt, self.shared.bucket(now));
                self.outgoing.push((from, packet::encode_challenge(salt, cookie)));
            }

            Ok(Handshake::Response { token, salt, cookie }) => {
                let existing = self.by_addr.get(&from).copied();
                if let Some(id) = existing {
                    let slot = &self.clients[&id];
                    if slot.salt == salt {
                        // Our Accepted was probably lost: resend it (idempotent).
                        let pkt = packet::encode_accepted(self.shared.cfg.protocol_id, slot.conn.send_cipher(), salt, id);
                        self.outgoing.push((from, pkt));
                        return;
                    }
                }
                let b = self.shared.bucket(now);
                let valid = cookie == self.shared.cookie(&from, salt, b)
                    || (b > 0 && cookie == self.shared.cookie(&from, salt, b - 1));
                if !valid || !self.owns(&from) {
                    self.dropped_packets += 1;
                    return;
                }
                let Ok(contents) = self.shared.tokens.open(token.server_id, token.expires, token.private, self.unix_now)
                else {
                    self.dropped_packets += 1;
                    return;
                };
                let c2s = Cipher::new(&contents.client_to_server_key);
                if !packet::verify_response(self.shared.cfg.protocol_id, &c2s, data, cookie) {
                    self.dropped_packets += 1; // has the token but not its keys
                    return;
                }
                if self.accepts_this_tick >= self.accept_budget {
                    // Admission control: no reply. The client resends and gets in on a later tick.
                    self.deferred_accepts += 1;
                    return;
                }
                let token_tag: [u8; 16] = token.private[token.private.len() - 16..].try_into().unwrap();
                let id = self.next_local.wrapping_mul(self.shared.router.shards).wrapping_add(self.index);
                let replaced = {
                    let mut reg = self.shared.registry.lock().unwrap();
                    if reg.used_tokens.contains_key(&token_tag) {
                        drop(reg);
                        self.dropped_packets += 1; // each token connects once
                        return;
                    }
                    // A new client at the address of a connected one takes its slot.
                    if existing.is_none() && !self.shared.reserve_slot() {
                        drop(reg);
                        self.outgoing.push((from, packet::encode_denied(salt, DenyReason::ServerFull)));
                        return;
                    }
                    let unix_now = self.unix_now;
                    if reg.used_tokens.len() >= reg.prune_at {
                        reg.used_tokens.retain(|_, &mut exp| exp > unix_now);
                        reg.prune_at = (reg.used_tokens.len() * 2).max(1024);
                    }
                    reg.used_tokens.insert(token_tag, token.expires);
                    reg.users.insert(contents.user_id, id)
                };
                if let Some(old) = existing {
                    self.remove_slot(old, DisconnectReason::Replaced, true, false);
                }
                if let Some(old) = replaced.filter(|&old| Some(old) != existing) {
                    let shard = old as usize % self.shared.replaced.len();
                    self.shared.replaced[shard].lock().unwrap().push(old);
                }
                self.next_local = self.next_local.wrapping_add(1);
                self.accepts_this_tick += 1;
                let (send, recv) = (&contents.server_to_client_key, &contents.client_to_server_key);
                let conn = match self.pool.pop() {
                    Some(mut c) => {
                        c.reset(send, recv, now);
                        c
                    }
                    None => Connection::new(self.shared.cfg.clone(), send, recv, now),
                };
                let pkt = packet::encode_accepted(self.shared.cfg.protocol_id, conn.send_cipher(), salt, id);
                self.outgoing.push((from, pkt));
                self.by_addr.insert(from, id);
                self.clients.insert(id, Slot { addr: from, salt, user_id: contents.user_id, conn });
                self.events.push_back(ServerEvent::Connected {
                    client: id,
                    addr: from,
                    user_id: contents.user_id,
                    user_data: contents.user_data,
                });
            }

            // Server->client packets, or junk.
            _ => self.dropped_packets += 1,
        }
    }

    fn receive_sealed(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        let Some(&id) = self.by_addr.get(&from) else {
            self.dropped_packets += 1;
            return;
        };
        let slot = self.clients.get_mut(&id).expect("by_addr and clients in sync");
        match slot.conn.receive_sealed(data, now) {
            Ok(Sealed::Payload) => {
                while let Some((channel, data)) = slot.conn.recv() {
                    self.events.push_back(ServerEvent::Message { client: id, channel, data });
                }
            }
            Ok(Sealed::Disconnect) => self.remove(id, DisconnectReason::ClientDisconnected, false),
            Err(_) => self.dropped_packets += 1,
        }
    }

    /// Detect timeouts and start a new accept budget. Call once per tick.
    /// `unix_now` is wall-clock seconds, for token expiry.
    pub fn update(&mut self, now: Instant, unix_now: u64) {
        self.accepts_this_tick = 0;
        self.unix_now = unix_now;
        let replaced = std::mem::take(&mut *self.shared.replaced[self.index as usize].lock().unwrap());
        for id in replaced {
            self.remove(id, DisconnectReason::Replaced, true);
        }
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

    /// An unreliable message whose delivery comes back through `take_acked`.
    pub fn send_tagged(&mut self, client: ClientId, data: Vec<u8>, tag: u32) -> Result<(), SendError> {
        self.clients.get_mut(&client).ok_or(SendError::UnknownClient)?.conn.send_tagged(data, tag)
    }

    /// Moves the tags of `client`'s acked tagged messages into `out`.
    pub fn take_acked(&mut self, client: ClientId, out: &mut Vec<u32>) {
        if let Some(slot) = self.clients.get_mut(&client) {
            slot.conn.take_acked(out);
        }
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

    /// Datagrams dropped: failed authentication, bad or used tokens, bad cookies, misrouting, junk.
    pub fn dropped_packets(&self) -> u64 {
        self.dropped_packets
    }

    /// Valid challenge responses left unanswered because this tick's accept
    /// budget was spent (the client retries).
    pub fn deferred_accepts(&self) -> u64 {
        self.deferred_accepts
    }

    fn owns(&self, addr: &SocketAddr) -> bool {
        let r = &self.shared.router;
        r.shard_in(r.group_of(self.index as usize), addr) == self.index as usize
    }

    fn remove(&mut self, id: ClientId, reason: DisconnectReason, notify: bool) {
        self.remove_slot(id, reason, notify, true);
    }

    /// `release`: give back its place under `max_clients` (not when a new
    /// client at the same address takes it over).
    fn remove_slot(&mut self, id: ClientId, reason: DisconnectReason, notify: bool, release: bool) {
        let Some(mut slot) = self.clients.remove(&id) else { return };
        self.by_addr.remove(&slot.addr);
        {
            let mut reg = self.shared.registry.lock().unwrap();
            if reg.users.get(&slot.user_id) == Some(&id) {
                reg.users.remove(&slot.user_id);
            }
        }
        if release {
            self.shared.clients.fetch_sub(1, Ordering::AcqRel);
        }
        if notify {
            // Redundant: this is fire-and-forget over UDP. Each is sealed with
            // its own sequence, so a forged or replayed one is ignored.
            for _ in 0..3 {
                let pkt = slot.conn.seal_disconnect();
                self.outgoing.push((slot.addr, pkt));
            }
        }
        self.pool.push(slot.conn);
        self.events.push_back(ServerEvent::Disconnected { client: id, reason });
    }
}

pub struct Server {
    shared: Arc<Shared>,
    shards: Vec<Shard>,
}

impl Server {
    /// A single-shard server.
    pub fn new(cfg: Config, identity: &ServerIdentity, max_clients: usize, now: Instant) -> Self {
        Self::with_shards(cfg, identity, max_clients, 1, now)
    }

    /// `shards` independent partitions of the connections. More shards than
    /// threads lets a work-stealing pool balance uneven shards.
    pub fn with_shards(cfg: Config, identity: &ServerIdentity, max_clients: usize, shards: usize, now: Instant) -> Self {
        Self::with_socket_groups(cfg, identity, max_clients, shards, 1, now)
    }

    /// Shards split into `groups` equal runs, one per receiving socket (see
    /// `Router`): route each datagram with `Router::shard_in(socket, from)`,
    /// or `receive_in`, and send each shard's datagrams from its group's socket.
    pub fn with_socket_groups(
        cfg: Config,
        identity: &ServerIdentity,
        max_clients: usize,
        shards: usize,
        groups: usize,
        now: Instant,
    ) -> Self {
        assert!((1..=u16::MAX as usize).contains(&shards), "shards must be 1..=65535");
        assert!(groups >= 1 && shards.is_multiple_of(groups), "shards must split evenly into socket groups");
        let accept_budget = match cfg.max_accepts_per_tick {
            0 => usize::MAX,
            n => n.div_ceil(shards),
        };
        let shared = Arc::new(Shared {
            tokens: TokenOpener::new(&identity.token_key, cfg.protocol_id, identity.server_id),
            cfg,
            registry: Mutex::new(Registry::default()),
            replaced: (0..shards).map(|_| Mutex::new(Vec::new())).collect(),
            max_clients,
            cookie_key: RandomState::new(),
            epoch: now,
            router: Router { key: RandomState::new(), shards: shards as u32, groups: groups as u32 },
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
                // Until the first `update`, so a token can't outlive its expiry.
                unix_now: unix_now(),
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
            // Keys are set when a connection is handed out (`reset`).
            shard.pool.extend((0..per_shard).map(|_| Connection::new(cfg.clone(), &[0; 32], &[0; 32], epoch)));
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

    /// A datagram from the single receiving socket (group 0).
    pub fn receive(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        self.receive_in(0, from, data, now);
    }

    /// A datagram that arrived on socket `group` (see `with_socket_groups`).
    pub fn receive_in(&mut self, group: usize, from: SocketAddr, data: &[u8], now: Instant) {
        let s = self.shared.router.shard_in(group, &from);
        self.shards[s].receive(from, data, now);
    }

    /// Detect timeouts and reset accept budgets in every shard. Call once per tick.
    /// `unix_now` is wall-clock seconds, for token expiry.
    pub fn update(&mut self, now: Instant, unix_now: u64) {
        self.shards.iter_mut().for_each(|s| s.update(now, unix_now));
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

    pub fn send_tagged(&mut self, client: ClientId, data: Vec<u8>, tag: u32) -> Result<(), SendError> {
        let s = self.shard_of_client(client);
        self.shards[s].send_tagged(client, data, tag)
    }

    pub fn take_acked(&mut self, client: ClientId, out: &mut Vec<u32>) {
        let s = self.shard_of_client(client);
        self.shards[s].take_acked(client, out);
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
