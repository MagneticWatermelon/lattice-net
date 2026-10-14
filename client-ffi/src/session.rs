//! A client's connection: a UDP socket, the `lattice_net` transport and the
//! client core, driven once per frame. Plain Rust, no engine, so it's tested
//! against a real server loop (`tests` below). The Unity plugin (`api`) and
//! the Bevy client both run it.

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime};

use lattice_client_core::{ClientConfig, ClientCore};
use lattice_game::movement::Input;
use lattice_net::token::{Key, USER_DATA_BYTES};
use lattice_net::{Channel, Client, ClientState, Config, ConnectToken};

/// Snapshot arrival gaps kept for the net graph.
pub const GAPS: usize = 120;

/// Stays under this long between polls when a frame stalls (the window is
/// dragged, say): the transport would otherwise see one huge gap.
pub const MAX_FRAME: Duration = Duration::from_millis(250);

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

    /// Queues a message on the reliable channel (a Deploy); false if we
    /// aren't connected.
    pub fn send_reliable(&mut self, data: Vec<u8>) -> bool {
        self.client.state() == ClientState::Connected && self.client.send(Channel::Reliable, data).is_ok()
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

    /// The connection's part of a playtester's report (`lattice-report.txt`):
    /// transport, prediction, smoothness per tier, combat and the render
    /// clock. A client adds its own frame times above it.
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let st = &self.core.stats;
        let mut out = String::new();
        if let Some(t) = self.transport().stats() {
            let _ = writeln!(
                out,
                "  rtt {:.1} ms (last seconds {:.1}-{:.1}) | loss {:.2}% | packets sent {} received {} lost {}",
                t.rtt_ms,
                t.rtt_min_ms,
                t.rtt_max_ms,
                t.loss * 100.0,
                t.packets_sent,
                t.packets_received,
                t.packets_lost
            );
        }
        let _ = writeln!(
            out,
            "  snapshots {} | corrections {} (largest {:.3} m) | push corrections {} | resyncs {} | own correction offset largest {:.3} m | shots {} | deaths/respawns {}",
            st.snapshots, st.corrections, st.correction_error_max, st.push_corrections, st.resyncs, st.own_offset_max, st.shots, st.life_events
        );
        if let Some(e) = self.core.entities() {
            for (t, name) in ["near", "mid", "far"].iter().enumerate() {
                let f = e.smooth.frames[t];
                let n = f.iter().sum::<u64>().max(1) as f64;
                let _ = writeln!(
                    out,
                    "  {name}: {} entity-frames, {:.2}% interpolated, {:.2}% extrapolated, {:.2}% held, {:.2}% new",
                    f.iter().sum::<u64>(),
                    100.0 * f[0] as f64 / n,
                    100.0 * f[1] as f64 / n,
                    100.0 * f[2] as f64 / n,
                    100.0 * f[3] as f64 / n
                );
            }
        }
        let _ = writeln!(
            out,
            "  hits confirmed {} (kills {}) | hit {} times for {} damage | kills heard {} | others' shots seen {} | distant fights: {} cells, {} shots, {} ambient",
            st.hits_confirmed, st.kills_confirmed, st.hurts, st.damage_taken, st.kills_heard, st.shots_seen, st.activity_cells, st.activity_shots, st.ambient
        );
        let delay = st.render_delay_sum / st.render_frames.max(1) as f64 * 1000.0 / 30.0;
        let mid = self.core.entities().map_or(0.0, |e| e.mid_lag()) * 1000.0 / 30.0;
        let _ = writeln!(out, "  render delay near {delay:.1} ms, mid/far +{mid:.0} ms, clock snaps {}", self.core.render_clock().snaps);
        out
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC` for unix seconds (Howard Hinnant's
/// civil_from_days; no time zone data). For report headers.
pub fn utc(unix: u64) -> String {
    let (days, secs) = ((unix / 86_400) as i64, unix % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", secs / 3600, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lattice_game::movement::BUTTON_SPRINT;
    use lattice_sim::server::{SimConfig, SimServer, SpawnMode};

    /// A server on loopback, ticked from the test's own loop.
    pub(crate) struct LoopServer {
        sock: UdpSocket,
        pub(crate) sim: SimServer,
        next: Instant,
    }

    impl LoopServer {
        pub(crate) fn new(now: Instant) -> Self {
            Self::with(SimConfig { spawn: SpawnMode::Uniform, ..Default::default() }, now)
        }

        pub(crate) fn with(cfg: SimConfig, now: Instant) -> Self {
            let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
            sock.set_nonblocking(true).unwrap();
            Self { sock, sim: SimServer::new(cfg, now), next: now }
        }

        pub(crate) fn addr(&self) -> SocketAddr {
            self.sock.local_addr().unwrap()
        }

        pub(crate) fn run(&mut self, now: Instant) {
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
        assert!(s.summary().contains("corrections 0"), "{}", s.summary());
        s.disconnect(Instant::now());
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(1_791_552_658), "2026-10-09 13:30:58 UTC");
        assert_eq!(utc(4_102_444_800), "2100-01-01 00:00:00 UTC");
    }
}
