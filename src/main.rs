//! Green TD, multiplayer-first on Bevy + lightyear.
//!
//! Run modes:
//!   cargo run -- server            dedicated-ish server (windowed, for debugging)
//!   cargo run -- client            a client, default source port 5100
//!   cargo run -- client 5101       a second client on one machine
//!   cargo run                      host: server + client in one process
//!
//! Architecture: the server owns a `Sim` and everything else is a mirror of it.
//! Clients send intents and render replicated state. See `sim.rs`.

use std::time::Duration;

use bevy::prelude::*;
use lightyear::prelude::client::ClientPlugins;
use lightyear::prelude::server::ServerPlugins;

use greentd::balance::{self, Balance};
use greentd::client::{GreenTdClientPlugin, NetConfig};
use greentd::protocol;
use greentd::server::{GreenTdServerPlugin, HostMode, SERVER_ADDR};
use greentd::visuals::VisualsPlugin;

/// Simulation rate. Matches lightyear's tick so replication is 1:1 with sim
/// steps, which keeps the "mirror" logic in `server.rs` trivial.
const TICK_HZ: f64 = 30.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("host");

    // The balance tables are read before any plugin exists: the sim cannot be
    // constructed without them, and a bad table has to abort before a window is
    // opened or a socket is bound.
    let balance = Balance::load_or_exit(&balance::default_balance_dir());

    let tick = Duration::from_secs_f64(1.0 / TICK_HZ);
    let mut app = App::new();

    app.insert_resource(balance)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("Green TD - {mode}"),
                resolution: (1280, 800).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ));

    // The protocol must be installed AFTER the lightyear plugin groups and
    // BEFORE any Client/Server entity exists.
    match mode {
        "server" => {
            app.add_plugins(ServerPlugins {
                tick_duration: tick,
            });
            protocol::build_protocol(&mut app);
            app.insert_resource(HostMode(false))
                .add_plugins((GreenTdServerPlugin, VisualsPlugin));
        }
        "client" => {
            let port: u16 = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(5100);
            app.add_plugins(ClientPlugins {
                tick_duration: tick,
            });
            protocol::build_protocol(&mut app);
            app.insert_resource(NetConfig {
                server: SERVER_ADDR.parse().expect("valid server addr"),
                bind: format!("127.0.0.1:{port}")
                    .parse()
                    .expect("valid bind addr"),
            })
            .add_plugins((GreenTdClientPlugin, VisualsPlugin));
        }
        other => {
            if other != "host" {
                eprintln!("unknown mode {other:?}; expected server | client | host");
            }
            // Host: both roles in one App. Gains simplicity, loses the ability
            // to test a real latency path, so the client mode above still exists.
            app.add_plugins(ClientPlugins {
                tick_duration: tick,
            });
            app.add_plugins(ServerPlugins {
                tick_duration: tick,
            });
            protocol::build_protocol(&mut app);
            app.insert_resource(HostMode(true))
                .insert_resource(NetConfig {
                    server: SERVER_ADDR.parse().expect("valid server addr"),
                    bind: "127.0.0.1:5100".parse().expect("valid bind addr"),
                })
                .add_plugins((GreenTdServerPlugin, GreenTdClientPlugin, VisualsPlugin));
        }
    }

    app.run();
}
