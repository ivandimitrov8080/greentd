//! The transport, and the two peers that use it.
//!
//! `net/` is the only place in the crate that names `lightyear`. It knows how a
//! peer connects, how state reaches it, and how an intent gets back; it does not
//! know the rules of the game, which are in `sim/`, or how anything looks, which
//! is in `ui/`.
//!
//! Both peers register the same `protocol.rs`, which is what makes them agree on
//! the wire. Nothing else is shared: the server owns a `Sim`, and a client owns
//! a mirror of the entities that `Sim` produced.

pub mod client;
pub mod messages;
pub mod protocol;
pub mod server;
