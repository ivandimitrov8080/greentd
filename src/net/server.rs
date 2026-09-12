//! Server: owns the authoritative `Sim`, mirrors it into replicated entities,
//! and validates client intents.
//!
//! The important architectural point is that `Sim` has no idea it is on a
//! network. Everything network-shaped is here, in one file, so the sim stays
//! testable and portable.

use std::collections::HashMap;

use bevy::prelude::*;
use lightyear::connection::client::Disconnecting;
use lightyear::connection::host::HostClient;
use lightyear::prelude::client::Connected;
use lightyear::prelude::server::*;
use lightyear::prelude::*;

use crate::config::Config;
use crate::data::components::*;
use crate::data::reject::Reject;
use crate::logging;
use crate::net::messages::*;
use crate::net::protocol::ProtocolVersion;
use crate::ratelimit::TokenBucket;
use crate::sim::*;

/// Bookkeeping the server needs to talk to clients. Not part of `Sim`.
#[derive(Resource, Default)]
pub struct Net {
    /// Player -> their connection entity.
    pub links: HashMap<PlayerKey, Entity>,
    /// Player -> their private HUD entity.
    pub views: HashMap<PlayerKey, Entity>,
    /// Player -> their command budget (`audit-007`). A peer that is not in
    /// `links` has no bucket: it is not allowed to act at all (D8).
    pub buckets: HashMap<PlayerKey, TokenBucket>,
    /// The single replicated match-state entity.
    pub match_entity: Option<Entity>,
    /// Sim creep -> its replicated mirror entity.
    pub creeps: HashMap<CreepId, Entity>,
    /// Grid cell -> its replicated tower entity.
    pub towers: HashMap<IVec2, Entity>,
    /// Set once we have broadcast game over, to avoid spamming.
    pub announced_over: bool,
}

fn peer_key(remote: &RemoteId) -> PlayerKey {
    PlayerKey(remote.0.to_bits())
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Bind the socket the config names. `found-002`: this used to parse the
/// `SERVER_ADDR` literal, which made a remote or an ephemeral port impossible.
///
/// The address comes from [`Config::server_bind_addr`] rather than
/// [`Config::bind_addr`] so that a host serves on the address its own client
/// dials, instead of opening two sockets on one port and panicking.
fn spawn_server(config: Res<Config>, mut commands: Commands) {
    let addr = config.server_bind_addr();
    let server = commands
        .spawn((
            Name::new("Server"),
            RawServer,
            LocalAddr(addr),
            ServerUdpIo::default(),
        ))
        .id();
    commands.trigger(Start { entity: server });
}

fn server_up(server: Query<(), (With<Server>, With<Started>)>) -> bool {
    !server.is_empty()
}

/// Mark the *host's own* view, so its HUD reads an identity rather than a
/// coincidence (`found-010`, D30).
///
/// A host runs both roles in one world, so its own `PlayerView` is one of the
/// server's and only the server knows which: [`Net::views`] is keyed by
/// `PlayerKey`, and the client half cannot reconstruct that key from anything
/// it holds. So the marking happens here, where the mapping is.
///
/// The peer to mark is this process's own client half, which in host mode is the
/// one entity carrying [`Client`]. Its [`LocalId`] is the `PeerId` lightyear
/// gave it when it connected, and that is the same value the server derived its
/// [`PlayerKey`] from, because a host client's `LocalId` and `RemoteId` are the
/// same `PeerId::Local(0)` (`HostPlugin::connect`). A dedicated server has no
/// such entity and does nothing.
///
/// This used to look for the host's `RawClient`, which stopped existing when
/// host mode moved to lightyear's in-process host client (D31).
fn mark_host_view(
    config: Res<Config>,
    net: Res<Net>,
    local: Query<&LocalId, (With<Client>, With<Connected>)>,
    views: Query<&PlayerView>,
    marked: Query<(), With<LocalPlayer>>,
    mut commands: Commands,
) {
    if !config.mode.is_host() || !marked.is_empty() {
        return;
    }
    let Ok(local_id) = local.single() else {
        return;
    };
    let key = PlayerKey(local_id.0.to_bits());
    if let Some(view) = net.views.get(&key) {
        // Logged at `debug` because this is the line that answers "why is the
        // HUD showing the wrong player?", which is the symptom D3 had.
        let gold = views.get(*view).map(|v| v.gold).unwrap_or_default();
        debug!(
            target: logging::target::REPLICATION,
            "host is {:?}: marked {view:?} as its own view (gold {gold})",
            key
        );
        commands.entity(*view).insert(LocalPlayer);
    }
}

/// The server reached the listening state, so the socket is bound and peers
/// can connect. This line is what a CI smoke test waits for (`found-007`): it
/// is the first thing a successful run prints, and the last thing worth
/// printing if the bind failed.
fn on_server_started(_trigger: On<Add, Started>, config: Res<Config>) {
    info!(
        target: logging::target::NET,
        "listening on {} ({})",
        config.server_bind_addr(),
        config.mode.as_str()
    );
}

/// Admit a peer as a player, or refuse it, once it states its protocol version
/// (`net-001`, D22).
///
/// One query covers both kinds of peer, because lightyear gives both a
/// `Connected` + `RemoteId` + `ClientOf`: a remote peer entity spawned by the
/// UDP transport, and -- in host mode -- the host's own client entity, which
/// `HostPlugin` promotes in place. What differs is what the connection *is*, and
/// that is the `Has<HostClient>` flag below.
///
/// Deliberately *not* an `On<Add, Connected>` observer any more. Lightyear marks
/// a socket connected the moment it links, but a peer is not a player until it
/// has said what it speaks, and the whole point of `net-001` is that nothing --
/// no `Replicate` entity, no `Sim` player record -- may be created before that
/// version is checked. Splitting "connected" from "in the match" is what makes
/// the refusal land *before* any entity exists rather than nearly before, and
/// it is where `net-002`'s real identity and `net-014`'s handshake timeout will
/// both attach.
/// A connected peer and the two ends of its handshake.
///
/// Spelled out here because the five-element tuple is the whole admission
/// question: who the peer is, whether it is the host, what it says it speaks,
/// and where to answer. A peer already [`Refused`] is excluded, so a client
/// that retransmits its handshake during the grace window is not refused twice.
type HandshakePeers<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static RemoteId,
        Has<HostClient>,
        &'static mut MessageReceiver<Handshake>,
        &'static mut MessageSender<HandshakeAck>,
    ),
    (With<ClientOf>, Without<Refused>),
>;

/// A peer that stated an incompatible version, being let down over a few frames
/// (`net-001`).
///
/// The refusal has to be *written* before the link is torn down, and lightyear
/// only serialises a connection that still has `Connected` -- which lightyear's
/// own `Disconnecting` hook removes the instant it is inserted. Dropping the
/// peer in the same tick it is refused therefore sends nothing, which is how a
/// refusal becomes a bare disconnect with no reason. So the drop is deferred.
#[derive(Component)]
struct Refused {
    /// Frames left before the link is dropped.
    grace: u8,
}

/// Frames a refused peer is kept alive so its refusal can reach the socket.
///
/// One frame is enough -- `PostUpdate` writes the bytes and `Last` would drop
/// the peer -- and the second is margin. It is not tunable because a refused
/// peer never reaches the match whichever value is used.
const REFUSAL_GRACE_FRAMES: u8 = 2;

/// Drop peers whose refusal has had time to be written (`net-001`).
///
/// Runs before [`handle_handshakes`] so that the frame a peer is refused is
/// never also the frame it is dropped.
fn age_refusals(mut peers: Query<(Entity, &mut Refused)>, mut commands: Commands) {
    for (entity, mut refused) in peers.iter_mut() {
        match refused.grace.checked_sub(1) {
            Some(left) => refused.grace = left,
            None => {
                commands.entity(entity).insert(Disconnecting);
            }
        }
    }
}

fn handle_handshakes(
    version: Res<ProtocolVersion>,
    config: Res<Config>,
    mut sim: ResMut<Sim>,
    mut net: ResMut<Net>,
    mut commands: Commands,
    mut peers: HandshakePeers,
) {
    for (entity, remote, is_host_client, mut recv, mut ack) in peers.iter_mut() {
        let key = peer_key(remote);
        // Already admitted: a peer may retransmit its handshake, and a second
        // one must not spawn a second player.
        if net.links.contains_key(&key) {
            continue;
        }
        // Drains the buffer, so the answer is given once per stated version
        // rather than once per frame.
        let Some(handshake) = recv.receive().last() else {
            continue;
        };

        if let Some(reason) = version.disagreement(&handshake.version) {
            warn!(
                target: logging::target::NET,
                "refusing {:?} (remote {}, speaks {}): {reason}",
                key, remote.0, handshake.version
            );
            ack.send::<ReliableChannel>(HandshakeAck::Refused { reason });
            // The refusal is written to the socket over the next frames, then
            // the link is dropped; the peer is not a player now and never
            // becomes one, so nothing has to be undone.
            commands.entity(entity).insert(Refused {
                grace: REFUSAL_GRACE_FRAMES,
            });
            continue;
        }

        join_player(
            entity,
            remote,
            is_host_client,
            &config,
            &mut sim,
            &mut net,
            &mut commands,
            &mut ack,
        );
    }
}

/// Everything that turns a verified peer into a player in the match.
///
/// This used to be the body of the `Connected` observer; it lives on its own so
/// that no part of it can run before the handshake (`net-001`).
#[allow(
    clippy::too_many_arguments,
    reason = "one call site, and every argument is a distinct service the join needs"
)]
fn join_player(
    entity: Entity,
    remote: &RemoteId,
    is_host_client: bool,
    config: &Config,
    sim: &mut Sim,
    net: &mut Net,
    commands: &mut Commands,
    ack: &mut MessageSender<HandshakeAck>,
) {
    let key = peer_key(remote);
    info!(target: logging::target::NET, "player {:?} joined", key);
    let start_gold = sim.balance.match_rules.start_gold;

    // Opt this connection into replication and into receiving intents. A host
    // client already has both receivers (`net::client::spawn_client` put them
    // there at spawn, because both directions land on that one entity), and it
    // must *not* get a `ReplicationSender`: it is not a replicon peer, and the
    // entities it reads are the server's own.
    //
    // Note what is *not* inserted here: a fresh `MessageManager`. It is a
    // required component of `MessageSender`/`MessageReceiver`, so the entity
    // already has one, and it is the manager each of those components registers
    // itself in through an `on_add` hook. Inserting a second, default one would
    // replace it and wipe that registration -- which is precisely what made
    // every `ServerNotice` vanish on the way to a remote client (D32).
    if is_host_client {
        commands.entity(entity).insert(Name::new("HostClientLink"));
    } else {
        commands.entity(entity).insert((
            Name::new("ClientLink"),
            ReplicationSender,
            MessageReceiver::<ClientCmd>::default(),
        ));
    }

    // A private HUD entity, addressed to this player alone. A host client is
    // deliberately *not* addressed: it needs no copy of anything, because the
    // authoritative `PlayerView` is already in its world (D3, `audit-003`).
    // `mark_host_view` binds the HUD to that entity instead.
    let view = commands
        .spawn((
            Name::new("PlayerView"),
            PlayerView {
                gold: start_gold,
                kills: 0,
            },
        ))
        .id();
    if !is_host_client {
        commands
            .entity(view)
            .insert(Replicate::to_clients(NetworkTarget::Single(remote.0)));
        debug!(
            target: logging::target::REPLICATION,
            "addressing: private view for {:?} (remote {})",
            key,
            remote.0
        );
    } else {
        debug!(
            target: logging::target::REPLICATION,
            "in-process: private view for {:?} (remote {}); not replicated",
            key,
            remote.0
        );
    }

    net.links.insert(key, entity);
    net.views.insert(key, view);
    // A fresh full budget, so a rejoin is not punished for an earlier burst.
    // The bucket is keyed by the same `PlayerKey` as everything else, which is
    // the socket address until `net-002` gives players a real identity.
    net.buckets.insert(
        key,
        TokenBucket::new(config.command_burst, config.commands_per_second),
    );
    sim.add_player(key);

    // The sender may now send intents; before this line the server drops
    // everything it says.
    ack.send::<ReliableChannel>(HandshakeAck::Accepted);
}

fn on_client_disconnected(
    trigger: On<Remove, Connected>,
    links: Query<&RemoteId, With<ClientOf>>,
    mut sim: ResMut<Sim>,
    mut net: ResMut<Net>,
    mut commands: Commands,
) {
    let Ok(remote) = links.get(trigger.entity) else {
        return;
    };
    let key = peer_key(remote);
    if net.links.remove(&key).is_none() {
        return;
    }
    info!(target: logging::target::NET, "player {:?} left (towers remain)", key);
    sim.remove_player(key);
    net.buckets.remove(&key);
    if let Some(view) = net.views.remove(&key) {
        commands.entity(view).despawn();
    }
}

/// Spawn the single replicated match-state entity once the server is live.
fn setup_match(
    sim: Res<Sim>,
    mut net: ResMut<Net>,
    mut commands: Commands,
    live: Query<(), (With<Server>, With<Started>)>,
) {
    if net.match_entity.is_some() || live.is_empty() {
        return;
    }
    let e = commands
        .spawn((
            Name::new("MatchState"),
            sim.match_view(),
            Replicate::to_clients(NetworkTarget::All),
        ))
        .id();
    net.match_entity = Some(e);
    debug!(
        target: logging::target::REPLICATION,
        "match state entity {e:?} created"
    );
}

// ---------------------------------------------------------------------------
// Intents
// ---------------------------------------------------------------------------

/// Drain every peer's commands and apply the ones the peer may afford.
///
/// Two bounded things happen here: a command from a peer that is not in the
/// match is dropped without execution (D8), and a registered peer may only
/// spend the tokens its bucket holds (`audit-007`). Over-budget commands are
/// dropped rather than deferred, and the peer hears about it exactly once per
/// frame, so a client cannot turn a burst into a backlog.
fn handle_cmds(
    time: Res<Time>,
    config: Res<Config>,
    mut sim: ResMut<Sim>,
    mut net: ResMut<Net>,
    mut links: Query<(
        &RemoteId,
        &mut MessageReceiver<ClientCmd>,
        &mut MessageSender<ServerNotice>,
    )>,
) {
    let frame = time.delta_secs();
    for (remote, mut recv, mut send) in links.iter_mut() {
        let key = peer_key(remote);
        // Always drain, so a peer that is not allowed to act cannot leave a
        // queue growing behind it.
        let cmds: Vec<ClientCmd> = recv.receive().collect();
        if !net.links.contains_key(&key) {
            if !cmds.is_empty() {
                debug!(
                    target: logging::target::COMMANDS,
                    "dropping {} command(s) from unregistered peer {:?}",
                    cmds.len(),
                    key
                );
            }
            continue;
        }

        // Settle the frame's budget first. The block ends the borrow of
        // `net.buckets`, so `net.links` is readable again below.
        let (allowed, over_budget) = {
            let bucket = net.buckets.entry(key).or_insert_with(|| {
                TokenBucket::new(config.command_burst, config.commands_per_second)
            });
            bucket.refill(frame);
            let mut allowed: Vec<ClientCmd> = Vec::with_capacity(cmds.len());
            let mut over_budget = 0usize;
            for cmd in cmds {
                if bucket.try_consume() {
                    allowed.push(cmd);
                } else {
                    over_budget += 1;
                }
            }
            (allowed, over_budget)
        };

        for cmd in allowed {
            let outcome = match cmd {
                ClientCmd::Build { x, y, kind } => sim.try_build(key, IVec2::new(x, y), kind),
                ClientCmd::Upgrade { x, y } => sim.try_upgrade(key, IVec2::new(x, y)),
                ClientCmd::Sell { x, y } => sim.try_sell(key, IVec2::new(x, y)),
                ClientCmd::CallWave => sim.call_wave(key),
            };
            let notice = match outcome {
                Ok(()) => ServerNotice::Ok,
                Err(reason) => {
                    debug!(
                        target: logging::target::COMMANDS,
                        "command rejected for {:?}: {:?}", key, reason
                    );
                    ServerNotice::Err(reason)
                }
            };
            send.send::<ReliableChannel>(notice);
        }

        if over_budget > 0 {
            // One refusal per frame, however many commands were dropped, so a
            // peer that floods cannot make the server flood back.
            debug!(
                target: logging::target::COMMANDS,
                "rate limited {:?}: {} command(s) over budget",
                key, over_budget
            );
            send.send::<ReliableChannel>(ServerNotice::Err(Reject::RateLimited));
        }
    }
}

// ---------------------------------------------------------------------------
// Simulation + replication mirror
// ---------------------------------------------------------------------------

/// Step the authoritative sim and turn its events into traffic and logs.
///
/// The wave notification is broadcast here rather than in `announce_game_over`
/// because the event is in hand; it takes the same path, so every connected
/// peer hears about every wave exactly once (D5). A wave that starts with no
/// peer connected simply has nobody to tell.
fn tick_sim(
    time: Res<Time>,
    mut sim: ResMut<Sim>,
    net: Res<Net>,
    mut links: Query<&mut MessageSender<ServerNotice>>,
) {
    if sim.over {
        return;
    }
    let events = sim.step(time.delta_secs());
    for ev in events {
        match ev {
            SimEvent::WaveStarted(w) => {
                info!(
                    target: logging::target::SIM,
                    "wave {} started ({} creeps)",
                    w,
                    sim.creeps_per_wave()
                );
                let notice = ServerNotice::WaveStarted(w);
                for entity in net.links.values() {
                    if let Ok(mut send) = links.get_mut(*entity) {
                        send.send::<ReliableChannel>(notice.clone());
                    }
                }
            }
            SimEvent::CreepSpawned(id) => {
                trace!(target: logging::target::SIM, "creep {:?} spawned", id)
            }
            SimEvent::CreepKilled(id) => {
                trace!(target: logging::target::SIM, "creep {:?} killed", id)
            }
            SimEvent::GameOver => {
                warn!(
                    target: logging::target::SIM,
                    "GAME OVER: {} creeps alive (cap {})",
                    sim.creeps_alive(),
                    sim.overrun_cap()
                );
            }
        }
    }
}

fn sync_creeps(
    sim: Res<Sim>,
    mut net: ResMut<Net>,
    mut commands: Commands,
    mut mirrors: Query<(&mut CreepVis, &mut CreepHp)>,
) {
    // Reap.
    let live: std::collections::HashSet<CreepId> = sim.creeps.keys().copied().collect();
    net.creeps.retain(|id, entity| {
        if live.contains(id) {
            true
        } else {
            trace!(
                target: logging::target::REPLICATION,
                "creep {:?} no longer mirrored",
                id
            );
            commands.entity(*entity).despawn();
            false
        }
    });

    // Spawn or update.
    for (id, creep) in sim.creeps.iter() {
        let pos = creep.pos(&sim.path);
        let existing = net.creeps.get(id).copied();
        match existing {
            Some(entity) => {
                if let Ok((mut vis, mut hp)) = mirrors.get_mut(entity) {
                    vis.x = pos.x;
                    vis.y = pos.y;
                    hp.hp = creep.hp;
                    hp.max = creep.max_hp;
                }
            }
            None => {
                let entity = commands
                    .spawn((
                        Name::new("Creep"),
                        CreepVis { x: pos.x, y: pos.y },
                        CreepHp {
                            hp: creep.hp,
                            max: creep.max_hp,
                        },
                        Replicate::to_clients(NetworkTarget::All),
                    ))
                    .id();
                trace!(target: logging::target::REPLICATION, "creep {:?} mirrored", id);
                net.creeps.insert(*id, entity);
            }
        }
    }
}

fn sync_towers(
    sim: Res<Sim>,
    mut net: ResMut<Net>,
    mut commands: Commands,
    mut mirrors: Query<(&mut TowerVis, &mut TowerAt)>,
) {
    let live: std::collections::HashSet<IVec2> = sim.towers.keys().copied().collect();
    net.towers.retain(|cell, entity| {
        if live.contains(cell) {
            true
        } else {
            trace!(
                target: logging::target::REPLICATION,
                "tower at {:?} no longer mirrored",
                cell
            );
            commands.entity(*entity).despawn();
            false
        }
    });

    for (cell, tower) in sim.towers.iter() {
        let existing = net.towers.get(cell).copied();
        match existing {
            Some(entity) => {
                if let Ok((mut vis, mut at)) = mirrors.get_mut(entity) {
                    vis.kind = tower.kind;
                    vis.level = tower.level;
                    at.x = cell.x;
                    at.y = cell.y;
                }
            }
            None => {
                let entity = commands
                    .spawn((
                        Name::new("Tower"),
                        TowerAt {
                            x: cell.x,
                            y: cell.y,
                        },
                        TowerVis {
                            kind: tower.kind,
                            level: tower.level,
                        },
                        Replicate::to_clients(NetworkTarget::All),
                    ))
                    .id();
                trace!(
                    target: logging::target::REPLICATION,
                    "tower at {:?} mirrored",
                    cell
                );
                net.towers.insert(*cell, entity);
            }
        }
    }
}

fn sync_match(sim: Res<Sim>, net: Res<Net>, mut views: Query<&mut MatchView>) {
    let Some(entity) = net.match_entity else {
        return;
    };
    if let Ok(mut mv) = views.get_mut(entity) {
        // Note there is deliberately no early return for `sim.over`: the last
        // frame of the game is exactly the one the HUD needs to show the final
        // overrun count, so the mirror keeps running (D4).
        //
        // Compare before writing: assigning through `Mut` marks the component
        // changed, which would replicate every frame for no reason.
        let want = sim.match_view();
        if *mv != want {
            *mv = want;
        }
    }
}

fn sync_player_views(sim: Res<Sim>, net: Res<Net>, mut views: Query<&mut PlayerView>) {
    for (key, entity) in net.views.iter() {
        let Some(player) = sim.players.get(key) else {
            continue;
        };
        // One write per player per frame at most: the change-diffing guard is
        // what keeps a quiet board off the wire.
        if let Ok(mut v) = views.get_mut(*entity)
            && (v.gold != player.gold || v.kills != player.kills)
        {
            v.gold = player.gold;
            v.kills = player.kills;
        }
    }
}

/// Tell the players it's over, once.
fn announce_game_over(
    sim: Res<Sim>,
    mut net: ResMut<Net>,
    mut links: Query<&mut MessageSender<ServerNotice>>,
) {
    if !sim.over || net.announced_over {
        return;
    }
    net.announced_over = true;
    let notice = ServerNotice::GameOver {
        wave: sim.wave,
        live: sim.creeps_alive(),
    };
    for entity in net.links.values() {
        if let Ok(mut send) = links.get_mut(*entity) {
            send.send::<ReliableChannel>(notice.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct GreenTdServerPlugin;

impl Plugin for GreenTdServerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Sim>()
            .init_resource::<Net>()
            .add_systems(Startup, spawn_server)
            .add_observer(on_server_started)
            .add_observer(on_client_disconnected)
            // Match setup and the sim itself only make sense once we're listening.
            .add_systems(
                Update,
                (
                    setup_match,
                    mark_host_view,
                    // A refusal is aged before new ones are made, so the frame
                    // a peer is refused is never the frame it is dropped.
                    age_refusals,
                    // The handshake is admitted before intents are read, so a
                    // peer that handshakes and acts in the same frame is a
                    // player by the time its command is handled (`net-001`).
                    handle_handshakes,
                    handle_cmds,
                    // Mirror order matters only for readability: creeps then
                    // towers then the aggregate views.
                    (sync_creeps, sync_towers).chain(),
                    (sync_match, sync_player_views).chain(),
                    announce_game_over,
                )
                    .chain()
                    .run_if(server_up),
            )
            .add_systems(FixedUpdate, tick_sim.run_if(server_up));
    }
}
