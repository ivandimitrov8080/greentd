//! Types shared between client and server.
//!
//! Rule of thumb: anything in here must be *replicable* (Component + Serialize)
//! or a *message*. The authoritative simulation itself lives in `sim.rs` and is
//! only ever compiled into the server.

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
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct MatchView {
    pub wave: u32,
    /// Live creeps on the map. **This is the lose condition.**
    pub live_creeps: u32,
    pub overrun_cap: u32,
    pub creeps_per_wave: u32,
    pub phase: u8,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Client -> server intents. The client never mutates the world directly; it
/// only asks. The server decides.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientCmd {
    /// Try to place a tower. `kind` indexes `tower_stats`.
    Build { x: i32, y: i32, kind: u8 },
    Upgrade { x: i32, y: i32 },
    Sell { x: i32, y: i32 },
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
    MatchOver,
}

impl Reject {
    pub fn text(self) -> &'static str {
        match self {
            Reject::NotEnoughGold => "not enough gold",
            Reject::Occupied => "cell occupied",
            Reject::PathBlocked => "cannot build on the path",
            Reject::OutOfBounds => "out of bounds",
            Reject::BadKind => "unknown tower type",
            Reject::NoSuchTower => "no tower there",
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
// Game data
// ---------------------------------------------------------------------------

pub const START_GOLD: u32 = 300;

/// Green TD's real lose condition: too many creeps alive at once. Creeps loop
/// forever, so this only ever goes down by killing them.
pub const OVERRUN_CAP: u32 = 150;

pub const WAVE_INTERVAL: f32 = 20.0;

/// Tower kinds. `0` basic, `1` cannon, `2` frost.
pub struct TowerStats {
    pub name: &'static str,
    pub cost: u32,
    pub range: f32,
    pub damage: f32,
    /// Seconds between shots.
    pub cooldown: f32,
    /// Splash radius in world units (0.0 = single target).
    pub splash: f32,
    /// Speed multiplier applied to hit creeps (1.0 = no slow).
    pub slow: f32,
}

pub fn tower_stats(kind: u8) -> TowerStats {
    match kind {
        1 => TowerStats {
            name: "Cannon",
            cost: 150,
            range: 130.0,
            damage: 26.0,
            cooldown: 1.1,
            splash: 48.0,
            slow: 1.0,
        },
        2 => TowerStats {
            name: "Frost",
            cost: 120,
            range: 140.0,
            damage: 7.0,
            cooldown: 0.8,
            splash: 0.0,
            slow: 0.55,
        },
        // Basic
        _ => TowerStats {
            name: "Basic",
            cost: 80,
            range: 115.0,
            damage: 14.0,
            cooldown: 0.55,
            splash: 0.0,
            slow: 1.0,
        },
    }
}

pub const TOWER_KIND_COUNT: u8 = 3;

pub fn tower_color(kind: u8) -> Color {
    match kind {
        1 => Color::srgb(0.85, 0.45, 0.15),
        2 => Color::srgb(0.35, 0.65, 0.95),
        _ => Color::srgb(0.90, 0.85, 0.30),
    }
}
