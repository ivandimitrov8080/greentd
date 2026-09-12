//! Stable player identity across a reconnect (`net-002`, D9).
//!
//! All headless. The admission rules are pure functions of an identity, a
//! connection entity and the set of links already in the match, so the three
//! interesting cases -- a fresh identity, a retransmitted handshake, and a
//! second connection claiming an identity that is already in the match -- need
//! no socket and no `App`.
//!
//! What is *not* here, and where it is instead: the live half -- two peers, one
//! of them reconnecting from a new port -- needs an `App` with a running
//! `Server`, which is `tests/12-testing.org`'s `test-008` harness. Until it
//! exists, that half is a transcript, exactly as `net-001`'s refusal is.

use std::collections::HashMap;

use bevy::prelude::Entity;
use greentd::net::messages::PlayerId;
use greentd::net::server::{Claim, claim, player_key};
use greentd::sim::PlayerKey;

/// A stand-in connection entity. Tests need a distinct `Entity` per peer, not a
/// real one, and `Entity` is constructible from a raw index without a `World`.
fn conn(index: u32) -> Entity {
    Entity::from_raw_u32(index).expect("a valid entity index")
}

#[test]
fn the_key_comes_from_the_identity_and_not_the_socket() {
    // The whole point of D9: the key is a function of the client's identity, so
    // two peers on different ports with the same id are one player.
    assert_eq!(player_key(PlayerId(7)), PlayerKey(7));
    assert_eq!(player_key(PlayerId(0xdead_beef)), PlayerKey(0xdead_beef));
    // And it is total: assigning the same identity twice is the same key.
    assert_eq!(player_key(PlayerId(42)), player_key(PlayerId(42)));
}

#[test]
fn a_fresh_identity_is_admitted_under_its_own_key() {
    let links: HashMap<PlayerKey, Entity> = HashMap::new();
    assert_eq!(
        claim(PlayerId(99), conn(1), &links),
        Claim::Fresh(PlayerKey(99))
    );
}

#[test]
fn a_retransmitted_handshake_is_not_a_second_player() {
    // The client repeats its handshake until the server answers (`net-001`), so
    // the same connection stating the same identity again must be a no-op.
    let mut links = HashMap::new();
    links.insert(PlayerKey(99), conn(1));
    assert_eq!(
        claim(PlayerId(99), conn(1), &links),
        Claim::AlreadyHeld(PlayerKey(99))
    );
}

#[test]
fn a_second_connection_finds_a_live_identity_taken() {
    // The documented rule of `net-002`: the newest connection presenting an
    // identity is the player. `claim` reports that a *different* connection
    // already holds it; the server's response is to drop that connection and
    // admit this one under the same key -- never a second player, and never
    // silently.
    let mut links = HashMap::new();
    links.insert(PlayerKey(99), conn(1));
    assert_eq!(
        claim(PlayerId(99), conn(2), &links),
        Claim::Taken(PlayerKey(99))
    );
}

#[test]
fn an_identity_can_be_rejoined_once_its_link_is_gone() {
    // A clean disconnect removes the link (and keeps the player), which is what
    // makes a reconnect from a *new* connection -- and therefore a new port --
    // resolve to the same key rather than to a stranger.
    let mut links = HashMap::new();
    links.insert(PlayerKey(99), conn(1));
    assert_eq!(
        claim(PlayerId(99), conn(2), &links),
        Claim::Taken(PlayerKey(99)),
        "while the first link is live the identity is taken"
    );

    links.remove(&PlayerKey(99));
    assert_eq!(
        claim(PlayerId(99), conn(2), &links),
        Claim::Fresh(PlayerKey(99)),
        "and once it is gone, the same identity is the same player again"
    );
}

#[test]
fn two_identities_never_collide() {
    // Two peers in one map, and a third identity neither holds.
    let mut links = HashMap::new();
    links.insert(player_key(PlayerId(1)), conn(1));
    links.insert(player_key(PlayerId(2)), conn(2));

    assert_eq!(
        claim(PlayerId(1), conn(3), &links),
        Claim::Taken(PlayerKey(1))
    );
    assert_eq!(
        claim(PlayerId(3), conn(3), &links),
        Claim::Fresh(PlayerKey(3))
    );
}

#[test]
fn a_generated_identity_is_nonzero_and_generally_distinct() {
    // The sim reserves `PlayerKey(0)` for "no player" (`Creep::last_hit_by`), so
    // a generated id must not be zero, and two generations must not be equal --
    // else two clients started together would be one player.
    let a = PlayerId::generate();
    let b = PlayerId::generate();
    assert_ne!(a.0, 0, "zero is the sim's 'no player' sentinel");
    assert_ne!(b.0, 0, "zero is the sim's 'no player' sentinel");
    assert_ne!(a, b, "two clients must not generate one identity");
}
