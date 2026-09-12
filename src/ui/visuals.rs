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

fn attach_creep_sprite(
    add: On<Add, CreepVis>,
    vis: Query<&CreepVis>,
    map: Res<Map>,
    mut commands: Commands,
) {
    // Seed the transform so a creep never renders at the origin for a frame
    // while waiting for the first `track_creeps` pass.
    let pos = vis
        .get(add.entity)
        .map(|v| Vec2::new(v.x, v.y))
        .unwrap_or_default();
    commands.entity(add.entity).insert((
        Name::new("CreepVisual"),
        Sprite::from_color(CREEP_BASE, Vec2::splat(map.0.tile_size * 0.35)),
        Transform::from_xyz(pos.x, pos.y, 10.0),
    ));
}

fn attach_tower_sprite(
    add: On<Add, TowerAt>,
    at: Query<&TowerAt>,
    map: Res<Map>,
    mut commands: Commands,
) {
    let pos = at
        .get(add.entity)
        .map(|a| map.0.cell_to_world(IVec2::new(a.x, a.y)))
        .unwrap_or_default();
    commands.entity(add.entity).insert((
        Name::new("TowerVisual"),
        Sprite::from_color(Color::WHITE, Vec2::splat(map.0.tile_size * 0.7)),
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

fn track_towers(map: Res<Map>, mut q: TowerRender) {
    for (at, vis, mut transform, mut sprite) in &mut q {
        let p = map.0.cell_to_world(IVec2::new(at.x, at.y));
        let tile = map.0.tile_size;
        transform.translation.x = p.x;
        transform.translation.y = p.y;
        // Tiers grow the sprite, so an upgraded tower reads at a glance.
        let scale = tile * (0.60 + 0.07 * (vis.level.min(6) as f32));
        sprite.custom_size = Some(Vec2::splat(scale));
        sprite.color = tower_color(vis.kind);
    }
}

// ---------------------------------------------------------------------------
// Static world
// ---------------------------------------------------------------------------

/// Presentation constants for the board. Colours are code rather than balance
/// because `11-content.org` owns the real art; these are what tells a lane from
/// the grass it cuts through and a spawn point from the goal.
const GRASS: Color = Color::srgb(0.07, 0.16, 0.09);
const ZONE_INK: Color = Color::srgb(0.10, 0.22, 0.12);
const LANE_INK: Color = Color::srgb(0.30, 0.24, 0.16);
const SPAWN_INK: Color = Color::srgb(0.65, 0.20, 0.20);
const GOAL_INK: Color = Color::srgb(0.95, 0.85, 0.25);
/// How wide a lane is drawn, as a fraction of a tile.
const LANE_WIDTH: f32 = 0.55;

fn setup_camera_and_map(map: Res<Map>, mut commands: Commands) {
    let map = &map.0;
    let half = map.half_extents();

    commands.spawn((
        Name::new("MainCamera"),
        MainCamera,
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            // AutoMin keeps the aspect ratio while guaranteeing the whole board
            // is visible regardless of window size. The bounds come from the
            // map, not from a constant, so a bigger board frames itself
            // (`map-009`).
            scaling_mode: ScalingMode::AutoMin {
                min_width: half.x * 2.0 + 256.0,
                min_height: half.y * 2.0 + 256.0,
            },
            ..OrthographicProjection::default_2d()
        }),
    ));

    // The ground. One sprite behind everything rather than one entity per tile:
    // the board is 96x96 tiles now, and 9216 entities of static colour is 9216
    // entities of nothing (`audit-023`, `ui-018`).
    commands.spawn((
        Name::new("Ground"),
        Sprite::from_color(GRASS, half * 2.0),
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));

    // The zones the real map declares. Decoration: they tile the board and
    // nothing restricts building to one, which is what the original does too
    // (`map-001`).
    for zone in &map.zones {
        let centre = zone.centre();
        commands.spawn((
            Name::new(zone.name.clone()),
            Sprite::from_color(ZONE_INK, zone.size()),
            Transform::from_xyz(centre.x, centre.y, -0.5),
        ));
    }

    // The lanes, one bar per segment, so a bend is a joint rather than a
    // staircase of tiles.
    let width = map.tile_size * LANE_WIDTH;
    for lane in &map.lanes {
        for segment in lane.points.windows(2) {
            let (a, b) = (segment[0], segment[1]);
            let mid = (a + b) * 0.5;
            let len = a.distance(b);
            commands.spawn((
                Name::new("Lane"),
                Sprite::from_color(LANE_INK, Vec2::new(len, width)),
                Transform::from_xyz(mid.x, mid.y, 0.0)
                    .with_rotation(Quat::from_rotation_z((b - a).to_angle())),
            ));
        }
        // Where the creeps come from: one marker per lane, so a player can see
        // which direction a wave is about to arrive from (`audit-013`).
        let spawn = lane.spawn();
        commands.spawn((
            Name::new(format!("Spawn {}", lane.name)),
            Sprite::from_color(SPAWN_INK, Vec2::splat(map.tile_size * 0.7)),
            Transform::from_xyz(spawn.x, spawn.y, 1.0),
        ));
    }

    // The goal. This is the one thing on the board that matters: a creep that
    // reaches it costs a life (`sim-006`).
    let centre = map.goal.centre;
    commands.spawn((
        Name::new(format!("Goal {}", map.goal.name)),
        Sprite::from_color(GOAL_INK, map.goal.half * 2.0),
        Transform::from_xyz(centre.x, centre.y, 2.0),
    ));
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
