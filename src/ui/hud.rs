//! The HUD, the notices, and the input that turns a click into an intent.
//!
//! This is the whole of the client's *presentation*: a text overlay, a short
//! lived notice line, and the translation of keys and clicks into the four
//! [`ClientCmd`]s. It owns no authoritative state -- every number it prints came
//! from the server, and every click it sends is a request the server may refuse.

use bevy::prelude::*;
use lightyear::prelude::client::*;
use lightyear::prelude::*;

use crate::data::balance::Balance;
use crate::data::components::{LocalPlayer, MatchView, Phase, PlayerView};
use crate::map::{in_bounds, world_to_cell};
use crate::net::messages::{ClientCmd, ReliableChannel, ServerNotice};
use crate::ui::visuals::MainCamera;

/// Which tower the player would place next, and what the server last said.
#[derive(Resource, Default)]
pub struct ClientUi {
    /// Which tower the player would place next.
    pub kind: u8,
    pub notice: String,
    pub notice_timer: f32,
    pub over: Option<(u32, u32)>,
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
            // `greentd::commands` is where a client's view of the exchange
            // belongs: the target is documented as "client intents, and what the
            // server did with them", and this is the second half. It is also the
            // only place a refusal, a wave notice or the match end becomes
            // visible in a log -- the HUD shows it and nothing else does
            // (`audit-005`).
            debug!(
                target: crate::logging::target::COMMANDS,
                "notice: {notice:?}"
            );
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
    // The match state *this* process owns, if it also runs the server, and the
    // copy a client was sent otherwise. Exactly one of the two matches on any
    // peer, and a host is the one case where both do -- its client half is sent
    // the whole `MatchView` because it is global state (`NetworkTarget::All`),
    // so those two are the same match and the authoritative one, which never
    // lags, wins. Counting views instead of reading a marked one is D30.
    authority: Query<&MatchView, Without<Remote>>,
    received: Query<&MatchView, With<Remote>>,
    // The one view this process owns, marked by whoever could prove it
    // (`found-010`). Never `single()` over every view: in host mode that is
    // every view in the match, not one (D30, D3).
    me: Query<&PlayerView, With<LocalPlayer>>,
    balance: Res<Balance>,
    mut hud: Query<&mut Text, With<Hud>>,
    ui: Res<ClientUi>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let mut out = String::new();

    match authority.iter().next().or_else(|| received.iter().next()) {
        Some(mv) => out.push_str(&format!(
            "wave {:<3}  creeps {}/{}   players {}   [{}]\n",
            mv.wave,
            mv.live_creeps,
            mv.overrun_cap,
            mv.players,
            Phase::from_u8(mv.phase).text()
        )),
        None => out.push_str("connecting...\n"),
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

/// The HUD, added by [`GreenTdClientPlugin`](crate::net::client::GreenTdClientPlugin).
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientUi>()
            .add_systems(Startup, setup_hud)
            .add_systems(
                Update,
                (player_input, read_notices, decay_notice, update_hud).chain(),
            );
    }
}
