//! The authoritative simulation. Server-only.
//!
//! This module deliberately knows nothing about networking. It is a plain
//! `step(dt)` state machine that can be unit-tested headless and, if you ever
//! want lockstep, replayed. The server's job in `server.rs` is to (a) feed it
//! validated intents and (b) mirror its state into replicated entities.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;

use crate::balance::{Balance, BalanceData};
use crate::game::*;
use crate::map::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct CreepId(pub u32);

/// Stable identity for a player. We key on the peer's address because that is
/// the only durable identity raw UDP gives us.
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
        let mut sim = Self {
            balance,
            path: Path::default(),
            creeps: HashMap::new(),
            towers: HashMap::new(),
            players: HashMap::new(),
            wave: 0,
            wave_timer: 0.0,
            next_id: 1,
            over: false,
        };
        // Wave 1 arrives promptly rather than after a full interval.
        sim.wave_timer = sim.balance.match_rules.first_wave_delay;
        sim
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
            phase: self.phase().as_u8(),
        }
    }

    pub fn creeps_per_wave(&self) -> u32 {
        // Faithful to the original: every player has their own spawn point, so
        // a bigger lobby means more creeps. Solo would be trivially easy
        // otherwise.
        let players = self.players.values().filter(|p| p.connected).count().max(1) as u32;
        self.balance.creeps_per_wave(self.wave, players)
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
    // Intent handlers. Every one of these returns whether it was accepted, so
    // the server can send the player a reason it failed.
    //
    // Two rules hold for all of them (D1, D2, D8):
    //
    //   1. The caller must already have a player record. Nothing here ever
    //      inserts one: a `Player` is created by `add_player`, with the table's
    //      starting gold, and by nothing else.
    //   2. A tower may only be touched by its owner, and refunds go to the
    //      owner -- which, given rule 1 and the check, is also the caller.
    // -----------------------------------------------------------------------

    pub fn try_build(&mut self, who: PlayerKey, cell: IVec2, kind: u8) -> Result<(), Reject> {
        if self.over {
            return Err(Reject::MatchOver);
        }
        if !self.players.contains_key(&who) {
            return Err(Reject::NotInMatch);
        }
        let Some(tower) = self.balance.tower(kind) else {
            return Err(Reject::BadKind);
        };
        let cost = tower.cost;
        if !in_bounds(cell) {
            return Err(Reject::OutOfBounds);
        }
        if !self.is_buildable(cell) {
            return Err(Reject::PathBlocked);
        }
        if self.towers.contains_key(&cell) {
            return Err(Reject::Occupied);
        }
        let player = self.players.get_mut(&who).ok_or(Reject::NotInMatch)?;
        if player.gold < cost {
            return Err(Reject::NotEnoughGold);
        }
        player.gold -= cost;
        self.towers.insert(
            cell,
            Tower {
                kind,
                level: 1,
                owner: who,
                cooldown: 0.0,
            },
        );
        Ok(())
    }

    /// Upgrade cost scales with the tier, so going tall is a real commitment.
    /// The curve itself lives in the tower's balance entry.
    pub fn upgrade_cost(&self, kind: u8, level: u8) -> u32 {
        match self.balance.tower(kind) {
            Some(tower) => self.balance.upgrade_cost(tower, level),
            None => 0,
        }
    }

    pub fn try_upgrade(&mut self, who: PlayerKey, cell: IVec2) -> Result<(), Reject> {
        if self.over {
            return Err(Reject::MatchOver);
        }
        if !self.players.contains_key(&who) {
            return Err(Reject::NotInMatch);
        }
        // Ownership is checked before anything is spent (D1).
        let cost = {
            let t = self.towers.get(&cell).ok_or(Reject::NoSuchTower)?;
            if t.owner != who {
                return Err(Reject::NotOwner);
            }
            let tower = self.balance.tower(t.kind).ok_or(Reject::BadKind)?;
            self.balance.upgrade_cost(tower, t.level)
        };
        let player = self.players.get_mut(&who).ok_or(Reject::NotInMatch)?;
        if player.gold < cost {
            return Err(Reject::NotEnoughGold);
        }
        player.gold -= cost;
        if let Some(t) = self.towers.get_mut(&cell) {
            t.level += 1;
        }
        Ok(())
    }

    pub fn try_sell(&mut self, who: PlayerKey, cell: IVec2) -> Result<(), Reject> {
        if !self.players.contains_key(&who) {
            return Err(Reject::NotInMatch);
        }
        let (refund, owner) = {
            let t = self.towers.get(&cell).ok_or(Reject::NoSuchTower)?;
            if t.owner != who {
                return Err(Reject::NotOwner);
            }
            // 70% refund, as in several of the original variants.
            let tower = self.balance.tower(t.kind).ok_or(Reject::BadKind)?;
            (self.balance.sell_refund(tower, t.level), t.owner)
        };
        if self.towers.remove(&cell).is_none() {
            return Err(Reject::NoSuchTower);
        }
        // The refund goes to the tower's owner, which the check above has just
        // proven is the caller (D2).
        if let Some(player) = self.players.get_mut(&owner) {
            player.gold += refund;
        }
        Ok(())
    }

    pub fn call_wave(&mut self) -> Result<(), Reject> {
        if self.over {
            return Err(Reject::MatchOver);
        }
        self.wave_timer = 0.0;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Stepping
    // -----------------------------------------------------------------------

    /// Returns the new wave number and the creeps it spawned.
    fn start_wave(&mut self) -> (u32, Vec<CreepId>) {
        self.wave += 1;
        self.wave_timer = self.balance.match_rules.wave_interval;

        let n = self.creeps_per_wave();
        let hp = self.balance.creep_hp(self.wave);
        let speed = self.balance.creep_speed(self.wave);
        let bounty = self.balance.creep_bounty(self.wave);

        // Spread the wave evenly around the loop so it reads as a single
        // "pulse" of creeps rather than one clump.
        let spacing = self.path.total / n as f32;
        let mut spawned = Vec::with_capacity(n as usize);
        for i in 0..n {
            let id = CreepId(self.next_id);
            self.next_id += 1;
            spawned.push(id);
            self.creeps.insert(
                id,
                Creep {
                    dist: spacing * i as f32,
                    hp,
                    max_hp: hp,
                    base_speed: speed,
                    slow_mult: 1.0,
                    slow_timer: 0.0,
                    bounty,
                    last_hit_by: PlayerKey(0),
                },
            );
        }
        (self.wave, spawned)
    }

    pub fn step(&mut self, dt: f32) -> Vec<SimEvent> {
        let mut events = Vec::new();
        if self.over {
            return events;
        }

        self.wave_timer -= dt;
        if self.wave_timer <= 0.0 {
            let (w, spawned) = self.start_wave();
            events.push(SimEvent::WaveStarted(w));
            for id in spawned {
                events.push(SimEvent::CreepSpawned(id));
            }
        }

        // --- move ---
        for c in self.creeps.values_mut() {
            if c.slow_timer > 0.0 {
                c.slow_timer -= dt;
            }
            c.dist += c.base_speed * c.speed_mult() * dt;
        }

        // --- fire ---
        // Collect shots first so we don't hold a borrow on `creeps` while
        // mutating it.
        struct Shot {
            target: CreepId,
            center: Vec2,
            damage: f32,
            splash: f32,
            slow: f32,
            owner: PlayerKey,
        }
        let mut shots: Vec<Shot> = Vec::new();
        for (&cell, tower) in self.towers.iter_mut() {
            tower.cooldown -= dt;
            if tower.cooldown > 0.0 {
                continue;
            }
            let Some(stats) = self.balance.tower(tower.kind) else {
                continue;
            };
            // Tier scales damage and rate modestly.
            let tier = 1.0 + 0.45 * (tower.level.saturating_sub(1)) as f32;
            let damage = stats.damage * tier;
            let range = stats.range * (1.0 + 0.04 * (tower.level.saturating_sub(1)) as f32);

            let origin = cell_to_world(cell);
            // Classic TD targeting: the creep furthest along the path is the
            // one about to leak (or, here, the one most likely to survive).
            let mut best: Option<(CreepId, f32, Vec2)> = None;
            for (&id, creep) in self.creeps.iter() {
                if creep.hp <= 0.0 {
                    continue;
                }
                let pos = creep.pos(&self.path);
                if pos.distance(origin) <= range && best.is_none_or(|(_, far, _)| creep.dist > far)
                {
                    best = Some((id, creep.dist, pos));
                }
            }
            let Some((target, _, center)) = best else {
                continue;
            };
            tower.cooldown = stats.cooldown / tier;
            shots.push(Shot {
                target,
                center,
                damage,
                splash: stats.splash,
                slow: stats.slow,
                owner: tower.owner,
            });
        }

        // --- apply damage ---
        for shot in shots {
            // Splash hits everything near the impact point, plus the target.
            let mut hit: Vec<CreepId> = vec![shot.target];
            if shot.splash > 0.0 {
                for (&id, creep) in self.creeps.iter() {
                    if id != shot.target
                        && creep.hp > 0.0
                        && creep.pos(&self.path).distance(shot.center) <= shot.splash
                    {
                        hit.push(id);
                    }
                }
            }
            for id in hit {
                if let Some(c) = self.creeps.get_mut(&id) {
                    if c.hp <= 0.0 {
                        continue;
                    }
                    c.hp -= shot.damage;
                    c.last_hit_by = shot.owner;
                    if shot.slow < 1.0 {
                        c.slow_mult = c.slow_mult.min(shot.slow);
                        c.slow_timer = 1.5;
                    }
                }
            }
        }

        // --- reap ---
        let dead: Vec<(CreepId, PlayerKey, u32)> = self
            .creeps
            .iter()
            .filter(|(_, c)| c.hp <= 0.0)
            .map(|(&id, c)| (id, c.last_hit_by, c.bounty))
            .collect();
        for (id, owner, bounty) in dead {
            self.creeps.remove(&id);
            if let Some(p) = self.players.get_mut(&owner) {
                p.gold += bounty;
                p.kills += 1;
            }
            events.push(SimEvent::CreepKilled(id));
        }

        // --- lose condition ---
        // Green TD's signature: creeps never leave the map. Lineage A ends the
        // match when there are simply too many of them alive at once. See the
        // lineage section in `tasks/README.org`; `sim-005` makes the rule
        // selectable, `audit-012` picks the default.
        if self.creeps.len() as u32 > self.overrun_cap() {
            self.over = true;
            events.push(SimEvent::GameOver);
        }

        events
    }
}
