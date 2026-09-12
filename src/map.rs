//! The board: geometry both peers agree on, and the path creeps walk.
//!
//! A map is *data* (`assets/maps/*.ron`), loaded and validated before any
//! plugin exists, exactly like the balance tables (`found-004`). Before this,
//! the geometry was a `const RING` in this file: a closed loop that creeps
//! walked forever, which is Lineage A of Green TD. The board that ships now is
//! the real one, read out of Green TD v36.0 -- ten lanes from ten spawn points,
//! merging into one funnel, ending at a single goal. A creep that reaches the
//! goal *leaks*: it leaves the match and costs a life (`sim-006`), and the match
//! is lost when the lives run out (`sim-005`). The point of the game is
//! therefore the point it always was: nothing may reach the goal.
//!
//! What the map owns:
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | [`Lane`] | One spawn point and the polyline from it to the goal |
//! | [`Goal`] | Where a lane ends, and where a leak happens |
//! | [`Zone`] | A named rectangle of the board. Decoration: see the map file |
//!
//! The grid is the map's: 96x96 tiles of 128 world units, so the playable area
//! is about -6144..6144 and the origin is the middle of the board.
//!
//! Nothing here is server-only: the client loads the same file, which is why
//! the map is hashed into the protocol version (`net-001`) and a peer with a
//! different board is refused rather than allowed to draw a lie.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::data::balance::{above, fnv1a_64};

/// Where maps are read from when nothing says otherwise.
pub const DEFAULT_MAP_DIR: &str = "assets/maps";

/// Environment variable naming the map directory. The same escape hatch the
/// balance loader has, and for the same reason: a binary that has been copied
/// out of the tree still has to be able to find (or be pointed at) its assets.
pub const MAP_DIR_ENV: &str = "GREENTD_MAP_DIR";

/// The map directory to use when nothing else is configured.
///
/// Prefers `GREENTD_MAP_DIR`, then the crate's own `assets/maps` (which is what
/// `cargo run` and `cargo test` see, wherever they are invoked from), then a
/// CWD-relative path so a copied-out binary can find a sibling `assets`.
pub fn default_map_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MAP_DIR_ENV) {
        return PathBuf::from(dir);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_MAP_DIR);
    if manifest.is_dir() {
        return manifest;
    }
    PathBuf::from(DEFAULT_MAP_DIR)
}

/// The board this build ships with.
pub const DEFAULT_MAP_ID: &str = "green_td_v36";

/// The map field a startup refusal names when the board and the towers disagree
/// about how close a tower may stand to a lane. It is the map's field, so it is
/// named here, next to where it is read (`config::check_map_against_balance`).
pub const PATH_CLEARANCE_KEY: &str = "path_clearance";

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// One map asset. Field names match `assets/maps/*.ron`; the prose that
/// explains the geometry lives in the file itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapDef {
    pub id: String,
    pub name: String,
    pub tiles_x: i32,
    pub tiles_y: i32,
    pub tile_size: f32,
    /// A cell whose centre is within this of any lane cannot hold a tower.
    pub path_clearance: f32,
    pub goal: GoalDef,
    pub lanes: Vec<LaneDef>,
    #[serde(default)]
    pub zones: Vec<ZoneDef>,
}

/// The goal a lane ends at: where a creep leaves the match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalDef {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub half_w: f32,
    pub half_h: f32,
}

/// One lane: a name, the point creeps enter, and the waypoints they follow.
///
/// `waypoints[0]` is the spawn point and the last one is the goal, so a lane is
/// a complete description of where a creep starts and how it gets there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneDef {
    pub name: String,
    pub spawn: (f32, f32),
    pub waypoints: Vec<(f32, f32)>,
}

/// A named rectangle of the board. See the map file for why these exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneDef {
    pub name: String,
    pub min: (f32, f32),
    pub max: (f32, f32),
}

// ---------------------------------------------------------------------------
// What the loader produces
// ---------------------------------------------------------------------------

/// Where a creep that finishes its lane leaves the match.
#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    pub name: String,
    pub centre: Vec2,
    pub half: Vec2,
}

/// A named rectangle of ground.
#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub name: String,
    pub min: Vec2,
    pub max: Vec2,
}

impl Zone {
    pub fn centre(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }
}

/// A lane as the sim uses it: waypoints plus the prefix lengths that turn
/// "distance travelled" into a position.
///
/// There is no wrapping here, unlike the ring this replaced: a lane *ends*. A
/// creep whose distance passes [`Lane::total`] has arrived at the goal, which is
/// what `Sim::step` looks for in order to make it leak.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub name: String,
    pub points: Vec<Vec2>,
    cumulative: Vec<f32>,
    pub total: f32,
}

impl Lane {
    fn new(name: String, points: Vec<Vec2>) -> Self {
        let mut cumulative = Vec::with_capacity(points.len());
        let mut acc = 0.0;
        cumulative.push(0.0);
        for w in points.windows(2) {
            acc += w[0].distance(w[1]);
            cumulative.push(acc);
        }
        Self {
            name,
            points,
            cumulative,
            total: acc,
        }
    }

    /// Where a creep is after travelling `dist` along this lane.
    ///
    /// A distance past the end reads as the goal, and a distance that is not a
    /// finite number reads as the spawn point rather than a `NaN`: a `NaN` here
    /// would travel into a `Transform` and be far harder to find than a creep
    /// standing still (`found-006`).
    pub fn sample(&self, dist: f32) -> Vec2 {
        if !dist.is_finite() {
            return self.points.first().copied().unwrap_or(Vec2::ZERO);
        }
        let d = dist.clamp(0.0, self.total);
        // `f32::total_cmp` rather than `partial_cmp` so the comparison cannot
        // fail; `Err(0)` is unreachable for a finite `d` and exists so that
        // `i - 1` cannot underflow.
        let idx = match self.cumulative.binary_search_by(|p| p.total_cmp(&d)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let idx = idx.min(self.points.len() - 2);
        let seg_start = self.cumulative[idx];
        let seg_len = self.cumulative[idx + 1] - seg_start;
        let t = if seg_len > 0.0 {
            (d - seg_start) / seg_len
        } else {
            0.0
        };
        self.points[idx].lerp(self.points[idx + 1], t)
    }

    /// Shortest distance from a world point to this lane.
    pub fn distance_to(&self, p: Vec2) -> f32 {
        let mut best = f32::MAX;
        for w in self.points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let len_sq = ab.length_squared();
            let t = if len_sq > 0.0 {
                ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            best = best.min(p.distance(a + ab * t));
        }
        best
    }

    /// Where creeps of this lane enter the board.
    pub fn spawn(&self) -> Vec2 {
        self.points[0]
    }
}

/// A loaded, validated board.
#[derive(Debug)]
pub struct MapData {
    pub id: String,
    pub name: String,
    pub tiles_x: i32,
    pub tiles_y: i32,
    pub tile_size: f32,
    pub path_clearance: f32,
    pub goal: Goal,
    pub lanes: Vec<Lane>,
    pub zones: Vec<Zone>,
}

/// The loaded board as a Bevy resource, shared by the sim and the UI.
#[derive(Resource, Debug, Clone)]
pub struct Map(pub Arc<MapData>);

impl Map {
    pub fn load(dir: &Path, id: &str) -> Result<Self, MapError> {
        Ok(Self(Arc::new(MapData::load(dir, id)?)))
    }

    /// Load the map this build ships with, from [`default_map_dir`].
    pub fn shipped() -> Result<Self, MapError> {
        Self::load(&default_map_dir(), DEFAULT_MAP_ID)
    }
}

/// Why a map would not load. Fatal, and printed by `main` as a startup error.
#[derive(Debug)]
pub enum MapError {
    /// The file could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is not valid RON.
    ///
    /// The RON error is folded into a message rather than stored, because a
    /// `SpannedError` is large enough to make this error the biggest thing in
    /// every `Result` that returns it, and the position is all anybody acts on.
    /// `data::balance::BalanceError` does the same for the same reason.
    Parse { path: PathBuf, message: String },
    /// The file parsed but describes a board that cannot be played.
    Invalid { id: String, why: String },
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Read { path, source } => write!(f, "map: {}: {source}", path.display()),
            MapError::Parse { path, message } => write!(f, "map: {}: {message}", path.display()),
            MapError::Invalid { id, why } => write!(f, "map: {id}: {why}"),
        }
    }
}

impl std::error::Error for MapError {}

impl MapData {
    /// Read `<dir>/<id>.ron` and check it describes a playable board.
    pub fn load(dir: &Path, id: &str) -> Result<Self, MapError> {
        let path = dir.join(format!("{id}.ron"));
        let text = std::fs::read_to_string(&path).map_err(|source| MapError::Read {
            path: path.clone(),
            source,
        })?;
        let def: MapDef = ron::from_str(&text).map_err(|source| MapError::Parse {
            path: path.clone(),
            message: source.to_string(),
        })?;
        Self::from_def(def).map_err(|why| MapError::Invalid {
            id: id.to_string(),
            why,
        })
    }

    /// The shipped board, for a test or a tool with no directory to read.
    pub fn shipped() -> Result<Self, MapError> {
        Self::load(&default_map_dir(), DEFAULT_MAP_ID)
    }

    /// Validate and lower a parsed map.
    ///
    /// Every refusal names the field, because a hand-written map is a thing a
    /// person debugs (`found-004`'s rule, applied to geometry).
    pub fn from_def(def: MapDef) -> Result<Self, String> {
        if def.tiles_x < 1 || def.tiles_y < 1 {
            return Err(format!(
                "tiles_x/tiles_y must be at least 1 (got {}x{})",
                def.tiles_x, def.tiles_y
            ));
        }
        if !above(def.tile_size, 0.0) {
            return Err(format!(
                "tile_size must be positive (got {})",
                def.tile_size
            ));
        }
        if !above(def.path_clearance, 0.0) {
            return Err(format!(
                "path_clearance must be positive (got {})",
                def.path_clearance
            ));
        }
        if def.lanes.is_empty() {
            return Err("lanes: a map needs at least one lane".to_string());
        }

        let goal = Goal {
            name: def.goal.name.clone(),
            centre: Vec2::new(def.goal.x, def.goal.y),
            half: Vec2::new(def.goal.half_w, def.goal.half_h),
        };
        if !above(goal.half.x, 0.0) || !above(goal.half.y, 0.0) {
            return Err(format!(
                "goal: half_w and half_h must be positive (got {}x{})",
                goal.half.x, goal.half.y
            ));
        }

        let half = Vec2::new(
            def.tiles_x as f32 * def.tile_size * 0.5,
            def.tiles_y as f32 * def.tile_size * 0.5,
        );

        let mut lanes = Vec::with_capacity(def.lanes.len());
        for (i, lane) in def.lanes.iter().enumerate() {
            if lane.waypoints.len() < 2 {
                return Err(format!(
                    "lanes[{i}].waypoints: a lane needs a spawn and a goal (got {})",
                    lane.waypoints.len()
                ));
            }
            let points: Vec<Vec2> = lane
                .waypoints
                .iter()
                .map(|(x, y)| Vec2::new(*x, *y))
                .collect();
            for (j, p) in points.iter().enumerate() {
                if !p.is_finite() {
                    return Err(format!("lanes[{i}].waypoints[{j}]: not a finite point"));
                }
                if p.x.abs() > half.x || p.y.abs() > half.y {
                    return Err(format!(
                        "lanes[{i}].waypoints[{j}]: ({}, {}) is off a {}x{} board",
                        p.x, p.y, def.tiles_x, def.tiles_y
                    ));
                }
            }
            let spawn = Vec2::new(lane.spawn.0, lane.spawn.1);
            if spawn.distance(points[0]) > def.tile_size {
                return Err(format!(
                    "lanes[{i}].spawn: ({}, {}) is not the first waypoint ({}, {})",
                    spawn.x, spawn.y, points[0].x, points[0].y
                ));
            }
            let last = *points.last().expect("checked non-empty");
            if last.distance(goal.centre) > def.tile_size {
                return Err(format!(
                    "lanes[{i}]: ends at ({}, {}) rather than at the goal {}",
                    last.x, last.y, goal.name
                ));
            }
            let lane = Lane::new(lane.name.clone(), points);
            if !above(lane.total, def.tile_size) {
                return Err(format!(
                    "lanes[{i}]: is {} units long, which is shorter than a tile",
                    lane.total
                ));
            }
            lanes.push(lane);
        }

        let zones = def
            .zones
            .iter()
            .map(|z| Zone {
                name: z.name.clone(),
                min: Vec2::new(z.min.0, z.min.1),
                max: Vec2::new(z.max.0, z.max.1),
            })
            .collect();

        Ok(Self {
            id: def.id,
            name: def.name,
            tiles_x: def.tiles_x,
            tiles_y: def.tiles_y,
            tile_size: def.tile_size,
            path_clearance: def.path_clearance,
            goal,
            lanes,
            zones,
        })
    }

    // -----------------------------------------------------------------------
    // Grid
    // -----------------------------------------------------------------------

    pub fn half_extents(&self) -> Vec2 {
        Vec2::new(
            self.tiles_x as f32 * self.tile_size * 0.5,
            self.tiles_y as f32 * self.tile_size * 0.5,
        )
    }

    pub fn in_bounds(&self, cell: IVec2) -> bool {
        cell.x >= 0 && cell.y >= 0 && cell.x < self.tiles_x && cell.y < self.tiles_y
    }

    /// Centre of a grid cell, in world space. The grid is centred on the origin.
    pub fn cell_to_world(&self, cell: IVec2) -> Vec2 {
        Vec2::new(
            (cell.x as f32 - (self.tiles_x as f32 - 1.0) * 0.5) * self.tile_size,
            (cell.y as f32 - (self.tiles_y as f32 - 1.0) * 0.5) * self.tile_size,
        )
    }

    /// Inverse of [`MapData::cell_to_world`].
    pub fn world_to_cell(&self, p: Vec2) -> IVec2 {
        IVec2::new(
            (p.x / self.tile_size + (self.tiles_x as f32 - 1.0) * 0.5).round() as i32,
            (p.y / self.tile_size + (self.tiles_y as f32 - 1.0) * 0.5).round() as i32,
        )
    }

    // -----------------------------------------------------------------------
    // Rules
    // -----------------------------------------------------------------------

    /// How close this point is to the nearest lane, in world units.
    pub fn path_distance(&self, p: Vec2) -> f32 {
        self.lanes
            .iter()
            .map(|lane| lane.distance_to(p))
            .fold(f32::MAX, f32::min)
    }

    /// Green TD's whole placement rule: the board is one open field, and the
    /// only thing you may not build on is the path.
    ///
    /// It is deliberately *not* per-zone. The original declares its nine zone
    /// rectangles and never reads them, so a rule restricting each player to
    /// their own zone would be this project inventing a mechanic and calling it
    /// fidelity; the map file says so where a reader will find it.
    pub fn is_buildable(&self, cell: IVec2) -> bool {
        self.in_bounds(cell) && self.path_distance(self.cell_to_world(cell)) > self.path_clearance
    }

    /// A stable hash of the geometry, for the protocol version (`net-001`).
    ///
    /// Two peers that agree on this hash are walking the same lanes to the same
    /// goal. Floats are canonicalised so that `-0.0` and `0.0` cannot disagree.
    pub fn hash(&self) -> u64 {
        let mut canonical = format!(
            "{}|{}x{}|{}|",
            self.id, self.tiles_x, self.tiles_y, self.tile_size
        );
        canonical.push_str(&format!(
            "{}|{:?}|{:?};",
            self.goal.name,
            canon(self.goal.centre),
            canon(self.goal.half)
        ));
        for lane in &self.lanes {
            canonical.push_str(&lane.name);
            canonical.push(':');
            for p in &lane.points {
                canonical.push_str(&format!("{:?}", canon(*p)));
            }
            canonical.push(';');
        }
        fnv1a_64(canonical.as_bytes())
    }
}

/// Canonicalise a point for hashing: `-0.0` becomes `0.0`, and a non-finite
/// value -- which validation refuses to let through -- hashes as zero rather
/// than as a `NaN`, which never equals itself.
fn canon(p: Vec2) -> Vec2 {
    let f = |v: f32| if v == 0.0 { 0.0 } else { v };
    Vec2::new(f(p.x), f(p.y))
}

impl Default for Map {
    /// The shipped board, for tests and tools. A real run loads a map and
    /// reports a [`MapError`] rather than silently falling back to this.
    fn default() -> Self {
        Map(Arc::new(
            MapData::shipped().expect("assets/maps must contain the shipped board"),
        ))
    }
}
