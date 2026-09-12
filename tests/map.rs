//! The board: what loads, what is refused, and what two peers must agree on.
//!
//! A map became data when the real Green TD board replaced the `const RING`
//! (`03-map.org` `map-001`). Data that a person hand-writes needs a validator
//! with opinions, and that is what most of this file is: every refusal below
//! names the field, so a broken map is a message rather than a crash or, worse,
//! a board that loads and cannot be played.
//!
//! Nothing here opens a socket or a window: a map is a file and a function.

use bevy::prelude::*;
use greentd::data::balance::Balance;
use greentd::map::{GoalDef, LaneDef, Map, MapData, MapDef, MapError, ZoneDef};

/// The board that ships, as a peer loads it.
fn shipped() -> MapData {
    MapData::shipped().expect("assets/maps/green_td_v36.ron loads")
}

/// One valid lane down the left-hand side of the board below.
fn lane(name: &str) -> LaneDef {
    LaneDef {
        name: name.to_string(),
        spawn: (-700.0, 700.0),
        waypoints: vec![(-700.0, 700.0), (-700.0, -700.0), (0.0, -700.0)],
    }
}

/// A small board that *is* valid, for the tests that break one thing at a time.
fn valid_def() -> MapDef {
    MapDef {
        id: "test".to_string(),
        name: "Test".to_string(),
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
        lanes: vec![lane("Left"), lane("Right")],
        zones: vec![ZoneDef {
            name: "LeftHalf".to_string(),
            min: (-800.0, -800.0),
            max: (0.0, 800.0),
        }],
    }
}

#[test]
fn the_shipped_board_loads_and_describes_itself() {
    let map = shipped();

    assert_eq!(map.id, "green_td_v36");
    assert_eq!((map.tiles_x, map.tiles_y), (96, 96));
    assert_eq!(map.tile_size, 128.0);
    assert_eq!(
        map.lanes.len(),
        10,
        "nine colours, and red holds two spawns"
    );
    assert_eq!(
        map.zones.len(),
        11,
        "nine colour zones, plus the second Blue and Teal zones the south strip is \
         split into"
    );
    assert_eq!(map.goal.name, "END");

    // The board is a square centred on the origin, which is what the extracted
    // coordinates assume (-6144..6144).
    assert_eq!(map.half_extents(), Vec2::splat(6144.0));
    assert_eq!(map.world_to_cell(map.goal.centre), IVec2::new(48, 4));
}

#[test]
fn every_lane_is_named_and_arrives_at_the_goal() {
    let map = shipped();
    let mut names: Vec<&str> = map.lanes.iter().map(|lane| lane.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), map.lanes.len(), "lanes are named uniquely");

    for lane in &map.lanes {
        assert!(
            lane.spawn().distance(lane.points[0]) < f32::EPSILON,
            "lane {}: the spawn point is the first waypoint",
            lane.name
        );
        assert!(
            lane.sample(lane.total).distance(map.goal.centre) < f32::EPSILON,
            "lane {}: the last waypoint is the goal",
            lane.name
        );
        assert!(
            lane.total > map.tile_size,
            "lane {} is longer than a tile",
            lane.name
        );
    }
}

#[test]
fn the_lanes_share_one_goal_and_a_common_funnel() {
    // The funnel is the point of the board: ten lanes merge, and the last thing
    // they touch before the goal is the same place for all of them.
    let map = shipped();
    let entry = map
        .lanes
        .iter()
        .map(|lane| map.world_to_cell(lane.points[lane.points.len() - 2]))
        .collect::<std::collections::HashSet<_>>();
    assert!(entry.len() < map.lanes.len(), "the lanes meet somewhere");

    let latest: f32 = map
        .lanes
        .iter()
        .map(|lane| lane.total)
        .fold(f32::MIN, f32::max);
    let earliest: f32 = map
        .lanes
        .iter()
        .map(|lane| lane.total)
        .fold(f32::MAX, f32::min);
    assert!(
        earliest > 10_000.0 && latest < 40_000.0,
        "the lanes are 13k to 32k units, got {earliest} to {latest}"
    );
}

#[test]
fn a_wave_of_creeps_never_starts_within_a_tile_of_the_goal() {
    // `audit-013` (D10): a creep used to be able to spawn next to the
    // destination. A wave is a column at the spawn point, so the nearest a
    // fresh creep can be to the goal is the whole lane minus the arc.
    let map = shipped();
    let scaling = Balance::shipped()
        .expect("assets/balance loads")
        .0
        .waves
        .scaling
        .clone();

    for lane in &map.lanes {
        let clear = lane.total - scaling.spawn_arc;
        assert!(
            clear > map.tile_size * 10.0,
            "lane {} leaves only {clear} units between the wave's arrival window 
             and the goal",
            lane.name
        );
    }
}

// ---------------------------------------------------------------------------
// What the validator refuses
// ---------------------------------------------------------------------------

/// The validator's message for a board, or `None` if it accepted it.
fn refusal(def: MapDef) -> Option<String> {
    MapData::from_def(def).err()
}

#[test]
fn a_board_without_lanes_is_refused() {
    let mut def = valid_def();
    def.lanes.clear();
    let why = refusal(def).expect("a board needs lanes");
    assert!(why.contains("lanes"), "{why}");
}

#[test]
fn a_lane_that_does_not_end_at_the_goal_is_refused() {
    let mut def = valid_def();
    def.lanes[0].waypoints = vec![(-700.0, 700.0), (-700.0, 0.0)];
    let why = refusal(def).expect("the lane stops short");
    assert!(why.contains("lanes[0]") && why.contains("goal"), "{why}");
}

#[test]
fn a_spawn_point_that_is_not_the_first_waypoint_is_refused() {
    let mut def = valid_def();
    def.lanes[1].spawn = (0.0, 0.0);
    let why = refusal(def).expect("the spawn is somewhere else");
    assert!(why.contains("lanes[1].spawn"), "{why}");
}

#[test]
fn a_waypoint_off_the_board_is_refused() {
    let mut def = valid_def();
    def.lanes[0].waypoints[1] = (-900.0, 0.0); // the board is +-800
    let why = refusal(def).expect("that point is off the board");
    assert!(
        why.contains("waypoints[1]") && why.contains("board"),
        "{why}"
    );
}

#[test]
fn a_lane_with_nowhere_to_walk_is_refused() {
    let mut def = valid_def();
    def.lanes[0].waypoints = vec![(0.0, -700.0), (0.0, -700.0)];
    def.lanes[0].spawn = (0.0, -700.0);
    let why = refusal(def).expect("a lane of no length is not a lane");
    assert!(why.contains("shorter than a tile"), "{why}");
}

#[test]
fn a_goal_with_no_size_is_refused() {
    let mut def = valid_def();
    def.goal.half_h = 0.0;
    let why = refusal(def).expect("the goal must be a region");
    assert!(why.contains("goal"), "{why}");
}

#[test]
fn a_board_with_no_tiles_is_refused() {
    let mut def = valid_def();
    def.tiles_x = 0;
    let why = refusal(def).expect("a board of no tiles");
    assert!(why.contains("tiles_x"), "{why}");
}

#[test]
fn a_missing_file_and_a_malformed_file_are_both_named() {
    let missing = Map::load(std::path::Path::new("/nonexistent/maps"), "nope")
        .expect_err("there is no such map");
    assert!(
        matches!(missing, MapError::Read { .. }),
        "a missing map is a read failure, not a panic: {missing:?}"
    );
    assert!(missing.to_string().contains("nope.ron"), "{missing}");

    let dir = std::env::temp_dir().join("greentd-map-parse");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join("broken.ron");
    std::fs::write(&path, "(id: \"broken\", tiles_x: )").expect("the file is written");
    let malformed = Map::load(&dir, "broken").expect_err("that is not RON");
    assert!(
        matches!(malformed, MapError::Parse { .. }),
        "a malformed map names the file and the position: {malformed:?}"
    );
    assert!(malformed.to_string().contains("broken.ron"), "{malformed}");
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// What two peers must agree on (`net-001`)
// ---------------------------------------------------------------------------

#[test]
fn the_same_board_hashes_the_same_and_a_moved_waypoint_does_not() {
    let one = shipped();
    let two = shipped();
    assert_eq!(one.hash(), two.hash(), "the same file, the same hash");

    // A board is rules, not decoration: moving one waypoint changes where the
    // creeps walk and therefore every leak, so the hash has to move.
    let mut def = valid_def();
    let moved = MapData::from_def(def.clone()).expect("the valid board");
    def.lanes[0].waypoints[1].0 = -600.0;
    let shifted = MapData::from_def(def).expect("still valid, one waypoint along");
    assert_ne!(moved.hash(), shifted.hash());
}

#[test]
fn a_board_that_only_gained_a_zone_hashes_the_same() {
    // Zones are decoration and nothing reads them (the map file says so), so
    // they must not be able to make two peers refuse each other.
    let mut def = valid_def();
    let before = MapData::from_def(def.clone()).expect("valid").hash();
    def.zones.push(ZoneDef {
        name: "RightHalf".to_string(),
        min: (0.0, -800.0),
        max: (800.0, 800.0),
    });
    let after = MapData::from_def(def).expect("valid").hash();
    assert_eq!(before, after);
}

#[test]
fn the_clearance_must_leave_the_lanes_shootable() {
    // `config::check_map_against_balance` refuses a board whose clearance is
    // wider than every tower's range -- at which point no tower on the board
    // could ever hit a creep. The check lives in `config` because it needs both
    // halves; this is what it is protecting against, stated as numbers.
    let balance = Balance::shipped().expect("assets/balance loads");
    let map = shipped();
    let shortest = balance
        .0
        .towers
        .iter()
        .map(|tower| tower.range)
        .min_by(f32::total_cmp)
        .expect("there are towers");

    assert!(
        map.path_clearance < shortest,
        "the board asks for {} units of clearance and the shortest tower reaches \
         {shortest}: nothing could be built within range of a lane",
        map.path_clearance
    );

    // And the pairing is checked at startup, so this cannot quietly stop being
    // true when somebody scales a board: `config::startup_from` refuses to run
    // when the clearance is wider than the shortest tower's reach.
    let broken = MapData::from_def(MapDef {
        path_clearance: 10_000.0,
        ..valid_def()
    });
    let broken = broken.expect("a board is allowed to be badly tuned");
    assert!(
        broken.path_clearance > shortest,
        "which is exactly the shape the startup check refuses"
    );
}

#[test]
fn the_map_is_part_of_the_protocol_version() {
    // Two peers on the same tables but different boards must refuse each other:
    // the board decides where creeps walk and where the goal is, so a client
    // drawing a different one would be lying about every leak (D22).
    let balance = Balance::shipped().expect("assets/balance loads");
    let map = Map::load(
        std::path::Path::new(greentd::map::DEFAULT_MAP_DIR),
        "green_td_v36",
    )
    .expect("the shipped board");

    let ours = greentd::net::protocol::ProtocolVersion::current(&balance, &map);
    let theirs = greentd::net::protocol::ProtocolVersion {
        map_hash: ours.map_hash ^ 1,
        ..ours
    };
    let reason = ours.disagreement(&theirs).expect("a different board");
    assert!(reason.contains("map data"), "{reason}");
    assert!(!reason.contains("balance data"), "{reason}");
}
