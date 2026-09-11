//! The wave loop: when a wave starts, who is in it, and who may summon one.
//!
//! Green TD runs forever, and a wave is a fixed number of creeps spread around
//! a closed ring. Both of those are properties of the wave, not of the creep, so
//! they live here rather than in `mod.rs`.

use crate::data::reject::Reject;

use super::{Creep, CreepId, PlayerKey, Sim, SimEvent};

impl Sim {
    /// Creep count for the next wave, for the players currently connected.
    ///
    /// Faithful to the original: every player has their own spawn point, so a
    /// bigger lobby means more creeps. Solo would be trivially easy otherwise.
    pub fn creeps_per_wave(&self) -> u32 {
        let players = self.players_connected().max(1);
        self.balance.creeps_per_wave(self.wave, players)
    }

    /// Skip the rest of the wave timer, if the caller is allowed to (D6).
    ///
    /// The rule, written down here because `audit-006` asks for it: *any player
    /// in the match* may call the next wave, because calling a wave is a
    /// shared, cooperative action in a map where every player spawns into the
    /// same ring, but each player may only do so once every
    /// `match_rules.call_wave_cooldown` seconds. The cooldown is enough to stop
    /// the exploit the defect describes -- one client zeroing the timer every
    /// frame to drag the table to the overrun cap -- without inventing a gold
    /// or food cost the original does not have.
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
    fn start_wave(&mut self) -> (u32, Vec<CreepId>) {
        self.wave += 1;
        self.wave_timer = self.balance.match_rules.wave_interval;

        let n = self.creeps_per_wave();
        let hp = self.balance.creep_hp(self.wave);
        let speed = self.balance.creep_speed(self.wave);
        let bounty = self.balance.creep_bounty(self.wave);

        // Spread the wave evenly around the loop so it reads as a single
        // "pulse" of creeps rather than one clump, with a bounded jitter so the
        // pulse is not a rigid comb.
        //
        // The jitter is strictly less than half a slot -- the balance loader
        // refuses anything else -- so it can never reorder the wave; it only
        // decides where within its own slot each creep stands. It is also the
        // seed's only consumer so far, which is what makes `match_rules.seed`
        // observable at all (`found-009`, and the chaos waves of `waves-002`).
        let spacing = self.path.total / n as f32;
        let jitter = spacing * self.balance.waves.scaling.spawn_jitter;

        // Copied out of `self` because `Rng` is `Copy`: the loop needs to write
        // `self.creeps` and `self.next_id` at the same time as it draws.
        let mut rng = self.rng;
        let mut spawned = Vec::with_capacity(n as usize);
        for i in 0..n {
            let id = CreepId(self.next_id);
            self.next_id += 1;
            spawned.push(id);
            self.creeps.insert(
                id,
                Creep {
                    dist: spacing * i as f32 + rng.range_f32(-jitter, jitter),
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
        self.rng = rng;
        (self.wave, spawned)
    }
}
