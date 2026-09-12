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
//! | `combat.rs`   | Creep movement and everything a tower's shot does          |
//! | `economy.rs`  | Every place gold changes hands                             |
//! | `rng.rs`      | The match's seeded generator (`found-009`)                 |

mod combat;
mod economy;
pub mod rng;
mod waves;

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;

use crate::data::balance::{Balance, BalanceData};
use crate::data::components::{MatchView, Phase};
use crate::map::*;

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
    /// Distance travelled along the closed path. Position is derived from this.
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
    pub fn pos(&self, path: &Path) -> Vec2 {
        path.sample(self.dist)
    }

    pub fn speed_mult(&self) -> f32 {
        if self.slow_timer > 0.0 {
            self.slow_mult
        } else {
            1.0
        }
    }
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
    pub path: Path,
    pub creeps: HashMap<CreepId, Creep>,
    pub towers: HashMap<IVec2, Tower>,
    pub players: HashMap<PlayerKey, Player>,
    pub wave: u32,
    pub wave_timer: f32,
    next_id: u32,
    /// The match's seed, kept so a replay can be described by it.
    seed: u64,
    /// The sim's own generator. Nothing else may draw randomness: `step` is a
    /// function of the sim's state and `dt`, and a global generator would make
    /// that false (`found-009`).
    rng: Rng,
    /// Set once the lose condition is met. Terminal until `audit-018` adds a
    /// reset path.
    pub over: bool,
}

/// What happened during one step, for the server to turn into network traffic.
#[derive(Debug)]
pub enum SimEvent {
    CreepSpawned(CreepId),
    CreepKilled(CreepId),
    WaveStarted(u32),
    GameOver,
}

impl FromWorld for Sim {
    /// Built from the [`Balance`] resource, which `main` inserts before any
    /// plugin is added. There is deliberately no `Default` impl: the sim cannot
    /// exist without its numbers.
    fn from_world(world: &mut World) -> Self {
        let balance = world.resource::<Balance>().0.clone();
        Self::new(balance)
    }
}

impl Sim {
    pub fn new(balance: Arc<BalanceData>) -> Self {
        let seed = balance.match_rules.seed;
        let mut sim = Self {
            balance,
            path: Path::default(),
            creeps: HashMap::new(),
            towers: HashMap::new(),
            players: HashMap::new(),
            wave: 0,
            wave_timer: 0.0,
            next_id: 1,
            seed,
            rng: Rng::from_seed(seed),
            over: false,
        };
        // Wave 1 arrives promptly rather than after a full interval.
        sim.wave_timer = sim.balance.match_rules.first_wave_delay;
        sim
    }

    /// The seed this match was built from, as the tables asked for it.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Lineage A lose condition: more live creeps than this ends the match.
    pub fn overrun_cap(&self) -> u32 {
        self.balance.match_rules.overrun_cap
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
            creeps_per_wave: self.creeps_per_wave(),
            players: self.players_connected(),
            phase: self.phase().as_u8(),
        }
    }

    /// Players in the match. The HUD's lobby size, and the multiplier behind
    /// [`Sim::creeps_per_wave`].
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
        // Qualified: the method shares a name with the free function.
        crate::map::is_buildable(&self.path, cell)
    }

    // -----------------------------------------------------------------------
    // Stepping
    // -----------------------------------------------------------------------

    /// One tick.
    ///
    /// The order of the phases *is* the contract, and every one of them lives
    /// in a different file:
    ///
    ///   1. wave bookkeeping, so a wave summoned this tick exists this tick;
    ///   2. the creeps that exist, moved;
    ///   3. the towers, firing;
    ///   4. the bills, paid -- a creep killed this tick is paid for this tick;
    ///   5. the lose condition, last, so it sees the tick's full result.
    pub fn step(&mut self, dt: f32) -> Vec<SimEvent> {
        let mut events = Vec::new();
        if self.over {
            return events;
        }

        self.tick_wave(dt, &mut events);
        self.tick_wave_calls(dt);
        self.advance_creeps(dt);
        self.fire(dt);
        self.reap(&mut events);
        self.check_overrun(&mut events);

        events
    }

    /// Green TD's signature: creeps never leave the map. Lineage A ends the
    /// match when there are simply too many of them alive at once. See the
    /// lineage section in `tasks/README.org`; `sim-005` makes the rule
    /// selectable, `audit-012` picks the default.
    fn check_overrun(&mut self, events: &mut Vec<SimEvent>) {
        if self.creeps.len() as u32 > self.overrun_cap() {
            self.over = true;
            events.push(SimEvent::GameOver);
        }
    }
}
