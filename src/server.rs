//! Sans-IO server: feed it datagrams, drain events and outgoing datagrams.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher};
use std::net::SocketAddr;
use std::time::Instant;

use crate::connection::{Channel, Config, Connection, SendError, Stats};
use crate::packet::{self, session_from_cookie, DenyReason, Packet};

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

struct Slot {
    addr: SocketAddr,
    salt: u64,
    conn: Connection,
}

/// Cookies are valid for the current and previous bucket (10-20 s).
const COOKIE_BUCKET_SECS: u64 = 10;

pub struct Server {
    cfg: Config,
    max_clients: usize,
    /// Randomly keyed SipHash: the server secret for handshake cookies.
    key: RandomState,
    epoch: Instant,
    by_addr: HashMap<SocketAddr, ClientId>,
    clients: HashMap<ClientId, Slot>,
    next_id: ClientId,
    events: VecDeque<ServerEvent>,
    outgoing: Vec<(SocketAddr, Vec<u8>)>,
    /// Datagrams dropped for bad CRC, bad cookie, wrong session, etc.
    pub dropped_packets: u64,
}

impl Server {
    pub fn new(cfg: Config, max_clients: usize, now: Instant) -> Self {
        Self {
            cfg,
            max_clients,
            key: RandomState::new(),
            epoch: now,
            by_addr: HashMap::new(),
            clients: HashMap::new(),
            next_id: 0,
            events: VecDeque::new(),
            outgoing: Vec::new(),
            dropped_packets: 0,
        }
    }

    pub fn receive(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        let Ok(pkt) = packet::decode(self.cfg.protocol_id, data) else {
            self.dropped_packets += 1;
            return;
        };
        match pkt {
            Packet::ConnectionRequest { client_salt } => {
                if self.by_addr.contains_key(&from) {
                    return;
                }
                if self.clients.len() >= self.max_clients {
                    self.push(from, Packet::Denied { client_salt, reason: DenyReason::ServerFull });
                    return;
                }
                // Stateless: nothing is allocated until the cookie comes back.
                let cookie = self.cookie(&from, client_salt, self.bucket(now));
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
                let b = self.bucket(now);
                let valid = cookie == self.cookie(&from, client_salt, b)
                    || (b > 0 && cookie == self.cookie(&from, client_salt, b - 1));
                if !valid {
                    self.dropped_packets += 1;
                    return;
                }
                if self.clients.len() >= self.max_clients {
                    self.push(from, Packet::Denied { client_salt, reason: DenyReason::ServerFull });
                    return;
                }
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                let conn = Connection::new(self.cfg.clone(), session_from_cookie(cookie), now);
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

    /// Detect timeouts. Call once per tick.
    pub fn update(&mut self, now: Instant) {
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

    /// Build packets for every client. Call once per tick after queuing sends.
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

    fn remove(&mut self, id: ClientId, reason: DisconnectReason, notify: bool) {
        let Some(slot) = self.clients.remove(&id) else { return };
        self.by_addr.remove(&slot.addr);
        if notify {
            // Redundant: this is fire-and-forget over UDP.
            for _ in 0..3 {
                self.push(slot.addr, Packet::Disconnect { session: slot.conn.session() });
            }
        }
        self.events.push_back(ServerEvent::Disconnected { client: id, reason });
    }

    fn push(&mut self, to: SocketAddr, p: Packet<'_>) {
        self.outgoing.push((to, packet::encode(self.cfg.protocol_id, &p)));
    }

    fn bucket(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.epoch).as_secs() / COOKIE_BUCKET_SECS
    }

    fn cookie(&self, addr: &SocketAddr, salt: u64, bucket: u64) -> u64 {
        let mut h = self.key.build_hasher();
        addr.hash(&mut h);
        salt.hash(&mut h);
        bucket.hash(&mut h);
        h.finish()
    }
}
