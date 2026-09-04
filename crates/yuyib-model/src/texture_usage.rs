//! High-level texture binding inventory for imported models.
//!
//! Complements [`crate::Model::material_usage`]: answer which textures are
//! unused, external, empty, or referenced with a UV set the mesh does not
//! provide — without walking low-level binding storage by hand.

use std::collections::BTreeSet;

use crate::{
    Material, MeshPrimitiveRef, Model, ModelTextureIndex, ModelTextureSource, TextureBinding,
};

/// One texture slot and the materials that reference it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelTextureUsageEntry {
    /// Texture slot index.
    pub index: ModelTextureIndex,
    /// Optional importer label.
    pub label: Option<String>,
    /// Whether the source is an external URI (bytes not embedded).
    pub external: bool,
    /// Whether an embedded blob is empty.
    pub empty_embedded: bool,
    /// Material indices that bind this texture.
    pub material_users: Vec<usize>,
}

/// High-level inventory of texture slots and UV-binding issues.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelTextureUsage {
    textures: Vec<ModelTextureUsageEntry>,
    missing_uv_bindings: Vec<MissingUvBinding>,
}

/// A material texture binding that asks for a TEXCOORD set absent on a mesh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingUvBinding {
    /// Mesh/primitive that lacks the requested set.
    pub primitive: MeshPrimitiveRef,
    /// Material slot that requested the UV set.
    pub material_index: usize,
    /// Texture slot involved.
    pub texture: ModelTextureIndex,
    /// Requested TEXCOORD set.
    pub tex_coord_set: u8,
}

impl ModelTextureUsage {
    /// Per-texture rows in texture-index order.
    #[must_use]
    pub fn textures(&self) -> &[ModelTextureUsageEntry] {
        &self.textures
    }

    /// Material→mesh UV mismatches discovered while scanning the model.
    #[must_use]
    pub fn missing_uv_bindings(&self) -> &[MissingUvBinding] {
        &self.missing_uv_bindings
    }

    /// Formats a stable multi-line summary for logs and demos.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for entry in &self.textures {
            let label = entry
                .label
                .as_deref()
                .map_or_else(|| format!("#{}", entry.index.get()), ToOwned::to_owned);
            let kind = if entry.external {
                "external-uri"
            } else if entry.empty_embedded {
                "empty-embedded"
            } else {
                "embedded"
            };
            let _ = writeln!(
                out,
                "texture `{label}` ({kind}): {} material user(s)",
                entry.material_users.len()
            );
            for material in &entry.material_users {
                let _ = writeln!(out, "  - material #{material}");
            }
        }
        if !self.missing_uv_bindings.is_empty() {
            let _ = writeln!(
                out,
                "missing UV bindings: {} issue(s)",
                self.missing_uv_bindings.len()
            );
            for issue in &self.missing_uv_bindings {
                let _ = writeln!(
                    out,
                    "  - mesh {} primitive {} material #{} texture {} TEXCOORD_{}",
                    issue.primitive.mesh,
                    issue.primitive.primitive,
                    issue.material_index,
                    issue.texture.get(),
                    issue.tex_coord_set
                );
            }
        }
        out
    }
}

impl Model {
    /// Builds a high-level texture usage inventory for diagnostics and tools.
    #[must_use]
    pub fn texture_usage(&self) -> ModelTextureUsage {
        let mut users: Vec<BTreeSet<usize>> = (0..self.textures().len())
            .map(|_| BTreeSet::new())
            .collect();
        for (material_index, material) in self.materials().iter().enumerate() {
            for binding in material_texture_bindings(material) {
                if let Some(slot) = users.get_mut(binding.texture().get()) {
                    slot.insert(material_index);
                }
            }
        }

        let textures = self
            .textures()
            .iter()
            .enumerate()
            .map(|(index, texture)| {
                let (external, empty_embedded) = match texture.source() {
                    ModelTextureSource::ExternalUri(_) => (true, false),
                    ModelTextureSource::Encoded { bytes, .. } => (false, bytes.is_empty()),
                    ModelTextureSource::DecodedRgba8 { pixels, .. } => (false, pixels.is_empty()),
                };
                ModelTextureUsageEntry {
                    index: ModelTextureIndex::new(index),
                    label: texture.label().map(ToOwned::to_owned),
                    external,
                    empty_embedded,
                    material_users: users
                        .get(index)
                        .map(|set| set.iter().copied().collect())
                        .unwrap_or_default(),
                }
            })
            .collect();

        let mut missing_uv_bindings = Vec::new();
        for (mesh, mesh_value) in self.meshes().iter().enumerate() {
            for (primitive, primitive_value) in mesh_value.primitives().iter().enumerate() {
                let Some(material_index) = primitive_value.material() else {
                    continue;
                };
                let Some(material) = self.materials().get(material_index.get()) else {
                    continue;
                };
                for binding in material_texture_bindings(material) {
                    let set = binding.tex_coord_set();
                    if primitive_value.tex_coords(set).is_none() {
                        missing_uv_bindings.push(MissingUvBinding {
                            primitive: MeshPrimitiveRef { mesh, primitive },
                            material_index: material_index.get(),
                            texture: binding.texture(),
                            tex_coord_set: set,
                        });
                    }
                }
            }
        }

        ModelTextureUsage {
            textures,
            missing_uv_bindings,
        }
    }
}

fn material_texture_bindings(material: &Material) -> Vec<TextureBinding> {
    let mut bindings = Vec::new();
    if let Some(binding) = material.base_color_texture() {
        bindings.push(binding);
    }
    if let Some(normal) = material.normal_texture() {
        bindings.push(normal.binding());
    }
    if let Some(binding) = material.metallic_roughness_texture() {
        bindings.push(binding);
    }
    if let Some(binding) = material.emissive_texture() {
        bindings.push(binding);
    }
    if let Some(workflow) = material.specular_glossiness() {
        if let Some(binding) = workflow.diffuse_texture() {
            bindings.push(binding);
        }
        if let Some(binding) = workflow.specular_glossiness_texture() {
            bindings.push(binding);
        }
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Material, Mesh, MeshPrimitive, ModelTexture, TextureBinding};

    #[test]
    fn texture_usage_reports_unused_and_missing_uv_sets() {
        let prim = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("triangle")
            .with_tex_coords_0(vec![[0.0, 0.0]; 3])
            .expect("uv0")
            .with_material(crate::MaterialIndex::new(0));
        let model = Model::new(
            vec![Mesh::new(Some("panel".to_owned()), vec![prim]).expect("mesh")],
            vec![
                Material::new()
                    .with_name("textured")
                    .with_base_color_texture(TextureBinding::new(ModelTextureIndex::new(0), 1)),
            ],
            vec![
                ModelTexture::embedded("image/png", vec![1, 2, 3]).with_label("used"),
                ModelTexture::new("orphan.png").with_label("unused"),
            ],
        )
        .expect("valid model");

        let usage = model.texture_usage();
        assert_eq!(usage.textures()[0].material_users, vec![0]);
        assert!(usage.textures()[1].material_users.is_empty());
        assert!(usage.textures()[1].external);
        assert_eq!(usage.missing_uv_bindings().len(), 1);
        assert_eq!(usage.missing_uv_bindings()[0].tex_coord_set, 1);
        assert!(usage.summary().contains("unused"));
    }
}
