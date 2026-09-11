//! Server: owns the authoritative `Sim`, mirrors it into replicated entities,
//! and validates client intents.
//!
//! The important architectural point is that `Sim` has no idea it is on a
//! network. Everything network-shaped is here, in one file, so the sim stays
//! testable and portable.

use std::collections::HashMap;
use std::net::SocketAddr;

use bevy::prelude::*;
use lightyear::prelude::client::Connected;
use lightyear::prelude::server::*;
use lightyear::prelude::*;

use crate::game::*;
use crate::sim::*;

pub const SERVER_ADDR: &str = "127.0.0.1:5000";

/// True when one process plays both roles. Host clients share the server's
/// world, so they cannot be addressed by `NetworkTarget::Single` and already
/// contain authoritative entities (no `Remote` marker). Set from `main`.
#[derive(Resource)]
pub struct HostMode(pub bool);

impl Default for HostMode {
    fn default() -> Self {
        Self(false)
    }
}

/// Bookkeeping the server needs to talk to clients. Not part of `Sim`.
#[derive(Resource, Default)]
pub struct Net {
    /// Player -> their connection entity.
    pub links: HashMap<PlayerKey, Entity>,
    /// Player -> their private HUD entity.
    pub views: HashMap<PlayerKey, Entity>,
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

fn spawn_server(mut commands: Commands) {
    let addr: SocketAddr = SERVER_ADDR.parse().expect("valid server addr");
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
    host: Res<HostMode>,
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
    // the server's world, so `Single` cannot target them -- they get everything.
    let target = if host.0 {
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

fn handle_cmds(
    mut sim: ResMut<Sim>,
    net: Res<Net>,
    mut links: Query<(
        &RemoteId,
        &mut MessageReceiver<ClientCmd>,
        &mut MessageSender<ServerNotice>,
    )>,
) {
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
        for cmd in cmds {
            let outcome = match cmd {
                ClientCmd::Build { x, y, kind } => sim.try_build(key, IVec2::new(x, y), kind),
                ClientCmd::Upgrade { x, y } => sim.try_upgrade(key, IVec2::new(x, y)),
                ClientCmd::Sell { x, y } => sim.try_sell(key, IVec2::new(x, y)),
                ClientCmd::CallWave => sim.call_wave(),
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
    }
}

// ---------------------------------------------------------------------------
// Simulation + replication mirror
// ---------------------------------------------------------------------------

fn tick_sim(time: Res<Time>, mut sim: ResMut<Sim>) {
    if sim.over {
        return;
    }
    for ev in sim.step(time.delta_secs()) {
        match ev {
            SimEvent::WaveStarted(w) => {
                info!("wave {} started ({} creeps)", w, sim.creeps_per_wave());
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
            .init_resource::<HostMode>()
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
