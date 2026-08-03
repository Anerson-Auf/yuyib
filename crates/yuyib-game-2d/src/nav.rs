//! Grid pathfinding over [`TileCollision2d`] walkability.

use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    error::Error,
    fmt,
};

use crate::{TileCollision2d, TileCollisionError};

/// 4-connected navigation grid derived from solid collision flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TileNavGrid2d {
    grid: [u32; 2],
    walkable: Vec<bool>,
}

/// Failure building or querying a [`TileNavGrid2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileNavError2d {
    /// Grid / solid length rejected by [`TileCollision2d`].
    Collision(TileCollisionError),
    /// Cell coordinate outside the grid.
    OutOfBounds,
}

impl fmt::Display for TileNavError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collision(error) => write!(formatter, "tile nav: {error}"),
            Self::OutOfBounds => formatter.write_str("tile nav: cell out of bounds"),
        }
    }
}

impl Error for TileNavError2d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Collision(error) => Some(error),
            Self::OutOfBounds => None,
        }
    }
}

impl TileNavGrid2d {
    /// Builds a nav grid where `!solid` cells are walkable.
    ///
    /// # Errors
    ///
    /// Returns [`TileNavError2d::Collision`] when `grid`/`solid` disagree.
    pub fn from_collision(
        grid: [u32; 2],
        collision: &TileCollision2d,
    ) -> Result<Self, TileNavError2d> {
        let _ = TileCollision2d::new(grid, collision.solid().to_vec())
            .map_err(TileNavError2d::Collision)?;
        let walkable = collision.solid().iter().map(|solid| !solid).collect();
        Ok(Self { grid, walkable })
    }

    /// Grid width/height in tiles.
    #[must_use]
    pub const fn grid(&self) -> [u32; 2] {
        self.grid
    }

    /// Returns whether `cell` is walkable.
    #[must_use]
    pub fn is_walkable(&self, cell: [u32; 2]) -> bool {
        self.index(cell)
            .and_then(|index| self.walkable.get(index).copied())
            .unwrap_or(false)
    }

    /// A* path on 4-connected walkable cells (inclusive start → goal).
    ///
    /// Returns `None` when start/goal are blocked or unreachable.
    #[must_use]
    pub fn find_path(&self, start: [u32; 2], goal: [u32; 2]) -> Option<Vec<[u32; 2]>> {
        if !self.is_walkable(start) || !self.is_walkable(goal) {
            return None;
        }
        if start == goal {
            return Some(vec![start]);
        }

        let start_i = self.index(start)?;
        let goal_i = self.index(goal)?;
        let mut open = BinaryHeap::new();
        open.push(NavNode {
            cost: 0,
            heuristic: manhattan(start, goal),
            index: start_i,
        });
        let mut came_from: HashMap<usize, usize> = HashMap::new();
        let mut g_score: HashMap<usize, u32> = HashMap::from([(start_i, 0)]);

        while let Some(current) = open.pop() {
            if current.index == goal_i {
                return Some(reconstruct_path(&came_from, current.index, self.grid));
            }
            let current_g = g_score[&current.index];
            let cell = index_to_cell(current.index, self.grid);
            for next in neighbors4(cell, self.grid) {
                if !self.is_walkable(next) {
                    continue;
                }
                let next_i = self.index(next)?;
                let tentative = current_g.saturating_add(1);
                if tentative < *g_score.get(&next_i).unwrap_or(&u32::MAX) {
                    came_from.insert(next_i, current.index);
                    g_score.insert(next_i, tentative);
                    open.push(NavNode {
                        cost: tentative,
                        heuristic: manhattan(next, goal),
                        index: next_i,
                    });
                }
            }
        }
        None
    }

    fn index(&self, cell: [u32; 2]) -> Option<usize> {
        if cell[0] >= self.grid[0] || cell[1] >= self.grid[1] {
            return None;
        }
        usize::try_from(u64::from(cell[1]) * u64::from(self.grid[0]) + u64::from(cell[0])).ok()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct NavNode {
    cost: u32,
    heuristic: u32,
    index: usize,
}

impl Ord for NavNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap via reverse total (cost + heuristic).
        let self_f = self.cost.saturating_add(self.heuristic);
        let other_f = other.cost.saturating_add(other.heuristic);
        other_f
            .cmp(&self_f)
            .then_with(|| other.heuristic.cmp(&self.heuristic))
            .then_with(|| self.index.cmp(&other.index))
    }
}

impl PartialOrd for NavNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn manhattan(a: [u32; 2], b: [u32; 2]) -> u32 {
    a[0].abs_diff(b[0]).saturating_add(a[1].abs_diff(b[1]))
}

fn neighbors4(cell: [u32; 2], grid: [u32; 2]) -> impl Iterator<Item = [u32; 2]> {
    let mut out = Vec::with_capacity(4);
    if cell[0] > 0 {
        out.push([cell[0] - 1, cell[1]]);
    }
    if cell[0] + 1 < grid[0] {
        out.push([cell[0] + 1, cell[1]]);
    }
    if cell[1] > 0 {
        out.push([cell[0], cell[1] - 1]);
    }
    if cell[1] + 1 < grid[1] {
        out.push([cell[0], cell[1] + 1]);
    }
    out.into_iter()
}

fn index_to_cell(index: usize, grid: [u32; 2]) -> [u32; 2] {
    let width = grid[0] as usize;
    [
        u32::try_from(index % width).unwrap_or(0),
        u32::try_from(index / width).unwrap_or(0),
    ]
}

fn reconstruct_path(
    came_from: &HashMap<usize, usize>,
    mut current: usize,
    grid: [u32; 2],
) -> Vec<[u32; 2]> {
    let mut path = vec![index_to_cell(current, grid)];
    while let Some(&prev) = came_from.get(&current) {
        current = prev;
        path.push(index_to_cell(current, grid));
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TileCollision2d;

    #[test]
    fn finds_path_around_wall() {
        // 3x3 with center solid
        let solid = vec![
            false, false, false, //
            false, true, false, //
            false, false, false,
        ];
        let collision = TileCollision2d::new([3, 3], solid).expect("c");
        let nav = TileNavGrid2d::from_collision([3, 3], &collision).expect("nav");
        let path = nav.find_path([0, 1], [2, 1]).expect("path");
        assert_eq!(path.first().copied(), Some([0, 1]));
        assert_eq!(path.last().copied(), Some([2, 1]));
        assert!(!path.iter().any(|cell| *cell == [1, 1]));
    }

    #[test]
    fn blocked_start_returns_none() {
        let collision = TileCollision2d::new([2, 1], vec![true, false]).expect("c");
        let nav = TileNavGrid2d::from_collision([2, 1], &collision).expect("nav");
        assert!(nav.find_path([0, 0], [1, 0]).is_none());
    }
}
