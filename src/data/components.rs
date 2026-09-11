//! The shape of replicated state, and where a match is in its life.
//!
//! These are the components `net/protocol.rs` registers, and the sim builds
//! several of them (`MatchView`) directly. They live in `data/` rather than in
//! `net/` for that reason: the sim has to name `MatchView` and `Phase` to
//! publish its own state, and the sim must not import anything from the
//! networking side.
//!
//! Nothing here contains a transport concern. Every one of them is a plain
//! `Component + Serialize + Deserialize`, and the only bevy they need is the
//! derive.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

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

/// Marks the one [`PlayerView`] that *this process* owns, so a HUD reads an
/// identity instead of a coincidence (`found-010`, D30).
///
/// It is a local marker and deliberately not replicated: a marker is about who
/// is looking, and the server's copies of other players' views must not carry
/// it. `audit-003` and `audit-028` are written against this marker rather than
/// inventing a second one.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LocalPlayer;

/// Global match state. Replicated to everyone.
#[derive(Component, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchView {
    pub wave: u32,
    /// Live creeps on the map. Part of the lose condition (see `Phase::Over`).
    pub live_creeps: u32,
    pub overrun_cap: u32,
    pub creeps_per_wave: u32,
    /// Connected players. The HUD reads the lobby size from here rather than
    /// counting the views it happens to hold, which in host mode is every view
    /// in the match (`found-010`).
    pub players: u32,
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
