//! The client's connection: a UDP socket, the transport and the client core,
//! driven once per frame. It lives in `client-ffi` (`lattice_client::session`),
//! shared with the Unity plugin, and is tested there against a real server
//! loop.

pub use lattice_ffi::session::{dev_token, utc, Session, GAPS, MAX_FRAME};
