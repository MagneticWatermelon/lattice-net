//! Real-UDP client: sends 60 Hz unreliable "inputs" plus a reliable chat line
//! every second, and prints the server's echoes.
//!   cargo run --release --example client [server_addr] [seconds]

use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use lattice_net::{Channel, Client, ClientState, Config};

fn main() -> std::io::Result<()> {
    let server: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:40000".into())
        .parse()
        .expect("server addr");
    let seconds: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5);

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_nonblocking(true)?;

    let start = Instant::now();
    let mut client = Client::new(Config::default(), server, start);
    let tick = Duration::from_micros(16_667);
    let mut buf = [0u8; 1500];
    let mut frame = 0u32;
    let (mut inputs_echoed, mut chats_echoed) = (0u32, 0u32);
    let mut was_connected = false;

    while start.elapsed() < Duration::from_secs(seconds) {
        let now = Instant::now();
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => client.receive(from, &buf[..n], now),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // Windows turns an ICMP port-unreachable (e.g. server not up yet)
                // into WSAECONNRESET on the next recv. It's not fatal for UDP.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(e) => return Err(e),
            }
        }
        client.update(now);

        match client.state() {
            ClientState::Connected => {
                if !was_connected {
                    println!("connected as client {} after {:?}", client.client_id().unwrap(), start.elapsed());
                    was_connected = true;
                }
                // Input: frame number + 3 redundant prior inputs would go here.
                client.send(Channel::Unreliable, frame.to_le_bytes().to_vec()).unwrap();
                if frame.is_multiple_of(60) {
                    client.send(Channel::Reliable, format!("chat {}", frame / 60).into_bytes()).unwrap();
                }
                frame += 1;
            }
            ClientState::Connecting => {}
            other => {
                println!("connection ended: {other:?}");
                break;
            }
        }

        while let Some((ch, data)) = client.recv() {
            match ch {
                Channel::Unreliable => inputs_echoed += 1,
                Channel::Reliable => {
                    chats_echoed += 1;
                    println!("reliable echo: {}", String::from_utf8_lossy(&data));
                }
            }
        }

        client.flush(now);
        for pkt in client.drain_outgoing() {
            socket.send_to(&pkt, server)?;
        }
        std::thread::sleep(tick.saturating_sub(now.elapsed()));
    }

    if let Some(s) = client.stats() {
        println!(
            "done: {frame} inputs sent, {inputs_echoed} echoed, {chats_echoed} chats echoed | rtt {:.2} ms, {} pkts sent, {} B sent",
            s.rtt_ms, s.packets_sent, s.bytes_sent
        );
    }
    client.disconnect();
    for pkt in client.drain_outgoing() {
        socket.send_to(&pkt, server)?;
    }
    Ok(())
}
