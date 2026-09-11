//! What client and server both are, without knowing how they talk.
//!
//! Everything in here is *shared*: the balance tables, the shape of replicated
//! state, and the vocabulary of a refusal. None of it knows about sockets,
//! replication or which side of the connection it is on -- the transport lives
//! in `net/`, the presentation in `ui/`, and the authoritative state machine in
//! `sim/`.
//!
//! The test for whether something belongs here is whether the sim could name it
//! without importing anything from `net/`. If it could not, it does not belong
//! in `data/` either, because the sim must stay replayable with no network
//! types in scope.

pub mod balance;
pub mod components;
pub mod reject;
