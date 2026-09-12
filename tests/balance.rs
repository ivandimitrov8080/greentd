//! The balance loader: parsing, cross-reference validation and the hash.
//!
//! `found-004` requires that a bad table fails loudly, naming the offending
//! field path, and that two peers that load the same assets agree on the hash.
//! These tests are the proof; they never touch a window or a socket.

use std::path::PathBuf;

use greentd::data::balance::{BalanceData, BalanceError, WaveCreep, WaveDef};

fn balance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/balance")
}

#[test]
fn the_shipped_tables_load_and_hash_stably() {
    let first = BalanceData::shipped().expect("assets/balance loads");
    let second = BalanceData::shipped().expect("assets/balance loads twice");

    assert_eq!(
        first.hash(),
        second.hash(),
        "loading the same assets twice must produce the same hash"
    );
    assert!(!first.towers.is_empty());
    assert!(!first.creeps.is_empty());
    assert_eq!(
        first.tower_kind_count() as usize,
        first.towers.len(),
        "the wire-level tower kind count matches the table"
    );
}

#[test]
fn the_hash_ignores_entry_order_but_not_values() {
    let mut reordered = BalanceData::shipped().expect("assets/balance loads");
    let baseline = reordered.hash();

    reordered.towers.reverse();
    reordered.creeps.reverse();
    assert_eq!(
        reordered.hash(),
        baseline,
        "moving entries around must not change the hash"
    );

    reordered.towers[0].cost += 1;
    assert_ne!(
        reordered.hash(),
        baseline,
        "changing a number must change the hash"
    );
}

#[test]
fn an_unknown_creep_model_names_the_field_path() {
    let mut data = BalanceData::shipped().expect("assets/balance loads");
    data.waves.waves = vec![WaveDef {
        index: 1,
        creeps: vec![WaveCreep {
            model: "not-a-creep".to_string(),
            count: 6,
        }],
    }];

    let err = data
        .validate()
        .expect_err("an unknown model is a hard error");
    let text = err.to_string();
    assert!(
        text.contains("waves[0].creeps[0].model"),
        "the message must name the field path, got: {text}"
    );
}

#[test]
fn a_tier_chain_must_resolve_be_acyclic_and_get_stronger() {
    let mut data = BalanceData::shipped().expect("assets/balance loads");

    // A self-cycle.
    data.towers[0].next = Some("basic".to_string());
    let cycle = data.validate().expect_err("a cycle is a hard error");
    assert!(cycle.to_string().contains("towers[0].next"), "{cycle}");

    // A chain that resolves and strengthens is fine.
    data.towers[0].next = Some("cannon".to_string());
    data.validate().expect("basic -> cannon is monotonic");

    // ... but a cheaper successor is not.
    data.towers[1].next = Some("frost".to_string());
    let weaker = data
        .validate()
        .expect_err("a cheaper successor is a hard error");
    assert!(weaker.to_string().contains("towers[1].next"), "{weaker}");

    // An unresolvable link is not either.
    data.towers[1].next = Some("ghost".to_string());
    let unknown = data
        .validate()
        .expect_err("an unknown tower is a hard error");
    assert!(unknown.to_string().contains("towers[1].next"), "{unknown}");
}

#[test]
fn a_missing_directory_fails_loudly() {
    let err = BalanceData::load(&balance_dir().join("no-such-directory"))
        .expect_err("a missing directory is a hard error");
    assert!(matches!(err, BalanceError::Io { .. }), "{err}");
}

#[test]
fn a_malformed_table_is_a_parse_error_naming_the_file() {
    let dir = std::env::temp_dir().join("greentd-balance-malformed");
    std::fs::create_dir_all(&dir).expect("temp dir");
    // `match.ron` is read first, so the malformed file is the one that fails.
    std::fs::write(dir.join("match.ron"), "(start_gold: oops)").expect("write the bad table");

    let err = BalanceData::load(&dir).expect_err("a malformed table is a hard error");
    assert!(matches!(err, BalanceError::Parse { .. }), "{err}");
    assert!(err.to_string().contains("match.ron"), "{err}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_curves_reproduce_wave_one() {
    let data = BalanceData::shipped().expect("assets/balance loads");
    let scaling = &data.waves.scaling;

    assert_eq!(data.creep_hp(1), scaling.hp_base);
    assert_eq!(data.creep_speed(1), scaling.speed_base);
    assert_eq!(data.creep_bounty(1), scaling.bounty_base as u32);
    assert_eq!(data.creeps_per_wave(1, 1), scaling.count_base as u32);
    assert_eq!(
        data.creeps_per_wave(3, 2),
        data.creeps_per_wave(3, 1) * 2,
        "a bigger lobby spawns proportionally more creeps"
    );
}

#[test]
fn the_last_sim_literals_are_data_now() {
    // D18's remainder (audit-011): the per-tier growth, the slow duration and
    // the sell refund are table values, so a re-tune is a data edit and both
    // peers hash them.
    let mut data = BalanceData::shipped().expect("assets/balance loads");
    let before = data.hash();

    data.towers[0].damage_per_level = 1.25;
    assert_ne!(
        data.hash(),
        before,
        "the tier growth is part of the ruleset, so it must move the hash"
    );
    data.validate().expect("a larger growth is still valid");

    data.towers[0].damage_per_level = -1.0;
    let err = data
        .validate()
        .expect_err("negative growth is a hard error");
    assert!(
        err.to_string().contains("towers[0].damage_per_level"),
        "{err}"
    );
    data.towers[0].damage_per_level = 0.45;

    data.match_rules.sell_refund_percent = 101;
    let err = data
        .validate()
        .expect_err("a refund over 100% is a hard error");
    assert!(
        err.to_string().contains("match_rules.sell_refund_percent"),
        "{err}"
    );

    // The refund is exact integer arithmetic, not `cost * 0.7` with a float.
    let mut data = BalanceData::shipped().expect("assets/balance loads");
    data.match_rules.sell_refund_percent = 70;
    let basic = data.towers[0].clone();
    assert_eq!(data.sell_refund(&basic, 1), 56, "70% of 80 is 56, not 55");
    data.match_rules.sell_refund_percent = 100;
    assert_eq!(data.sell_refund(&basic, 2), 160, "100% of 80 x 2 is 160");
}
