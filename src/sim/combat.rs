//! Creep movement and tower fire: everything one tick does to a creep.
//!
//! Three phases, in the order [`Sim::step`] runs them: the creeps that exist
//! move, the towers that are off cooldown shoot, and the dead are reaped and
//! paid for. Firing is resolved in two passes -- collect the shots, then apply
//! them -- because a shot is aimed at a creep and the damage loop has to be free
//! to mutate the same map it aimed at.

use bevy::prelude::*;

use crate::map::cell_to_world;

use super::{CreepId, PlayerKey, Sim, SimEvent};

impl Sim {
    /// Move every creep along the path and run its slow timer down.
    pub(super) fn advance_creeps(&mut self, dt: f32) {
        for creep in self.creeps.values_mut() {
            if creep.slow_timer > 0.0 {
                creep.slow_timer -= dt;
            }
            creep.dist += creep.base_speed * creep.speed_mult() * dt;
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
            // Tier scales damage and rate modestly.
            let tier = 1.0 + 0.45 * (tower.level.saturating_sub(1)) as f32;
            let damage = stats.damage * tier;
            let range = stats.range * (1.0 + 0.04 * (tower.level.saturating_sub(1)) as f32);
            let cooldown = stats.cooldown / tier;
            let (splash, slow) = (stats.splash, stats.slow);

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
            tower.cooldown = cooldown;
            shots.push(Shot {
                target,
                center,
                damage,
                splash,
                slow,
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
                    && creep.pos(&self.path).distance(shot.center) <= shot.splash
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
                creep.slow_timer = 1.5;
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
    owner: PlayerKey,
}
