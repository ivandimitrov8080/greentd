//! The wave loop: when a wave starts, who is in it, and who may summon one.
//!
//! Green TD runs forever, and a wave arrives on *every lane*: the real board's
//! wave triggers create units at all of its spawn regions whether or not a human
//! holds that colour, so a wave is not "your" creeps, it is the board's. One
//! wave is therefore `lanes` groups of creeps, each entering at its own spawn
//! point and walking the same walk to the same goal.

use crate::data::reject::Reject;

use super::{Creep, CreepId, PlayerKey, Sim, SimEvent};

impl Sim {
    /// Creeps one lane sends in the next wave.
    ///
    /// Faithful to the original in spirit: a bigger lobby sends more creeps per
    /// lane, because a bigger lobby has more towers to stop them.
    pub fn creeps_per_lane(&self) -> u32 {
        let players = self.players_connected().max(1);
        self.balance.creeps_per_lane(self.wave, players)
    }

    /// Creeps the next wave will put on the board: every lane's share, summed.
    pub fn wave_size(&self) -> u32 {
        self.creeps_per_lane() * self.lane_count()
    }

    /// How many lanes the board has.
    pub fn lane_count(&self) -> u32 {
        self.map.lanes.len() as u32
    }

    /// Skip the rest of the wave timer, if the caller is allowed to (D6).
    ///
    /// The rule, written down here because `audit-006` asks for it: *any player
    /// in the match* may call the next wave, because calling a wave is a
    /// shared, cooperative action on a board every player is defending, but each
    /// player may only do so once every `match_rules.call_wave_cooldown`
    /// seconds. The cooldown is enough to stop the exploit the defect describes
    /// -- one client zeroing the timer every frame to drag the table past the
    /// lose condition -- without inventing a gold or food cost the original does
    /// not have.
    ///
    /// A refusal is never silent: an unknown caller is `NotInMatch`, a caller
    /// inside its cooldown is `RateLimited`, and a finished match is
    /// `MatchOver`.
    pub fn call_wave(&mut self, who: PlayerKey) -> Result<(), Reject> {
        if self.over {
            return Err(Reject::MatchOver);
        }
        let cooldown = self.balance.match_rules.call_wave_cooldown;
        {
            let player = self.players.get_mut(&who).ok_or(Reject::NotInMatch)?;
            if player.wave_call_cooldown > 0.0 {
                return Err(Reject::RateLimited);
            }
            player.wave_call_cooldown = cooldown;
        }
        self.wave_timer = 0.0;
        Ok(())
    }

    /// Phase 1 of a tick (`sim-001`): the wave clock, the per-player wave-call
    /// cooldowns, and the creeps of any wave that is due.
    ///
    /// One named phase rather than two inline calls, because `step`'s job is to
    /// name the order and this is the part of it that decides what exists. A
    /// wave summoned this tick exists this tick, which is the property the rest
    /// of the tick is built on.
    pub(super) fn spawn(&mut self, dt: f32, events: &mut Vec<SimEvent>) {
        self.tick_wave(dt, events);
        self.tick_wave_calls(dt);
    }

    /// Run the wave timer down, and start a wave when it expires.
    pub(super) fn tick_wave(&mut self, dt: f32, events: &mut Vec<SimEvent>) {
        self.wave_timer -= dt;
        if self.wave_timer > 0.0 {
            return;
        }
        let (wave, spawned) = self.start_wave();
        events.push(SimEvent::WaveStarted(wave));
        for id in spawned {
            events.push(SimEvent::CreepSpawned(id));
        }
    }

    /// Run every player's wave-call cooldown down.
    ///
    /// Match time, not wall-clock time, so a replay sees the same refusals (D6).
    pub(super) fn tick_wave_calls(&mut self, dt: f32) {
        for player in self.players.values_mut() {
            player.wave_call_cooldown = (player.wave_call_cooldown - dt).max(0.0);
        }
    }

    /// Returns the new wave number and the creeps it spawned.
    ///
    /// Every lane gets its share, and within a lane the creeps enter in a
    /// **column**: at the spawn point, `spawn_gap` apart, with a bounded jitter
    /// so the column is not a rigid comb.
    ///
    /// This is `audit-013` (D10). A wave used to be spread evenly around a
    /// closed ring, which gave it no front, made it arrive from every direction
    /// at once, and could place a creep next to the destination on the tick it
    /// spawned. A lane has a start, so a wave now has one too: it enters at the
    /// start and walks to the goal, and nothing spawns within `spawn_arc` of
    /// anything but the spawn point.
    ///
    /// The column is compressed when a late wave has more creeps than the gap
    /// alone would fit in `spawn_arc`, so the *shape* of the arrival holds even
    /// when the count grows -- and so a wave can never start near the goal.
    fn start_wave(&mut self) -> (u32, Vec<CreepId>) {
        self.wave += 1;
        self.wave_timer = self.balance.match_rules.wave_interval;

        let n = self.creeps_per_lane();
        let hp = self.balance.creep_hp(self.wave);
        let speed = self.balance.creep_speed(self.wave);
        let bounty = self.balance.creep_bounty(self.wave);

        let scaling = &self.balance.waves.scaling;
        let gap = column_gap(n, scaling.spawn_gap, scaling.spawn_arc);
        let jitter = gap * scaling.spawn_jitter;

        // Copied out of `self` because `Rng` is `Copy`: the loop needs to write
        // `self.creeps` and `self.next_id` at the same time as it draws.
        let mut rng = self.rng;
        let mut spawned = Vec::with_capacity((n as usize) * self.map.lanes.len());
        for lane in 0..self.map.lanes.len() {
            for i in 0..n {
                let id = CreepId(self.next_id);
                self.next_id += 1;
                spawned.push(id);
                self.creeps.insert(
                    id,
                    Creep {
                        lane: lane as u8,
                        dist: i as f32 * gap + rng.range_f32(-jitter, jitter),
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
        }
        self.rng = rng;
        (self.wave, spawned)
    }
}

/// How far apart to space `count` creeps so that the column fits in `arc`.
///
/// The gap the table asks for, unless that would push the last creep further
/// from the spawn point than the arc allows -- in which case the gap shrinks to
/// fit. A single creep needs no room at all (there is no second creep to keep
/// away from), so the answer for one creep is the table's gap, whether or not it
/// would fit.
fn column_gap(count: u32, gap: f32, arc: f32) -> f32 {
    let steps = count.saturating_sub(1) as f32;
    if steps <= 0.0 {
        return gap;
    }
    gap.min(arc / steps)
}

/// A lane's creeps stand in a column from the spawn point; this is the length
/// of that column, for a test that wants to assert the shape of an arrival.
#[cfg(test)]
fn column_length(count: u32, gap: f32, arc: f32) -> f32 {
    column_gap(count, gap, arc) * count.saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_never_starts_past_its_arc() {
        assert_eq!(column_length(1, 40.0, 1600.0), 0.0);
        assert_eq!(column_length(2, 40.0, 1600.0), 40.0);
        // Past the arc the gap shrinks rather than the column overrunning.
        assert!((column_length(100, 40.0, 1600.0) - 1600.0).abs() < 0.01);
        assert!(column_gap(100, 40.0, 1600.0) < 40.0);
        // Exactly at the arc the table's gap is kept.
        assert_eq!(column_length(41, 40.0, 1600.0), 1600.0);
    }
}
