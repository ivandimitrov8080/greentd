//! The headless simulation harness, and the first tests that use it.
//!
//! This is the safety net `tasks/12-testing.org` `test-001` and
//! `tasks/01-current-code-audit.org` `audit-010` ask for, and it closes D17
//! (zero tests). It builds a `Sim` with **no `App`, no window, no GPU and no
//! socket**, drives it with a fixed `dt`, and asserts on both the resulting
//! state and the `SimEvent`s of each individual tick. Every sim-mutating audit
//! task adds its test here, because here is where the harness is.
//!
//! Every number a test asserts on is read back from the balance tables rather
//! than repeated as a literal, so a re-tune changes the expected value in one
//! place (see `found-004`).

use std::sync::Arc;

use bevy::prelude::IVec2;
use greentd::balance::BalanceData;
use greentd::game::{Phase, Reject, ServerNotice};
use greentd::map::{GRID_H, GRID_W, in_bounds};
use greentd::sim::{PlayerKey, Sim, SimEvent};

/// One simulation tick. The server runs at 30 Hz; the sim itself only requires
/// that `dt` is fixed.
const TICK: f32 = 1.0 / 30.0;

/// The tables that ship in `assets/balance`.
fn shipped_balance() -> Arc<BalanceData> {
    Arc::new(BalanceData::shipped().expect("assets/balance must load"))
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Everything a test needs to drive a match by hand.
struct Harness {
    sim: Sim,
    tick: f32,
}

impl Harness {
    fn new() -> Self {
        Self {
            sim: Sim::new(shipped_balance()),
            tick: TICK,
        }
    }

    /// A match running on modified tables. Used to make the retuned numbers
    /// observable without waiting for a real 150-creep overrun.
    fn with_balance(balance: Arc<BalanceData>) -> Self {
        Self {
            sim: Sim::new(balance),
            tick: TICK,
        }
    }

    /// A harness with `ids` joined, mirroring what the server does on connect.
    fn with_players(ids: &[u64]) -> Self {
        let mut harness = Self::new();
        for id in ids {
            harness.sim.add_player(player(*id));
        }
        harness
    }

    /// One tick, and the events that tick produced.
    fn step(&mut self) -> Vec<SimEvent> {
        self.sim.step(self.tick)
    }

    /// `ticks` ticks, and every event produced along the way.
    fn advance(&mut self, ticks: u32) -> Vec<SimEvent> {
        let mut events = Vec::new();
        for _ in 0..ticks {
            events.extend(self.step());
        }
        events
    }

    /// Step until `ready` holds, returning how many ticks that took and every
    /// event seen on the way. Panics rather than looping forever.
    fn advance_until(&mut self, limit: u32, ready: impl Fn(&Sim) -> bool) -> (u32, Vec<SimEvent>) {
        let mut events = Vec::new();
        for ticks in 0..limit {
            if ready(&self.sim) {
                return (ticks, events);
            }
            events.extend(self.step());
        }
        panic!("condition not met within {limit} ticks");
    }
}

fn player(id: u64) -> PlayerKey {
    PlayerKey(id)
}

fn gold(sim: &Sim, who: PlayerKey) -> u32 {
    sim.players.get(&who).map(|p| p.gold).unwrap_or_default()
}

fn kills(sim: &Sim, who: PlayerKey) -> u32 {
    sim.players.get(&who).map(|p| p.kills).unwrap_or_default()
}

fn count_matching(events: &[SimEvent], pred: impl Fn(&SimEvent) -> bool) -> usize {
    events.iter().filter(|event| pred(event)).count()
}

/// A buildable cell that has a path cell for a neighbour, so a tower placed
/// there actually has something to shoot at.
fn cell_beside_the_path(sim: &Sim) -> IVec2 {
    for y in 0..GRID_H {
        for x in 0..GRID_W {
            let cell = IVec2::new(x, y);
            if !sim.is_buildable(cell) {
                continue;
            }
            let neighbours = [
                IVec2::new(x + 1, y),
                IVec2::new(x - 1, y),
                IVec2::new(x, y + 1),
                IVec2::new(x, y - 1),
            ];
            if neighbours
                .iter()
                .any(|n| in_bounds(*n) && !sim.is_buildable(*n))
            {
                return cell;
            }
        }
    }
    panic!("the map has no buildable cell beside the path");
}

// ---------------------------------------------------------------------------
// The harness itself
// ---------------------------------------------------------------------------

#[test]
fn a_sim_can_be_built_and_stepped_with_no_app() {
    let mut harness = Harness::new();

    assert_eq!(harness.sim.wave, 0);
    assert_eq!(harness.sim.creeps_alive(), 0);
    assert!(harness.sim.players.is_empty());

    let events = harness.advance(1);
    assert!(events.is_empty(), "nothing happens on the very first tick");
}

#[test]
fn a_wave_spawns_after_the_configured_delay() {
    let mut harness = Harness::new();
    let delay = harness.sim.balance.match_rules.first_wave_delay;
    let expected_ticks = (delay / TICK).ceil() as u32;

    let (ticks, events) = harness.advance_until(600, |sim| sim.wave == 1);

    assert_eq!(harness.sim.wave, 1, "wave 1 must have started");
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::WaveStarted(1))),
        1,
        "exactly one WaveStarted event"
    );

    let spawned = harness.sim.creeps_alive();
    assert_eq!(
        spawned,
        harness.sim.balance.creeps_per_wave(1, 1),
        "a wave spawns the table's count"
    );
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::CreepSpawned(_))),
        spawned as usize
    );

    // One tick of slack: the timer is stepped before it is compared, so float
    // accumulation decides whether the wave lands on tick N or N + 1.
    assert!(
        ticks.abs_diff(expected_ticks) <= 1,
        "wave 1 arrived at tick {ticks}, the table implies about {expected_ticks}"
    );
}

#[test]
fn spawned_creeps_use_the_wave_table() {
    let mut harness = Harness::new();
    harness.advance_until(600, |sim| sim.wave == 1);

    let hp = harness.sim.balance.creep_hp(1);
    let speed = harness.sim.balance.creep_speed(1);
    let bounty = harness.sim.balance.creep_bounty(1);

    assert!(!harness.sim.creeps.is_empty());
    for creep in harness.sim.creeps.values() {
        assert_eq!(creep.hp, hp);
        assert_eq!(creep.max_hp, hp);
        assert_eq!(creep.base_speed, speed);
        assert_eq!(creep.bounty, bounty);
    }
}

#[test]
fn a_tower_kills_a_creep_and_credits_its_owner() {
    let who = player(7);
    let mut harness = Harness::with_players(&[7]);

    let start_gold = harness.sim.balance.match_rules.start_gold;
    let cost = harness
        .sim
        .balance
        .tower(0)
        .map(|tower| tower.cost)
        .expect("kind 0 is in the tower table");
    let bounty = harness.sim.balance.creep_bounty(1);

    let cell = cell_beside_the_path(&harness.sim);
    harness
        .sim
        .try_build(who, cell, 0)
        .expect("the first build must be accepted");
    assert_eq!(gold(&harness.sim, who), start_gold - cost);

    // A lap takes about 50 s, so allow generous headroom for the creep to come
    // into range.
    let (_, events) = harness.advance_until(60 * 30, |sim| kills(sim, who) > 0);

    assert_eq!(kills(&harness.sim, who), 1);
    assert_eq!(
        gold(&harness.sim, who),
        start_gold - cost + bounty,
        "the bounty goes to the tower's owner"
    );
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::CreepKilled(_))),
        1
    );
}

#[test]
fn exceeding_the_overrun_cap_ends_the_match() {
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.overrun_cap = 3;
    let mut harness = Harness::with_balance(Arc::new(tuned));

    let (_, events) = harness.advance_until(600, |sim| sim.over);

    assert!(harness.sim.over, "the match must be over");
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::GameOver)),
        1,
        "game over is announced once"
    );
    assert!(
        harness.sim.creeps_alive() > harness.sim.overrun_cap(),
        "the cap is what ended it"
    );
    assert!(harness.step().is_empty(), "a finished match does not step");
}

#[test]
fn the_sim_reads_the_tables_rather_than_a_const() {
    let mut tuned = (*shipped_balance()).clone();
    tuned.waves.scaling.hp_base = 7.0;
    tuned.waves.scaling.speed_base = 100.0;
    tuned.waves.scaling.bounty_base = 3.0;
    tuned.match_rules.wave_interval = 1.0;
    let mut harness = Harness::with_balance(Arc::new(tuned));

    harness.advance_until(600, |sim| sim.wave == 1);

    assert!(!harness.sim.creeps.is_empty());
    for creep in harness.sim.creeps.values() {
        assert_eq!(creep.hp, 7.0, "hp comes from the in-memory table");
        assert_eq!(creep.base_speed, 100.0);
        assert_eq!(creep.bounty, 3);
    }
    assert_eq!(
        harness.sim.wave_timer, 1.0,
        "the wave interval comes from the in-memory table"
    );
}

// ---------------------------------------------------------------------------
// Reject (audit-001)
// ---------------------------------------------------------------------------

#[test]
fn every_reject_variant_round_trips_with_a_message() {
    for reject in Reject::ALL {
        let notice = ServerNotice::Err(reject);
        let encoded = ron::to_string(&notice).expect("ServerNotice serialises");
        let decoded: ServerNotice = ron::from_str(&encoded).expect("ServerNotice deserialises");

        let ServerNotice::Err(back) = decoded else {
            panic!("{notice:?} came back as {decoded:?}");
        };
        assert_eq!(back, reject, "{encoded} did not round-trip");
        assert!(
            !back.text().is_empty(),
            "{reject:?} has no player-readable message"
        );
    }
}

// ---------------------------------------------------------------------------
// Ownership and identity (audit-002, audit-008)
// ---------------------------------------------------------------------------

#[test]
fn a_foreign_player_cannot_upgrade_or_sell_someone_elses_tower() {
    let (a, b) = (player(1), player(2));
    let mut harness = Harness::with_players(&[1, 2]);

    let cell = cell_beside_the_path(&harness.sim);
    harness.sim.try_build(a, cell, 0).expect("the first build");

    let a_gold = gold(&harness.sim, a);
    let b_gold = gold(&harness.sim, b);
    let level = harness
        .sim
        .towers
        .get(&cell)
        .map(|tower| tower.level)
        .expect("A's tower exists");

    assert_eq!(harness.sim.try_upgrade(b, cell), Err(Reject::NotOwner));
    assert_eq!(harness.sim.try_sell(b, cell), Err(Reject::NotOwner));

    // Nothing at all may have changed on the rejected path.
    assert_eq!(gold(&harness.sim, a), a_gold, "A's gold is untouched");
    assert_eq!(gold(&harness.sim, b), b_gold, "B minted nothing");
    assert_eq!(
        harness.sim.towers.get(&cell).map(|tower| tower.level),
        Some(level),
        "the tower was not upgraded"
    );
    assert!(
        harness.sim.towers.contains_key(&cell),
        "the tower was not sold"
    );
}

#[test]
fn selling_pays_the_owner_the_table_refund() {
    let who = player(1);
    let mut harness = Harness::with_players(&[1]);
    let start_gold = harness.sim.balance.match_rules.start_gold;

    let cell = cell_beside_the_path(&harness.sim);
    harness
        .sim
        .try_build(who, cell, 0)
        .expect("the first build");

    let (kind, level) = {
        let tower = harness.sim.towers.get(&cell).expect("the tower exists");
        (tower.kind, tower.level)
    };
    let (cost, refund) = {
        let tower = harness
            .sim
            .balance
            .tower(kind)
            .expect("kind 0 is in the table");
        (tower.cost, harness.sim.balance.sell_refund(tower, level))
    };

    harness.sim.try_sell(who, cell).expect("the owner may sell");
    assert!(harness.sim.towers.is_empty());
    assert_eq!(
        gold(&harness.sim, who),
        start_gold - cost + refund,
        "the sale is cost minus the table's refund"
    );
}

#[test]
fn a_command_from_an_unregistered_key_changes_nothing() {
    let mut harness = Harness::with_players(&[1]);
    let stranger = player(999);
    let cell = cell_beside_the_path(&harness.sim);

    let players_before = harness.sim.players.len();
    let gold_before = gold(&harness.sim, player(1));

    assert_eq!(
        harness.sim.try_build(stranger, cell, 0),
        Err(Reject::NotInMatch)
    );
    assert_eq!(
        harness.sim.try_upgrade(stranger, cell),
        Err(Reject::NotInMatch)
    );
    assert_eq!(
        harness.sim.try_sell(stranger, cell),
        Err(Reject::NotInMatch)
    );
    assert_eq!(harness.sim.call_wave(stranger), Err(Reject::NotInMatch));

    assert_eq!(
        harness.sim.players.len(),
        players_before,
        "no player record may be minted for an unknown key"
    );
    assert!(!harness.sim.players.contains_key(&stranger));
    assert!(harness.sim.towers.is_empty(), "no tower was built");
    assert_eq!(gold(&harness.sim, player(1)), gold_before);
}

#[test]
fn no_intent_handler_produces_a_zero_gold_player() {
    let mut harness = Harness::with_players(&[1]);
    let stranger = player(999);
    let start_gold = harness.sim.balance.match_rules.start_gold;
    let kinds = harness.sim.balance.tower_kind_count();
    let cell = cell_beside_the_path(&harness.sim);

    // Every kind, including one past the end of the table, from a key that was
    // never registered.
    for kind in 0..=kinds {
        let _ = harness.sim.try_build(stranger, cell, kind);
        let _ = harness.sim.try_upgrade(stranger, cell);
        let _ = harness.sim.try_sell(stranger, cell);
        let _ = harness.sim.call_wave(stranger);
    }

    assert_eq!(harness.sim.players.len(), 1, "only the registered player");
    for (key, record) in harness.sim.players.iter() {
        assert_eq!(record.gold, start_gold, "{key:?} has the table's gold");
    }
}

// ---------------------------------------------------------------------------
// Call wave (audit-006)
// ---------------------------------------------------------------------------

#[test]
fn a_player_cannot_call_two_waves_inside_the_cooldown() {
    let who = player(1);
    let mut harness = Harness::with_players(&[1]);
    let cooldown = harness.sim.balance.match_rules.call_wave_cooldown;
    assert!(
        cooldown > 0.0,
        "the table must give the cooldown a positive value for this test to mean anything"
    );

    // The first call is accepted, and zeroes the timer so the next `step`
    // starts the wave.
    harness.advance(1);
    harness.sim.call_wave(who).expect("the first call is allowed");
    assert_eq!(harness.sim.wave_timer, 0.0);

    let events = harness.step();
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::WaveStarted(1))),
        1,
        "the called wave is wave 1"
    );
    let armed = harness.sim.wave_timer;
    assert!(
        armed > 0.0,
        "starting a wave re-arms the timer from the table, not from a literal"
    );

    // The second call, inside the cooldown window, is refused and changes
    // nothing at all: the timer keeps the value the wave just gave it.
    assert_eq!(harness.sim.call_wave(who), Err(Reject::RateLimited));
    assert_eq!(
        harness.sim.wave_timer, armed,
        "a refused call must not touch the wave timer"
    );

    // Once the cooldown has run out the same player may call again.
    let ticks = (cooldown / TICK).ceil() as u32 + 2;
    harness.advance(ticks);
    harness.sim
        .call_wave(who)
        .expect("the cooldown has expired");
    assert_eq!(
        harness.sim.wave_timer, 0.0,
        "an accepted call zeroes the timer again"
    );
}

#[test]
fn one_player_cannot_call_waves_for_another() {
    let (a, b) = (player(1), player(2));
    let mut harness = Harness::with_players(&[1, 2]);
    let cooldown = harness.sim.balance.match_rules.call_wave_cooldown;
    assert!(
        cooldown > TICK,
        "a cooldown shorter than one tick would let the next tick re-arm it"
    );

    harness.advance(1);
    harness.sim.call_wave(a).expect("A's first call");
    harness.step();

    // The cooldown is the caller's, not the table's: B is unaffected by A's
    // call, and A cannot call again yet.
    assert_eq!(harness.sim.call_wave(a), Err(Reject::RateLimited));
    harness.sim.call_wave(b).expect("B may call");
}

#[test]
fn a_finished_match_refuses_a_wave_call() {
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.overrun_cap = 3;
    let mut harness = Harness::with_balance(Arc::new(tuned));
    harness.sim.add_player(player(1));

    harness.advance_until(600, |sim| sim.over);
    assert_eq!(harness.sim.call_wave(player(1)), Err(Reject::MatchOver));
}

// ---------------------------------------------------------------------------
// Match phase (audit-004)
// ---------------------------------------------------------------------------

#[test]
fn the_match_phase_follows_the_sim() {
    let mut waiting = Harness::new();
    assert_eq!(waiting.sim.phase(), Phase::Waiting, "nobody has played yet");
    assert_eq!(waiting.sim.match_view().phase, Phase::Waiting.as_u8());

    waiting.advance_until(600, |sim| sim.wave == 1);
    assert_eq!(
        waiting.sim.phase(),
        Phase::InMatch,
        "a started match is no longer waiting"
    );

    // Force the lose condition instead of waiting for a real overrun.
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.overrun_cap = 3;
    let mut over = Harness::with_balance(Arc::new(tuned));
    over.advance_until(600, |sim| sim.over);

    assert_eq!(over.sim.phase(), Phase::Over);

    // This is what the server mirrors, so it is what the HUD reads: the final
    // overrun count, not whatever it was on the frame the cap was crossed (D4).
    let mirrored = over.sim.match_view();
    assert_eq!(mirrored.phase, Phase::Over.as_u8());
    assert_eq!(mirrored.live_creeps, over.sim.creeps_alive());
    assert_eq!(mirrored.live_creeps, 4);
    assert_eq!(mirrored.overrun_cap, over.sim.overrun_cap());
}
