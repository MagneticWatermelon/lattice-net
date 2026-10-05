//! The client's connection: a UDP socket, the `lattice_net` transport and the
//! client core, driven once per frame. Plain Rust, no Bevy, so it's tested
//! against a real server loop (`tests` below).

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime};

use lattice_client_core::{ClientConfig, ClientCore};
use lattice_game::movement::Input;
use lattice_net::token::{Key, USER_DATA_BYTES};
use lattice_net::{Channel, Client, ClientState, Config, ConnectToken};

/// Snapshot arrival gaps kept for the net graph.
pub const GAPS: usize = 120;

pub struct Session {
    sock: UdpSocket,
    client: Client,
    pub core: ClientCore,
    server: SocketAddr,
    buf: Vec<u8>,
    /// Gaps between frames that delivered a snapshot, in ms, newest last.
    pub snapshot_gaps: Vec<f32>,
    last_snapshot: Option<Instant>,
    seen_snapshots: u64,
}

/// What a login service would hand this client: a connect token for `user`,
/// minted with the key the server was started with.
pub fn dev_token(key: &Key, server_id: u64, user: u64) -> ConnectToken {
    let unix = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let cfg = Config::default();
    ConnectToken::mint(key, cfg.protocol_id, server_id, unix + 60, user, &[0; USER_DATA_BYTES])
}

impl Session {
    pub fn connect(server: SocketAddr, token: ConnectToken, cfg: ClientConfig, now: Instant) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
        sock.connect(server)?;
        sock.set_nonblocking(true)?;
        Ok(Self {
            sock,
            client: Client::new(Config::default(), server, token, now),
            core: ClientCore::new(cfg),
            server,
            buf: vec![0; 2048],
            snapshot_gaps: Vec::with_capacity(GAPS),
            last_snapshot: None,
            seen_snapshots: 0,
        })
    }

    pub fn state(&self) -> ClientState {
        self.client.state()
    }

    pub fn transport(&self) -> &Client {
        &self.client
    }

    /// Start of a frame: everything that arrived goes to the transport, and
    /// every message it delivers to the core.
    pub fn poll(&mut self, now: Instant) -> std::io::Result<()> {
        loop {
            match self.sock.recv(&mut self.buf) {
                Ok(n) => self.client.receive(self.server, &self.buf[..n], now),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // ICMP port unreachable (no server yet) surfaces here.
                Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset) => break,
                Err(e) => return Err(e),
            }
        }
        self.client.update(now);
        if self.client.state() == ClientState::Connected {
            while let Some((_, data)) = self.client.recv() {
                self.core.on_message(&data, now);
            }
        }
        let snaps = self.core.stats.snapshots;
        if snaps > self.seen_snapshots {
            if let Some(t) = self.last_snapshot {
                if self.snapshot_gaps.len() == GAPS {
                    self.snapshot_gaps.remove(0);
                }
                self.snapshot_gaps.push((now - t).as_secs_f32() * 1000.0);
            }
            (self.last_snapshot, self.seen_snapshots) = (Some(now), snaps);
        }
        Ok(())
    }

    /// Runs the input clock with this frame's input; queues any batch.
    pub fn send_inputs(&mut self, now: Instant, input: Input) {
        if self.client.state() != ClientState::Connected {
            return;
        }
        for batch in self.core.tick_inputs(now, |_, _| input) {
            let _ = self.client.send(Channel::Unreliable, batch);
        }
    }

    /// End of a frame: packets out.
    pub fn flush(&mut self, now: Instant) {
        self.client.flush(now);
        for pkt in self.client.drain_outgoing() {
            let _ = self.sock.send(&pkt);
        }
    }

    pub fn disconnect(&mut self, now: Instant) {
        self.client.disconnect();
        self.flush(now);
    }
}

/// Stays under this long between polls when a frame stalls (the window is
/// dragged, say): the transport would otherwise see one huge gap.
pub const MAX_FRAME: Duration = Duration::from_millis(250);

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_game::movement::BUTTON_SPRINT;
    use lattice_sim::server::{SimConfig, SimServer, SpawnMode};

    /// A server on loopback, ticked from the test's own loop.
    struct LoopServer {
        sock: UdpSocket,
        sim: SimServer,
        next: Instant,
    }

    impl LoopServer {
        fn new(now: Instant) -> Self {
            let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
            sock.set_nonblocking(true).unwrap();
            let cfg = SimConfig { spawn: SpawnMode::Uniform, ..Default::default() };
            Self { sock, sim: SimServer::new(cfg, now), next: now }
        }

        fn addr(&self) -> SocketAddr {
            self.sock.local_addr().unwrap()
        }

        fn run(&mut self, now: Instant) {
            if now < self.next {
                return;
            }
            self.next += self.sim.tick_period();
            let router = self.sim.router();
            let mut inbound = vec![Vec::new(); router.shard_count()];
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = self.sock.recv_from(&mut buf) {
                inbound[router.shard(&from)].push((from, now, buf[..n].to_vec()));
            }
            let mut out = vec![Vec::new(); router.shard_count()];
            self.sim.tick(&mut inbound, now, &mut out);
            for (to, pkt) in out.into_iter().flatten() {
                let _ = self.sock.send_to(&pkt, to);
            }
        }
    }

    #[test]
    fn a_frame_loop_joins_moves_and_predicts_exactly() {
        let start = Instant::now();
        let mut server = LoopServer::new(start);
        let cfg = server.sim.config().identity.clone();
        let token = dev_token(&cfg.token_key, cfg.server_id, 42);
        let mut s = Session::connect(server.addr(), token, ClientConfig::default(), start).unwrap();
        // 120 Hz frames for 4 s of wall time, running forward.
        let frame = Duration::from_micros(8333);
        let mut next = start;
        let run = Input { move_x: 127, buttons: BUTTON_SPRINT, ..Default::default() };
        while next < start + Duration::from_secs(4) {
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let now = Instant::now();
            s.poll(now).unwrap();
            s.send_inputs(now, run);
            s.flush(now);
            server.run(now);
            next += frame;
        }
        assert_eq!(s.state(), ClientState::Connected);
        let w = *s.core.welcome().expect("welcomed");
        let st = &s.core.stats;
        assert!(st.snapshots > 60, "{} snapshots", st.snapshots);
        assert_eq!(st.corrections, 0, "prediction is exact");
        let truth = server.sim.entity_state(w.entity).unwrap();
        let moved = truth.pos[0] - w.spawn[0];
        assert!(moved > 10.0 || truth.vel[0].abs() < 0.1, "it ran: {moved} m (or hit something)");
        let (own, _) = s.core.server_own().unwrap();
        assert!((own.pos[0] - truth.pos[0]).abs() < 2.0);
        assert!(s.core.stats.render_frames == 0, "render() is the caller's");
        s.disconnect(Instant::now());
    }
}
