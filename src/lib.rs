//! # lattice-net
//!
//! A small, dependency-free UDP transport for a large-scale shooter.
//!
//! Design goals:
//! - **Sans-IO.** `Server`, `Client` and `Connection` never touch a socket. You feed
//!   them datagrams + a timestamp and drain the datagrams they want sent. This makes
//!   them trivially testable against a simulated lossy link, and lets you shard
//!   10k connections across threads, or swap `std::net::UdpSocket` for
//!   `recvmmsg`/`sendmmsg`, GSO or AF_XDP, without touching protocol code.
//! - **Every packet acks the last 33 packets** (Gaffer-style `ack` + 32-bit `ack_bits`).
//!   Acks are redundant, so reliability doesn't need separate ack packets.
//! - **Two channels:** `Unreliable` (latest-wins state, dropped if it doesn't fit this
//!   tick) and `Reliable` (ordered, exactly-once, resent until acked).
//! - **Sharded server.** Connections are partitioned by a keyed hash of the peer
//!   address into `Shard`s that share nothing mutable but an atomic client count,
//!   so a caller's thread pool can receive and flush them in parallel.
//! - **Stateless, amplification-safe handshake.** Client->server handshake packets are
//!   padded larger than the server's replies. The server keeps no state until the client
//!   echoes a keyed cookie bound to its address.
//!
//! Not included yet (see README): encryption, fragmentation of >MTU messages, and
//! congestion / bandwidth budgeting.

pub mod bitpack;
mod channel;
mod client;
mod connection;
pub mod packet;
pub mod seq;
mod server;
pub mod token;
pub mod wire;

pub use client::{Client, ClientState};
pub use connection::{Channel, Config, Connection, SendError, Stats};
pub use packet::DenyReason;
pub use server::{ClientId, DisconnectReason, Router, Server, ServerEvent, Shard};
pub use token::{ConnectToken, TokenContents, TokenError, TokenOpener};
