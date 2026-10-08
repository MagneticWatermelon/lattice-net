//! # lattice-sim
//!
//! M1 headless scale test on top of `lattice-net`: a movement-only authoritative
//! server with the full tick phase pipeline, and a bot brain with client-side
//! prediction. Both are sans-IO; the `lattice-server` and `lattice-bots` binaries
//! own the sockets.

pub mod activity;
pub mod bot;
pub mod debugmap;
pub mod cli;
pub mod cpus;
pub mod grid;
pub mod interest;
pub mod ladder;
pub mod pool;
pub mod rng;
pub mod server;
pub mod shots;
pub mod stats;
pub mod udp;

// The shared game rules live in `lattice-game`; re-exported so the server and
// bots keep their paths.
pub use lattice_game::{delta, movement, msg};
