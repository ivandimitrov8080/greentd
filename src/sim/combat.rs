//! Creep movement, leaks, and tower fire: everything one tick does to a creep.
//!
//! The phases of [`Sim::step`] that live here, in order: the creeps that exist
//! move, the creeps that arrived at the goal leak (before anything may shoot
//! them -- `sim-006`), the towers that are off cooldown acquire a target, the
//! aiming towers fire, the shots resolve, and the dead are reaped and paid for.
//!
//! Firing is three passes rather than one, because the order is a contract
//! (`sim-001`): [`Sim::acquire`] picks a target, [`Sim::fire`] commits to the
//! shot and spends the cooldown, and [`Sim::damage`] resolves what was fired.
//! Aiming and applying are separate because a shot is aimed at a creep and the
//! damage loop has to be free to mutate the same map it aimed at.

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

    /// Phase 4 of a tick (`sim-001`): every tower that is off cooldown chooses a
    /// target, and nothing else happens.
    ///
    /// The cooldown ticks down here so a tower that is ready is ready, but it is
    /// not *spent* -- that is [`Sim::fire`] -- and no damage is dealt. `sim-004`
    /// replaces the scan below with a named, indexed policy; giving target
    /// selection a phase of its own is what makes it replaceable without
    /// touching the rest of the tick.
    pub(super) fn acquire(&mut self, dt: f32) -> Vec<Aim> {
        let mut aims: Vec<Aim> = Vec::new();

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

            let origin = self.map.cell_to_world(cell);
            // Classic TD targeting: the creep closest to the goal is the one
            // about to cost a life.
            //
            // The comparison is on *distance remaining*, not on distance
            // travelled: the board's lanes differ in length by a factor of two,
            // so "has walked furthest" and "is nearly home" stop agreeing as
            // soon as two creeps are on different lanes. `sim-004` gives this a
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
            aims.push(Aim {
                cell,
                target,
                center,
                damage,
                splash: stats.splash,
                slow: stats.slow,
                slow_duration: stats.slow_duration,
                cooldown,
                owner: tower.owner,
            });
        }

        aims
    }

    /// Phase 5 of a tick (`sim-001`): the towers [`Sim::acquire`] chose commit
    /// to a shot, and spend their cooldown doing it.
    ///
    /// The cooldown is spent here rather than in `acquire` so that *aiming* and
    /// *firing* are different events, which is what `sim-010` needs: it swaps
    /// the instant hit below for a projectile with a flight time, created here
    /// and resolved a tick later by [`Sim::damage`].
    pub(super) fn fire(&mut self, aims: Vec<Aim>) -> Vec<Shot> {
        let mut shots = Vec::with_capacity(aims.len());
        for aim in aims {
            if let Some(tower) = self.towers.get_mut(&aim.cell) {
                tower.cooldown = aim.cooldown;
            }
            shots.push(Shot {
                target: aim.target,
                center: aim.center,
                damage: aim.damage,
                splash: aim.splash,
                slow: aim.slow,
                slow_duration: aim.slow_duration,
                owner: aim.owner,
            });
        }
        shots
    }

    /// Phase 6 of a tick (`sim-001`): resolve the shots [`Sim::fire`] scheduled.
    ///
    /// A shot lands in the tick it is fired until `sim-010` gives it a flight
    /// time. Every point of damage a tower deals arrives here, through
    /// [`Sim::apply_shot`], so `sim-009` has one place to add armour multipliers,
    /// crits and splash falloff and there is no second path that writes a
    /// creep's health.
    pub(super) fn damage(&mut self, shots: &[Shot]) {
        for shot in shots {
            self.apply_shot(shot);
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

/// One tower's chosen target, and the numbers its shot will use: the value
/// [`Sim::acquire`] hands to [`Sim::fire`]. It never leaves the tick.
pub(super) struct Aim {
    /// The tower that aimed, so `fire` can charge its cooldown.
    cell: IVec2,
    target: CreepId,
    center: Vec2,
    damage: f32,
    splash: f32,
    slow: f32,
    /// How long the slow lasts, from the firing tower's table entry (D18).
    slow_duration: f32,
    /// The cooldown to charge the tower once it has fired.
    cooldown: f32,
    owner: PlayerKey,
}

/// One tower's scheduled shot: the value [`Sim::fire`] hands to [`Sim::damage`].
pub(super) struct Shot {
    target: CreepId,
    center: Vec2,
    damage: f32,
    splash: f32,
    slow: f32,
    /// How long the slow lasts, from the firing tower's table entry (D18).
    slow_duration: f32,
    owner: PlayerKey,
}
