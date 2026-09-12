//! The authoritative simulation. Server-only.
//!
//! Nothing under `sim/` imports `lightyear`, opens a socket or names a
//! replicated component. It is a plain `step(dt)` state machine that can be
//! unit-tested headless and, if you ever want lockstep, replayed. The server's
//! job in `net/server.rs` is to (a) feed it validated intents and (b) mirror its
//! state into replicated entities.
//!
//! The folder is the shape of one tick:
//!
//! | File          | Owns                                                      |
//! |---------------+-----------------------------------------------------------|
//! | `mod.rs`      | The state, and the order the phases below run in           |
//! | `waves.rs`    | When a wave starts, who is in it, and who may summon one   |
//! | `combat.rs`   | Creep movement, leaks, and everything a tower's shot does  |
//! | `economy.rs`  | Every place gold changes hands                             |
//! | `rng.rs`      | The match's seeded generator (`found-009`)                 |
//!
//! # The board, and what a creep is trying to do
//!
//! A creep enters the board at its lane's spawn point and walks toward one
//! thing: the goal. It has no other behaviour, because the goal *is* the game.
//! Reaching it is a **leak** (`sim-006`): the creep leaves the match and the
//! match loses a life, and running out of lives loses the match (`sim-005`).
//! Nothing here is specific to which lineage is configured -- [`LoseRule`]
//! picks the rule and the phases run either way.
//!
//! [`LoseRule`]: crate::data::balance::LoseRule

mod combat;
mod economy;
pub mod rng;
mod waves;

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;

use crate::data::balance::{Balance, BalanceData, LoseRule};
use crate::data::components::{MatchView, Phase};
use crate::map::{Map, MapData};

pub use rng::Rng;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct CreepId(pub u32);

/// Stable identity for a player, within one match (`net-002`).
///
/// It is derived by the network layer from the clients' own persistent
/// `PlayerId` -- never from a socket address -- so a player who reconnects
/// from a new port is the same player, with the same gold, kills and towers.
/// The sim only ever sees this key; it never reads a `SocketAddr`, and it must
/// stay that way for the sim to remain replayable from a seed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PlayerKey(pub u64);

#[derive(Debug)]
pub struct Creep {
    /// Which lane of the board this creep entered on. An index into
    /// [`MapData::lanes`], so a creep's position is `lanes[lane].sample(dist)`.
    ///
    /// It is a lane and not a player: the real board attacks from every spawn
    /// region on every wave, so a lane is not owned by whoever happens to be
    /// standing near it.
    pub lane: u8,
    /// Distance travelled along that lane. `>= lane.total` means the creep has
    /// arrived at the goal and is about to leak.
    pub dist: f32,
    pub hp: f32,
    pub max_hp: f32,
    pub base_speed: f32,
    pub slow_mult: f32,
    pub slow_timer: f32,
    pub bounty: u32,
    pub last_hit_by: PlayerKey,
}

impl Creep {
    pub fn pos(&self, map: &MapData) -> Vec2 {
        lane_of(map, self.lane).sample(self.dist)
    }

    pub fn speed_mult(&self) -> f32 {
        if self.slow_timer > 0.0 {
            self.slow_mult
        } else {
            1.0
        }
    }

    /// How far this creep still has to walk, in world units. Zero or less means
    /// it is at the goal.
    pub fn remaining(&self, map: &MapData) -> f32 {
        lane_of(map, self.lane).total - self.dist
    }
}

/// The lane a wire-level index names.
///
/// A creep's lane index is validated when the creep is spawned and comes from
/// the map itself, so an out-of-range index is a bug in the sim rather than
/// something a client can ask for. Rather than panic in the tick loop, it reads
/// as the first lane; `the_shipped_map_has_lanes` and the spawn path are what
/// keep that unreachable.
fn lane_of(map: &MapData, lane: u8) -> &crate::map::Lane {
    map.lanes.get(lane as usize).unwrap_or(&map.lanes[0])
}

#[derive(Debug)]
pub struct Tower {
    pub kind: u8,
    pub level: u8,
    pub owner: PlayerKey,
    pub cooldown: f32,
}

#[derive(Debug, Default)]
pub struct Player {
    pub gold: u32,
    pub kills: u32,
    /// Cleared when the player disconnects; their towers stay.
    pub connected: bool,
    /// Seconds until this player may call another wave (D6). Runs down in
    /// [`Sim::step`], so it is match time rather than wall-clock time.
    pub wave_call_cooldown: f32,
}

#[derive(Resource, Debug)]
pub struct Sim {
    /// The loaded balance tables. Held by handle, never re-read from disk, so a
    /// running match cannot change its numbers underneath itself.
    pub balance: Arc<BalanceData>,
    /// The loaded board: lanes, spawn points and the goal. Loaded by both
    /// peers and hashed into the protocol version (`net-001`).
    pub map: Arc<MapData>,
    pub creeps: HashMap<CreepId, Creep>,
    pub towers: HashMap<IVec2, Tower>,
    pub players: HashMap<PlayerKey, Player>,
    pub wave: u32,
    pub wave_timer: f32,
    /// Lives left, when `lose_rule` is [`LoseRule::Lives`]: one per creep that
    /// reaches the goal. Starts at `match_rules.starting_lives`.
    pub lives: u32,
    /// Creeps that have reached the goal this match, for the HUD and for
    /// `economy` telemetry. The sim's own record of the leak count, which is
    /// what makes a leak *observable* rather than only fatal.
    pub leaks: u32,
    next_id: u32,
    /// The match's seed, kept so a replay can be described by it.
    seed: u64,
    /// The sim's own generator. Nothing else may draw randomness: `step` is a
    /// function of the sim's state and `dt`, and a global generator would make
    /// that false (`found-009`).
    rng: Rng,
    /// Set once the lose condition is met. `step` returns immediately while it
    /// is set; [`Sim::reset`] is the only thing that clears it (`audit-018`).
    pub over: bool,
}

/// What happened during one step, for the server to turn into network traffic.
#[derive(Debug)]
pub enum SimEvent {
    CreepSpawned(CreepId),
    CreepKilled(CreepId),
    /// A creep reached the goal (`sim-006`). It is gone, and unless the match
    /// ran out of lives this event is the only trace of it.
    Leaked(CreepId),
    WaveStarted(u32),
    GameOver,
}

impl FromWorld for Sim {
    /// Built from the [`Balance`] and [`Map`] resources, which `main` inserts
    /// before any plugin is added. There is deliberately no `Default` impl: the
    /// sim cannot exist without its numbers or its board.
    fn from_world(world: &mut World) -> Self {
        let balance = world.resource::<Balance>().0.clone();
        let map = world.resource::<Map>().0.clone();
        Self::new(balance, map)
    }
}

impl Sim {
    pub fn new(balance: Arc<BalanceData>, map: Arc<MapData>) -> Self {
        let seed = balance.match_rules.seed;
        let mut sim = Self {
            balance,
            map,
            creeps: HashMap::new(),
            towers: HashMap::new(),
            players: HashMap::new(),
            wave: 0,
            wave_timer: 0.0,
            lives: 0,
            leaks: 0,
            next_id: 1,
            seed,
            rng: Rng::from_seed(seed),
            over: false,
        };
        sim.lives = sim.balance.match_rules.starting_lives;
        // Wave 1 arrives promptly rather than after a full interval.
        sim.wave_timer = sim.balance.match_rules.first_wave_delay;
        sim
    }

    /// The seed this match was built from, as the tables asked for it.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// How this match is lost (`sim-005`).
    pub fn lose_rule(&self) -> LoseRule {
        self.balance.match_rules.lose_rule
    }

    /// Lineage A's cap. Only meaningful when the rule is [`LoseRule::Overrun`],
    /// but published either way so the HUD can show what the board is doing.
    pub fn overrun_cap(&self) -> u32 {
        self.balance.match_rules.overrun_cap
    }

    /// Return the sim to the state [`Sim::new`] leaves it in, for a rematch
    /// (D20, `audit-018`).
    ///
    /// Everything the match accumulated goes: the board, the wave clock and the
    /// wave counter, the creep ids, the lives, the leak count, and the lose flag
    /// that otherwise made `over` terminal -- a match that could be lost once and
    /// never again was not a match, it was a process.
    ///
    /// What is *kept* is who is in the match. The player records stay keyed and
    /// stay connected, so a rematch is the same people with fresh gold and kills
    /// rather than a lobby that has to be rebuilt; and the generator is reseeded,
    /// because a fresh match on the same tables should be the *same* match, which
    /// is exactly what `found-009` promises a seed buys.
    ///
    /// This is the sim half of one reset path. The other half is `Net`, whose
    /// `announced_over` latch would otherwise swallow the second defeat's notice
    /// (D21); `net::server::reset_match` clears both together.
    pub fn reset(&mut self) {
        self.creeps.clear();
        self.towers.clear();
        self.wave = 0;
        self.wave_timer = self.balance.match_rules.first_wave_delay;
        self.lives = self.balance.match_rules.starting_lives;
        self.leaks = 0;
        self.next_id = 1;
        self.rng = Rng::from_seed(self.seed);
        self.over = false;
        let gold = self.balance.match_rules.start_gold;
        for player in self.players.values_mut() {
            player.gold = gold;
            player.kills = 0;
            player.wave_call_cooldown = 0.0;
        }
    }

    /// Where the match is in its life, for the replicated `MatchView` (D4).
    pub fn phase(&self) -> Phase {
        if self.over {
            Phase::Over
        } else if self.wave > 0 {
            Phase::InMatch
        } else {
            Phase::Waiting
        }
    }

    /// Exactly what the server should be replicating right now.
    ///
    /// Keeping this here, rather than in the mirror system, is what makes D4
    /// testable headless: the mirror is a comparison of this against the
    /// replicated component, so the interesting logic lives in the sim.
    pub fn match_view(&self) -> MatchView {
        MatchView {
            wave: self.wave,
            live_creeps: self.creeps_alive(),
            overrun_cap: self.overrun_cap(),
            creeps_per_wave: self.wave_size(),
            lives: self.lives,
            leaks: self.leaks,
            players: self.players_connected(),
            phase: self.phase().as_u8(),
        }
    }

    /// Players in the match. The HUD's lobby size.
    pub fn players_connected(&self) -> u32 {
        self.players.values().filter(|p| p.connected).count() as u32
    }

    pub fn creeps_alive(&self) -> u32 {
        self.creeps.len() as u32
    }

    /// Register (or re-register) a player. Idempotent, so a reconnect keeps
    /// the same towers and gold.
    pub fn add_player(&mut self, key: PlayerKey) {
        let gold = self.balance.match_rules.start_gold;
        let p = self.players.entry(key).or_insert_with(|| Player {
            gold,
            kills: 0,
            connected: false,
            wave_call_cooldown: 0.0,
        });
        p.connected = true;
    }

    pub fn remove_player(&mut self, key: PlayerKey) {
        if let Some(p) = self.players.get_mut(&key) {
            p.connected = false;
        }
    }

    pub fn is_buildable(&self, cell: IVec2) -> bool {
        self.map.is_buildable(cell)
    }

    // -----------------------------------------------------------------------
    // Stepping
    // -----------------------------------------------------------------------

    /// One tick.
    ///
    /// The order of the phases *is* the contract, and every one of them lives in
    /// a different file:
    ///
    ///   1. wave bookkeeping, so a wave summoned this tick exists this tick;
    ///   2. the creeps that exist, moved;
    ///   3. the creeps that arrived, leaked -- before any tower fires, so a
    ///      creep that reaches the goal this tick cannot also be shot;
    ///   4. the towers, firing;
    ///   5. the bills, paid -- a creep killed this tick is paid for this tick;
    ///   6. the lose condition, last, so it sees the tick's full result.
    pub fn step(&mut self, dt: f32) -> Vec<SimEvent> {
        let mut events = Vec::new();
        if self.over {
            return events;
        }

        self.tick_wave(dt, &mut events);
        self.tick_wave_calls(dt);
        self.advance_creeps(dt);
        self.leak_arrivals(&mut events);
        self.fire(dt);
        self.reap(&mut events);
        self.check_lose(&mut events);

        events
    }

    /// The lose condition, whichever rule the match is playing (`sim-005`).
    ///
    /// `Lives` is Lineage B: creeps that reached the goal have used up the
    /// chances, and the last one ends it. `Overrun` is Lineage A, the rule this
    /// project started with: too many creeps alive at once.
    fn check_lose(&mut self, events: &mut Vec<SimEvent>) {
        let lost = match self.lose_rule() {
            LoseRule::Lives => self.lives == 0,
            LoseRule::Overrun => self.creeps.len() as u32 > self.overrun_cap(),
        };
        if lost {
            self.over = true;
            events.push(SimEvent::GameOver);
        }
    }
}
