//! Messages: the two directions of the wire, and the channel they use.
//!
//! The split is deliberate and absolute. A [`ClientCmd`] carries *intent* and
//! nothing else -- a cell, a kind -- and the server re-derives every consequence
//! of it. A [`ServerNotice`] carries *outcome*: what happened, or why it did
//! not. A client that sends state rather than intent is a client that can cheat,
//! which is what `net-003` exists to keep true.

use serde::{Deserialize, Serialize};

use crate::data::reject::Reject;
use crate::net::protocol::ProtocolVersion;

/// A client-generated identity, stable for the life of the client process and
/// therefore across a reconnect from a new socket (`net-002`, D9).
///
/// It is deliberately *not* the UDP address. An address changes the moment a
/// socket is rebound, and the whole point of this type is that a player's gold,
/// kills and towers do not change with it. The server keys the player on `id`,
/// so the same `id` from a new port is the same player and a different `id` from
/// the same port is a different one.
///
/// There are no accounts, so the id lives in memory and is not persisted: a
/// fresh process is a fresh player. That is the compromise raw UDP forces, made
/// explicit rather than accidental -- the previous design had the same property
/// only because a *port*, not a process, defined a player. See the open question
/// on session lifetime in `tasks/09-networking.org`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PlayerId(pub u64);

impl PlayerId {
    /// A fresh identity for this process.
    ///
    /// [`RandomState`](std::collections::hash_map::RandomState) is the standard
    /// library's OS-seeded hasher, so two processes -- and two clients started on
    /// one machine -- almost surely differ. Sixteen bytes of entropy do not
    /// warrant a `rand` dependency in a crate that hand-rolls its own RNG
    /// (`found-009`) rather than pull one in.
    ///
    /// Zero is avoided because the sim reserves `PlayerKey(0)` for "no player":
    /// it is what [`Creep::last_hit_by`](crate::sim::Creep::last_hit_by) starts
    /// as, so a real player keyed 0 would be credited with creeps nobody hit.
    pub fn generate() -> Self {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};

        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(0);
        let id = hasher.finish();
        Self(if id == 0 { 1 } else { id })
    }
}

/// Client -> server, once, before anything else: which protocol it speaks and
/// who it is.
///
/// It carries no intent, so a server may act on it before the sender is a
/// player. A peer that never sends one is never admitted, which is what lets the
/// server keep "connected" and "in the match" separate (`D22`, `net-001`).
///
/// The identity (`net-002`) is stated here rather than on the first
/// [`ClientCmd`] because admission is where it is decided: a peer whose identity
/// is refused must have a `HandshakeAck` to hear and must never have been a
/// player, and both of those are true of the handshake and not of the intent
/// path.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Handshake {
    pub version: ProtocolVersion,
    /// Who the sender claims to be. The server keys the player on this, not on
    /// the sender's socket address, so a reconnect from a new port is the same
    /// player and two peers cannot silently become one.
    pub id: PlayerId,
}

/// Server -> client: the answer to a [`Handshake`].
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum HandshakeAck {
    /// The versions agree and the sender is now a player in the match.
    Accepted,
    /// The versions disagree, so the sender is not admitted. The reason names
    /// the part of the version that differed (`net-001`).
    Refused { reason: String },
}

/// Client -> server intents. The client never mutates the world directly; it
/// only asks. The server decides.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientCmd {
    /// Try to place a tower. `kind` indexes the tower table.
    Build {
        x: i32,
        y: i32,
        kind: u8,
    },
    Upgrade {
        x: i32,
        y: i32,
    },
    Sell {
        x: i32,
        y: i32,
    },
    /// Skip the rest of the wave timer.
    CallWave,
}

/// Server -> client feedback, sent only to the offending player.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ServerNotice {
    Ok,
    Err(Reject),
    WaveStarted(u32),
    GameOver { wave: u32, lives: u32, leaks: u32 },
}

/// Reliable, ordered. Used for commands and notices.
pub struct ReliableChannel;
