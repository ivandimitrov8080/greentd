//! Everything a peer with a window does: the board, the creeps on it, and the
//! text at the top of the screen.
//!
//! `ui/` only ever *reads* replicated or authoritative state and attaches local
//! rendering components. Nothing here is registered for replication, so a sprite
//! never travels over the wire, and nothing here can change the outcome of a
//! match.

pub mod hud;
pub mod visuals;
