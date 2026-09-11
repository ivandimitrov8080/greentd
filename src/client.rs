//! Client: a dumb renderer of authoritative state.
//!
//! It has no simulation, no prediction and no authority. It sends intents,
//! receives replicated components, and draws them. Because a TD has no
//! twitch input, that latency model is entirely acceptable -- and it removes
//! every determinism problem that a per-peer sim would introduce.

use std::net::SocketAddr;

use bevy::prelude::*;
use lightyear::prelude::client::*;
use lightyear::prelude::*;

use crate::balance::Balance;
use crate::game::*;
use crate::map::*;
use crate::visuals::MainCamera;

#[derive(Resource, Clone)]
pub struct NetConfig {
    pub server: SocketAddr,
    /// Each client needs its own source port on one machine.
    pub bind: SocketAddr,
}

fn spawn_client(mut commands: Commands, cfg: Res<NetConfig>) {
    let client = commands
        .spawn((
            Name::new("Client"),
            RawClient,
            // The IO layer. Without this the link never binds a socket and
            // nothing is ever sent -- `Connect` would silently do nothing.
            UdpIo::default(),
            LocalAddr(cfg.bind),
            PeerAddr(cfg.server),
            Link::default(),
            ReplicationReceiver,
            // Inserted eagerly so we never miss the first notice while the
            // receiver component is being created lazily by the message layer.
            MessageReceiver::<ServerNotice>::default(),
        ))
        .id();
    commands.trigger(Connect { entity: client });
}

// ---------------------------------------------------------------------------
// Local UI state
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct ClientUi {
    /// Which tower the player would place next.
    pub kind: u8,
    pub notice: String,
    pub notice_timer: f32,
    pub over: Option<(u32, u32)>,
}

impl Default for ClientUi {
    fn default() -> Self {
        Self {
            kind: 0,
            notice: String::new(),
            notice_timer: 0.0,
            over: None,
        }
    }
}

#[derive(Component)]
struct Hud;

fn setup_hud(mut commands: Commands) {
    commands.spawn((
        Hud,
        Text::new(String::new()),
        TextFont {
            font_size: FontSize::Px(18.0),
            ..default()
        },
        TextColor(Color::srgb(0.95, 0.95, 0.95)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            ..default()
        },
    ));
}

// ---------------------------------------------------------------------------
// Intents out
// ---------------------------------------------------------------------------

fn player_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    mut senders: Query<&mut MessageSender<ClientCmd>, With<Connected>>,
    mut ui: ResMut<ClientUi>,
) {
    let Ok(mut sender) = senders.single_mut() else {
        return;
    };

    if keys.just_pressed(KeyCode::Digit1) {
        ui.kind = 0;
    }
    if keys.just_pressed(KeyCode::Digit2) {
        ui.kind = 1;
    }
    if keys.just_pressed(KeyCode::Digit3) {
        ui.kind = 2;
    }
    if keys.just_pressed(KeyCode::Space) {
        sender.send::<ReliableChannel>(ClientCmd::CallWave);
    }

    // Translate the cursor into a grid cell.
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, cam_transform)) = cameras.single() else {
        return;
    };
    let Ok(world) = camera.viewport_to_world_2d(cam_transform, cursor) else {
        return;
    };
    let cell = world_to_cell(world);
    if !in_bounds(cell) {
        return;
    }

    if buttons.just_pressed(MouseButton::Left) {
        sender.send::<ReliableChannel>(ClientCmd::Build {
            x: cell.x,
            y: cell.y,
            kind: ui.kind,
        });
    }
    if keys.just_pressed(KeyCode::KeyU) {
        sender.send::<ReliableChannel>(ClientCmd::Upgrade {
            x: cell.x,
            y: cell.y,
        });
    }
    if buttons.just_pressed(MouseButton::Right) {
        sender.send::<ReliableChannel>(ClientCmd::Sell {
            x: cell.x,
            y: cell.y,
        });
    }
}

// ---------------------------------------------------------------------------
// Notices in
// ---------------------------------------------------------------------------

fn read_notices(
    mut receivers: Query<&mut MessageReceiver<ServerNotice>>,
    mut ui: ResMut<ClientUi>,
) {
    for mut receiver in receivers.iter_mut() {
        for notice in receiver.receive() {
            match notice {
                ServerNotice::Ok => {
                    ui.notice.clear();
                    ui.notice_timer = 0.0;
                }
                ServerNotice::Err(reason) => {
                    ui.notice = reason.text().to_string();
                    ui.notice_timer = 2.0;
                }
                ServerNotice::WaveStarted(w) => {
                    ui.notice = format!("wave {w}");
                    ui.notice_timer = 1.5;
                }
                ServerNotice::GameOver { wave, live } => {
                    ui.over = Some((wave, live));
                }
            }
        }
    }
}

fn decay_notice(time: Res<Time>, mut ui: ResMut<ClientUi>) {
    if ui.notice_timer > 0.0 {
        ui.notice_timer -= time.delta_secs();
        if ui.notice_timer <= 0.0 {
            ui.notice.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// HUD
// ---------------------------------------------------------------------------

fn update_hud(
    sim: Query<&MatchView>,
    // No `Remote` filter: a remote client only ever receives its own view,
    // while a host already holds the authoritative one.
    me: Query<&PlayerView>,
    balance: Res<Balance>,
    mut hud: Query<&mut Text, With<Hud>>,
    ui: Res<ClientUi>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let mut out = String::new();

    let players = me.iter().count();
    if let Ok(mv) = sim.single() {
        out.push_str(&format!(
            "wave {:<3}  creeps {}/{}   players {}   [{}]\n",
            mv.wave,
            mv.live_creeps,
            mv.overrun_cap,
            players,
            Phase::from_u8(mv.phase).text()
        ));
    } else {
        out.push_str("connecting...\n");
    }

    if let Ok(pv) = me.single() {
        out.push_str(&format!("gold {}   kills {}\n", pv.gold, pv.kills));
    }

    if let Some(current) = balance.tower(ui.kind) {
        out.push_str(&format!(
            "\n[{}] {}  cost {}   range {:.0}\n",
            ui.kind + 1,
            current.name,
            current.cost,
            current.range
        ));
    }
    out.push_str("1/2/3 pick  LMB build  U upgrade  RMB sell\n");
    out.push_str("Space: call next wave early\n");

    if !ui.notice.is_empty() {
        out.push_str(&format!("\n{}", ui.notice));
    }
    if let Some((wave, live)) = ui.over {
        out.push_str(&format!(
            "\n\nGAME OVER - overrun at wave {wave} ({live} creeps)"
        ));
    }

    **text = out;
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct GreenTdClientPlugin;

impl Plugin for GreenTdClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientUi>()
            .add_systems(Startup, (spawn_client, setup_hud).chain())
            .add_systems(
                Update,
                (player_input, read_notices, decay_notice, update_hud).chain(),
            );
    }
}
