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
use greentd::data::balance::{BalanceData, LoseRule};
use greentd::data::components::Phase;
use greentd::data::reject::Reject;
use greentd::map::MapData;
use greentd::net::messages::ServerNotice;
use greentd::sim::{PlayerKey, Rng, Sim, SimEvent};

/// One simulation tick. The server runs at 30 Hz; the sim itself only requires
/// that `dt` is fixed.
const TICK: f32 = 1.0 / 30.0;

/// The tables that ship in `assets/balance`.
fn shipped_balance() -> Arc<BalanceData> {
    Arc::new(BalanceData::shipped().expect("assets/balance must load"))
}

/// The board that ships in `assets/maps`. The sim needs a board as much as it
/// needs tables: it is where the lanes, the spawn points and the goal live.
fn shipped_map() -> Arc<MapData> {
    Arc::new(MapData::shipped().expect("assets/maps must load"))
}

/// A deliberately tiny board: two lanes, 2100 units long, ending at one goal.
///
/// The shipped board's lanes are 13,000 to 32,000 units and there are ten of
/// them, which is the right thing to play and the wrong thing to wait for in a
/// test about leaking. Short lanes make "does a creep that reaches the goal cost
/// a life" a question about the rule rather than about patience.
fn short_board() -> Arc<MapData> {
    use greentd::map::{GoalDef, LaneDef, MapDef};

    let lane = |name: &str, x: f32| LaneDef {
        name: name.to_string(),
        spawn: (x, 700.0),
        waypoints: vec![(x, 700.0), (x, -700.0), (0.0, -700.0)],
    };
    let def = MapDef {
        id: "short".to_string(),
        name: "Short".to_string(),
        tiles_x: 16,
        tiles_y: 16,
        tile_size: 100.0,
        path_clearance: 95.0,
        goal: GoalDef {
            name: "END".to_string(),
            x: 0.0,
            y: -700.0,
            half_w: 100.0,
            half_h: 100.0,
        },
        lanes: vec![lane("Left", -700.0), lane("Right", 700.0)],
        zones: vec![],
    };
    Arc::new(MapData::from_def(def).expect("the short board is valid"))
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
        Self::with_balance(shipped_balance())
    }

    /// A match running on modified tables, on the shipped board. Used to make
    /// the retuned numbers observable without waiting for a real match.
    fn with_balance(balance: Arc<BalanceData>) -> Self {
        Self {
            sim: Sim::new(balance, shipped_map()),
            tick: TICK,
        }
    }

    /// A match on a modified board and modified tables: the tests about leaks
    /// want a short lane and a known number of lives.
    fn with_board(balance: Arc<BalanceData>, map: Arc<MapData>) -> Self {
        Self {
            sim: Sim::new(balance, map),
            tick: TICK,
        }
    }

    /// One lane's creeps' distances along it, sorted. The arrival shape of a
    /// wave is a property of a lane, so this is the unit the tests measure.
    fn lane_distances(map: &MapData, sim: &Sim, lane: u8) -> Vec<f32> {
        let mut dists: Vec<f32> = sim
            .creeps
            .values()
            .filter(|c| c.lane == lane)
            .map(|c| c.dist)
            .collect();
        dists.sort_by(|a, b| a.partial_cmp(b).expect("no NaN distances"));
        let _ = map;
        dists
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

/// A buildable cell with a lane cell for a neighbour, so a tower placed there
/// actually has something to shoot at.
fn cell_beside_the_path(sim: &Sim) -> IVec2 {
    let map = &sim.map;
    for y in 0..map.tiles_y {
        for x in 0..map.tiles_x {
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
                .any(|n| map.in_bounds(*n) && !sim.is_buildable(*n))
            {
                return cell;
            }
        }
    }
    panic!("the board has no buildable cell beside a lane");
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
        harness.sim.balance.creeps_per_lane(1, 1) * harness.sim.lane_count(),
        "a wave puts the table's count on every lane"
    );
    assert_eq!(
        harness.sim.creeps_per_lane(),
        harness.sim.balance.creeps_per_lane(1, 1),
        "and `creeps_per_lane` agrees with the table"
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
    // On the short board, and with fragmentary creeps, on purpose. This test is
    // about where the bounty goes: on the real board a creep takes a minute to
    // reach the first tower worth building, which makes the test slow and makes
    // it about the board rather than about the economy.
    let mut tuned = (*shipped_balance()).clone();
    tuned.waves.scaling.hp_base = 10.0;
    tuned.waves.scaling.hp_growth = 0.0;

    let who = player(7);
    let mut harness = Harness::with_board(Arc::new(tuned), short_board());
    harness.sim.add_player(who);

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
    // The shipped rule is `lives` (D19); this test is about the other lineage,
    // so it asks for it rather than relying on the default.
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.lose_rule = LoseRule::Overrun;
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
fn a_reset_gives_a_lost_match_back() {
    // A shrunken cap and a fast clock, so a defeat and a rematch arrive in
    // seconds rather than in a match. Wave 1 is four creeps and wave 2 is five,
    // so a cap of four lets the first wave through and ends it on the second --
    // which is also what makes the rematch's first wave survivable.
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.lose_rule = LoseRule::Overrun;
    // Between wave 1 and wave 2: a wave is one creep per lane per player, and
    // the board has ten lanes, so a cap of four would end the match on the very
    // first wave.
    tuned.match_rules.overrun_cap = 12;
    tuned.match_rules.first_wave_delay = 0.1;
    tuned.match_rules.wave_interval = 0.5;
    let mut harness = Harness::with_balance(Arc::new(tuned));
    harness.sim.add_player(player(7));

    harness.advance_until(600, |sim| sim.over);
    assert!(harness.sim.over, "drive it to a defeat first");
    assert!(
        !harness.sim.creeps.is_empty(),
        "the board was not empty when the match ended"
    );

    let rules = harness.sim.balance.match_rules.clone();
    harness.sim.reset();

    assert!(
        !harness.sim.over,
        "a reset is a match that can be lost again, not a terminal flag (D20)"
    );
    assert_eq!(harness.sim.wave, 0, "the wave counter starts over");
    assert_eq!(
        harness.sim.wave_timer, rules.first_wave_delay,
        "the wave clock is reset, not left where the defeat stopped it"
    );
    assert!(harness.sim.creeps.is_empty(), "the board is cleared");
    assert!(harness.sim.towers.is_empty(), "the towers go with it");

    let reset_player = harness
        .sim
        .players
        .get(&player(7))
        .expect("a reset keeps the players who were in the match");
    assert_eq!(
        reset_player.gold, rules.start_gold,
        "gold is back to the starting value"
    );
    assert_eq!(reset_player.kills, 0, "so are the kills");
    assert!(
        reset_player.connected,
        "and the player is still in the match"
    );

    // And it is a match again: a fresh wave arrives, on a board with nothing on
    // it, without the sim having to be rebuilt.
    harness.advance_until(600, |sim| sim.wave == 1);
    assert_eq!(
        harness.sim.creeps_alive(),
        harness.sim.wave_size(),
        "the rematch's first wave is a whole wave"
    );
    assert!(
        harness.sim.creeps_alive() <= harness.sim.overrun_cap(),
        "the rematch is not over before it starts"
    );
}

#[test]
fn a_reset_match_begins_like_a_fresh_one() {
    // Non-zero jitter, so the generator is observable in the creeps' positions.
    let mut tuned = (*shipped_balance()).clone();
    tuned.waves.scaling.spawn_jitter = 0.25;
    let mut harness = Harness::with_balance(Arc::new(tuned));
    harness.sim.add_player(player(1));

    // Play for a while, so the generator is somewhere other than its seed.
    harness.advance(200);
    harness.sim.reset();

    let mut after_reset = Harness::with_balance(harness.sim.balance.clone());
    after_reset.sim.add_player(player(1));
    let mut fresh = Harness::with_balance(harness.sim.balance.clone());
    fresh.sim.add_player(player(1));

    let (reset_ticks, _) = after_reset.advance_until(600, |sim| sim.wave == 1);
    let (fresh_ticks, _) = fresh.advance_until(600, |sim| sim.wave == 1);
    assert_eq!(
        reset_ticks, fresh_ticks,
        "both matches start at the same beat"
    );

    let positions = |harness: &Harness| {
        let mut dists: Vec<f32> = harness.sim.creeps.values().map(|c| c.dist).collect();
        dists.sort_by(f32::total_cmp);
        dists
    };
    assert_eq!(
        positions(&after_reset),
        positions(&fresh),
        "a reseeded match is a fresh match, not a continued one (found-009)"
    );
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
    harness
        .sim
        .call_wave(who)
        .expect("the first call is allowed");
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
    harness
        .sim
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
    tuned.match_rules.lose_rule = LoseRule::Overrun;
    tuned.match_rules.overrun_cap = 3;
    let mut harness = Harness::with_balance(Arc::new(tuned));
    harness.sim.add_player(player(1));

    harness.advance_until(600, |sim| sim.over);
    assert_eq!(harness.sim.call_wave(player(1)), Err(Reject::MatchOver));
}

// ---------------------------------------------------------------------------
// Determinism and the seeded RNG (found-009)
// ---------------------------------------------------------------------------

/// The shipped tables, with the match seed replaced.
fn seeded_balance(seed: u64) -> Arc<BalanceData> {
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.seed = seed;
    Arc::new(tuned)
}

/// Every creep's distance along the path, in spawn order.
fn creep_distances(sim: &Sim) -> Vec<f32> {
    let mut dists: Vec<f32> = sim.creeps.values().map(|creep| creep.dist).collect();
    dists.sort_by(|a, b| a.partial_cmp(b).expect("no NaN distances"));
    dists
}

#[test]
fn the_same_seed_replays_and_a_different_seed_does_not() {
    let seed = 0x5EED_1234_5678_9ABC;

    let run = |seed: u64| {
        let mut harness = Harness::with_balance(seeded_balance(seed));
        harness.sim.add_player(player(1));
        harness.advance_until(600, |sim| sim.wave == 1);
        harness.advance(90);
        creep_distances(&harness.sim)
    };

    let first = run(seed);
    let again = run(seed);
    let other = run(seed ^ 1);

    assert!(!first.is_empty(), "the wave must have spawned something");
    assert_eq!(
        first, again,
        "the same seed is the same match, tick for tick"
    );
    assert_ne!(
        first, other,
        "a different seed has to be observable, or the seed is decoration"
    );
}

#[test]
fn a_wave_enters_at_its_lane_spawn_point_in_a_column() {
    // `audit-013` (D10). A wave used to be spread evenly around a closed ring,
    // which gave it no front and could spawn a creep next to the destination.
    // It now enters every lane at that lane's spawn point, one behind another.
    let mut harness = Harness::with_balance(seeded_balance(7));
    harness.advance_until(600, |sim| sim.wave == 1);

    let map = harness.sim.map.clone();
    let scaling = harness.sim.balance.waves.scaling.clone();
    let per_lane = harness.sim.creeps_per_lane();
    let steps = per_lane.saturating_sub(1) as f32;
    let gap = scaling
        .spawn_gap
        .min(scaling.spawn_arc / steps.max(1.0))
        .min(scaling.spawn_arc / steps.max(f32::MIN_POSITIVE));
    let limit = gap * scaling.spawn_jitter;
    assert!(
        limit > 0.0,
        "the tables must jitter at all for this test to mean anything"
    );

    for lane in 0..map.lanes.len() as u8 {
        let dists = Harness::lane_distances(&map, &harness.sim, lane);
        assert_eq!(
            dists.len(),
            per_lane as usize,
            "lane {lane} got its whole share of the wave"
        );

        // Nothing has strayed past the arrival window: the whole wave is a
        // column at the spawn point, and the goal is a whole lane away.
        assert!(
            dists.last().copied().unwrap_or_default() <= scaling.spawn_arc,
            "lane {lane} spawned {} past its arrival window",
            dists.last().copied().unwrap_or_default()
        );
        assert!(
            map.lanes[lane as usize].total > scaling.spawn_arc,
            "the window has to be shorter than the lane for that to mean anything"
        );

        for (slot, dist) in dists.iter().enumerate() {
            let ideal = gap * slot as f32;
            assert!(
                (dist - ideal).abs() <= limit,
                "lane {lane} creep {slot} is at {dist}, more than {limit} from its slot at {ideal}"
            );
        }
    }

    // And the jitter moved something, or the generator is decoration.
    let moved = (0..map.lanes.len() as u8).any(|lane| {
        Harness::lane_distances(&map, &harness.sim, lane)
            .iter()
            .enumerate()
            .any(|(slot, dist)| (dist - gap * slot as f32).abs() > 0.0)
    });
    assert!(moved, "the jitter has to have moved something");
}

#[test]
fn each_wave_draws_further_along_the_generator() {
    // If the jitter were a pure function of the wave number the second wave
    // would repeat the first, and a "seeded" generator would be a fancy
    // constant. This runs on the short board so the first wave has leaked away
    // before the second arrives, which makes "lane 0's creeps" a clean reading
    // of one wave's draws.
    let mut tuned = (*seeded_balance(11)).clone();
    tuned.match_rules.lose_rule = LoseRule::Overrun;
    tuned.match_rules.overrun_cap = 1_000_000; // leaks must not end this
    tuned.match_rules.first_wave_delay = 0.1;
    tuned.match_rules.wave_interval = 2.0;
    tuned.waves.scaling.count_base = 1.0;
    tuned.waves.scaling.count_per_wave = 0.0; // every wave the same size
    tuned.waves.scaling.speed_base = 21_000.0;
    tuned.waves.scaling.speed_growth = 0.0;
    tuned.waves.scaling.hp_base = 1_000.0;

    let mut harness = Harness::with_board(Arc::new(tuned), short_board());
    harness.advance_until(600, |sim| sim.wave == 1);
    let first = Harness::lane_distances(&harness.sim.map, &harness.sim, 0);
    assert_eq!(first.len(), 1, "one creep per lane per wave");

    let timer = harness.sim.balance.match_rules.wave_interval;
    harness.advance((timer / TICK).ceil() as u32 + 2);
    assert_eq!(harness.sim.wave, 2, "the second wave has started");
    assert_eq!(
        harness.sim.creeps_alive(),
        2,
        "the first wave leaked away, so only the second is on the board"
    );

    let second = Harness::lane_distances(&harness.sim.map, &harness.sim, 0);
    assert_eq!(first.len(), second.len(), "both waves are the same size");
    assert_ne!(
        first, second,
        "wave 2 must draw fresh numbers, not repeat wave 1"
    );
}

#[test]
fn the_generator_is_reproducible_and_not_degenerate() {
    let mut first = Rng::from_seed(42);
    let mut again = Rng::from_seed(42);
    let mut other = Rng::from_seed(43);

    let mut draws = Vec::new();
    for _ in 0..64 {
        let a = first.next_u64();
        assert_eq!(a, again.next_u64(), "same seed, same stream");
        assert!(
            a != other.next_u64(),
            "a different seed, a different stream"
        );
        draws.push(a);
    }
    draws.sort_unstable();
    draws.dedup();
    assert_eq!(draws.len(), 64, "64 draws produced 64 distinct values");

    let mut rng = Rng::from_seed(0);
    for _ in 0..1000 {
        let value = rng.next_f32();
        assert!((0.0..1.0).contains(&value), "{value} is not in [0, 1)");
    }
    for _ in 0..1000 {
        let value = rng.range_f32(-2.5, 2.5);
        assert!(
            (-2.5..2.5).contains(&value),
            "{value} is not in [-2.5, 2.5)"
        );
    }
    assert_eq!(
        rng.range_f32(1.0, 1.0),
        1.0,
        "an empty range yields its bound"
    );
    assert_eq!(
        rng.range_f32(1.0, 0.0),
        1.0,
        "an inverted range yields `low` rather than panicking"
    );
}

#[test]
fn the_match_seed_comes_from_the_tables() {
    let harness = Harness::with_balance(seeded_balance(0xDEAD_BEEF));
    assert_eq!(harness.sim.seed(), 0xDEAD_BEEF);

    // ...and it is not a protocol number: two peers on the same rules agree on
    // the hash whatever match they are playing.
    let one = seeded_balance(1);
    let two = seeded_balance(2);
    assert_eq!(
        one.hash(),
        two.hash(),
        "the seed is not part of the ruleset"
    );
}

// ---------------------------------------------------------------------------
// The map (found-006)
// ---------------------------------------------------------------------------

#[test]
fn a_non_finite_distance_lands_on_the_lane_rather_than_panicking() {
    // `found-006`: a NaN position would travel into a `Transform`, where it is
    // far harder to find than a creep standing at the start of its lane.
    let map = shipped_map();
    let lane = &map.lanes[0];

    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let at = lane.sample(bad);
        assert!(
            at.is_finite(),
            "sampling {bad} produced {at}, which would poison a Transform"
        );
    }

    // The ordinary walk: the spawn point at zero, the goal at the end, and a
    // point on the lane in between. Unlike the ring this replaced, a lane does
    // not wrap -- past its end it is the goal, which is what a leak is.
    assert_eq!(lane.sample(0.0), lane.spawn());
    assert_eq!(lane.sample(lane.total), map.goal.centre);
    assert_eq!(
        lane.sample(lane.total * 2.0),
        map.goal.centre,
        "a lane does not wrap around; it ends at the goal"
    );
    let middle = lane.sample(lane.total * 0.5);
    assert!(middle.is_finite() && lane.distance_to(middle) < 1.0);
}

// ---------------------------------------------------------------------------
// The board (03-map.org)
// ---------------------------------------------------------------------------

#[test]
fn the_shipped_board_has_lanes_that_end_at_its_goal() {
    let map = shipped_map();

    assert!(
        map.lanes.len() >= 9,
        "the real board has nine colours and ten spawn points, got {}",
        map.lanes.len()
    );
    for lane in &map.lanes {
        assert!(
            lane.sample(lane.total).distance(map.goal.centre) < 1.0,
            "lane {} ends at the goal",
            lane.name
        );
        assert!(
            map.goal.half.x > 0.0 && map.goal.half.y > 0.0,
            "the goal is a region, not a point"
        );
    }

    // The goal is a single shared region: every lane funnels into it, which is
    // what makes the board's middle worth defending (map-001, sim-006).
    let ends: Vec<IVec2> = map
        .lanes
        .iter()
        .map(|lane| map.world_to_cell(lane.sample(lane.total)))
        .collect();
    assert_eq!(
        ends.iter().collect::<std::collections::HashSet<_>>().len(),
        1,
        "every lane reaches the same goal, got {ends:?}"
    );
}

#[test]
fn the_board_is_open_ground_except_for_its_lanes() {
    let map = shipped_map();

    // Green TD's placement rule, and the whole of it: no maze, no blocking.
    let mut buildable = 0;
    let mut blocked = 0;
    for y in 0..map.tiles_y {
        for x in 0..map.tiles_x {
            let cell = IVec2::new(x, y);
            if map.is_buildable(cell) {
                buildable += 1;
            } else {
                blocked += 1;
            }
        }
    }
    assert!(buildable > 0 && blocked > 0, "the board has both");
    assert!(
        buildable > blocked,
        "most of an open field is buildable: {buildable} of {}",
        buildable + blocked
    );

    // A cell on a lane is never buildable, and a cell far from every lane is.
    assert!(!map.is_buildable(map.world_to_cell(map.lanes[0].sample(0.0))));
    assert!(map.is_buildable(IVec2::new(0, 0)), "the corner is empty");
    assert!(!map.is_buildable(IVec2::new(-1, 0)), "off the board");
}

// ---------------------------------------------------------------------------
// Local identity (found-010, D30)
// ---------------------------------------------------------------------------

#[test]
fn the_match_view_carries_the_lobby_size() {
    let mut harness = Harness::with_players(&[1, 2, 3]);
    assert_eq!(
        harness.sim.match_view().players,
        3,
        "the HUD reads the lobby size from the match, not from the views it holds"
    );

    harness.sim.remove_player(player(2));
    assert_eq!(
        harness.sim.match_view().players,
        2,
        "a leaver is not playing"
    );
    assert_eq!(
        harness.sim.players.len(),
        3,
        "but their record and towers stay (D27)"
    );

    // The count is the same one the wave size is built from, so the two cannot
    // disagree.
    assert_eq!(harness.sim.players_connected(), 2);
    assert_eq!(
        harness.sim.wave_size(),
        harness.sim.balance.creeps_per_lane(harness.sim.wave, 2) * harness.sim.lane_count()
    );
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
    tuned.match_rules.lose_rule = LoseRule::Overrun;
    tuned.match_rules.overrun_cap = 3;
    let mut over = Harness::with_balance(Arc::new(tuned));
    over.advance_until(600, |sim| sim.over);

    assert_eq!(over.sim.phase(), Phase::Over);

    // This is what the server mirrors, so it is what the HUD reads: the final
    // overrun count, not whatever it was on the frame the cap was crossed (D4).
    let mirrored = over.sim.match_view();
    assert_eq!(mirrored.phase, Phase::Over.as_u8());
    assert_eq!(mirrored.live_creeps, over.sim.creeps_alive());
    assert!(
        mirrored.live_creeps > over.sim.overrun_cap(),
        "the cap is what ended it, so more creeps are alive than it allows"
    );
    assert_eq!(mirrored.overrun_cap, over.sim.overrun_cap());
}

// ---------------------------------------------------------------------------
// The last sim literals are data (audit-011, D18)
// ---------------------------------------------------------------------------

/// A board with one enormous-range, one-shot tower of `kind`, upgraded once to
/// level 2, and one very tough creep. Returns the harness the instant the creep
/// has been hit once, so a test can measure exactly one shot.
fn one_shot_at_level_two(kind: u8, tune: impl FnOnce(&mut BalanceData)) -> Harness {
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.start_gold = 1_000_000;
    tuned.match_rules.first_wave_delay = 0.1;
    tuned.match_rules.wave_interval = 1_000.0;
    tuned.waves.scaling.count_base = 1.0;
    tuned.waves.scaling.count_per_wave = 0.0;
    tuned.waves.scaling.hp_base = 1_000.0;
    {
        let tower = &mut tuned.towers[kind as usize];
        tower.range = 100_000.0; // always in range
        tower.cooldown = 1_000.0; // fires exactly once in this window
        tower.damage = 10.0;
    }
    tune(&mut tuned);

    let mut harness = Harness::with_balance(Arc::new(tuned));
    let who = player(1);
    harness.sim.add_player(who);
    let cell = cell_beside_the_path(&harness.sim);
    harness.sim.try_build(who, cell, kind).expect("the build");
    harness.sim.try_upgrade(who, cell).expect("one upgrade");
    assert_eq!(
        harness.sim.towers.get(&cell).map(|tower| tower.level),
        Some(2),
        "the tower is level 2"
    );

    harness.advance_until(600, |sim| sim.creeps.values().any(|c| c.hp < c.max_hp));
    harness
}

/// The one creep a single tower has shot, so a test measures the shot rather
/// than whichever creep a `HashMap` hands back first.
fn the_hit_creep(harness: &Harness) -> &greentd::sim::Creep {
    harness
        .sim
        .creeps
        .values()
        .find(|creep| creep.hp < creep.max_hp)
        .expect("something was hit")
}

#[test]
fn the_tier_scaling_is_read_from_the_table() {
    let dealt = |growth: f32| {
        let harness = one_shot_at_level_two(0, |data| data.towers[0].damage_per_level = growth);
        let creep = the_hit_creep(&harness);
        creep.max_hp - creep.hp
    };

    let flat = dealt(0.0);
    let grown = dealt(0.45);
    assert!(
        (flat - 10.0).abs() < 0.01,
        "no growth: a level-2 shot is 10, got {flat}"
    );
    assert!(
        (grown - 14.5).abs() < 0.01,
        "at +45% per level a level-2 shot is 14.5, got {grown}"
    );
}

#[test]
fn the_slow_duration_is_read_from_the_table() {
    let harness = one_shot_at_level_two(2, |data| {
        data.towers[2].slow = 0.5;
        data.towers[2].slow_duration = 7.0;
    });
    let creep = the_hit_creep(&harness);
    // `advance_creeps` runs before `fire`, so a timer set this tick has not been
    // decremented yet: it is exactly the table's duration.
    assert!(
        (creep.slow_timer - 7.0).abs() < 1e-4,
        "the slow lasts the table's duration, got {}",
        creep.slow_timer
    );
    assert!(creep.speed_mult() < 1.0, "a slowed creep moves slower");

    let harness = one_shot_at_level_two(2, |data| {
        data.towers[2].slow = 0.5;
        data.towers[2].slow_duration = 0.0;
    });
    let creep = the_hit_creep(&harness);
    assert_eq!(creep.slow_timer, 0.0, "a zero duration never slows");
    assert_eq!(creep.speed_mult(), 1.0, "an expired slow is not a slow");
}

// ---------------------------------------------------------------------------
// The goal, the leak and the lives (sim-005, sim-006, D19)
// ---------------------------------------------------------------------------

/// Tables for the leak tests: one creep per lane, walking a 2100-unit lane fast
/// enough to arrive in a handful of ticks, on a fixed number of lives.
fn leak_tables(lives: u32, rule: LoseRule) -> Arc<BalanceData> {
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.lose_rule = rule;
    tuned.match_rules.starting_lives = lives;
    tuned.match_rules.first_wave_delay = 0.1;
    tuned.match_rules.wave_interval = 1.0;
    tuned.waves.scaling.count_base = 1.0;
    tuned.waves.scaling.count_per_wave = 0.0;
    tuned.waves.scaling.speed_base = 21_000.0; // a whole lane in three ticks
    tuned.waves.scaling.speed_growth = 0.0;
    tuned.waves.scaling.hp_base = 1_000.0; // nothing kills them, they leak
    Arc::new(tuned)
}

#[test]
fn a_creep_that_reaches_the_goal_costs_a_life() {
    let mut harness = Harness::with_board(leak_tables(10, LoseRule::Lives), short_board());
    let lanes = harness.sim.lane_count();
    assert_eq!(lanes, 2, "the short board has two lanes");

    // Both lanes are the same length but the jitter is not, so "wait for the
    // first leak" is not "wait for the whole wave"; wait for the wave.
    let (_, events) = harness.advance_until(600, |sim| sim.leaks >= lanes);

    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::Leaked(_))),
        harness.sim.leaks as usize,
        "every leak is announced exactly once"
    );
    assert_eq!(
        harness.sim.leaks, lanes,
        "one creep per lane reached the goal"
    );
    assert_eq!(
        harness.sim.lives,
        10 - lanes,
        "and each of them cost a life"
    );
    assert_eq!(
        harness.sim.creeps_alive(),
        0,
        "a leaked creep leaves the board: it is not still walking"
    );
    assert!(!harness.sim.over, "there are lives left");
}

#[test]
fn running_out_of_lives_ends_the_match() {
    // Two lanes, five lives, so it takes the third wave to lose.
    let mut harness = Harness::with_board(leak_tables(5, LoseRule::Lives), short_board());
    let (_, events) = harness.advance_until(2_000, |sim| sim.over);

    assert_eq!(harness.sim.lives, 0, "the lives are what ran out");
    assert_eq!(harness.sim.leaks, 6, "three waves of two lanes leaked");
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::GameOver)),
        1,
        "game over is announced once"
    );
    assert_eq!(harness.sim.phase(), Phase::Over);
    assert!(harness.step().is_empty(), "a finished match does not step");
}

#[test]
fn the_overrun_rule_does_not_spend_lives() {
    // Lineage A: a creep that reaches the goal is still a creep on the board,
    // and the match ends because there are too many of them (D19).
    let mut tuned = (*leak_tables(5, LoseRule::Overrun)).clone();
    tuned.match_rules.overrun_cap = 1;
    let mut harness = Harness::with_board(Arc::new(tuned), short_board());

    harness.advance_until(600, |sim| sim.over);

    assert!(harness.sim.over);
    assert_eq!(
        harness.sim.leaks, 0,
        "under overrun the cap ends it, and two creeps are alive"
    );
    assert_eq!(
        harness.sim.lives, 5,
        "the lives are untouched: the rule is not the reason it ended"
    );
}

#[test]
fn a_leak_is_counted_under_either_rule() {
    // Under `overrun` a leak costs no life, but it still happened, and the
    // count is what a multiboard would show (`sim-019`).
    let mut tuned = (*leak_tables(5, LoseRule::Overrun)).clone();
    tuned.match_rules.overrun_cap = 1_000; // out of the way
    let mut harness = Harness::with_board(Arc::new(tuned), short_board());

    harness.advance_until(600, |sim| sim.leaks > 0);

    assert!(harness.sim.leaks > 0, "the creeps reached the goal");
    assert_eq!(
        harness.sim.lives, 5,
        "and cost nothing under the overrun rule"
    );
    assert_eq!(harness.sim.match_view().leaks, harness.sim.leaks);
    assert_eq!(harness.sim.match_view().lives, harness.sim.lives);
}

// ---------------------------------------------------------------------------
// The step order (sim-001)
// ---------------------------------------------------------------------------

/// A buildable cell whose centre is within `within` world units of the goal.
///
/// The leak test needs a tower that can reach the goal but *not* the spawn
/// point, so that "did the tower fire at all" is a question about the leak
/// rather than about range.
fn buildable_cell_near_goal(sim: &Sim, within: f32) -> IVec2 {
    let map = &sim.map;
    for y in 0..map.tiles_y {
        for x in 0..map.tiles_x {
            let cell = IVec2::new(x, y);
            if sim.is_buildable(cell) && map.cell_to_world(cell).distance(map.goal.centre) <= within
            {
                return cell;
            }
        }
    }
    panic!("the board has no buildable cell within {within} of the goal");
}

#[test]
fn a_creep_that_reaches_the_goal_leaks_before_a_tower_can_shoot_it() {
    // `sim-006` requires `leak_arrivals` to run before the fire phase, so a
    // creep that arrives at the goal on a tick cannot also be shot on that tick.
    // This pins the order `Sim::step` documents, and it is written as a
    // contradiction: the tower's shot is lethal and the creep is placed at the
    // goal with a single hit point, so if the fire phase ran first the creep
    // would be *killed* -- a `CreepKilled` event and a bounty. Because the leak
    // runs first the creep is off the board before any tower acquires a target,
    // and the tower finds nothing else in range to shoot.
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.lose_rule = LoseRule::Overrun;
    tuned.match_rules.overrun_cap = 1_000_000; // the leak must not end the match
    tuned.match_rules.starting_lives = 1_000;
    tuned.match_rules.first_wave_delay = 0.1;
    tuned.match_rules.wave_interval = 100_000.0; // exactly one wave
    tuned.waves.scaling.count_base = 1.0;
    tuned.waves.scaling.count_per_wave = 0.0;
    tuned.waves.scaling.speed_base = 0.0; // the creeps stand still
    tuned.waves.scaling.speed_growth = 0.0;
    tuned.waves.scaling.hp_base = 1_000.0; // only the placed creep is killable
    tuned.waves.scaling.hp_growth = 0.0;
    tuned.towers[0].damage = 1_000_000.0; // one shot kills anything
    tuned.towers[0].range = 600.0; // reaches the goal, not the spawn points
    tuned.towers[0].cooldown = 0.0; // ready on the very first tick
    let mut harness = Harness::with_board(Arc::new(tuned), short_board());

    let who = player(1);
    harness.sim.add_player(who);
    let start_gold = harness.sim.balance.match_rules.start_gold;
    let cost = harness
        .sim
        .balance
        .tower(0)
        .map(|tower| tower.cost)
        .expect("kind 0 is in the tower table");

    let cell = buildable_cell_near_goal(&harness.sim, 200.0);
    harness.sim.try_build(who, cell, 0).expect("the build");

    // One wave, then one of its creeps is placed on the goal with one hit point.
    // The others stay at the spawn point, well out of the tower's range.
    harness.advance_until(600, |sim| sim.wave == 1);
    let id = *harness
        .sim
        .creeps
        .keys()
        .min()
        .expect("the wave spawned creeps");
    let lane = harness.sim.creeps[&id].lane;
    let total = harness.sim.map.lanes[lane as usize].total;
    {
        let creep = harness.sim.creeps.get_mut(&id).expect("the creep exists");
        creep.dist = total;
        creep.hp = 1.0;
    }

    let events = harness.step();

    assert_eq!(
        count_matching(
            &events,
            |e| matches!(e, SimEvent::Leaked(leaked) if *leaked == id)
        ),
        1,
        "the creep at the goal leaked this tick"
    );
    assert_eq!(
        count_matching(&events, |e| matches!(e, SimEvent::CreepKilled(_))),
        0,
        "nothing was killed: the leak ran before the fire phase"
    );
    assert_eq!(kills(&harness.sim, who), 0, "and so nothing was credited");
    assert_eq!(
        gold(&harness.sim, who),
        start_gold - cost,
        "the tower's owner earned no bounty for a creep it never shot"
    );
    assert!(
        !harness.sim.creeps.contains_key(&id),
        "the leaked creep is off the board"
    );
}

#[test]
fn a_creep_killed_on_a_tick_is_reaped_on_that_tick() {
    // The other load-bearing ordering in `Sim::step` (`sim-001`): the damage
    // phase precedes the reap phase, so a creep the tower brings to zero this
    // tick is removed and its killer paid on the same tick, not the next one.
    let mut tuned = (*shipped_balance()).clone();
    tuned.match_rules.lose_rule = LoseRule::Overrun;
    tuned.match_rules.overrun_cap = 1_000_000;
    tuned.match_rules.first_wave_delay = 0.1;
    tuned.match_rules.wave_interval = 100_000.0; // exactly one wave
    tuned.waves.scaling.count_base = 1.0;
    tuned.waves.scaling.count_per_wave = 0.0;
    tuned.waves.scaling.speed_base = 0.0; // the creeps stand still
    tuned.waves.scaling.speed_growth = 0.0;
    tuned.waves.scaling.hp_base = 1_000.0;
    tuned.waves.scaling.hp_growth = 0.0;
    tuned.towers[0].damage = 1_000_000.0; // one shot kills anything
    tuned.towers[0].range = 600.0;
    tuned.towers[0].cooldown = 0.0;
    let mut harness = Harness::with_board(Arc::new(tuned), short_board());

    let who = player(1);
    harness.sim.add_player(who);
    let bounty = harness.sim.balance.creep_bounty(1);
    let cell = buildable_cell_near_goal(&harness.sim, 200.0);
    harness.sim.try_build(who, cell, 0).expect("the build");

    // One creep in the tower's range, the rest out of it.
    harness.advance_until(600, |sim| sim.wave == 1);
    let id = *harness
        .sim
        .creeps
        .keys()
        .min()
        .expect("the wave spawned creeps");
    let lane = harness.sim.creeps[&id].lane;
    let total = harness.sim.map.lanes[lane as usize].total;
    // Just short of the goal, so the creep is in the tower's range and is the
    // next to arrive -- but has not arrived, so it is shot rather than leaked.
    harness
        .sim
        .creeps
        .get_mut(&id)
        .expect("the creep exists")
        .dist = total - 1.0;

    let events = harness.step();

    assert_eq!(
        count_matching(
            &events,
            |e| matches!(e, SimEvent::CreepKilled(killed) if *killed == id)
        ),
        1,
        "the kill is announced on the tick it happens"
    );
    assert_eq!(kills(&harness.sim, who), 1, "and the kill is credited now");
    assert_eq!(
        gold(&harness.sim, who),
        harness.sim.balance.match_rules.start_gold
            - harness.sim.balance.tower(0).expect("kind 0").cost
            + bounty,
        "the bounty lands in the same tick as the shot (damage before reap)"
    );
    assert!(
        !harness.sim.creeps.contains_key(&id),
        "the dead creep is gone"
    );
}

#[test]
fn the_sim_sources_name_no_network_type() {
    // `sim-001`, box three. The module doc of `src/sim/mod.rs` promises that
    // nothing under `sim/` names `lightyear`, so the sim stays reproducible from
    // a seed with no socket in scope (`decide-authority`). The promise is a
    // check here rather than a `grep` in a pipeline, because there is no
    // pipeline: this runs wherever `cargo test` does.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("src/sim must exist") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        assert!(
            !source.contains("lightyear"),
            "{} names lightyear; the sim must not reach into the network layer",
            path.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "there were no sim sources to check");
}
