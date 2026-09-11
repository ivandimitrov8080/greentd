//! Green TD, multiplayer-first on Bevy + lightyear.
//!
//! Modes and options are `greentd::config`; `cargo run -- --help` prints them.
//! The short version:
//!
//!   cargo run                          host: server + client in one process
//!   cargo run -- server                a dedicated server
//!   cargo run -- client                a client, joining 127.0.0.1:5000
//!   cargo run -- client 5101           a second client on one machine
//!   cargo run -- client --server 10.0.0.5:5000
//!
//! Architecture: the server owns a `Sim` and everything else is a mirror of it.
//! Clients send intents and render replicated state.
//!
//! # Where a change belongs
//!
//! This file is the entry point and nothing else: it resolves a run, picks the
//! plugin groups for the mode, and starts the app. Everything else is in one of
//! four layers, and the layer a change belongs in is nearly always decided by
//! *who has to agree about it*.
//!
//! | Layer   | Files                                                        | Belongs there when |
//! |---------+--------------------------------------------------------------+--------------------|
//! | `data/` | `balance`, `components`, `reject`                            | both peers must agree on it, and it is not how they talk |
//! | `sim/`  | `mod`, `waves`, `combat`, `economy`, `rng`                   | only the server may know it, and it decides an outcome |
//! | `net/`  | `protocol`, `messages`, `server`, `client`                   | it is a socket, a channel or a message |
//! | `ui/`   | `visuals`, `hud`                                             | a player sees it, and it cannot change the match |
//!
//! Three rules keep the layers from rotting into each other:
//!
//! 1. *`sim/` imports nothing from `net/` or `ui/`.* The simulation has to be
//!    replayable from a seed with no socket in scope, so the only thing it may
//!    name from outside its own folder is `data/` and `map`.
//! 2. *`ui/` never writes authoritative state.* It sends intents and reads what
//!    comes back, and the server decides. Every visual bug is therefore a
//!    display bug, never a rules bug.
//! 3. *A number the players can feel is not a literal.* It is a field in
//!    `assets/balance/*.ron`, loaded by `data::balance` and hashed into the
//!    protocol version (`decide-data-driven` in `tasks/README.org`).
//!
//! Three modules sit outside the four layers because they are the seam between
//! them: `config.rs` (one run's mode, addresses, tick rate and budgets),
//! `logging.rs` (the log targets, the level policy and the per-match log file,
//! `found-003`) and `ratelimit.rs` (how much a peer may spend per frame).
//! `map.rs` is static geometry shared by both peers, and `03-map.org` turns it
//! into a folder.

use std::time::Duration;

use bevy::app::{PluginGroupBuilder, ScheduleRunnerPlugin};
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use lightyear::prelude::client::ClientPlugins;
use lightyear::prelude::server::ServerPlugins;

use greentd::config::{self, Config, Mode, Startup};
use greentd::net::client::GreenTdClientPlugin;
use greentd::net::protocol;
use greentd::net::server::GreenTdServerPlugin;
use greentd::ui::visuals::VisualsPlugin;

fn main() {
    // Configuration and balance are resolved before any plugin exists: the sim
    // cannot be constructed without its tables, and a bad value has to abort
    // before a window is opened or a socket is bound. Both failures arrive as
    // one `StartupError`, so this is the only place a run can refuse to start
    // and the only place that has to print a reason (`found-006`).
    let run = match config::startup() {
        Ok(Startup::Run(run)) => run,
        Ok(Startup::Help) => {
            print!("{}", config::USAGE);
            return;
        }
        Err(err) => {
            eprintln!("greentd: {err}");
            std::process::exit(2);
        }
    };
    let config = run.config;
    let balance = run.balance;

    let tick = config.tick();
    let mut app = App::new();

    app.insert_resource(config.clone())
        .insert_resource(balance)
        .insert_resource(Time::<Fixed>::from_hz(config.tick_hz))
        .add_plugins(engine_plugins(&config, tick));

    // The protocol must be installed AFTER the lightyear plugin groups and
    // BEFORE any Client/Server entity exists.
    match config.mode {
        Mode::Server => {
            app.add_plugins(ServerPlugins {
                tick_duration: tick,
            });
            protocol::build_protocol(&mut app);
            app.add_plugins(GreenTdServerPlugin);
        }
        Mode::Client => {
            app.add_plugins(ClientPlugins {
                tick_duration: tick,
            });
            protocol::build_protocol(&mut app);
            app.add_plugins(GreenTdClientPlugin);
        }
        Mode::Host => {
            // Host: both roles in one App. Gains simplicity, loses the ability
            // to exercise a real latency path, so the client mode still exists.
            app.add_plugins(ClientPlugins {
                tick_duration: tick,
            });
            app.add_plugins(ServerPlugins {
                tick_duration: tick,
            });
            protocol::build_protocol(&mut app);
            app.add_plugins((GreenTdServerPlugin, GreenTdClientPlugin));
        }
    }

    // Presentation, and only for a peer that has a window. A dedicated server
    // renders nothing, so it gets neither a window nor 768 map tiles, and can
    // run on a box with no GPU at all (found-001, D15).
    if config.mode.runs_client() {
        app.add_plugins(VisualsPlugin);
    }

    app.run();
}

/// The engine plumbing one mode needs.
///
/// A rendering peer gets all of [`DefaultPlugins`] with the window it names; a
/// dedicated server gets [`MinimalPlugins`] plus logging and nothing else. The
/// distinction is the whole of `found-001`: a windowed server cannot run under
/// CI or on a headless box, and it was pure waste even on a desktop.
///
/// The server's schedule runner wakes once per sim tick rather than spinning,
/// so a dedicated server's frame and the sim's tick are the same clock.
fn engine_plugins(config: &Config, tick: Duration) -> PluginGroupBuilder {
    if config.mode.runs_client() {
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: format!("Green TD - {}", config.mode.as_str()),
                    resolution: (1280, 800).into(),
                    ..default()
                }),
                ..default()
            })
            .set(log_plugin(config))
    } else {
        MinimalPlugins
            .set(ScheduleRunnerPlugin::run_loop(tick))
            .add(StatesPlugin)
            .add(log_plugin(config))
    }
}

/// Bevy's own logging, unless the config asked for a filter. An empty filter
/// keeps Bevy's default, which is the one that silences wgpu and naga noise and
/// is therefore the right thing to leave alone.
///
/// The extra layer is the per-match log file (`found-003`); it is a bare `fn`
/// because that is what `LogPlugin` takes, and it finds the directory by
/// reading the `Config` resource out of the world.
fn log_plugin(config: &Config) -> LogPlugin {
    let mut plugin = LogPlugin::default();
    if !config.log_filter.is_empty() {
        plugin.filter = config.log_filter.clone();
    }
    plugin.custom_layer = greentd::logging::file_layer;
    plugin
}
