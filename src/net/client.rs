//! The client's link: bind a socket, join a host, own one view of the match.
//!
//! There is no simulation, no prediction and no authority on this side. It sends
//! intents, receives replicated components, and hands them to `ui/`. Because a TD
//! has no twitch input, that latency model is entirely acceptable -- and it
//! removes every determinism problem that a per-peer sim would introduce
//! (`decide-authority` in `tasks/README.org`).

use bevy::prelude::*;
use lightyear::prelude::client::*;
use lightyear::prelude::*;

use crate::config::Config;
use crate::data::components::{LocalPlayer, PlayerView};
use crate::net::messages::ServerNotice;
use crate::ui::hud::HudPlugin;

fn spawn_client(mut commands: Commands, cfg: Res<Config>) {
    let client = commands
        .spawn((
            Name::new("Client"),
            RawClient,
            // The IO layer. Without this the link never binds a socket and
            // nothing is ever sent -- `Connect` would silently do nothing.
            UdpIo::default(),
            LocalAddr(cfg.bind_addr),
            PeerAddr(cfg.server_addr),
            Link::default(),
            ReplicationReceiver,
            // Inserted eagerly so we never miss the first notice while the
            // receiver component is being created lazily by the message layer.
            MessageReceiver::<ServerNotice>::default(),
        ))
        .id();
    commands.trigger(Connect { entity: client });
}

/// Mark the view a *remote* client was sent, so its HUD reads an identity
/// rather than a coincidence (`found-010`, D30).
///
/// The server addresses each peer's view with `NetworkTarget::Single`, so a
/// remote client holds exactly one `PlayerView` and it is necessarily its own.
/// A host holds every view in the match (D3) and nothing distinguishes them, so
/// it does not guess here: `net::server::mark_host_view`, which owns the
/// key-to-view mapping, marks the right one instead.
fn mark_local_view(
    config: Res<Config>,
    marked: Query<(), With<LocalPlayer>>,
    views: Query<Entity, With<PlayerView>>,
    mut commands: Commands,
) {
    if config.mode.is_host() || !marked.is_empty() {
        return;
    }
    if let Some(view) = views.iter().next() {
        commands.entity(view).insert(LocalPlayer);
    }
}

/// A client: the link, and the HUD that reads it.
///
/// The two are separate modules because they fail differently -- a link that
/// will not connect is a network problem, a HUD that shows nothing is a
/// presentation problem -- but a client is not useful without both, so this is
/// the plugin `main` adds and `HudPlugin` is an implementation detail of it.
pub struct GreenTdClientPlugin;

impl Plugin for GreenTdClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(HudPlugin)
            .add_systems(Startup, spawn_client)
            .add_systems(Update, mark_local_view);
    }
}
