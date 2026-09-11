//! Server: owns the authoritative `Sim`, mirrors it into replicated entities,
//! and validates client intents.
//!
//! The important architectural point is that `Sim` has no idea it is on a
//! network. Everything network-shaped is here, in one file, so the sim stays
//! testable and portable.

use std::collections::HashMap;

use bevy::prelude::*;
use lightyear::prelude::client::Connected;
use lightyear::prelude::server::*;
use lightyear::prelude::*;

use crate::config::Config;
use crate::game::*;
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
fn spawn_server(config: Res<Config>, mut commands: Commands) {
    let addr = config.bind_addr;
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

/// A client finished its handshake, so we now know its stable peer identity.
/// This is where a player joins the match.
fn on_client_connected(
    trigger: On<Add, Connected>,
    links: Query<&RemoteId, With<ClientOf>>,
    config: Res<Config>,
    mut sim: ResMut<Sim>,
    mut net: ResMut<Net>,
    mut commands: Commands,
) {
    let Ok(remote) = links.get(trigger.entity) else {
        return;
    };
    let key = peer_key(remote);
    if net.links.contains_key(&key) {
        return;
    }

    info!("player {:?} joined", key);
    let start_gold = sim.balance.match_rules.start_gold;

    // Opt this connection into replication and message passing.
    commands.entity(trigger.entity).insert((
        Name::new("ClientLink"),
        ReplicationSender,
        MessageManager::default(),
        MessageReceiver::<ClientCmd>::default(),
    ));

    // A private HUD entity, replicated only to this player. Host clients share
    // the server's world, so `Single` cannot target them and they get
    // everything -- that is D3, which `audit-003` removes by addressing the
    // host's own view instead of broadcasting every view.
    let target = if config.mode.is_host() {
        NetworkTarget::All
    } else {
        NetworkTarget::Single(remote.0)
    };
    let view = commands
        .spawn((
            Name::new("PlayerView"),
            PlayerView {
                gold: start_gold,
                kills: 0,
            },
            Replicate::to_clients(target),
        ))
        .id();

    net.links.insert(key, trigger.entity);
    net.views.insert(key, view);
    // A fresh full budget, so a rejoin is not punished for an earlier burst.
    // The bucket is keyed by the same `PlayerKey` as everything else, which is
    // the socket address until `net-002` gives players a real identity.
    net.buckets.insert(
        key,
        TokenBucket::new(config.command_burst, config.commands_per_second),
    );
    sim.add_player(key);
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
    info!("player {:?} left (towers remain)", key);
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
                    debug!("command rejected for {:?}: {:?}", key, reason);
                    ServerNotice::Err(reason)
                }
            };
            send.send::<ReliableChannel>(notice);
        }

        if over_budget > 0 {
            // One refusal per frame, however many commands were dropped, so a
            // peer that floods cannot make the server flood back.
            debug!(
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
                info!("wave {} started ({} creeps)", w, sim.creeps_per_wave());
                let notice = ServerNotice::WaveStarted(w);
                for entity in net.links.values() {
                    if let Ok(mut send) = links.get_mut(*entity) {
                        send.send::<ReliableChannel>(notice.clone());
                    }
                }
            }
            SimEvent::CreepSpawned(id) => trace!("creep {:?} spawned", id),
            SimEvent::CreepKilled(id) => trace!("creep {:?} killed", id),
            SimEvent::GameOver => {
                warn!(
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
        if let Ok(mut v) = views.get_mut(*entity) {
            if v.gold != player.gold || v.kills != player.kills {
                v.gold = player.gold;
                v.kills = player.kills;
            }
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
            .add_observer(on_client_connected)
            .add_observer(on_client_disconnected)
            // Match setup and the sim itself only make sense once we're listening.
            .add_systems(
                Update,
                (
                    setup_match,
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
