//! Shared protocol: the contract between client and server.
//!
//! This must be identical on both peers and must be installed *after* the
//! Client/Server plugin groups but *before* any client or server entity is
//! spawned. Getting this wrong is the most common lightyear setup error.
//!
//! # The protocol version (`net-001`, D22)
//!
//! Before [`found-004`](crate::data::balance) every balance number was compiled
//! in, so two peers built from the same source necessarily agreed about the
//! rules. They are data now, and a client built from a different balance
//! directory would render tooltips that lie and desync silently the first time
//! it acted on one. So the peers exchange a [`ProtocolVersion`] in a
//! [`Handshake`] before either of them treats the other as a player, and a
//! mismatch ends the connection with a reason that names the part that
//! differed.
//!
//! The version is two numbers, because a mismatch has two very different
//! causes and a player deserves to be told which one they have:
//!
//! * [`ProtocolVersion::schema`] is a wire-format revision bumped by hand
//!   whenever a message, a component or a channel changes shape.
//! * [`ProtocolVersion::balance_hash`] is
//!   [`BalanceData::hash`](crate::data::balance::BalanceData::hash), so it
//!   changes when a number the client is allowed to display changes -- and does
//!   not change when the files merely gain comments or reorder.
//!
//! It is computed **once**, in [`build_protocol`], and kept as a resource, so
//! both halves of one process and the two halves of a connection can never
//! disagree about what they speak.

use std::fmt;

use bevy::prelude::*;
use lightyear::prelude::*;
use serde::{Deserialize, Serialize};

use crate::data::balance::Balance;
use crate::data::components::*;
use crate::map::Map;
use crate::net::messages::*;

/// The wire-format revision this build speaks (`net-001`).
///
/// Bump it by hand when the *shape* of the protocol changes: a message added,
/// removed or reordered, a replicated component added or removed, a channel's
/// mode changed. It is deliberately not derived from the crate version, because
/// a release that changes only rendering must not invalidate a peer.
///
/// Revision 1 was the vertical slice's implicit format -- one message per
/// direction on one reliable channel, six replicated components. Revision 2 is
/// the handshake that makes the revision checkable (`net-001`). Revision 3 adds
/// the client identity to that handshake (`net-002`), which is a wire change
/// even though the message count is unchanged. Revision 4 replaces the closed
/// ring with a real board of lanes ending at a goal: `MatchView` gains the lives
/// and the leak count, and `ServerNotice::GameOver` reports lives instead of a
/// live-creep count, so an older client would mis-read the match it is watching.
pub const SCHEMA_REVISION: u32 = 4;

/// What a peer speaks, exchanged in [`Handshake`] before it is admitted.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Resource)]
pub struct ProtocolVersion {
    /// The wire-format revision ([`SCHEMA_REVISION`]).
    pub schema: u32,
    /// [`BalanceData::hash`](crate::data::balance::BalanceData::hash) of the
    /// tables this peer loaded.
    pub balance_hash: u64,
    /// [`MapData::hash`](crate::map::MapData::hash) of the board this peer
    /// loaded.
    ///
    /// The board is rules, not decoration: it decides where creeps enter, how
    /// long their walk is and where the goal is. Two peers on different boards
    /// would disagree about every leak, so a mismatch is refused exactly as a
    /// balance mismatch is -- and a client that is drawing a board the server is
    /// not playing is the one thing a "dumb client" must never do.
    pub map_hash: u64,
}

impl ProtocolVersion {
    /// The version of *this* process, from the tables and the board it loaded.
    pub fn current(balance: &Balance, map: &Map) -> Self {
        Self {
            schema: SCHEMA_REVISION,
            balance_hash: balance.0.hash(),
            map_hash: map.0.hash(),
        }
    }

    /// `None` when two peers agree, otherwise a player-readable reason naming
    /// every part that differed.
    ///
    /// Both parts are reported when both differ, because the two have different
    /// fixes -- a schema bump means "upgrade the client", a balance mismatch
    /// means "we are not playing the same numbers" -- and a player who is told
    /// only the first will fix the wrong thing.
    pub fn disagreement(&self, peer: &Self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.schema != peer.schema {
            parts.push(format!(
                "schema revision (server {ours}, client {theirs})",
                ours = self.schema,
                theirs = peer.schema
            ));
        }
        if self.balance_hash != peer.balance_hash {
            parts.push(format!(
                "balance data (server {ours:016x}, client {theirs:016x})",
                ours = self.balance_hash,
                theirs = peer.balance_hash
            ));
        }
        if self.map_hash != peer.map_hash {
            parts.push(format!(
                "map data (server {ours:016x}, client {theirs:016x})",
                ours = self.map_hash,
                theirs = peer.map_hash
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "v{} (balance {:016x}, map {:016x})",
            self.schema, self.balance_hash, self.map_hash
        )
    }
}

pub fn build_protocol(app: &mut App) {
    // The version is a property of the tables this process loaded, and the
    // tables are a resource by the time this runs. Computing it here, once, is
    // what makes "both peers agree" checkable rather than hopeful.
    let version = ProtocolVersion::current(
        app.world().resource::<Balance>(),
        app.world().resource::<Map>(),
    );
    app.insert_resource(version);

    // --- messages -----------------------------------------------------------
    // Intents go up, feedback comes down. The client cannot send anything that
    // mutates authoritative state directly.
    app.register_message::<ClientCmd>()
        .add_direction(NetworkDirection::ClientToServer);
    app.register_message::<ServerNotice>()
        .add_direction(NetworkDirection::ServerToClient);
    // The handshake is the one thing a peer says before it is a player, so it
    // is its own message rather than a `ClientCmd` variant: a peer that has not
    // been admitted must not be able to reach the intent path at all.
    app.register_message::<Handshake>()
        .add_direction(NetworkDirection::ClientToServer);
    app.register_message::<HandshakeAck>()
        .add_direction(NetworkDirection::ServerToClient);

    // --- channels -----------------------------------------------------------
    // One reliable ordered channel is plenty here: commands are small and rare
    // relative to the replication traffic, which lightyear handles separately.
    app.add_channel::<ReliableChannel>(ChannelSettings {
        mode: ChannelMode::OrderedReliable(ReliableSettings::default()),
        ..default()
    })
    .add_direction(NetworkDirection::Bidirectional);

    // --- replicated components ---------------------------------------------
    app.component::<CreepVis>().replicate();
    app.component::<CreepHp>().replicate();
    app.component::<TowerAt>().replicate();
    app.component::<TowerVis>().replicate();
    app.component::<PlayerView>().replicate();
    app.component::<MatchView>().replicate();
}
