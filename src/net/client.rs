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
//!
//! # The handshake, and why both kinds of client send one
//!
//! Since `net-001` a client is not a player until it has said which protocol it
//! speaks, and the server drops everything an unhandshaken peer says. A host is
//! no exception, even though it shares the server's world: it sends its
//! handshake to itself, in process, and the local delivery path carries it like
//! any other message. That keeps one admission rule rather than two.
//!
//! The statement is repeated on a short timer until the server answers. That is
//! not belt-and-braces: the first datagram is the one the server *creates* the
//! peer on, so it can be parsed by nobody, and a reliable channel cannot
//! recover from that because there is no one yet to acknowledge it. See
//! `HANDSHAKE_RETRY_SECS`.

use bevy::prelude::*;
use lightyear::prelude::client::*;
use lightyear::prelude::*;

use crate::config::Config;
use crate::data::components::{LocalPlayer, PlayerView};
use crate::net::messages::{
    ClientCmd, Handshake, HandshakeAck, PlayerId, ReliableChannel, ServerNotice,
};
use crate::net::protocol::ProtocolVersion;
use crate::ui::hud::HudPlugin;

/// How this process's client half stands with the server (`net-001`).
///
/// The handshake is a phase of the connection, not a message: before it the
/// client is a stranger whose intents the server drops, and after it the client
/// is a player whose `PlayerView` is on its way. Keeping the phase in a resource
/// -- rather than inferring it from whether a `PlayerView` has arrived -- is
/// what lets a connect screen say "connecting", "joining" or "refused", and it
/// is what a test can assert without reading a log.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub enum HandshakeStatus {
    /// The handshake is in flight, or the link is not up yet.
    #[default]
    Connecting,
    /// The server agreed on the version; this client is a player.
    Accepted,
    /// The server refused the client, naming the part of the version that
    /// differed.
    Refused(String),
}

/// This process's player identity, generated once and reused for every
/// connection it makes (`net-002`, D9).
///
/// It is a resource rather than a field of the client entity because it must
/// outlive a connection: a reconnecting client states the *same* id, and that is
/// what makes the reconnect a reconnect rather than a new player. The value is
/// `Config::player_id` when the run pinned one (which is how a reconnect is
/// exercised by hand) and [`PlayerId::generate`] otherwise.
///
/// A host is a client too (`audit-029`), so it has one of these as well; its id
/// is what the server keys the host player on.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalIdentity(pub PlayerId);

impl LocalIdentity {
    /// This run's identity: the pinned one, or a fresh random one.
    ///
    /// Kept as a free function so [`GreenTdClientPlugin::build`] and any test
    /// that wants to know the rule agree without building an `App`.
    pub fn from_config(config: &Config) -> Self {
        match config.player_id {
            Some(id) => Self(PlayerId(id.get())),
            None => Self(PlayerId::generate()),
        }
    }
}

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
/// so it does not guess here: the host's own view is marked server-side, in
/// `net::server::join_player`, where the key-to-view mapping is known.
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

/// How long the client waits for an answer before stating its version again
/// (`net-001`).
///
/// A handshake is the one message that must not be lost. The first datagram a
/// client sends is also the datagram on which the server *creates* the peer
/// entity, and it can be processed in a frame where that entity exists but is
/// not yet `Connected` -- in which case the message is parsed by nobody, and
/// because the server has nothing to acknowledge it with, it is never
/// retransmitted either. A client that stated its version exactly once would
/// then sit at "connecting" forever, which is exactly what a test run showed.
///
/// So the client repeats the statement until the server answers. The server
/// treats a repeat as a no-op: a peer already in `links` is a player and a peer
/// already `Refused` is not asked again.
const HANDSHAKE_RETRY_SECS: f32 = 0.25;

/// Counts down to the next repeat of this client's [`Handshake`].
///
/// A component rather than a `Local<f32>` so that one process could later run
/// two client halves without them sharing one clock, and so that a reconnect
/// can be given a fresh one (`net-003`).
#[derive(Component)]
struct HandshakeRetry(f32);

/// A linked client half, with its retry clock if it already has one.
///
/// Spelled out here because `Option<&mut HandshakeRetry>` is the whole state
/// machine: absent on the first frame, present until the server answers.
type PendingHandshake<'w, 's> =
    Query<'w, 's, (Entity, Option<&'static mut HandshakeRetry>), (With<Client>, With<Connected>)>;

/// State this process's version, and repeat it until the server answers
/// (`net-001`).
///
/// The `Connected` bound is what stops the handshake going out before there is
/// anywhere to send it: a `RawClient` links asynchronously, and a `Connect`
/// triggered before the socket exists would otherwise be silently wasted.
fn send_handshake(
    time: Res<Time>,
    version: Res<ProtocolVersion>,
    identity: Res<LocalIdentity>,
    status: Res<HandshakeStatus>,
    mut pending: PendingHandshake,
    mut senders: Query<&mut MessageSender<Handshake>>,
    mut commands: Commands,
) {
    // A settled client says nothing. `Accepted` means the statement was heard;
    // `Refused` means repeating it would only be noise at a server that has
    // already answered.
    if !matches!(*status, HandshakeStatus::Connecting) {
        return;
    }
    let version = *version;
    let id = identity.0;
    for (entity, retry) in pending.iter_mut() {
        let first = match retry {
            Some(mut retry) => {
                retry.0 -= time.delta_secs();
                if retry.0 > 0.0 {
                    continue;
                }
                retry.0 = HANDSHAKE_RETRY_SECS;
                false
            }
            None => {
                commands
                    .entity(entity)
                    .insert(HandshakeRetry(HANDSHAKE_RETRY_SECS));
                true
            }
        };
        let Ok(mut sender) = senders.get_mut(entity) else {
            continue;
        };
        sender.send::<ReliableChannel>(Handshake { version, id });
        if first {
            info!(
                target: crate::logging::target::NET,
                "handshake sent ({version}, id {:016x})",
                id.0
            );
        } else {
            debug!(
                target: crate::logging::target::NET,
                "handshake repeated ({version}, id {:016x})",
                id.0
            );
        }
    }
}

/// Read the server's answer and remember it (`net-001`, D22).
///
/// A refusal names the part of the version that differed, and the client does
/// not sit on a dead link afterwards: it drops the connection, because the only
/// thing left to do with it is retry against a matching server.
fn read_handshake_ack(
    mut status: ResMut<HandshakeStatus>,
    mut clients: Query<(Entity, &mut MessageReceiver<HandshakeAck>)>,
    mut commands: Commands,
) {
    for (entity, mut recv) in clients.iter_mut() {
        for ack in recv.receive() {
            match ack {
                HandshakeAck::Accepted => {
                    if *status != HandshakeStatus::Accepted {
                        info!(target: crate::logging::target::NET, "handshake accepted");
                    }
                    *status = HandshakeStatus::Accepted;
                }
                HandshakeAck::Refused { reason } => {
                    error!(
                        target: crate::logging::target::NET,
                        "server refused this client: {reason}"
                    );
                    *status = HandshakeStatus::Refused(reason);
                    commands.trigger(Disconnect { entity });
                }
            }
        }
    }
}

/// Forget the retry clock when the link drops, so a reconnect states its
/// version again rather than assuming the last one was heard (`net-003`).
fn reset_handshake_on_disconnect(
    dropped: Query<Entity, (With<HandshakeRetry>, With<Disconnected>)>,
    mut commands: Commands,
) {
    for entity in dropped.iter() {
        commands.entity(entity).remove::<HandshakeRetry>();
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
        // The identity is resolved once, here, and lives as long as the process:
        // every connection this client makes states the same id, which is what
        // lets a reconnect resume the same player instead of becoming a new one
        // (`net-002`). `Config` is already a resource -- `main` inserts it before
        // any plugin is added -- so this is where the run's `--player-id`, if it
        // has one, is honoured.
        let identity = {
            let config = app.world().resource::<Config>();
            LocalIdentity::from_config(config)
        };
        app.insert_resource(identity)
            .init_resource::<HandshakeStatus>()
            .add_plugins(HudPlugin)
            .add_systems(
                Update,
                (
                    spawn_client,
                    send_handshake,
                    read_handshake_ack,
                    reset_handshake_on_disconnect,
                    mark_local_view,
                )
                    .chain(),
            );
    }
}
