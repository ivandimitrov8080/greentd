//! Presentation for replicated state. Runs on any peer that has a window
//! (client, or a host running both roles).
//!
//! These systems only ever *read* replicated components and attach local
//! rendering components. Nothing here is registered for replication, so the
//! sprites never travel over the wire.

use bevy::camera::ScalingMode;
use bevy::prelude::*;

use crate::data::components::*;
use crate::map::*;

/// Tower kind -> sprite colour.
///
/// Presentation only, so it is code rather than a balance table: `11-content`
/// owns the real art, and until then a colour is the cheapest way to tell three
/// towers apart and to see a tier change at a glance.
pub fn tower_color(kind: u8) -> Color {
    match kind {
        1 => Color::srgb(0.85, 0.45, 0.15),
        2 => Color::srgb(0.35, 0.65, 0.95),
        _ => Color::srgb(0.90, 0.85, 0.30),
    }
}

#[derive(Component)]
#[allow(dead_code)]
struct MapTile;

#[derive(Component)]
pub struct MainCamera;

/// A creep's replicated state and the local transform/sprite it drives, plus
/// the change filter that decides when to re-render it. Named because the
/// spelled-out query is four components wide and appears in a signature; the
/// alias is what lets the function read as "track creeps", not as "a tuple of
/// seven things".
type CreepRender<'w, 's> = Query<
    'w,
    's,
    (
        &'static CreepVis,
        &'static CreepHp,
        &'static mut Transform,
        &'static mut Sprite,
    ),
    Or<(Changed<CreepVis>, Changed<CreepHp>)>,
>;

/// The tower equivalent of [`CreepRender`].
type TowerRender<'w, 's> = Query<
    'w,
    's,
    (
        &'static TowerAt,
        &'static TowerVis,
        &'static mut Transform,
        &'static mut Sprite,
    ),
    Or<(Changed<TowerAt>, Changed<TowerVis>)>,
>;

// ---------------------------------------------------------------------------
// Spawn-time attachment
// ---------------------------------------------------------------------------

fn attach_creep_sprite(add: On<Add, CreepVis>, vis: Query<&CreepVis>, mut commands: Commands) {
    // Seed the transform so a creep never renders at the origin for a frame
    // while waiting for the first `track_creeps` pass.
    let pos = vis
        .get(add.entity)
        .map(|v| Vec2::new(v.x, v.y))
        .unwrap_or_default();
    commands.entity(add.entity).insert((
        Name::new("CreepVisual"),
        Sprite::from_color(CREEP_BASE, Vec2::splat(TILE * 0.55)),
        Transform::from_xyz(pos.x, pos.y, 10.0),
    ));
}

fn attach_tower_sprite(add: On<Add, TowerAt>, at: Query<&TowerAt>, mut commands: Commands) {
    let pos = at
        .get(add.entity)
        .map(|a| cell_to_world(IVec2::new(a.x, a.y)))
        .unwrap_or_default();
    commands.entity(add.entity).insert((
        Name::new("TowerVisual"),
        Sprite::from_color(Color::WHITE, Vec2::splat(TILE * 0.7)),
        Transform::from_xyz(pos.x, pos.y, 8.0),
    ));
}

const CREEP_BASE: Color = Color::srgb(0.80, 0.25, 0.25);

// ---------------------------------------------------------------------------
// Per-frame sync
// ---------------------------------------------------------------------------

fn track_creeps(mut q: CreepRender) {
    for (vis, hp, mut transform, mut sprite) in &mut q {
        transform.translation.x = vis.x;
        transform.translation.y = vis.y;
        // Bleed the sprite toward dark as the creep loses health: cheap,
        // readable feedback without a health-bar hierarchy.
        let f = (hp.hp / hp.max.max(1.0)).clamp(0.0, 1.0);
        sprite.color = Color::srgb(0.15 + 0.70 * f, 0.12 + 0.20 * f, 0.12 + 0.20 * f);
    }
}

fn track_towers(mut q: TowerRender) {
    for (at, vis, mut transform, mut sprite) in &mut q {
        let p = cell_to_world(IVec2::new(at.x, at.y));
        transform.translation.x = p.x;
        transform.translation.y = p.y;
        // Tiers grow the sprite, so an upgraded tower reads at a glance.
        let scale = TILE * (0.60 + 0.07 * (vis.level.min(6) as f32));
        sprite.custom_size = Some(Vec2::splat(scale));
        sprite.color = tower_color(vis.kind);
    }
}

// ---------------------------------------------------------------------------
// Static world
// ---------------------------------------------------------------------------

fn setup_camera_and_map(mut commands: Commands) {
    commands.spawn((
        Name::new("MainCamera"),
        MainCamera,
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            // AutoMin keeps the aspect ratio while guaranteeing the whole
            // board is visible regardless of window size.
            scaling_mode: ScalingMode::AutoMin {
                min_width: WORLD_W + 96.0,
                min_height: WORLD_H + 96.0,
            },
            ..OrthographicProjection::default_2d()
        }),
    ));

    let path = Path::default();
    for y in 0..GRID_H {
        for x in 0..GRID_W {
            let cell = IVec2::new(x, y);
            let p = cell_to_world(cell);
            let on_path = !is_buildable(&path, cell);
            let color = if on_path {
                Color::srgb(0.30, 0.24, 0.16)
            } else {
                Color::srgb(0.14, 0.40, 0.17)
            };
            commands.spawn((
                MapTile,
                Sprite::from_color(color, Vec2::splat(TILE - 1.0)),
                Transform::from_xyz(p.x, p.y, 0.0),
            ));
        }
    }
}

pub struct VisualsPlugin;

impl Plugin for VisualsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_camera_and_map)
            .add_observer(attach_creep_sprite)
            .add_observer(attach_tower_sprite)
            .add_systems(Update, (track_creeps, track_towers));
    }
}
