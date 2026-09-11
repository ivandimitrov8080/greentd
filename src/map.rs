//! Static map geometry. Shared by client and server, contains no authority.
//!
//! Green TD's defining trait is that the creep path is FIXED and non-blockable.
//! Creeps spawn and walk a closed loop forever; you never build a maze. So the
//! "map" is just a ring plus a buildability test.

use bevy::prelude::*;

pub const GRID_W: i32 = 32;
pub const GRID_H: i32 = 24;
pub const TILE: f32 = 32.0;

/// Half-extent of the world, used by the camera.
pub const WORLD_W: f32 = GRID_W as f32 * TILE;
pub const WORLD_H: f32 = GRID_H as f32 * TILE;

/// Corner of the ring path, in grid coordinates.
const RING: [IVec2; 4] = [
    IVec2::new(3, 3),
    IVec2::new(GRID_W - 4, 3),
    IVec2::new(GRID_W - 4, GRID_H - 4),
    IVec2::new(3, GRID_H - 4),
];

/// Center of a grid cell in world space. Grid is centered on the origin.
pub fn cell_to_world(cell: IVec2) -> Vec2 {
    Vec2::new(
        (cell.x as f32 - (GRID_W as f32 - 1.0) * 0.5) * TILE,
        (cell.y as f32 - (GRID_H as f32 - 1.0) * 0.5) * TILE,
    )
}

/// Inverse of [`cell_to_world`].
pub fn world_to_cell(p: Vec2) -> IVec2 {
    IVec2::new(
        (p.x / TILE + (GRID_W as f32 - 1.0) * 0.5).round() as i32,
        (p.y / TILE + (GRID_H as f32 - 1.0) * 0.5).round() as i32,
    )
}

pub fn in_bounds(cell: IVec2) -> bool {
    cell.x >= 0 && cell.y >= 0 && cell.x < GRID_W && cell.y < GRID_H
}

/// The ring path as a closed polyline of world-space waypoints.
///
/// Returns waypoints plus the cumulative distance at each, so a creep's
/// position is a simple binary search on "distance travelled".
#[derive(Debug)]
pub struct Path {
    pub points: Vec<Vec2>,
    /// `cumulative[i]` = arc length from the start to `points[i]`.
    pub cumulative: Vec<f32>,
    pub total: f32,
}

impl Path {
    fn build() -> Self {
        let mut points: Vec<Vec2> = RING.iter().map(|c| cell_to_world(*c)).collect();
        // Close the loop.
        points.push(points[0]);

        let mut cumulative = Vec::with_capacity(points.len());
        let mut acc = 0.0;
        cumulative.push(0.0);
        for w in points.windows(2) {
            acc += w[0].distance(w[1]);
            cumulative.push(acc);
        }
        Self {
            points,
            cumulative,
            total: acc,
        }
    }

    /// World position at `dist` along the path, wrapping around the loop.
    pub fn sample(&self, dist: f32) -> Vec2 {
        let d = dist.rem_euclid(self.total);
        // Find the segment containing `d`.
        let idx = match self
            .cumulative
            .binary_search_by(|probe| probe.partial_cmp(&d).unwrap())
        {
            Ok(i) => i,
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

    /// Shortest distance from a world point to the path polyline.
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
}

/// A cell can hold a tower if it is in bounds and not sitting on the path.
///
/// This is the whole of Green TD's placement rule: no maze, no blocking.
pub fn is_buildable(path: &Path, cell: IVec2) -> bool {
    in_bounds(cell) && path.distance_to(cell_to_world(cell)) > TILE * 0.95
}

impl Default for Path {
    fn default() -> Self {
        Self::build()
    }
}
