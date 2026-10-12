//! # lattice-client-ffi
//!
//! The client stack (`lattice-net`, `lattice-game`, `lattice-client-core`)
//! as a native plugin for the Unity client (GDD §12): a C ABI of `lt_*`
//! functions (`api`) over blittable structs (`abi`). The engine draws, plays
//! sound, reads input and shows UI; everything that touches the wire, the
//! prediction or a hit stays here, the same code the server is checked
//! against.
//!
//! - `session`: a UDP socket, the transport and a `ClientCore`, pumped once
//!   per frame. The Bevy client shares it (this crate is also an rlib).
//! - `handle`: a session plus its frame clock, tracers and buffers.
//! - `net_thread`: the session run on a plugin thread (U3), so the caller's
//!   stalled frames cost no inputs.
//! - `api`: the C functions; `abi`: the structs and codes they use.
//! - `csharp`: the C# bindings, generated from `abi` and `api`'s own records
//!   of their layouts and signatures (`cargo run -p lattice-client-ffi --bin
//!   gen-cs`), with the game's constants. `lt_version` and `lt_layout_hash`
//!   let the C# side refuse a plugin that doesn't match its bindings.

pub mod abi;
pub mod api;
pub mod csharp;
pub mod handle;
pub mod net_thread;
pub mod session;

#[cfg(test)]
mod tests;
