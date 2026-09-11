//! greentd, as a library.
//!
//! `src/main.rs` is a thin wrapper around these modules, which exist as a lib so
//! the simulation, the balance loader and the rules can be exercised from
//! `tests/` with no window, no GPU and no socket (see `tasks/12-testing.org`
//! `test-001`).
//!
//! Module map, and where a change belongs:
//!
//! | Module      | Owns                                                    |
//! |-------------+---------------------------------------------------------|
//! | `balance`   | every tuned number, loaded from `assets/balance/*.ron`   |
//! | `game`      | replicated components, client/server messages, `Reject`  |
//! | `map`       | static geometry: the ring, cells, buildability           |
//! | `sim`       | the authoritative state machine; no networking types     |
//! | `protocol`  | the lightyear contract: messages, channels, components   |
//! | `server`    | authority: intents in, replicated mirrors out            |
//! | `client`    | intents out, notices in, HUD                             |
//! | `visuals`   | presentation of replicated state; no authority           |
//!
//! `found-005` splits these further into `sim/`, `data/`, `net/` and `ui/`.

pub mod balance;
pub mod client;
pub mod game;
pub mod map;
pub mod protocol;
pub mod server;
pub mod sim;
pub mod visuals;
