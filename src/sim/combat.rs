//! Creep movement, leaks, and tower fire: everything one tick does to a creep.
//!
//! Three phases, in the order [`Sim::step`] runs them: the creeps that exist
//! move, the creeps that arrived at the goal leak, the towers that are off
//! cooldown shoot, and the dead are reaped and paid for. Firing is resolved in
//! two passes -- collect the shots, then apply them -- because a shot is aimed
//! at a creep and the damage loop has to be free to mutate the same map it aimed
//! at.

use bevy::prelude::*;

use super::{CreepId, PlayerKey, Sim, SimEvent};

impl Sim {
    /// Move every creep along its lane and run its slow timer down.
    pub(super) fn advance_creeps(&mut self, dt: f32) {
        for creep in self.creeps.values_mut() {
            if creep.slow_timer > 0.0 {
                creep.slow_timer -= dt;
            }
            creep.dist += creep.base_speed * creep.speed_mult() * dt;
        }
    }

    /// A creep that has walked its whole lane has reached the goal: it leaves
    /// the match and costs a life.
    ///
    /// This is the mechanic the game is named for. In the original a trigger
    /// fires when a creep enters the goal region, removes the unit, plays a
    /// death effect there and subtracts one from a counter of "chances" -- the
    /// same counter the map starts at 60 and the same one that ends the match at
    /// zero. Here the goal is the end of a lane, so the test is arithmetic
    /// rather than a region event, and the effect is a [`SimEvent::Leaked`] for
    /// the server to turn into a notice and a sound (`11-content.org`).
    ///
    /// Lives are only spent when the rule is [`LoseRule::Lives`], but a leak is
    /// counted and announced either way: under the overrun rule a creep that
    /// escapes is still a thing that happened, and an economy or a multiboard
    /// that wants to show it should not have to ask which rule is on.
    ///
    /// [`LoseRule::Lives`]: crate::data::balance::LoseRule::Lives
    pub(super) fn leak_arrivals(&mut self, events: &mut Vec<SimEvent>) {
        let arrived: Vec<CreepId> = self
            .creeps
            .iter()
            .filter(|(_, c)| c.dist >= self.map.lanes[c.lane as usize].total)
            .map(|(&id, _)| id)
            .collect();

        for id in arrived {
            self.creeps.remove(&id);
            self.leaks += 1;
            if self.lose_rule() == crate::data::balance::LoseRule::Lives {
                self.lives = self.lives.saturating_sub(1);
            }
            events.push(SimEvent::Leaked(id));
        }
    }

    /// Every tower that is off cooldown takes one shot.
    pub(super) fn fire(&mut self, dt: f32) {
        let mut shots: Vec<Shot> = Vec::new();

        for (&cell, tower) in self.towers.iter_mut() {
            tower.cooldown -= dt;
            if tower.cooldown > 0.0 {
                continue;
            }
            let Some(stats) = self.balance.tower(tower.kind) else {
                continue;
            };
            // Tier scales damage and rate modestly. The growth is data, not a
            // literal (D18): `towers-002` will replace this flat model with an
            // explicit tier graph, but until then the table owns every number.
            let steps = tower.level.saturating_sub(1) as f32;
            let tier = 1.0 + stats.damage_per_level * steps;
            let damage = stats.damage * tier;
            let range = stats.range * (1.0 + stats.range_per_level * steps);
            let cooldown = stats.cooldown / tier;
            let (splash, slow) = (stats.splash, stats.slow);

            let origin = self.map.cell_to_world(cell);
            // Classic TD targeting: the creep closest to the goal is the one
            // about to cost a life.
            //
            // The comparison is on *distance remaining*, not on distance
            // travelled: the board's lanes differ in length by a factor of two,
            // so "has walked furthest" and "is nearly home" stop agreeing as
            // soon as two creeps are on different lanes. `sim-010` gives this a
            // name and an index; this is the policy it starts from.
            let mut best: Option<(CreepId, f32, Vec2)> = None;
            for (&id, creep) in self.creeps.iter() {
                if creep.hp <= 0.0 {
                    continue;
                }
                let pos = creep.pos(&self.map);
                if pos.distance(origin) <= range
                    && best.is_none_or(|(_, remaining, _)| creep.remaining(&self.map) < remaining)
                {
                    best = Some((id, creep.remaining(&self.map), pos));
                }
            }
            let Some((target, _, center)) = best else {
                continue;
            };
            tower.cooldown = cooldown;
            shots.push(Shot {
                target,
                center,
                damage,
                splash,
                slow,
                slow_duration: stats.slow_duration,
                owner: tower.owner,
            });
        }

        for shot in shots {
            self.apply_shot(&shot);
        }
    }

    /// Damage one shot's target, and -- for a splash tower -- everything near
    /// the impact point.
    fn apply_shot(&mut self, shot: &Shot) {
        let mut hit: Vec<CreepId> = vec![shot.target];
        if shot.splash > 0.0 {
            for (&id, creep) in self.creeps.iter() {
                if id != shot.target
                    && creep.hp > 0.0
                    && creep.pos(&self.map).distance(shot.center) <= shot.splash
                {
                    hit.push(id);
                }
            }
        }
        for id in hit {
            let Some(creep) = self.creeps.get_mut(&id) else {
                continue;
            };
            if creep.hp <= 0.0 {
                continue;
            }
            creep.hp -= shot.damage;
            creep.last_hit_by = shot.owner;
            if shot.slow < 1.0 {
                creep.slow_mult = creep.slow_mult.min(shot.slow);
                creep.slow_timer = shot.slow_duration;
            }
        }
    }

    /// Remove the creeps that died, pay whoever killed them, and announce them.
    pub(super) fn reap(&mut self, events: &mut Vec<SimEvent>) {
        let dead: Vec<(CreepId, PlayerKey, u32)> = self
            .creeps
            .iter()
            .filter(|(_, c)| c.hp <= 0.0)
            .map(|(&id, c)| (id, c.last_hit_by, c.bounty))
            .collect();

        for (id, owner, bounty) in dead {
            self.creeps.remove(&id);
            self.credit_kill(owner, bounty);
            events.push(SimEvent::CreepKilled(id));
        }
    }
}

/// One tower's shot, resolved after every tower has been given the chance to
/// fire.
struct Shot {
    target: CreepId,
    center: Vec2,
    damage: f32,
    splash: f32,
    slow: f32,
    /// How long the slow lasts, from the firing tower's table entry (D18).
    slow_duration: f32,
    owner: PlayerKey,
}
