//! # lattice-sim
//!
//! M1 headless scale test on top of `lattice-net`: a movement-only authoritative
//! server with the full tick phase pipeline, and a bot brain with client-side
//! prediction. Both are sans-IO; the `lattice-server` and `lattice-bots` binaries
//! own the sockets.

pub mod bot;
pub mod cli;
pub mod grid;
pub mod movement;
pub mod msg;
pub mod rng;
pub mod server;
pub mod stats;
