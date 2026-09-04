//! Validated CPU-side 3D model data for Yuyib.
//!
//! This crate is deliberately renderer-neutral. It is the boundary between an
//! importer (eventually glTF first) and a future 3D renderer: meshes are
//! indexed triangle lists, materials are PBR-oriented metadata, and texture
//! references remain separate from decoded image/GPU resources.
//!
//! The initial API includes a high-level [`Model::cube`] for prototypes and
//! low-level [`MeshPrimitive`] construction for importers. Source 1/2 maps,
//! Hammer assets, skeletal animation, glTF file import, PBR rendering and LOD
//! streaming are **not** implemented by this crate.

#![forbid(unsafe_code)]

mod material_policy;
mod texture_usage;

pub use material_policy::{
    MaterialFactorPatch, MeshPrimitiveRef, ModelMaterialPolicy, ModelMaterialPolicyError,
    ModelMaterialPolicyReport, ModelMaterialUsage, ModelMaterialUsageEntry,
};
pub use texture_usage::{MissingUvBinding, ModelTextureUsage, ModelTextureUsageEntry};

use std::{error::Error, fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use yuyib_assets::AssetId;

/// A two-dimensional floating-point vector.
pub type Vec2 = [f32; 2];

/// A three-dimensional floating-point vector.
pub type Vec3 = [f32; 3];

/// A four-dimensional floating-point vector.
pub type Vec4 = [f32; 4];

/// Maximum glTF-compatible texture-coordinate sets retained per primitive.
pub const MAX_TEX_COORD_SETS: usize = 8;
/// Largest glTF-compatible texture-coordinate set index retained per primitive.
pub const MAX_TEX_COORD_SET: u8 = 7;

/// A typed handle for a [`Model`] in [`yuyib_assets::Assets`].
pub type ModelHandle = AssetId<Model>;

/// A complete renderable model made of meshes, PBR material metadata and
/// texture descriptors.
///
/// `Model` contains no GPU objects and does not read files. This lets asset
/// cooking, scene loading and render residency evolve independently.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    meshes: Vec<Mesh>,
    materials: Vec<Material>,
    textures: Vec<ModelTexture>,
}

impl Model {
    /// Creates a model after validating material and texture references.
    ///
    /// # Errors
    ///
    /// Returns [`ModelValidationError`] when the model is empty or when a mesh
    /// material/texture reference is outside the supplied collections.
    pub fn new(
        meshes: Vec<Mesh>,
        materials: Vec<Material>,
        textures: Vec<ModelTexture>,
    ) -> Result<Self, ModelValidationError> {
        let model = Self {
            meshes,
            materials,
            textures,
        };
        model.validate()?;
        Ok(model)
    }

    /// Creates a unit cube centred on the origin for prototypes and tests.
    ///
    /// The cube has six faces, per-face normals and UVs, so it intentionally
    /// duplicates vertices at hard edges. `half_extent` must be finite and
    /// strictly positive.
    ///
    /// # Errors
    ///
    /// Returns [`PrimitiveError::InvalidHalfExtent`] for a non-finite or
    /// non-positive `half_extent`.
    pub fn cube(half_extent: f32) -> Result<Self, PrimitiveError> {
        let primitive = MeshPrimitive::cube(half_extent)?.with_material(MaterialIndex::new(0));
        let mesh = Mesh::new(Some("cube".to_owned()), vec![primitive])
            .map_err(|_| PrimitiveError::InvalidStaticGeometry)?;
        // Explicit default material: renderers must not invent a silent white
        // fallback for unbound prototype geometry.
        Self::new(
            vec![mesh],
            vec![
                Material::new()
                    .with_name("yuyib.default_cube")
                    .with_base_color_factor([0.82, 0.84, 0.88, 1.0])
                    .with_metallic_roughness(0.05, 0.55),
            ],
            Vec::new(),
        )
        .map_err(|_| PrimitiveError::InvalidStaticGeometry)
    }

    /// Returns the model meshes in source order.
    #[must_use]
    pub fn meshes(&self) -> &[Mesh] {
        &self.meshes
    }

    /// Returns PBR material metadata in source order.
    #[must_use]
    pub fn materials(&self) -> &[Material] {
        &self.materials
    }

    /// Returns texture descriptors in source order.
    #[must_use]
    pub fn textures(&self) -> &[ModelTexture] {
        &self.textures
    }

    /// Appends a validated material slot and returns its stable index.
    ///
    /// This is the safe low-level path for asset-specific post-import repair:
    /// geometry and texture storage stay untouched, and an invalid texture
    /// binding cannot partially mutate the model.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMaterialEditError::MissingTexture`] when `material`
    /// references a texture absent from this model.
    pub fn add_material(
        &mut self,
        material: Material,
    ) -> Result<MaterialIndex, ModelMaterialEditError> {
        validate_edited_material(&material, self.textures.len())?;
        let index = MaterialIndex::new(self.materials.len());
        self.materials.push(material);
        Ok(index)
    }

    /// Replaces one material slot after validating all texture bindings.
    ///
    /// # Errors
    ///
    /// Returns a typed missing-material or missing-texture error and leaves the
    /// model unchanged.
    pub fn replace_material(
        &mut self,
        index: MaterialIndex,
        replacement: Material,
    ) -> Result<Material, ModelMaterialEditError> {
        if index.get() >= self.materials.len() {
            return Err(ModelMaterialEditError::MissingMaterial { material: index });
        }
        validate_edited_material(&replacement, self.textures.len())?;
        Ok(std::mem::replace(
            &mut self.materials[index.get()],
            replacement,
        ))
    }

    /// Replaces one texture descriptor after import (for example re-encoded
    /// diffuse bytes). Material bindings are left unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMaterialEditError::MissingTexture`] when `index` is out
    /// of range and leaves the model unchanged.
    pub fn replace_texture(
        &mut self,
        index: ModelTextureIndex,
        replacement: ModelTexture,
    ) -> Result<ModelTexture, ModelMaterialEditError> {
        if index.get() >= self.textures.len() {
            return Err(ModelMaterialEditError::MissingTexture { texture: index });
        }
        Ok(std::mem::replace(
            &mut self.textures[index.get()],
            replacement,
        ))
    }

    /// Rebinds one physical mesh primitive to a validated material slot.
    ///
    /// This changes metadata only; vertex/index buffers are neither cloned nor
    /// rewritten. It is therefore suitable for large imported scenes.
    ///
    /// # Errors
    ///
    /// Returns a typed missing mesh, primitive or material error without
    /// mutating the model.
    pub fn set_primitive_material(
        &mut self,
        mesh: usize,
        primitive: usize,
        material: MaterialIndex,
    ) -> Result<Option<MaterialIndex>, ModelMaterialEditError> {
        if material.get() >= self.materials.len() {
            return Err(ModelMaterialEditError::MissingMaterial { material });
        }
        let mesh_value = self
            .meshes
            .get_mut(mesh)
            .ok_or(ModelMaterialEditError::MissingMesh { mesh })?;
        let primitive_value = mesh_value
            .primitives
            .get_mut(primitive)
            .ok_or(ModelMaterialEditError::MissingPrimitive { mesh, primitive })?;
        Ok(primitive_value.material.replace(material))
    }

    /// Validates cross-resource references after a caller has transformed model data.
    ///
    /// This is mostly useful to importers; normal public constructors preserve
    /// the invariant themselves.
    ///
    /// # Errors
    ///
    /// Returns the first invalid material or texture reference found.
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        if self.meshes.is_empty() {
            return Err(ModelValidationError::EmptyModel);
        }
        for (mesh_index, mesh) in self.meshes.iter().enumerate() {
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                if let Some(material) = primitive.material()
                    && material.get() >= self.materials.len()
                {
                    return Err(ModelValidationError::MissingMaterial {
                        mesh_index,
                        primitive_index,
                        material,
                    });
                }
            }
        }
        for (material_index, material) in self.materials.iter().enumerate() {
            for binding in material.texture_bindings() {
                if binding.texture().get() >= self.textures.len() {
                    return Err(ModelValidationError::MissingTexture {
                        material_index,
                        texture: binding.texture(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_edited_material(
    material: &Material,
    texture_count: usize,
) -> Result<(), ModelMaterialEditError> {
    if let Some(texture) = material
        .texture_bindings()
        .map(TextureBinding::texture)
        .find(|texture| texture.get() >= texture_count)
    {
        Err(ModelMaterialEditError::MissingTexture { texture })
    } else {
        Ok(())
    }
}

/// An indexed triangle mesh, potentially containing multiple draw primitives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    name: Option<String>,
    primitives: Vec<MeshPrimitive>,
}

impl Mesh {
    /// Creates a non-empty mesh.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::Empty`] when no primitives are supplied.
    pub fn new(name: Option<String>, primitives: Vec<MeshPrimitive>) -> Result<Self, MeshError> {
        if primitives.is_empty() {
            return Err(MeshError::Empty);
        }
        Ok(Self { name, primitives })
    }

    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns mesh primitives in source order.
    #[must_use]
    pub fn primitives(&self) -> &[MeshPrimitive] {
        &self.primitives
    }
}

/// One indexed triangle-list draw submission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshPrimitive {
    positions: Vec<Vec3>,
    indices: Vec<u32>,
    normals: Option<Vec<Vec3>>,
    tangents: Option<Vec<Vec4>>,
    tex_coords: [Option<Vec<Vec2>>; MAX_TEX_COORD_SETS],
    #[serde(default)]
    texture_blend_weights: Option<Vec<f32>>,
    material: Option<MaterialIndex>,
}

impl MeshPrimitive {
    /// Creates an indexed triangle-list primitive.
    ///
    /// Positions and indices are required. Normals, tangents and UVs are
    /// optional; a renderer must select a compatible material/pipeline rather
    /// than silently inventing normal-map inputs.
    ///
    /// # Errors
    ///
    /// Returns [`MeshValidationError`] for empty data, a non-triangle index
    /// count, or an index outside `positions`.
    pub fn new(positions: Vec<Vec3>, indices: Vec<u32>) -> Result<Self, MeshValidationError> {
        let primitive = Self {
            positions,
            indices,
            normals: None,
            tangents: None,
            tex_coords: std::array::from_fn(|_| None),
            texture_blend_weights: None,
            material: None,
        };
        primitive.validate()?;
        Ok(primitive)
    }

    /// Attaches one normal per position.
    ///
    /// # Errors
    ///
    /// Returns [`MeshValidationError::AttributeCountMismatch`] when the count
    /// differs from the position count.
    pub fn with_normals(mut self, normals: Vec<Vec3>) -> Result<Self, MeshValidationError> {
        self.normals = Some(normals);
        self.validate()?;
        Ok(self)
    }

    /// Attaches one tangent (XYZ plus handedness W) per position.
    ///
    /// # Errors
    ///
    /// Returns [`MeshValidationError::AttributeCountMismatch`] when the count
    /// differs from the position count.
    pub fn with_tangents(mut self, tangents: Vec<Vec4>) -> Result<Self, MeshValidationError> {
        self.tangents = Some(tangents);
        self.validate()?;
        Ok(self)
    }

    /// Attaches one primary UV coordinate per position.
    ///
    /// # Errors
    ///
    /// Returns [`MeshValidationError::AttributeCountMismatch`] when the count
    /// differs from the position count.
    pub fn with_tex_coords_0(self, tex_coords_0: Vec<Vec2>) -> Result<Self, MeshValidationError> {
        self.with_tex_coords(0, tex_coords_0)
    }

    /// Attaches one UV coordinate from `set` per position.
    ///
    /// Sets `0..=7` match the glTF `TEXCOORD_n` contract. Keeping the set
    /// index in renderer-neutral model data lets each material texture select
    /// its authored UV stream without remapping or silently falling back.
    ///
    /// # Errors
    ///
    /// Returns [`MeshValidationError::TextureCoordinateSetOutOfRange`] for a
    /// set above seven, or [`MeshValidationError::AttributeCountMismatch`]
    /// when the stream length differs from the position count.
    pub fn with_tex_coords(
        mut self,
        set: u8,
        tex_coords: Vec<Vec2>,
    ) -> Result<Self, MeshValidationError> {
        let index = usize::from(set);
        if index >= MAX_TEX_COORD_SETS {
            return Err(MeshValidationError::TextureCoordinateSetOutOfRange {
                actual: set,
                maximum: MAX_TEX_COORD_SET,
            });
        }
        self.tex_coords[index] = Some(tex_coords);
        self.validate()?;
        Ok(self)
    }

    /// Attaches one scalar texture-blend weight per position.
    ///
    /// Zero selects a material's first base-colour texture and one selects its
    /// second texture. Intermediate values are linearly interpolated by a
    /// compatible renderer. Keeping this stream explicit avoids overloading
    /// vertex colour alpha with Source-style terrain/displacement semantics.
    ///
    /// # Errors
    ///
    /// Returns [`MeshValidationError::AttributeCountMismatch`] when the count
    /// differs from the position count.
    pub fn with_texture_blend_weights(
        mut self,
        texture_blend_weights: Vec<f32>,
    ) -> Result<Self, MeshValidationError> {
        self.texture_blend_weights = Some(texture_blend_weights);
        self.validate()?;
        Ok(self)
    }

    /// Assigns the material slot used by this primitive.
    #[must_use]
    pub const fn with_material(mut self, material: MaterialIndex) -> Self {
        self.material = Some(material);
        self
    }

    /// Returns the required position stream.
    #[must_use]
    pub fn positions(&self) -> &[Vec3] {
        &self.positions
    }

    /// Returns indexed triangles in groups of three.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Returns the optional normal stream.
    #[must_use]
    pub fn normals(&self) -> Option<&[Vec3]> {
        self.normals.as_deref()
    }

    /// Returns the optional tangent stream.
    #[must_use]
    pub fn tangents(&self) -> Option<&[Vec4]> {
        self.tangents.as_deref()
    }

    /// Returns the optional primary UV stream.
    #[must_use]
    pub fn tex_coords_0(&self) -> Option<&[Vec2]> {
        self.tex_coords(0)
    }

    /// Returns one optional authored UV stream by glTF-compatible set index.
    #[must_use]
    pub fn tex_coords(&self, set: u8) -> Option<&[Vec2]> {
        self.tex_coords
            .get(usize::from(set))
            .and_then(Option::as_deref)
    }

    /// Returns the optional per-vertex texture-blend stream.
    #[must_use]
    pub fn texture_blend_weights(&self) -> Option<&[f32]> {
        self.texture_blend_weights.as_deref()
    }

    /// Returns the optional material slot.
    #[must_use]
    pub const fn material(&self) -> Option<MaterialIndex> {
        self.material
    }

    /// Creates a cube with face normals and primary UVs.
    ///
    /// # Errors
    ///
    /// Returns [`PrimitiveError::InvalidHalfExtent`] for a non-finite or
    /// non-positive `half_extent`.
    pub fn cube(half_extent: f32) -> Result<Self, PrimitiveError> {
        if !half_extent.is_finite() || half_extent <= 0.0 {
            return Err(PrimitiveError::InvalidHalfExtent { half_extent });
        }
        let h = half_extent;
        let positions = vec![
            [-h, -h, h],
            [h, -h, h],
            [h, h, h],
            [-h, h, h],
            [h, -h, -h],
            [-h, -h, -h],
            [-h, h, -h],
            [h, h, -h],
            [h, -h, h],
            [h, -h, -h],
            [h, h, -h],
            [h, h, h],
            [-h, -h, -h],
            [-h, -h, h],
            [-h, h, h],
            [-h, h, -h],
            [-h, h, h],
            [h, h, h],
            [h, h, -h],
            [-h, h, -h],
            [-h, -h, -h],
            [h, -h, -h],
            [h, -h, h],
            [-h, -h, h],
        ];
        let normals = [
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ]
        .into_iter()
        .flat_map(|normal| std::iter::repeat_n(normal, 4))
        .collect();
        let tex_coords_0 = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]].repeat(6);
        let indices = vec![
            0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16,
            17, 18, 16, 18, 19, 20, 21, 22, 20, 22, 23,
        ];
        let primitive =
            Self::new(positions, indices).map_err(|_| PrimitiveError::InvalidStaticGeometry)?;
        let primitive = primitive
            .with_normals(normals)
            .map_err(|_| PrimitiveError::InvalidStaticGeometry)?;
        primitive
            .with_tex_coords_0(tex_coords_0)
            .map_err(|_| PrimitiveError::InvalidStaticGeometry)
    }

    fn validate(&self) -> Result<(), MeshValidationError> {
        if self.positions.is_empty() {
            return Err(MeshValidationError::EmptyPositions);
        }
        if self.indices.is_empty() {
            return Err(MeshValidationError::EmptyIndices);
        }
        if !self.indices.len().is_multiple_of(3) {
            return Err(MeshValidationError::NonTriangleIndexCount {
                actual: self.indices.len(),
            });
        }
        for &index in &self.indices {
            if usize::try_from(index).map_or(true, |index| index >= self.positions.len()) {
                return Err(MeshValidationError::IndexOutOfBounds {
                    index,
                    vertex_count: self.positions.len(),
                });
            }
        }
        self.validate_attribute(AttributeKind::Normals, self.normals.as_ref().map(Vec::len))?;
        self.validate_attribute(
            AttributeKind::Tangents,
            self.tangents.as_ref().map(Vec::len),
        )?;
        for (set, tex_coords) in (0_u8..).zip(&self.tex_coords) {
            self.validate_attribute(
                AttributeKind::TexCoords(set),
                tex_coords.as_ref().map(Vec::len),
            )?;
        }
        self.validate_attribute(
            AttributeKind::TextureBlendWeights,
            self.texture_blend_weights.as_ref().map(Vec::len),
        )?;
        Ok(())
    }

    fn validate_attribute(
        &self,
        attribute: AttributeKind,
        actual: Option<usize>,
    ) -> Result<(), MeshValidationError> {
        if let Some(actual) = actual
            && actual != self.positions.len()
        {
            return Err(MeshValidationError::AttributeCountMismatch {
                attribute,
                expected: self.positions.len(),
                actual,
            });
        }
        Ok(())
    }
}

/// A material slot index within one [`Model`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct MaterialIndex(usize);

impl MaterialIndex {
    /// Creates an index from its zero-based material position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based material position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A texture slot index within one [`Model`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ModelTextureIndex(usize);

impl ModelTextureIndex {
    /// Creates an index from its zero-based texture position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based texture position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// The renderer-neutral source of one model texture.
///
/// Importers preserve a relative URI, original encoded image bytes, or already
/// decoded RGBA8 pixels when their source format has its own decoder. GPU
/// upload remains a separate asset-residency concern.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelTextureSource {
    /// A relative or virtual asset URI resolved by the application.
    ExternalUri(String),
    /// Image bytes embedded in the source asset, such as a GLB buffer view.
    Encoded {
        /// Declared source MIME type, for example `image/png`.
        mime_type: String,
        /// Original encoded image bytes shared between every texture slot that
        /// references the same source image.
        bytes: Arc<[u8]>,
    },
    /// Tightly packed decoded RGBA8 pixels owned by an importer.
    DecodedRgba8 {
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Four bytes per pixel in red, green, blue, alpha order.
        pixels: Arc<[u8]>,
    },
}

/// Invalid dimensions or byte length for decoded model texture pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelTextureRgba8Error {
    /// Width or height was zero.
    ZeroDimension,
    /// `width * height * 4` overflowed `usize`.
    SizeOverflow,
    /// Pixel byte length did not match the dimensions.
    ByteLength {
        /// Required byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
}

impl fmt::Display for ModelTextureRgba8Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimension => {
                formatter.write_str("decoded RGBA8 texture dimensions must be non-zero")
            }
            Self::SizeOverflow => {
                formatter.write_str("decoded RGBA8 texture dimensions overflow usize")
            }
            Self::ByteLength { expected, actual } => write!(
                formatter,
                "decoded RGBA8 texture requires {expected} bytes, got {actual}"
            ),
        }
    }
}

impl Error for ModelTextureRgba8Error {}

/// Metadata for one texture referenced by a model material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelTexture {
    label: Option<String>,
    source: ModelTextureSource,
    sampler: Option<ModelTextureSampler>,
}

/// Renderer-neutral coordinate wrapping for a model texture.
///
/// This deliberately mirrors the small glTF sampler vocabulary without
/// exposing a particular graphics backend to model data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ModelTextureAddressMode {
    /// Repeat the image outside the 0..1 coordinate range.
    Repeat,
    /// Repeat the image while alternating its direction every other copy.
    MirroredRepeat,
    /// Keep sampling the closest edge pixel outside the image.
    ClampToEdge,
}

/// Renderer-neutral magnification filter for a model texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ModelTextureMagFilter {
    /// Select the nearest source texel.
    Nearest,
    /// Blend adjacent source texels.
    Linear,
}

/// Renderer-neutral minification and mip-level filter for a model texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ModelTextureMinFilter {
    /// Select the nearest source texel.
    Nearest,
    /// Blend adjacent source texels.
    Linear,
    /// Nearest texel and nearest mip level.
    NearestMipmapNearest,
    /// Linear texel blend and nearest mip level.
    LinearMipmapNearest,
    /// Nearest texel and linear mip-level blend.
    NearestMipmapLinear,
    /// Linear texel and mip-level blend.
    LinearMipmapLinear,
}

/// Sampling settings attached to a texture by its source format.
///
/// Importers preserve these settings. A renderer translates them to its own
/// sampler object when it uploads the image. Manually-created textures can
/// omit the value and let the asset loader select its configured default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ModelTextureSampler {
    /// Horizontal coordinate behaviour outside the image.
    pub address_mode_u: ModelTextureAddressMode,
    /// Vertical coordinate behaviour outside the image.
    pub address_mode_v: ModelTextureAddressMode,
    /// Magnification filter.
    pub mag_filter: ModelTextureMagFilter,
    /// Minification and mip-level filter.
    pub min_filter: ModelTextureMinFilter,
}

impl Default for ModelTextureSampler {
    fn default() -> Self {
        Self {
            address_mode_u: ModelTextureAddressMode::Repeat,
            address_mode_v: ModelTextureAddressMode::Repeat,
            mag_filter: ModelTextureMagFilter::Linear,
            min_filter: ModelTextureMinFilter::Linear,
        }
    }
}

impl ModelTexture {
    /// Creates a texture descriptor for a relative or virtual asset URI.
    ///
    /// The URI is not read or normalized here. An importer should preserve the
    /// source reference; an asset resolver decides how to load it.
    #[must_use]
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            label: None,
            source: ModelTextureSource::ExternalUri(uri.into()),
            sampler: None,
        }
    }

    /// Creates a texture descriptor from bytes embedded in its source asset.
    ///
    /// `mime_type` remains source metadata; the asset loader validates the
    /// bytes against its own decoder policy before allocating CPU or GPU data.
    #[must_use]
    pub fn embedded(mime_type: impl Into<String>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            label: None,
            source: ModelTextureSource::Encoded {
                mime_type: mime_type.into(),
                bytes: bytes.into(),
            },
            sampler: None,
        }
    }

    /// Creates a descriptor from tightly packed decoded RGBA8 pixels.
    ///
    /// This avoids an encode/decode round trip for importers such as Source VTF
    /// that already own a bounded image decoder.
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureRgba8Error`] for zero/overflowing dimensions or a
    /// byte count other than `width * height * 4`.
    pub fn decoded_rgba8(
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<Self, ModelTextureRgba8Error> {
        if width == 0 || height == 0 {
            return Err(ModelTextureRgba8Error::ZeroDimension);
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ModelTextureRgba8Error::SizeOverflow)?;
        let pixels = pixels.into();
        if pixels.len() != expected {
            return Err(ModelTextureRgba8Error::ByteLength {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            label: None,
            source: ModelTextureSource::DecodedRgba8 {
                width,
                height,
                pixels,
            },
            sampler: None,
        })
    }

    /// Assigns an optional source/debug label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Attaches sampling settings preserved by an importer.
    #[must_use]
    pub const fn with_sampler(mut self, sampler: ModelTextureSampler) -> Self {
        self.sampler = Some(sampler);
        self
    }

    /// Returns the optional source/debug label.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns the renderer-neutral source descriptor.
    #[must_use]
    pub const fn source(&self) -> &ModelTextureSource {
        &self.source
    }

    /// Returns source-defined sampling settings, if the importer supplied them.
    ///
    /// A manually-created texture has no source sampler; the asset loader then
    /// uses its explicitly configured default sampler.
    #[must_use]
    pub const fn sampler(&self) -> Option<ModelTextureSampler> {
        self.sampler
    }

    /// Returns the unresolved relative or virtual URI, if this texture is not
    /// embedded in the source asset.
    #[must_use]
    pub fn uri(&self) -> Option<&str> {
        match &self.source {
            ModelTextureSource::ExternalUri(uri) => Some(uri),
            ModelTextureSource::Encoded { .. } | ModelTextureSource::DecodedRgba8 { .. } => None,
        }
    }

    /// Returns the encoded source image, if this texture is embedded.
    #[must_use]
    pub fn encoded(&self) -> Option<(&str, &[u8])> {
        match &self.source {
            ModelTextureSource::ExternalUri(_) | ModelTextureSource::DecodedRgba8 { .. } => None,
            ModelTextureSource::Encoded { mime_type, bytes } => Some((mime_type, bytes)),
        }
    }

    /// Returns decoded RGBA8 dimensions and pixels, when supplied by an importer.
    #[must_use]
    pub fn decoded_rgba8_pixels(&self) -> Option<(u32, u32, &[u8])> {
        match &self.source {
            ModelTextureSource::DecodedRgba8 {
                width,
                height,
                pixels,
            } => Some((*width, *height, pixels)),
            ModelTextureSource::ExternalUri(_) | ModelTextureSource::Encoded { .. } => None,
        }
    }
}

/// PBR-oriented material metadata. Every texture is optional.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Material {
    name: Option<String>,
    base_color_factor: Vec4,
    metallic_factor: f32,
    roughness_factor: f32,
    base_color_texture: Option<TextureBinding>,
    normal_texture: Option<NormalTextureBinding>,
    metallic_roughness_texture: Option<TextureBinding>,
    emissive_factor: Vec3,
    emissive_texture: Option<TextureBinding>,
    double_sided: bool,
    alpha_mode: AlphaMode,
    specular_glossiness: Option<SpecularGlossinessMaterial>,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: None,
            base_color_factor: [1.0; 4],
            metallic_factor: 1.0,
            roughness_factor: 1.0,
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            emissive_factor: [0.0; 3],
            emissive_texture: None,
            double_sided: false,
            alpha_mode: AlphaMode::Opaque,
            specular_glossiness: None,
        }
    }
}

impl Material {
    /// Creates a default opaque white metal/roughness material.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the optional source/debug name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the RGBA base-colour multiplier.
    #[must_use]
    pub const fn with_base_color_factor(mut self, factor: Vec4) -> Self {
        self.base_color_factor = factor;
        self
    }

    /// Sets the metallic and roughness multipliers without clamping them.
    #[must_use]
    pub const fn with_metallic_roughness(mut self, metallic: f32, roughness: f32) -> Self {
        self.metallic_factor = metallic;
        self.roughness_factor = roughness;
        self
    }

    /// Associates an sRGB base-colour texture.
    #[must_use]
    pub const fn with_base_color_texture(mut self, binding: TextureBinding) -> Self {
        self.base_color_texture = Some(binding);
        self
    }

    /// Clears the base-colour texture binding.
    #[must_use]
    pub const fn without_base_color_texture(mut self) -> Self {
        self.base_color_texture = None;
        self
    }

    /// Associates an optional linear-space normal map.
    #[must_use]
    pub const fn with_normal_texture(mut self, binding: NormalTextureBinding) -> Self {
        self.normal_texture = Some(binding);
        self
    }

    /// Clears the normal-map texture binding.
    #[must_use]
    pub const fn without_normal_texture(mut self) -> Self {
        self.normal_texture = None;
        self
    }

    /// Associates a linear metallic-roughness texture.
    #[must_use]
    pub const fn with_metallic_roughness_texture(mut self, binding: TextureBinding) -> Self {
        self.metallic_roughness_texture = Some(binding);
        self
    }

    /// Clears the metallic-roughness texture binding.
    #[must_use]
    pub const fn without_metallic_roughness_texture(mut self) -> Self {
        self.metallic_roughness_texture = None;
        self
    }

    /// Sets the linear emissive RGB multiplier.
    #[must_use]
    pub const fn with_emissive_factor(mut self, factor: Vec3) -> Self {
        self.emissive_factor = factor;
        self
    }

    /// Associates an sRGB emissive texture.
    #[must_use]
    pub const fn with_emissive_texture(mut self, binding: TextureBinding) -> Self {
        self.emissive_texture = Some(binding);
        self
    }

    /// Clears the emissive texture binding.
    #[must_use]
    pub const fn without_emissive_texture(mut self) -> Self {
        self.emissive_texture = None;
        self
    }

    /// Selects whether both triangle faces must be rasterized.
    ///
    /// `false` is the default opaque back-face-culling policy. `true` retains
    /// the glTF `material.doubleSided` contract; renderers must select a
    /// compatible no-cull pipeline rather than silently discarding it.
    #[must_use]
    pub const fn with_double_sided(mut self, double_sided: bool) -> Self {
        self.double_sided = double_sided;
        self
    }

    /// Selects how the material contributes to the colour target.
    ///
    /// This stores the source contract only. A renderer must either select a
    /// compatible phase or return a structured unsupported-feature error; it
    /// must not silently draw `Blend` as an opaque material.
    #[must_use]
    pub const fn with_alpha_mode(mut self, alpha_mode: AlphaMode) -> Self {
        self.alpha_mode = alpha_mode;
        self
    }

    /// Preserves a `KHR_materials_pbrSpecularGlossiness` source workflow.
    ///
    /// It is intentionally not approximated as metallic-roughness metadata:
    /// a renderer selects an explicit compatible path or returns a structured
    /// unsupported-workflow error.
    #[must_use]
    pub fn with_specular_glossiness(mut self, workflow: SpecularGlossinessMaterial) -> Self {
        self.specular_glossiness = Some(workflow);
        self
    }

    /// Returns the optional material name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the RGBA base-colour multiplier.
    #[must_use]
    pub const fn base_color_factor(&self) -> Vec4 {
        self.base_color_factor
    }

    /// Returns the metallic multiplier.
    #[must_use]
    pub const fn metallic_factor(&self) -> f32 {
        self.metallic_factor
    }

    /// Returns the roughness multiplier.
    #[must_use]
    pub const fn roughness_factor(&self) -> f32 {
        self.roughness_factor
    }

    /// Returns the optional base-colour texture binding.
    #[must_use]
    pub const fn base_color_texture(&self) -> Option<TextureBinding> {
        self.base_color_texture
    }

    /// Returns the optional normal map binding.
    #[must_use]
    pub const fn normal_texture(&self) -> Option<NormalTextureBinding> {
        self.normal_texture
    }

    /// Returns the optional metallic-roughness texture binding.
    #[must_use]
    pub const fn metallic_roughness_texture(&self) -> Option<TextureBinding> {
        self.metallic_roughness_texture
    }

    /// Returns the linear emissive RGB multiplier.
    #[must_use]
    pub const fn emissive_factor(&self) -> Vec3 {
        self.emissive_factor
    }

    /// Returns the optional emissive texture binding.
    #[must_use]
    pub const fn emissive_texture(&self) -> Option<TextureBinding> {
        self.emissive_texture
    }

    /// Returns whether both triangle faces must be rasterized.
    #[must_use]
    pub const fn double_sided(&self) -> bool {
        self.double_sided
    }

    /// Returns the declared source alpha policy.
    #[must_use]
    pub const fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    /// Returns the optional preserved specular-glossiness source workflow.
    #[must_use]
    pub const fn specular_glossiness(&self) -> Option<&SpecularGlossinessMaterial> {
        self.specular_glossiness.as_ref()
    }

    fn texture_bindings(&self) -> impl Iterator<Item = TextureBinding> {
        [
            self.base_color_texture,
            self.metallic_roughness_texture,
            self.emissive_texture,
        ]
        .into_iter()
        .flatten()
        .chain(self.normal_texture.map(NormalTextureBinding::binding))
        .chain(
            self.specular_glossiness
                .iter()
                .flat_map(SpecularGlossinessMaterial::texture_bindings),
        )
    }
}

/// Source alpha policy for a [`Material`].
///
/// The variants mirror glTF 2.0. `Mask` retains its cutoff for renderers that
/// implement coverage testing without reparsing the source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AlphaMode {
    /// Fully opaque material. It belongs to the depth-writing opaque phase.
    #[default]
    Opaque,
    /// Discard fragments whose alpha is strictly below this cutoff.
    Mask {
        /// glTF `alphaCutoff` value.
        cutoff: f32,
    },
    /// Source-over alpha blending. It belongs to a sorted, non-depth-writing phase.
    Blend,
}

/// Source metadata for `KHR_materials_pbrSpecularGlossiness`.
///
/// This workflow is kept losslessly until a renderer opts into a compatible
/// specular-glossiness pipeline. It is distinct from glTF metallic-roughness
/// material fields and must not be silently converted by an importer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpecularGlossinessMaterial {
    diffuse_factor: Vec4,
    diffuse_texture: Option<TextureBinding>,
    specular_factor: Vec3,
    glossiness_factor: f32,
    specular_glossiness_texture: Option<TextureBinding>,
}

impl SpecularGlossinessMaterial {
    /// Creates a source workflow with its declared diffuse, specular and
    /// glossiness factors.
    #[must_use]
    pub const fn new(diffuse_factor: Vec4, specular_factor: Vec3, glossiness_factor: f32) -> Self {
        Self {
            diffuse_factor,
            diffuse_texture: None,
            specular_factor,
            glossiness_factor,
            specular_glossiness_texture: None,
        }
    }

    /// Associates the optional sRGB diffuse texture.
    #[must_use]
    pub const fn with_diffuse_texture(mut self, binding: TextureBinding) -> Self {
        self.diffuse_texture = Some(binding);
        self
    }

    /// Associates the optional sRGB specular/glossiness texture.
    #[must_use]
    pub const fn with_specular_glossiness_texture(mut self, binding: TextureBinding) -> Self {
        self.specular_glossiness_texture = Some(binding);
        self
    }

    /// Returns the source diffuse RGBA multiplier.
    #[must_use]
    pub const fn diffuse_factor(&self) -> Vec4 {
        self.diffuse_factor
    }

    /// Returns the optional diffuse texture binding.
    #[must_use]
    pub const fn diffuse_texture(&self) -> Option<TextureBinding> {
        self.diffuse_texture
    }

    /// Returns the source RGB specular multiplier.
    #[must_use]
    pub const fn specular_factor(&self) -> Vec3 {
        self.specular_factor
    }

    /// Returns the source glossiness multiplier.
    #[must_use]
    pub const fn glossiness_factor(&self) -> f32 {
        self.glossiness_factor
    }

    /// Returns the optional specular/glossiness texture binding.
    #[must_use]
    pub const fn specular_glossiness_texture(&self) -> Option<TextureBinding> {
        self.specular_glossiness_texture
    }

    fn texture_bindings(&self) -> impl Iterator<Item = TextureBinding> {
        [self.diffuse_texture, self.specular_glossiness_texture]
            .into_iter()
            .flatten()
    }
}

/// A reference to a texture plus its UV set.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextureBinding {
    texture: ModelTextureIndex,
    tex_coord_set: u8,
}

impl TextureBinding {
    /// Creates a binding using `tex_coord_set` (usually zero).
    #[must_use]
    pub const fn new(texture: ModelTextureIndex, tex_coord_set: u8) -> Self {
        Self {
            texture,
            tex_coord_set,
        }
    }
    /// Returns the referenced model texture slot.
    #[must_use]
    pub const fn texture(self) -> ModelTextureIndex {
        self.texture
    }
    /// Returns the source UV set number.
    #[must_use]
    pub const fn tex_coord_set(self) -> u8 {
        self.tex_coord_set
    }
}

/// A normal texture binding and its material scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalTextureBinding {
    binding: TextureBinding,
    scale: f32,
}

impl NormalTextureBinding {
    /// Creates a normal-map binding with the supplied scale.
    #[must_use]
    pub const fn new(binding: TextureBinding, scale: f32) -> Self {
        Self { binding, scale }
    }
    /// Returns the underlying texture/UV binding.
    #[must_use]
    pub const fn binding(self) -> TextureBinding {
        self.binding
    }
    /// Returns the normal-map scale.
    #[must_use]
    pub const fn scale(self) -> f32 {
        self.scale
    }
}

/// A failed low-level mesh construction request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshValidationError {
    /// No position vertices were supplied.
    EmptyPositions,
    /// No indices were supplied.
    EmptyIndices,
    /// The index count cannot form complete triangles.
    NonTriangleIndexCount {
        /// Observed count.
        actual: usize,
    },
    /// An index points past the position stream.
    IndexOutOfBounds {
        /// Invalid index.
        index: u32,
        /// Position count.
        vertex_count: usize,
    },
    /// An optional per-vertex attribute has a different count than positions.
    AttributeCountMismatch {
        /// Stream type.
        attribute: AttributeKind,
        /// Position count.
        expected: usize,
        /// Observed attribute count.
        actual: usize,
    },
    /// A UV stream used a set outside the retained glTF-compatible range.
    TextureCoordinateSetOutOfRange {
        /// Requested set.
        actual: u8,
        /// Largest supported set.
        maximum: u8,
    },
}

impl fmt::Display for MeshValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPositions => f.write_str("mesh primitive requires at least one position"),
            Self::EmptyIndices => f.write_str("mesh primitive requires at least one triangle"),
            Self::NonTriangleIndexCount { actual } => write!(
                f,
                "mesh primitive has {actual} indices; triangle lists need a multiple of three"
            ),
            Self::IndexOutOfBounds {
                index,
                vertex_count,
            } => write!(f, "mesh index {index} is outside {vertex_count} positions"),
            Self::AttributeCountMismatch {
                attribute,
                expected,
                actual,
            } => write!(f, "{attribute} has {actual} entries; expected {expected}"),
            Self::TextureCoordinateSetOutOfRange { actual, maximum } => write!(
                f,
                "texture coordinate set {actual} is outside the supported range 0..={maximum}"
            ),
        }
    }
}
impl Error for MeshValidationError {}

/// The optional per-position stream whose length did not match positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeKind {
    /// Vertex normal stream.
    Normals,
    /// Vertex tangent stream.
    Tangents,
    /// UV stream with its authored set number.
    TexCoords(u8),
    /// Scalar texture-blend stream.
    TextureBlendWeights,
}
impl fmt::Display for AttributeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normals => f.write_str("normals"),
            Self::Tangents => f.write_str("tangents"),
            Self::TexCoords(set) => write!(f, "tex_coords_{set}"),
            Self::TextureBlendWeights => f.write_str("texture_blend_weights"),
        }
    }
}

/// A mesh cannot be created from the requested primitive collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshError {
    /// No primitives were supplied.
    Empty,
}
impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("mesh requires at least one primitive")
    }
}
impl Error for MeshError {}

/// A cross-resource reference inside a [`Model`] was invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelValidationError {
    /// No meshes were supplied.
    EmptyModel,
    /// A primitive references an absent material slot.
    MissingMaterial {
        /// Mesh position.
        mesh_index: usize,
        /// Primitive position.
        primitive_index: usize,
        /// Requested material slot.
        material: MaterialIndex,
    },
    /// A material references an absent texture slot.
    MissingTexture {
        /// Material position.
        material_index: usize,
        /// Requested texture slot.
        texture: ModelTextureIndex,
    },
}
impl fmt::Display for ModelValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyModel => f.write_str("model requires at least one mesh"),
            Self::MissingMaterial {
                mesh_index,
                primitive_index,
                material,
            } => write!(
                f,
                "mesh {mesh_index} primitive {primitive_index} references missing material {}",
                material.get()
            ),
            Self::MissingTexture {
                material_index,
                texture,
            } => write!(
                f,
                "material {material_index} references missing texture {}",
                texture.get()
            ),
        }
    }
}
impl Error for ModelValidationError {}

/// A validated post-import material edit could not be applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelMaterialEditError {
    /// The requested physical mesh does not exist.
    MissingMesh {
        /// Requested mesh index.
        mesh: usize,
    },
    /// The requested primitive does not exist in an existing mesh.
    MissingPrimitive {
        /// Requested mesh index.
        mesh: usize,
        /// Requested primitive index.
        primitive: usize,
    },
    /// The requested material slot does not exist.
    MissingMaterial {
        /// Requested material index.
        material: MaterialIndex,
    },
    /// A replacement material references an absent texture.
    MissingTexture {
        /// Requested texture index.
        texture: ModelTextureIndex,
    },
}

impl fmt::Display for ModelMaterialEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMesh { mesh } => write!(formatter, "model mesh {mesh} does not exist"),
            Self::MissingPrimitive { mesh, primitive } => {
                write!(formatter, "model mesh {mesh} has no primitive {primitive}")
            }
            Self::MissingMaterial { material } => write!(
                formatter,
                "model material {} does not exist",
                material.get()
            ),
            Self::MissingTexture { texture } => {
                write!(formatter, "model texture {} does not exist", texture.get())
            }
        }
    }
}

impl Error for ModelMaterialEditError {}

/// A built-in geometric primitive could not be generated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimitiveError {
    /// Cube half-extent was non-finite or not positive.
    InvalidHalfExtent {
        /// Rejected value.
        half_extent: f32,
    },
    /// Defensive error for static primitive data.
    InvalidStaticGeometry,
}
impl fmt::Display for PrimitiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHalfExtent { half_extent } => write!(
                f,
                "cube half extent must be finite and positive, got {half_extent}"
            ),
            Self::InvalidStaticGeometry => f.write_str("built-in primitive data failed validation"),
        }
    }
}
impl Error for PrimitiveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_rejects_indices_outside_positions() {
        let error = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 3])
            .expect_err("index three is absent");
        assert!(matches!(
            error,
            MeshValidationError::IndexOutOfBounds { index: 3, .. }
        ));
    }

    #[test]
    fn optional_streams_must_match_positions() {
        let primitive =
            MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2]).expect("valid triangle");
        let error = primitive
            .with_normals(vec![[0.0; 3]; 2])
            .expect_err("two normals do not match");
        assert!(matches!(
            error,
            MeshValidationError::AttributeCountMismatch {
                attribute: AttributeKind::Normals,
                ..
            }
        ));
    }

    #[test]
    fn primitive_retains_texture_blend_weights() {
        let primitive = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("valid triangle")
            .with_texture_blend_weights(vec![0.0, 0.35, 1.0])
            .expect("one blend weight per position");

        assert_eq!(
            primitive.texture_blend_weights(),
            Some(&[0.0, 0.35, 1.0][..])
        );
    }

    #[test]
    fn texture_blend_weights_must_match_positions() {
        let primitive =
            MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2]).expect("valid triangle");
        let error = primitive
            .with_texture_blend_weights(vec![0.0, 1.0])
            .expect_err("two blend weights do not match three positions");

        assert!(matches!(
            error,
            MeshValidationError::AttributeCountMismatch {
                attribute: AttributeKind::TextureBlendWeights,
                expected: 3,
                actual: 2,
            }
        ));
    }

    #[test]
    fn primitive_retains_multiple_uv_sets_without_remapping() {
        let primitive = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("valid triangle")
            .with_tex_coords(0, vec![[0.0, 0.0]; 3])
            .expect("valid UV0")
            .with_tex_coords(3, vec![[0.75, 0.25]; 3])
            .expect("valid UV3");

        assert_eq!(primitive.tex_coords_0(), Some(&[[0.0, 0.0]; 3][..]));
        assert_eq!(primitive.tex_coords(3), Some(&[[0.75, 0.25]; 3][..]));
        assert_eq!(primitive.tex_coords(7), None);
    }

    #[test]
    fn primitive_rejects_uv_set_above_gltf_core_range() {
        let primitive =
            MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2]).expect("valid triangle");
        assert!(matches!(
            primitive.with_tex_coords(8, vec![[0.0; 2]; 3]),
            Err(MeshValidationError::TextureCoordinateSetOutOfRange {
                actual: 8,
                maximum: 7
            })
        ));
    }

    #[test]
    fn model_rejects_absent_material_reference() {
        let primitive = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("valid triangle")
            .with_material(MaterialIndex::new(0));
        let mesh = Mesh::new(None, vec![primitive]).expect("non-empty");
        let error =
            Model::new(vec![mesh], Vec::new(), Vec::new()).expect_err("material zero is absent");
        assert!(matches!(
            error,
            ModelValidationError::MissingMaterial { .. }
        ));
    }

    #[test]
    fn cube_has_independent_face_normals_and_uvs() {
        let cube = Model::cube(0.5).expect("positive extent");
        let primitive = &cube.meshes()[0].primitives()[0];
        assert_eq!(primitive.positions().len(), 24);
        assert_eq!(primitive.indices().len(), 36);
        assert_eq!(primitive.normals().map(<[Vec3]>::len), Some(24));
        assert_eq!(primitive.tex_coords_0().map(<[Vec2]>::len), Some(24));
        assert_eq!(primitive.material(), Some(MaterialIndex::new(0)));
        assert_eq!(cube.materials()[0].name(), Some("yuyib.default_cube"));
    }

    #[test]
    fn material_retains_explicit_double_sided_rasterization() {
        assert!(!Material::default().double_sided());
        assert!(Material::new().with_double_sided(true).double_sided());
    }

    #[test]
    fn post_import_material_edits_are_validated_without_cloning_geometry() {
        let mut model = Model::cube(0.5).expect("cube");
        let material = model
            .add_material(
                Material::new()
                    .with_name("fallback")
                    .with_base_color_factor([0.1, 0.2, 0.3, 1.0])
                    .with_double_sided(true),
            )
            .expect("texture-free material");
        assert_eq!(
            model.set_primitive_material(0, 0, material),
            Ok(Some(MaterialIndex::new(0)))
        );
        assert_eq!(model.meshes()[0].primitives()[0].material(), Some(material));

        let before = model.materials()[material.get()].clone();
        let invalid = Material::new()
            .with_base_color_texture(TextureBinding::new(ModelTextureIndex::new(99), 0));
        assert_eq!(
            model.replace_material(material, invalid),
            Err(ModelMaterialEditError::MissingTexture {
                texture: ModelTextureIndex::new(99)
            })
        );
        assert_eq!(model.materials()[material.get()], before);
    }

    #[test]
    fn material_retains_explicit_alpha_mode() {
        let material = Material::new().with_alpha_mode(AlphaMode::Mask { cutoff: 0.25 });
        assert_eq!(material.alpha_mode(), AlphaMode::Mask { cutoff: 0.25 });
        assert_eq!(Material::default().alpha_mode(), AlphaMode::Opaque);
    }

    #[test]
    fn texture_can_keep_source_sampler_without_coupling_to_a_gpu_backend() {
        let sampler = ModelTextureSampler {
            address_mode_u: ModelTextureAddressMode::Repeat,
            address_mode_v: ModelTextureAddressMode::ClampToEdge,
            mag_filter: ModelTextureMagFilter::Nearest,
            min_filter: ModelTextureMinFilter::LinearMipmapLinear,
        };
        let texture = ModelTexture::new("wall.png").with_sampler(sampler);
        assert_eq!(texture.sampler(), Some(sampler));
        assert_eq!(ModelTexture::new("wall.png").sampler(), None);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "fixture values verify that authored workflow metadata is preserved exactly"
    )]
    fn embedded_texture_and_specular_glossiness_keep_typed_source_metadata() {
        let texture = ModelTexture::embedded("image/png", vec![0x89, b'P', b'N', b'G']);
        assert_eq!(texture.uri(), None);
        assert_eq!(
            texture.encoded(),
            Some(("image/png", &[0x89, b'P', b'N', b'G'][..]))
        );

        let workflow = SpecularGlossinessMaterial::new([0.2, 0.3, 0.4, 1.0], [0.5, 0.6, 0.7], 0.8)
            .with_diffuse_texture(TextureBinding::new(ModelTextureIndex::new(0), 0));
        let primitive = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("valid triangle")
            .with_material(MaterialIndex::new(0));
        let mesh = Mesh::new(None, vec![primitive]).expect("non-empty mesh");
        let model = Model::new(
            vec![mesh],
            vec![Material::new().with_specular_glossiness(workflow)],
            vec![texture],
        )
        .expect("workflow texture reference is valid");
        let imported = model.materials()[0]
            .specular_glossiness()
            .expect("workflow retained");
        assert_eq!(imported.specular_factor(), [0.5, 0.6, 0.7]);
        assert_eq!(imported.glossiness_factor(), 0.8);
    }
}
