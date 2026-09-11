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
//! Clients send intents and render replicated state. See `sim.rs`.

use bevy::log::LogPlugin;
use bevy::prelude::*;
use lightyear::prelude::client::ClientPlugins;
use lightyear::prelude::server::ServerPlugins;

use greentd::balance::Balance;
use greentd::client::GreenTdClientPlugin;
use greentd::config::{self, Config, Mode, Startup};
use greentd::protocol;
use greentd::server::GreenTdServerPlugin;
use greentd::visuals::VisualsPlugin;

fn main() {
    // Configuration and balance are resolved before any plugin exists: the sim
    // cannot be constructed without its tables, and a bad value has to abort
    // before a window is opened or a socket is bound.
    let config = match config::startup() {
        Ok(Startup::Run(config)) => config,
        Ok(Startup::Help) => {
            print!("{}", config::USAGE);
            return;
        }
        Err(err) => {
            eprintln!("greentd: {err}");
            std::process::exit(2);
        }
    };
    let balance = Balance::load_or_exit(&config.balance_dir);

    let tick = config.tick();
    let mut app = App::new();

    app.insert_resource(config.clone())
        .insert_resource(balance)
        .insert_resource(Time::<Fixed>::from_hz(config.tick_hz))
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: format!("Green TD - {}", config.mode.as_str()),
                        resolution: (1280, 800).into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(log_plugin(&config)),
        );

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

    // Presentation. `found-001` is what keeps this out of `server` mode, where
    // it opens a window a dedicated server has no use for (D15).
    app.add_plugins(VisualsPlugin);

    app.run();
}

/// Bevy's own logging, unless the config asked for a filter. An empty filter
/// keeps Bevy's default, which is the one that silences wgpu and naga noise and
/// is therefore the right thing to leave alone.
fn log_plugin(config: &Config) -> LogPlugin {
    if config.log_filter.is_empty() {
        LogPlugin::default()
    } else {
        LogPlugin {
            filter: config.log_filter.clone(),
            ..default()
        }
    }
}
