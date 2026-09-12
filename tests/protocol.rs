//! The protocol version: what two peers must agree on before one admits the
//! other (`net-001`, D22).
//!
//! Everything here is deliberately headless. The version is a function of the
//! loaded tables and one constant, so it can be judged without a socket -- and
//! the interesting cases are precisely the ones a live pair cannot be made to
//! produce on demand: a peer one schema revision behind, and a peer whose
//! tables disagree by a single number.

use std::sync::Arc;

use greentd::data::balance::{Balance, BalanceData};
use greentd::net::messages::{Handshake, HandshakeAck, PlayerId};
use greentd::net::protocol::{ProtocolVersion, SCHEMA_REVISION};

/// The shipped tables, as a peer would load them.
fn balance() -> Balance {
    Balance::shipped().expect("assets/balance loads")
}

/// The shipped tables, with one deliberate change, as an out-of-step peer would
/// have loaded them.
fn mutated(change: impl FnOnce(&mut BalanceData)) -> Balance {
    let mut data = balance().0.as_ref().clone();
    change(&mut data);
    Balance(Arc::new(data))
}

#[test]
fn two_peers_on_the_same_tables_agree() {
    let ours = ProtocolVersion::current(&balance());
    let theirs = ProtocolVersion::current(&balance());

    assert_eq!(ours, theirs, "the same assets must give the same version");
    assert_eq!(ours.disagreement(&theirs), None);
    assert_eq!(ours.schema, SCHEMA_REVISION);
    assert_eq!(
        ours.to_string(),
        format!("v{SCHEMA_REVISION} (balance {:016x})", ours.balance_hash)
    );
}

#[test]
fn a_schema_bump_names_the_schema_and_not_the_balance() {
    let ours = ProtocolVersion::current(&balance());
    let theirs = ProtocolVersion {
        schema: ours.schema + 1,
        ..ours
    };

    let reason = ours
        .disagreement(&theirs)
        .expect("a schema bump is a mismatch");
    assert!(reason.contains("schema revision"), "{reason}");
    assert!(
        reason.contains(&ours.schema.to_string()) && reason.contains(&theirs.schema.to_string()),
        "the reason must name both revisions, got: {reason}"
    );
    assert!(
        !reason.contains("balance data"),
        "the balance did not differ, so it must not be blamed: {reason}"
    );
}

#[test]
fn a_balance_change_names_the_balance_and_not_the_schema() {
    let ours = ProtocolVersion::current(&balance());
    let theirs = ProtocolVersion::current(&mutated(|data| {
        // One gold on one tower: the smallest change a player could feel, and
        // one that no schema revision would ever record.
        data.towers[0].cost += 1;
    }));

    assert_eq!(ours.schema, theirs.schema, "the schema itself is unchanged");

    let reason = ours
        .disagreement(&theirs)
        .expect("a changed number is a mismatch");
    assert!(reason.contains("balance data"), "{reason}");
    assert!(
        reason.contains(&format!("{:016x}", ours.balance_hash))
            && reason.contains(&format!("{:016x}", theirs.balance_hash)),
        "the reason must name both hashes, got: {reason}"
    );
    assert!(
        !reason.contains("schema revision"),
        "the schema did not differ, so it must not be blamed: {reason}"
    );
}

#[test]
fn both_parts_are_named_when_both_differ() {
    let ours = ProtocolVersion::current(&balance());
    let theirs = ProtocolVersion::current(&mutated(|data| data.creeps[0].hp_mult += 0.5));
    let theirs = ProtocolVersion {
        schema: ours.schema + 1,
        ..theirs
    };

    let reason = ours.disagreement(&theirs).expect("both differ");
    assert!(reason.contains("schema revision"), "{reason}");
    assert!(reason.contains("balance data"), "{reason}");
}

#[test]
fn the_version_tracks_the_rules_and_not_the_match() {
    // `found-009`: the seed is a *match* setting, not a rule. Two peers in two
    // matches on the same tables are not in disagreement, so changing only the
    // seed must leave the version alone ...
    let ours = ProtocolVersion::current(&balance());
    let other_match =
        ProtocolVersion::current(&mutated(|data| data.match_rules.seed ^= 0xdead_beef));
    assert_eq!(
        ours, other_match,
        "the seed is not part of the protocol version"
    );

    // ... while a number the client is allowed to display must move it.
    let retuned = ProtocolVersion::current(&mutated(|data| data.waves.scaling.hp_base += 1.0));
    assert_ne!(
        ours, retuned,
        "a tuned curve is part of the protocol version"
    );
}

#[test]
fn reordering_the_files_does_not_move_the_version() {
    // The hash canonicalises order, so two peers whose tables were written in
    // different orders still agree -- which is the whole reason the hash
    // normalises rather than hashing the files.
    let ours = ProtocolVersion::current(&balance());
    let reordered = ProtocolVersion::current(&mutated(|data| {
        data.towers.reverse();
        data.creeps.reverse();
    }));
    assert_eq!(ours, reordered);
}

#[test]
fn the_handshake_and_its_answer_round_trip() {
    // The two messages are the wire contract; a type that cannot survive a
    // serialise/deserialise cycle is a handshake that never completes. RON is
    // used here only because it needs no `App`; it exercises the same derives.
    let version = ProtocolVersion::current(&balance());
    let id = PlayerId(0x0bad_c0de_dead_beef);

    let sent = Handshake { version, id };
    let wire = ron::to_string(&sent).expect("a handshake serialises");
    let back: Handshake = ron::from_str(&wire).expect("a handshake deserialises");
    assert_eq!(back.version, version);
    assert_eq!(
        back.id, id,
        "the identity must survive the round trip, or a reconnect is a new player"
    );

    let refused = HandshakeAck::Refused {
        reason: "balance data (server 1, client 2)".to_string(),
    };
    let wire = ron::to_string(&refused).expect("a refusal serialises");
    let back: HandshakeAck = ron::from_str(&wire).expect("a refusal deserialises");
    match back {
        HandshakeAck::Refused { reason } => assert!(reason.contains("balance data"), "{reason}"),
        HandshakeAck::Accepted => panic!("a refusal came back as an acceptance"),
    }
}
