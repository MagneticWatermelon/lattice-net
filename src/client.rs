//! Sans-IO client: handshake state machine wrapping a `Connection`.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::SocketAddr;
use std::time::{Instant, SystemTime};

use crate::connection::{Channel, Config, Connection, SendError, Stats};
use crate::packet::{self, session_from_cookie, DenyReason, Packet};
use crate::server::ClientId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    Connecting,
    Connected,
    Denied(DenyReason),
    TimedOut,
    Disconnected,
}

#[allow(clippy::large_enum_variant)] // one per process, not worth a Box
enum Phase {
    Requesting,
    Responding { cookie: u64 },
    Connected { conn: Connection, id: ClientId },
    Denied(DenyReason),
    TimedOut,
    Disconnected,
}

pub struct Client {
    cfg: Config,
    server: SocketAddr,
    salt: u64,
    phase: Phase,
    started: Instant,
    last_handshake: Option<Instant>,
    outgoing: Vec<Vec<u8>>,
}

impl Client {
    pub fn new(cfg: Config, server: SocketAddr, now: Instant) -> Self {
        Self {
            cfg,
            server,
            salt: random_u64(),
            phase: Phase::Requesting,
            started: now,
            last_handshake: None,
            outgoing: Vec::new(),
        }
    }

    pub fn state(&self) -> ClientState {
        match self.phase {
            Phase::Requesting | Phase::Responding { .. } => ClientState::Connecting,
            Phase::Connected { .. } => ClientState::Connected,
            Phase::Denied(r) => ClientState::Denied(r),
            Phase::TimedOut => ClientState::TimedOut,
            Phase::Disconnected => ClientState::Disconnected,
        }
    }

    pub fn client_id(&self) -> Option<ClientId> {
        match self.phase {
            Phase::Connected { id, .. } => Some(id),
            _ => None,
        }
    }

    pub fn server_addr(&self) -> SocketAddr {
        self.server
    }

    pub fn receive(&mut self, from: SocketAddr, data: &[u8], now: Instant) {
        if from != self.server {
            return;
        }
        let Ok(pkt) = packet::decode(self.cfg.protocol_id, data) else { return };
        let salt = self.salt;
        let next = match (&mut self.phase, pkt) {
            (Phase::Requesting, Packet::Challenge { client_salt, cookie }) if client_salt == salt => {
                self.last_handshake = None; // answer on the next update, no waiting
                Some(Phase::Responding { cookie })
            }
            (Phase::Responding { cookie }, Packet::Accepted { client_salt, client_id }) if client_salt == salt => {
                let conn = Connection::new(self.cfg.clone(), session_from_cookie(*cookie), now);
                Some(Phase::Connected { conn, id: client_id })
            }
            (Phase::Requesting | Phase::Responding { .. }, Packet::Denied { client_salt, reason })
                if client_salt == salt =>
            {
                Some(Phase::Denied(reason))
            }
            (Phase::Connected { conn, .. }, Packet::Payload { session, header, body }) if session == conn.session() => {
                let _ = conn.on_payload(header, body, now);
                None
            }
            (Phase::Connected { conn, .. }, Packet::Disconnect { session }) if session == conn.session() => {
                Some(Phase::Disconnected)
            }
            _ => None,
        };
        if let Some(p) = next {
            self.phase = p;
        }
    }

    /// Handshake resends and timeouts. Call once per tick.
    pub fn update(&mut self, now: Instant) {
        match &self.phase {
            Phase::Requesting | Phase::Responding { .. } => {
                if now.saturating_duration_since(self.started) > self.cfg.timeout {
                    self.phase = Phase::TimedOut;
                    return;
                }
                let due = self
                    .last_handshake
                    .is_none_or(|t| now.saturating_duration_since(t) >= self.cfg.handshake_resend_interval);
                if due {
                    let p = match self.phase {
                        Phase::Responding { cookie } => Packet::ChallengeResponse { client_salt: self.salt, cookie },
                        _ => Packet::ConnectionRequest { client_salt: self.salt },
                    };
                    self.outgoing.push(packet::encode(self.cfg.protocol_id, &p));
                    self.last_handshake = Some(now);
                }
            }
            Phase::Connected { conn, .. }
                if conn.timed_out(now) => {
                    self.phase = Phase::TimedOut;
                }
            _ => {}
        }
    }

    pub fn send(&mut self, channel: Channel, data: Vec<u8>) -> Result<(), SendError> {
        match &mut self.phase {
            Phase::Connected { conn, .. } => conn.send(channel, data),
            _ => Err(SendError::NotConnected),
        }
    }

    pub fn recv(&mut self) -> Option<(Channel, Vec<u8>)> {
        match &mut self.phase {
            Phase::Connected { conn, .. } => conn.recv(),
            _ => None,
        }
    }

    pub fn flush(&mut self, now: Instant) {
        if let Phase::Connected { conn, .. } = &mut self.phase {
            conn.flush(now, &mut self.outgoing);
        }
    }

    pub fn drain_outgoing(&mut self) -> std::vec::Drain<'_, Vec<u8>> {
        self.outgoing.drain(..)
    }

    pub fn disconnect(&mut self) {
        if let Phase::Connected { conn, .. } = &self.phase {
            let pkt = packet::encode(self.cfg.protocol_id, &Packet::Disconnect { session: conn.session() });
            for _ in 0..3 {
                self.outgoing.push(pkt.clone());
            }
        }
        self.phase = Phase::Disconnected;
    }

    pub fn stats(&self) -> Option<&Stats> {
        match &self.phase {
            Phase::Connected { conn, .. } => Some(conn.stats()),
            _ => None,
        }
    }

    pub fn reliable_pending(&self) -> usize {
        match &self.phase {
            Phase::Connected { conn, .. } => conn.reliable_pending(),
            _ => 0,
        }
    }
}

fn random_u64() -> u64 {
    // RandomState is seeded from the OS RNG; good enough for a salt.
    
    
    RandomState::new().hash_one(SystemTime::now())
}
