//! greentd, as a library.
//!
//! `src/main.rs` is a thin wrapper around these modules, which exist as a lib so
//! the simulation, the balance loader and the rules can be exercised from
//! `tests/` with no window, no GPU and no socket (see `tasks/12-testing.org`
//! `test-001`).
//!
//! The layout, and which layer a change belongs in, is written down in the
//! module doc of `src/main.rs`. In one line: `data/` is what both peers are,
//! `sim/` is what only the server knows, `net/` is how the two agree, and `ui/`
//! is what a player sees.

pub mod config;
pub mod data;
pub mod logging;
pub mod map;
pub mod net;
pub mod ratelimit;
pub mod sim;
pub mod ui;
