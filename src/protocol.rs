//! Shared protocol: the contract between client and server.
//!
//! This must be identical on both peers and must be installed *after* the
//! Client/Server plugin groups but *before* any client or server entity is
//! spawned. Getting this wrong is the most common lightyear setup error.

use bevy::prelude::*;
use lightyear::prelude::*;

use crate::game::*;

pub fn build_protocol(app: &mut App) {
    // --- messages -----------------------------------------------------------
    // Intents go up, feedback comes down. The client cannot send anything that
    // mutates authoritative state directly.
    app.register_message::<ClientCmd>()
        .add_direction(NetworkDirection::ClientToServer);
    app.register_message::<ServerNotice>()
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
