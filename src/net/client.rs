//! The client's link: bind a socket, join a host, own one view of the match.
//!
//! There is no simulation, no prediction and no authority on this side. It sends
//! intents, receives replicated components, and hands them to `ui/`. Because a TD
//! has no twitch input, that latency model is entirely acceptable -- and it
//! removes every determinism problem that a per-peer sim would introduce
//! (`decide-authority` in `tasks/README.org`).
//!
//! # Two kinds of client, and why they are not the same thing
//!
//! A **remote** client is the obvious one: a socket, a server address, a
//! replicated view of the match. It is what `cargo run -- client` starts.
//!
//! A **host** client -- `cargo run`, the default -- is a client that shares the
//! server's `World`. It owns no socket and receives no replication, because the
//! authoritative entities it would be sent a copy of are *already in its world*.
//! Lightyear spells this with `HostClient`
//! (`lightyear::connection::host::HostClient`, deliberately not in the prelude),
//! which `ServerPlugins` recognises
//! and promotes: no `UdpIo`, no `PeerAddr`, no bytes, and -- crucially -- no
//! `Remote` markers and no client-side replication receive.
//!
//! That last part is not an optimisation. Running a conventional
//! `RawClient`-over-UDP in the same `App` as a started `RawServer` is a topology
//! lightyear calls invalid (`MixedClientServer`), and in practice it drives
//! `bevy_replicon`'s client receive path and its server path at the same time:
//! the client path removes `ServerEntityMap` and `ReplicationRegistry` for the
//! duration of its system, and the server path then panics with `registry
//! should always exist on the server`. Host mode used to do exactly that and
//! died about three seconds into the first wave. See D31.

use bevy::prelude::*;
use lightyear::prelude::client::*;
use lightyear::prelude::*;

use crate::config::Config;
use crate::data::components::{LocalPlayer, PlayerView};
use crate::net::messages::{ClientCmd, ServerNotice};
use crate::ui::hud::HudPlugin;

/// Spawn this process's client half, once the world it needs is ready.
///
/// It runs in `Update` rather than `Startup` for one reason: a host client needs
/// the entity of the `Server` it belongs to (that is what `LinkOf` is), and the
/// server entity is spawned by `GreenTdServerPlugin` in `Startup`. Adding both
/// plugins to one `App` does not order their `Startup` systems against each
/// other, so the server may not exist the first time this system runs. The
/// `Local<bool>` latch makes the retry a no-op after it succeeds.
fn spawn_client(
    mut done: Local<bool>,
    config: Res<Config>,
    servers: Query<Entity, With<Server>>,
    mut commands: Commands,
) {
    if *done {
        return;
    }

    if config.mode.is_host() {
        // A host client: no IO, no address, no `Connect` to a socket. From here
        // on `HostPlugin` owns the lifecycle -- it inserts `Connected`,
        // `LocalId`, `RemoteId`, `ClientOf` and `HostClient` together, and parks
        // the client in `Connecting` if the server has not started yet.
        let Ok(server) = servers.single() else {
            return;
        };
        let client = commands
            .spawn((
                Name::new("HostClient"),
                Client,
                LinkOf { server },
                // Both directions land on this one entity, because a host client
                // *is* its own server-side peer. `MessageSender<ClientCmd>` is a
                // required component of `Client` and `MessageSender<ServerNotice>`
                // of `ClientOf`, and this entity is both.
                MessageReceiver::<ClientCmd>::default(),
                MessageReceiver::<ServerNotice>::default(),
            ))
            .id();
        commands.trigger(Connect { entity: client });
        *done = true;
        return;
    }

    let client = commands
        .spawn((
            Name::new("Client"),
            RawClient,
            // The IO layer. Without this the link never binds a socket and
            // nothing is ever sent -- `Connect` would silently do nothing.
            UdpIo::default(),
            LocalAddr(config.bind_addr),
            PeerAddr(config.server_addr),
            Link::default(),
            ReplicationReceiver,
            // Inserted eagerly so we never miss the first notice while the
            // receiver component is being created lazily by the message layer.
            MessageReceiver::<ServerNotice>::default(),
        ))
        .id();
    commands.trigger(Connect { entity: client });
    *done = true;
}

/// Mark the view a *remote* client was sent, so its HUD reads an identity
/// rather than a coincidence (`found-010`, D30).
///
/// The server addresses each peer's view with `NetworkTarget::Single`, so a
/// remote client holds exactly one `PlayerView` and it is necessarily its own.
/// A host holds the server's views directly (D3) and nothing distinguishes them,
/// so it does not guess here: `net::server::mark_host_view`, which owns the
/// key-to-view mapping, marks the right one instead.
fn mark_local_view(
    config: Res<Config>,
    marked: Query<(), With<LocalPlayer>>,
    views: Query<(Entity, &PlayerView)>,
    mut commands: Commands,
) {
    if config.mode.is_host() || !marked.is_empty() {
        return;
    }
    if let Some((view, pv)) = views.iter().next() {
        // Logged at `debug` because this is the line that answers "why is the
        // HUD showing the wrong player?", which is the symptom D30 had.
        debug!(
            target: crate::logging::target::REPLICATION,
            "remote client: marked {view:?} as its own view (gold {})",
            pv.gold
        );
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
            .add_systems(Update, (spawn_client, mark_local_view));
    }
}
