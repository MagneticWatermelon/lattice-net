//! # lattice-game
//!
//! The game rules the server and every client run, bit for bit: movement
//! (`movement`), the game's message formats (`msg`, and the near tier's delta
//! codec in `delta`), and the tiers they speak of (`tier`). The server
//! (`lattice-sim`), the bots and the client all depend on this crate, so there
//! is exactly one copy of anything that must agree on both sides.

pub mod activity;
pub mod delta;
pub mod events;
pub mod faction;
pub mod hit;
pub mod movement;
pub mod msg;
pub mod tier;
pub mod weapon;
pub mod world;
