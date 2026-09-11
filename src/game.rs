//! Types shared between client and server.
//!
//! Rule of thumb: anything in here must be *replicable* (Component + Serialize)
//! or a *message*. The authoritative simulation itself lives in `sim.rs` and is
//! only ever compiled into the server.
//!
//! No tuned numbers live here. Every cost, range, curve and timer comes from the
//! balance tables in `balance.rs` (see `tasks/00-foundation.org` `found-004`).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Replicated components
// ---------------------------------------------------------------------------

/// A creep's world position. This is the only thing the client needs to draw it.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct CreepVis {
    pub x: f32,
    pub y: f32,
}

/// A creep's health, for the health bar.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct CreepHp {
    pub hp: f32,
    pub max: f32,
}

/// Which grid cell a tower occupies.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct TowerAt {
    pub x: i32,
    pub y: i32,
}

/// A tower's type and tier, for choosing the sprite/label.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct TowerVis {
    pub kind: u8,
    pub level: u8,
}

/// Per-player view. Replicated to exactly one client.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct PlayerView {
    pub gold: u32,
    pub kills: u32,
}

/// Global match state. Replicated to everyone.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchView {
    pub wave: u32,
    /// Live creeps on the map. Part of the lose condition (see `Phase::Over`).
    pub live_creeps: u32,
    pub overrun_cap: u32,
    pub creeps_per_wave: u32,
    /// A [`Phase`] as a byte, because that is what goes on the wire.
    pub phase: u8,
}

/// Where a match is in its life. Driven by the server from the sim, so the HUD
/// never has to guess from `wave == 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Waiting = 0,
    InMatch = 1,
    Over = 2,
}

impl Phase {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Unknown bytes read as `Waiting` rather than panicking: a newer server
    /// must not crash an older client.
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Phase::InMatch,
            2 => Phase::Over,
            _ => Phase::Waiting,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Phase::Waiting => "waiting",
            Phase::InMatch => "in match",
            Phase::Over => "over",
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Client -> server intents. The client never mutates the world directly; it
/// only asks. The server decides.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientCmd {
    /// Try to place a tower. `kind` indexes `tower_stats`.
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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    NotEnoughGold,
    Occupied,
    PathBlocked,
    OutOfBounds,
    BadKind,
    NoSuchTower,
    /// The caller does not own the tower it tried to upgrade or sell (D1/D2).
    NotOwner,
    /// The caller is issuing commands faster than the server will accept (D6/D7).
    RateLimited,
    /// The caller has no player record, so it is not in this match (D8).
    NotInMatch,
    MatchOver,
}

impl Reject {
    /// Every variant, so a test can prove none of them is unhandled.
    pub const ALL: [Reject; 10] = [
        Reject::NotEnoughGold,
        Reject::Occupied,
        Reject::PathBlocked,
        Reject::OutOfBounds,
        Reject::BadKind,
        Reject::NoSuchTower,
        Reject::NotOwner,
        Reject::RateLimited,
        Reject::NotInMatch,
        Reject::MatchOver,
    ];

    pub fn text(self) -> &'static str {
        match self {
            Reject::NotEnoughGold => "not enough gold",
            Reject::Occupied => "cell occupied",
            Reject::PathBlocked => "cannot build on the path",
            Reject::OutOfBounds => "out of bounds",
            Reject::BadKind => "unknown tower type",
            Reject::NoSuchTower => "no tower there",
            Reject::NotOwner => "that tower is not yours",
            Reject::RateLimited => "too many commands",
            Reject::NotInMatch => "you are not in this match",
            Reject::MatchOver => "match is over",
        }
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// Reliable, ordered. Used for commands and notices.
pub struct ReliableChannel;

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

/// Tower kind -> sprite colour. Presentation only, so it stays in code rather
/// than in the balance tables; `11-content.org` owns the real art.
pub fn tower_color(kind: u8) -> Color {
    match kind {
        1 => Color::srgb(0.85, 0.45, 0.15),
        2 => Color::srgb(0.35, 0.65, 0.95),
        _ => Color::srgb(0.90, 0.85, 0.30),
    }
}
