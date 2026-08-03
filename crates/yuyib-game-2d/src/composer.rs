//! Deterministic CPU tile-map composition (fill + stamp + border).

use yuyib_2d::TextureRegion;

use crate::{TileCollision2d, TileCollisionError, TileMap2d, TileMapError};

/// One cell written by [`TileMapComposer2d::stamp`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileStamp2d {
    /// Local atlas index, or empty when `None`.
    pub tile: Option<u32>,
    /// Whether the cell is solid for [`TileCollision2d`].
    pub solid: bool,
}

impl TileStamp2d {
    /// Empty non-solid cell.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            tile: None,
            solid: false,
        }
    }

    /// Filled cell with optional solidity.
    #[must_use]
    pub const fn filled(tile: u32, solid: bool) -> Self {
        Self {
            tile: Some(tile),
            solid,
        }
    }
}

/// Builds a [`TileMap2d`] + [`TileCollision2d`] by filling and stamping a grid.
#[derive(Clone, Debug)]
pub struct TileMapComposer2d {
    grid: [u32; 2],
    tile_size: [f32; 2],
    regions: Vec<TextureRegion>,
    tiles: Vec<Option<u32>>,
    solid: Vec<bool>,
}

impl TileMapComposer2d {
    /// Creates an empty (all `None`, non-solid) composer for `grid`.
    ///
    /// # Errors
    ///
    /// Returns [`TileMapComposerError2d`] for zero grid, invalid tile size, or
    /// empty / multi-texture atlases.
    pub fn new(
        grid: [u32; 2],
        tile_size: [f32; 2],
        regions: Vec<TextureRegion>,
    ) -> Result<Self, TileMapComposerError2d> {
        if grid[0] == 0 || grid[1] == 0 {
            return Err(TileMapComposerError2d::ZeroGrid);
        }
        if !tile_size
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        {
            return Err(TileMapComposerError2d::InvalidTileSize);
        }
        if regions.is_empty() {
            return Err(TileMapComposerError2d::NoRegions);
        }
        let texture = regions[0].texture();
        if regions.iter().any(|region| region.texture() != texture) {
            return Err(TileMapComposerError2d::MultipleTextures);
        }
        let expected = usize::try_from(u64::from(grid[0]) * u64::from(grid[1]))
            .map_err(|_| TileMapComposerError2d::GridTooLarge)?;
        Ok(Self {
            grid,
            tile_size,
            regions,
            tiles: vec![None; expected],
            solid: vec![false; expected],
        })
    }

    /// Fills every cell with the same atlas index (or clears when `None`).
    #[must_use]
    pub fn fill(mut self, tile: Option<u32>) -> Self {
        for cell in &mut self.tiles {
            *cell = tile;
        }
        self
    }

    /// Sets solidity for every cell.
    #[must_use]
    pub fn fill_solid(mut self, solid: bool) -> Self {
        for flag in &mut self.solid {
            *flag = solid;
        }
        self
    }

    /// Writes a solid border using `tile` (outermost ring of the grid).
    ///
    /// # Errors
    ///
    /// Returns [`TileMapComposerError2d::InvalidTileIndex`] when `tile` is
    /// outside the atlas.
    pub fn border(mut self, tile: u32, solid: bool) -> Result<Self, TileMapComposerError2d> {
        self.ensure_tile_index(tile)?;
        let width = self.grid[0];
        let height = self.grid[1];
        for column in 0..width {
            self.write_cell(column, 0, Some(tile), solid)?;
            self.write_cell(column, height - 1, Some(tile), solid)?;
        }
        for row in 1..height.saturating_sub(1) {
            self.write_cell(0, row, Some(tile), solid)?;
            self.write_cell(width - 1, row, Some(tile), solid)?;
        }
        Ok(self)
    }

    /// Stamps a rectangle `[x, y, w, h]` (tile units) via `stamp`.
    ///
    /// Callback receives absolute column/row. Returning `None` leaves the cell
    /// unchanged. Stamps are applied in row-major order inside the rect.
    ///
    /// # Errors
    ///
    /// Returns OOB or invalid atlas index errors.
    pub fn stamp<F>(
        mut self,
        rect: [u32; 4],
        mut stamp: F,
    ) -> Result<Self, TileMapComposerError2d>
    where
        F: FnMut(u32, u32) -> Option<TileStamp2d>,
    {
        let [x, y, width, height] = rect;
        let end_x = x
            .checked_add(width)
            .ok_or(TileMapComposerError2d::RectOutOfBounds { rect })?;
        let end_y = y
            .checked_add(height)
            .ok_or(TileMapComposerError2d::RectOutOfBounds { rect })?;
        if end_x > self.grid[0] || end_y > self.grid[1] {
            return Err(TileMapComposerError2d::RectOutOfBounds { rect });
        }
        for row in y..end_y {
            for column in x..end_x {
                if let Some(cell) = stamp(column, row) {
                    if let Some(tile) = cell.tile {
                        self.ensure_tile_index(tile)?;
                    }
                    self.write_cell(column, row, cell.tile, cell.solid)?;
                }
            }
        }
        Ok(self)
    }

    /// Builds validated map + collision components.
    ///
    /// # Errors
    ///
    /// Forwards [`TileMap2d`] / [`TileCollision2d`] construction failures.
    pub fn build(self) -> Result<(TileMap2d, TileCollision2d), TileMapComposerError2d> {
        let map = TileMap2d::new(self.grid, self.tile_size, self.regions, self.tiles)
            .map_err(TileMapComposerError2d::TileMap)?;
        let collision = TileCollision2d::new(self.grid, self.solid)
            .map_err(TileMapComposerError2d::Collision)?;
        Ok((map, collision))
    }

    fn ensure_tile_index(&self, tile: u32) -> Result<(), TileMapComposerError2d> {
        let index = usize::try_from(tile).map_err(|_| TileMapComposerError2d::InvalidTileIndex {
            tile,
            regions: self.regions.len(),
        })?;
        if index >= self.regions.len() {
            return Err(TileMapComposerError2d::InvalidTileIndex {
                tile,
                regions: self.regions.len(),
            });
        }
        Ok(())
    }

    fn write_cell(
        &mut self,
        column: u32,
        row: u32,
        tile: Option<u32>,
        solid: bool,
    ) -> Result<(), TileMapComposerError2d> {
        let index = usize::try_from(u64::from(row) * u64::from(self.grid[0]) + u64::from(column))
            .map_err(|_| TileMapComposerError2d::GridTooLarge)?;
        let Some(slot) = self.tiles.get_mut(index) else {
            return Err(TileMapComposerError2d::RectOutOfBounds {
                rect: [column, row, 1, 1],
            });
        };
        *slot = tile;
        self.solid[index] = solid;
        Ok(())
    }
}

/// Failure while composing a tile map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileMapComposerError2d {
    /// Grid has a zero dimension.
    ZeroGrid,
    /// Tile size was non-finite or not positive.
    InvalidTileSize,
    /// Grid product cannot fit indexing.
    GridTooLarge,
    /// No atlas regions supplied.
    NoRegions,
    /// Atlas regions use different texture handles.
    MultipleTextures,
    /// Stamp rectangle leaves the grid.
    RectOutOfBounds {
        /// Requested `[x, y, w, h]`.
        rect: [u32; 4],
    },
    /// Atlas index outside `regions`.
    InvalidTileIndex {
        /// Requested index.
        tile: u32,
        /// Available region count.
        regions: usize,
    },
    /// [`TileMap2d`] rejected the composed data.
    TileMap(TileMapError),
    /// [`TileCollision2d`] rejected the solid flags.
    Collision(TileCollisionError),
}

impl std::fmt::Display for TileMapComposerError2d {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroGrid => formatter.write_str("tile map composer grid has a zero dimension"),
            Self::InvalidTileSize => formatter.write_str("tile map composer tile size is invalid"),
            Self::GridTooLarge => formatter.write_str("tile map composer grid is too large"),
            Self::NoRegions => formatter.write_str("tile map composer requires atlas regions"),
            Self::MultipleTextures => {
                formatter.write_str("tile map composer requires a single atlas texture")
            }
            Self::RectOutOfBounds { rect } => write!(
                formatter,
                "tile map composer stamp {:?} is outside the grid",
                rect
            ),
            Self::InvalidTileIndex { tile, regions } => write!(
                formatter,
                "tile map composer tile {tile} is outside 0..{regions}"
            ),
            Self::TileMap(error) => write!(formatter, "tile map composer map: {error}"),
            Self::Collision(error) => write!(formatter, "tile map composer collision: {error}"),
        }
    }
}

impl std::error::Error for TileMapComposerError2d {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TileMap(error) => Some(error),
            Self::Collision(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TileMapComposer2d, TileStamp2d};
    use yuyib_2d::{PixelPoint, Texture, TextureRegion, TextureSize};
    use yuyib_assets::Assets;

    fn atlas() -> Vec<TextureRegion> {
        let mut textures = Assets::new();
        let size = TextureSize::new(16, 16).expect("size");
        let texture = textures.insert(Texture::new(size));
        let cell = TextureSize::new(8, 8).expect("cell");
        vec![
            TextureRegion::new(texture, size, PixelPoint { x: 0, y: 0 }, cell).expect("r0"),
            TextureRegion::new(texture, size, PixelPoint { x: 8, y: 0 }, cell).expect("r1"),
        ]
    }

    #[test]
    fn fill_border_and_stamp_build_room() {
        let (map, collision) = TileMapComposer2d::new([5, 4], [16.0, 16.0], atlas())
            .expect("composer")
            .fill(Some(0))
            .fill_solid(false)
            .border(1, true)
            .expect("border")
            .stamp([2, 1, 1, 1], |_, _| Some(TileStamp2d::empty()))
            .expect("stamp")
            .build()
            .expect("build");
        assert_eq!(map.grid(), [5, 4]);
        assert!(collision.solid()[0]);
        assert!(!collision.solid()[1 * 5 + 2]);
    }
}
