//! Why an intent was refused.
//!
//! [`Reject`] is shared vocabulary rather than transport: the sim *produces* it
//! (`try_build` and friends return `Result<(), Reject>`), and `net/messages.rs`
//! *carries* it back to the player inside a `ServerNotice`. It lives in `data/`
//! for exactly that reason -- so the sim can name its own refusals without
//! importing anything from the networking side.
//!
//! It is the opposite of `config::StartupError`: a `Reject` is an expected
//! refusal inside a running match, it carries a reason a player can act on, and
//! it is sent to that player. A `StartupError` is a programmer or configuration
//! mistake, it is fatal, it is printed to whoever started the process, and it
//! never travels over the network.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    NotEnoughGold,
    Occupied,
    PathBlocked,
    OutOfBounds,
    BadKind,
    NoSuchTower,
    /// The caller does not own the tower it tried to upgrade or sell (D1/D2).
    NotOwner,
    /// The caller is issuing commands faster than the server will accept (D6/D7).
    RateLimited,
    /// The caller has no player record, so it is not in this match (D8).
    NotInMatch,
    MatchOver,
}

impl Reject {
    /// Every variant, so a test can prove none of them is unhandled.
    pub const ALL: [Reject; 10] = [
        Reject::NotEnoughGold,
        Reject::Occupied,
        Reject::PathBlocked,
        Reject::OutOfBounds,
        Reject::BadKind,
        Reject::NoSuchTower,
        Reject::NotOwner,
        Reject::RateLimited,
        Reject::NotInMatch,
        Reject::MatchOver,
    ];

    pub fn text(self) -> &'static str {
        match self {
            Reject::NotEnoughGold => "not enough gold",
            Reject::Occupied => "cell occupied",
            Reject::PathBlocked => "cannot build on the path",
            Reject::OutOfBounds => "out of bounds",
            Reject::BadKind => "unknown tower type",
            Reject::NoSuchTower => "no tower there",
            Reject::NotOwner => "that tower is not yours",
            Reject::RateLimited => "too many commands",
            Reject::NotInMatch => "you are not in this match",
            Reject::MatchOver => "match is over",
        }
    }
}
