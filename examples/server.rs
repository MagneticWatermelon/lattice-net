//! Real-UDP server loop: fixed 30 Hz tick, echoes every message back.
//!   cargo run --release --example server [bind_addr]
//!
//! Production notes: swap the recv loop for recvmmsg/sendmmsg (or AF_XDP), shard
//! clients across threads with SO_REUSEPORT, and move send/recv to dedicated
//! network threads that hand datagrams to the sim thread through ring buffers.

use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use lattice_net::{Config, Server, ServerEvent};

fn main() -> std::io::Result<()> {
    let bind = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:40000".into());
    let socket = UdpSocket::bind(&bind)?;
    socket.set_nonblocking(true)?;
    println!("listening on {bind}");

    let tick = Duration::from_micros(33_333);
    let mut server = Server::new(Config::default(), 10_000, Instant::now());
    let mut buf = [0u8; 1500];
    let mut next_tick = Instant::now();
    let mut ticks = 0u64;

    loop {
        let now = Instant::now();

        // 1. ingress: drain the socket
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => server.receive(from, &buf[..n], now),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => eprintln!("recv error: {e}"),
            }
        }

        // 2. game logic (here: echo)
        while let Some(ev) = server.poll_event() {
            match ev {
                ServerEvent::Connected { client, addr } => println!("+ client {client} from {addr}"),
                ServerEvent::Disconnected { client, reason } => println!("- client {client}: {reason:?}"),
                ServerEvent::Message { client, channel, data } => {
                    let _ = server.send(client, channel, data);
                }
            }
        }

        // 3. timeouts + build packets
        server.update(now);
        server.flush(now);

        // 4. egress
        for (addr, pkt) in server.drain_outgoing() {
            if let Err(e) = socket.send_to(&pkt, addr) {
                eprintln!("send error to {addr}: {e}");
            }
        }

        ticks += 1;
        if ticks.is_multiple_of(150) {
            for id in server.client_ids() {
                let s = server.client_stats(id).unwrap();
                println!(
                    "  client {id}: rtt {:.1} ms, loss {:.1}%, sent {} pkts / {} B, recv {} pkts",
                    s.rtt_ms, s.loss * 100.0, s.packets_sent, s.bytes_sent, s.packets_received
                );
            }
        }

        next_tick += tick;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now; // overran the tick budget
        }
    }
}
