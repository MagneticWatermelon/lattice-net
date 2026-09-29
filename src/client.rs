//! Sans-IO client: handshake state machine wrapping a `Connection`.

use std::net::SocketAddr;
use std::time::Instant;

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::OsRng;

use crate::connection::{Channel, Config, Connection, Sealed, SendError, Stats};
use crate::crypto::Cipher;
use crate::packet::{self, DenyReason, Handshake, T_DISCONNECT, T_PAYLOAD};
use crate::server::ClientId;
use crate::token::ConnectToken;

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
    token: ConnectToken,
    salt: u64,
    phase: Phase,
    started: Instant,
    last_handshake: Option<Instant>,
    outgoing: Vec<Vec<u8>>,
}

impl Client {
    /// Connects to `server` with a token from the login service (which says
    /// where to connect). A token connects once: reconnecting takes a new one.
    pub fn new(cfg: Config, server: SocketAddr, token: ConnectToken, now: Instant) -> Self {
        Self {
            cfg,
            server,
            token,
            salt: OsRng.next_u64(),
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
        if let Phase::Connected { conn, .. } = &mut self.phase {
            if matches!(data.first(), Some(&(T_PAYLOAD | T_DISCONNECT)))
                && conn.receive_sealed(data, now) == Ok(Sealed::Disconnect)
            {
                self.phase = Phase::Disconnected;
            }
            return;
        }
        let Ok(pkt) = packet::decode_handshake(data) else { return };
        let salt = self.salt;
        let next = match (&self.phase, pkt) {
            (Phase::Requesting, Handshake::Challenge { salt: s, cookie }) if s == salt => {
                self.last_handshake = None; // answer on the next update, no waiting
                Some(Phase::Responding { cookie })
            }
            (Phase::Responding { .. }, Handshake::Accepted { salt: s }) if s == salt => {
                let s2c = Cipher::new(&self.token.server_to_client_key);
                packet::open_accepted(self.cfg.protocol_id, &s2c, data, salt).map(|id| {
                    let (send, recv) = (&self.token.client_to_server_key, &self.token.server_to_client_key);
                    Phase::Connected { conn: Connection::new(self.cfg.clone(), send, recv, now), id }
                })
            }
            (Phase::Requesting | Phase::Responding { .. }, Handshake::Denied { salt: s, reason }) if s == salt => {
                Some(Phase::Denied(reason))
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
                    let pkt = match self.phase {
                        Phase::Responding { cookie } => {
                            packet::encode_response(self.cfg.protocol_id, &self.token, self.salt, cookie)
                        }
                        _ => packet::encode_request(&self.token, self.salt),
                    };
                    self.outgoing.push(pkt);
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
        if let Phase::Connected { conn, .. } = &mut self.phase {
            for _ in 0..3 {
                let pkt = conn.seal_disconnect();
                self.outgoing.push(pkt);
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
