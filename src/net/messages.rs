//! Messages: the two directions of the wire, and the channel they use.
//!
//! The split is deliberate and absolute. A [`ClientCmd`] carries *intent* and
//! nothing else -- a cell, a kind -- and the server re-derives every consequence
//! of it. A [`ServerNotice`] carries *outcome*: what happened, or why it did
//! not. A client that sends state rather than intent is a client that can cheat,
//! which is what `net-003` exists to keep true.

use serde::{Deserialize, Serialize};

use crate::data::reject::Reject;

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
    GameOver { wave: u32, live: u32 },
}

/// Reliable, ordered. Used for commands and notices.
pub struct ReliableChannel;
