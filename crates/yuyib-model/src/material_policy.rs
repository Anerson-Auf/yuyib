//! Declarative post-import material override and fallback policy.
//!
//! # High-level vs low-level
//!
//! **High-level (preferred for game code):** build a [`ModelMaterialPolicy`] and
//! attach it to [`yuyib_render_3d::GltfSceneLoadConfig::with_material_policy`].
//! The worker applies the policy after import and before texture preparation;
//! importer + policy diagnostics land on
//! [`yuyib_render_3d::LoadedGltfScene::diagnostics`].
//!
//! ```ignore
//! use yuyib::model::{Material, MaterialFactorPatch, ModelMaterialPolicy};
//! use yuyib::render_3d::{GltfSceneLoad, GltfSceneLoadConfig};
//!
//! let policy = ModelMaterialPolicy::new()
//!     .patch_named(
//!         "material_0",
//!         MaterialFactorPatch::new()
//!             .with_base_color_factor([0.04, 0.06, 0.10, 1.0])
//!             .with_metallic_roughness(0.05, 0.82)
//!             .with_double_sided(true),
//!     )
//!     .add_and_remap_meshes(
//!         Material::new()
//!             .with_name("project.recovered_neon")
//!             .with_emissive_factor([2.4, 0.03, 0.12]),
//!         [1],
//!     );
//! let load = GltfSceneLoad::start(
//!     "map.glb",
//!     GltfSceneLoadConfig::default().with_material_policy(policy),
//! )?;
//! ```
//!
//! **Low-level escape hatch:** call
//! [`yuyib_render_3d::LoadedGltfScene::model_mut_before_publication`] and use
//! [`crate::Model::add_material`] / [`crate::Model::replace_material`] /
//! [`crate::Model::set_primitive_material`] directly when you need a one-off
//! edit that does not belong in a reusable policy. Do not hide geometry or
//! invent materials inside the renderer.
//!
//! Asset-specific repair belongs here (or in a cooker later), not in ad-hoc
//! example glue and never as silent renderer heuristics. Geometry stays
//! untouched; only validated material metadata and primitive bindings change.

use std::{error::Error, fmt};

use yuyib_assets::{ImportDiagnostic, ImportDiagnosticSeverity};

use crate::{Material, MaterialIndex, Model, ModelMaterialEditError, Vec3, Vec4};

/// Optional factor overrides applied to an existing named material slot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaterialFactorPatch {
    base_color_factor: Option<Vec4>,
    metallic_factor: Option<f32>,
    roughness_factor: Option<f32>,
    emissive_factor: Option<Vec3>,
    double_sided: Option<bool>,
}

impl MaterialFactorPatch {
    /// Creates an empty patch that leaves every factor unchanged.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            base_color_factor: None,
            metallic_factor: None,
            roughness_factor: None,
            emissive_factor: None,
            double_sided: None,
        }
    }

    /// Overrides the RGBA base-colour multiplier.
    #[must_use]
    pub const fn with_base_color_factor(mut self, factor: Vec4) -> Self {
        self.base_color_factor = Some(factor);
        self
    }

    /// Overrides metallic and roughness multipliers.
    #[must_use]
    pub const fn with_metallic_roughness(mut self, metallic: f32, roughness: f32) -> Self {
        self.metallic_factor = Some(metallic);
        self.roughness_factor = Some(roughness);
        self
    }

    /// Overrides only the metallic multiplier.
    #[must_use]
    pub const fn with_metallic_factor(mut self, metallic: f32) -> Self {
        self.metallic_factor = Some(metallic);
        self
    }

    /// Overrides only the roughness multiplier.
    #[must_use]
    pub const fn with_roughness_factor(mut self, roughness: f32) -> Self {
        self.roughness_factor = Some(roughness);
        self
    }

    /// Overrides the linear emissive RGB multiplier.
    #[must_use]
    pub const fn with_emissive_factor(mut self, factor: Vec3) -> Self {
        self.emissive_factor = Some(factor);
        self
    }

    /// Overrides the double-sided rasterization flag.
    #[must_use]
    pub const fn with_double_sided(mut self, double_sided: bool) -> Self {
        self.double_sided = Some(double_sided);
        self
    }

    /// Applies this factor-only patch to an existing material.
    #[must_use]
    pub fn apply_to(self, material: Material) -> Material {
        let mut material = material;
        if let Some(factor) = self.base_color_factor {
            material = material.with_base_color_factor(factor);
        }
        if let (Some(metallic), Some(roughness)) = (self.metallic_factor, self.roughness_factor) {
            material = material.with_metallic_roughness(metallic, roughness);
        } else {
            if let Some(metallic) = self.metallic_factor {
                let roughness = material.roughness_factor();
                material = material.with_metallic_roughness(metallic, roughness);
            }
            if let Some(roughness) = self.roughness_factor {
                let metallic = material.metallic_factor();
                material = material.with_metallic_roughness(metallic, roughness);
            }
        }
        if let Some(factor) = self.emissive_factor {
            material = material.with_emissive_factor(factor);
        }
        if let Some(double_sided) = self.double_sided {
            material = material.with_double_sided(double_sided);
        }
        material
    }
}

/// One mesh/primitive target for a material rebinding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshPrimitiveRef {
    /// Zero-based mesh index in source order.
    pub mesh: usize,
    /// Zero-based primitive index inside the mesh.
    pub primitive: usize,
}

impl MeshPrimitiveRef {
    /// Targets primitive `0` of `mesh`.
    #[must_use]
    pub const fn mesh(mesh: usize) -> Self {
        Self { mesh, primitive: 0 }
    }

    /// Targets an explicit mesh/primitive pair.
    #[must_use]
    pub const fn new(mesh: usize, primitive: usize) -> Self {
        Self { mesh, primitive }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ModelMaterialPolicyOp {
    PatchNamed {
        name: String,
        patch: MaterialFactorPatch,
        /// When true, a missing name emits a warning diagnostic and continues.
        optional: bool,
    },
    RemapToNamed {
        targets: Vec<MeshPrimitiveRef>,
        material_name: String,
    },
    RemapUsersOfNamed {
        from_name: String,
        to_name: String,
    },
    RemapNamedMeshesToNamed {
        mesh_names: Vec<String>,
        material_name: String,
    },
    AddAndRemap {
        material: Material,
        targets: Vec<MeshPrimitiveRef>,
    },
    AddAndRemapUsersOfNamed {
        from_name: String,
        material: Material,
    },
    AddAndRemapNamedMeshes {
        material: Material,
        mesh_names: Vec<String>,
    },
}

/// Explicit material override / fallback manifest applied after import.
///
/// Operations run in registration order. Lookups resolve the first material
/// whose name matches exactly. Required ops fail the whole policy without
/// partial mutation of earlier successful operations when using
/// [`ModelMaterialPolicy::apply`]: the model is cloned, mutated, then swapped
/// only on success. Optional patches ([`Self::patch_named_optional`]) skip
/// missing names with a warning so asset-specific quirks do not brick map swaps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelMaterialPolicy {
    ops: Vec<ModelMaterialPolicyOp>,
    unbound_primitive_fallback: Option<Material>,
}

impl ModelMaterialPolicy {
    /// Creates an empty policy.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ops: Vec::new(),
            unbound_primitive_fallback: None,
        }
    }

    /// Patches factors of the first material with the given source name.
    ///
    /// Missing names fail [`Self::apply`]. Prefer [`Self::patch_named_optional`]
    /// for asset-specific quirk lists that must survive map swaps.
    #[must_use]
    pub fn patch_named(mut self, name: impl Into<String>, patch: MaterialFactorPatch) -> Self {
        self.ops.push(ModelMaterialPolicyOp::PatchNamed {
            name: name.into(),
            patch,
            optional: false,
        });
        self
    }

    /// Like [`Self::patch_named`], but a missing material is a warning, not an error.
    #[must_use]
    pub fn patch_named_optional(
        mut self,
        name: impl Into<String>,
        patch: MaterialFactorPatch,
    ) -> Self {
        self.ops.push(ModelMaterialPolicyOp::PatchNamed {
            name: name.into(),
            patch,
            optional: true,
        });
        self
    }

    /// Rebinds listed primitives to an existing named material.
    #[must_use]
    pub fn remap_to_named(
        mut self,
        targets: impl IntoIterator<Item = MeshPrimitiveRef>,
        material_name: impl Into<String>,
    ) -> Self {
        self.ops.push(ModelMaterialPolicyOp::RemapToNamed {
            targets: targets.into_iter().collect(),
            material_name: material_name.into(),
        });
        self
    }

    /// Convenience for remapping primitive `0` of each mesh index.
    #[must_use]
    pub fn remap_meshes_to_named(
        self,
        meshes: impl IntoIterator<Item = usize>,
        material_name: impl Into<String>,
    ) -> Self {
        self.remap_to_named(
            meshes.into_iter().map(MeshPrimitiveRef::mesh),
            material_name,
        )
    }

    /// Rebinds every primitive currently using `from_name` to `to_name`.
    ///
    /// This is the high-level alternative to hard-coded mesh indices when an
    /// exporter left many primitives on one broken fallback material.
    #[must_use]
    pub fn remap_users_of_named(
        mut self,
        from_name: impl Into<String>,
        to_name: impl Into<String>,
    ) -> Self {
        self.ops.push(ModelMaterialPolicyOp::RemapUsersOfNamed {
            from_name: from_name.into(),
            to_name: to_name.into(),
        });
        self
    }

    /// Rebinds every primitive of the listed source mesh names to `material_name`.
    ///
    /// Prefer this over [`Self::remap_meshes_to_named`] when the content
    /// pipeline keeps stable mesh labels (Blender object names, etc.).
    #[must_use]
    pub fn remap_named_meshes_to_named(
        mut self,
        mesh_names: impl IntoIterator<Item = impl Into<String>>,
        material_name: impl Into<String>,
    ) -> Self {
        self.ops
            .push(ModelMaterialPolicyOp::RemapNamedMeshesToNamed {
                mesh_names: mesh_names.into_iter().map(Into::into).collect(),
                material_name: material_name.into(),
            });
        self
    }

    /// Appends a synthetic material and rebinds the listed primitives to it.
    #[must_use]
    pub fn add_and_remap(
        mut self,
        material: Material,
        targets: impl IntoIterator<Item = MeshPrimitiveRef>,
    ) -> Self {
        self.ops.push(ModelMaterialPolicyOp::AddAndRemap {
            material,
            targets: targets.into_iter().collect(),
        });
        self
    }

    /// Convenience for [`Self::add_and_remap`] against primitive `0`.
    #[must_use]
    pub fn add_and_remap_meshes(
        self,
        material: Material,
        meshes: impl IntoIterator<Item = usize>,
    ) -> Self {
        self.add_and_remap(material, meshes.into_iter().map(MeshPrimitiveRef::mesh))
    }

    /// Appends a synthetic material and rebinds every user of `from_name` to it.
    #[must_use]
    pub fn add_and_remap_users_of_named(
        mut self,
        from_name: impl Into<String>,
        material: Material,
    ) -> Self {
        self.ops
            .push(ModelMaterialPolicyOp::AddAndRemapUsersOfNamed {
                from_name: from_name.into(),
                material,
            });
        self
    }

    /// Appends a synthetic material and rebinds every primitive of the listed
    /// source mesh names to it.
    #[must_use]
    pub fn add_and_remap_named_meshes(
        mut self,
        material: Material,
        mesh_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.ops
            .push(ModelMaterialPolicyOp::AddAndRemapNamedMeshes {
                material,
                mesh_names: mesh_names.into_iter().map(Into::into).collect(),
            });
        self
    }

    /// Assigns an explicit fallback material to every unbound primitive.
    ///
    /// The fallback is appended once when at least one primitive lacks a
    /// material binding. Absence of this setting leaves unbound primitives
    /// unchanged and is reported only through importer diagnostics.
    #[must_use]
    pub fn with_unbound_primitive_fallback(mut self, material: Material) -> Self {
        self.unbound_primitive_fallback = Some(material);
        self
    }

    /// Returns whether the policy contains any operation or fallback.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.unbound_primitive_fallback.is_none()
    }

    /// Applies the policy to `model`, returning structured diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMaterialPolicyError`] without mutating `model` when a
    /// named material is missing or a mesh/primitive/texture reference is
    /// invalid.
    pub fn apply(
        &self,
        model: &mut Model,
    ) -> Result<ModelMaterialPolicyReport, ModelMaterialPolicyError> {
        let mut working = model.clone();
        let report = apply_in_place(self, &mut working)?;
        *model = working;
        Ok(report)
    }
}

/// Diagnostics produced by a successful [`ModelMaterialPolicy::apply`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelMaterialPolicyReport {
    diagnostics: Vec<ImportDiagnostic>,
}

impl ModelMaterialPolicyReport {
    /// Structured non-fatal diagnostics in apply order.
    #[must_use]
    pub fn diagnostics(&self) -> &[ImportDiagnostic] {
        &self.diagnostics
    }

    /// Consumes the report and returns owned diagnostics.
    #[must_use]
    pub fn into_diagnostics(self) -> Vec<ImportDiagnostic> {
        self.diagnostics
    }
}

/// Failure while applying a material policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelMaterialPolicyError {
    /// A named material required by the policy is absent.
    MissingNamedMaterial {
        /// Requested source/debug material name.
        name: String,
    },
    /// A named mesh required by the policy is absent.
    MissingNamedMesh {
        /// Requested source/debug mesh name.
        name: String,
    },
    /// An underlying validated material edit failed.
    Edit(ModelMaterialEditError),
}

impl fmt::Display for ModelMaterialPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNamedMaterial { name } => {
                write!(
                    formatter,
                    "material policy requires missing material `{name}`"
                )
            }
            Self::MissingNamedMesh { name } => {
                write!(formatter, "material policy requires missing mesh `{name}`")
            }
            Self::Edit(error) => write!(formatter, "material policy edit failed: {error}"),
        }
    }
}

impl Error for ModelMaterialPolicyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Edit(error) => Some(error),
            Self::MissingNamedMaterial { .. } | Self::MissingNamedMesh { .. } => None,
        }
    }
}

impl From<ModelMaterialEditError> for ModelMaterialPolicyError {
    fn from(value: ModelMaterialEditError) -> Self {
        Self::Edit(value)
    }
}

fn apply_in_place(
    policy: &ModelMaterialPolicy,
    model: &mut Model,
) -> Result<ModelMaterialPolicyReport, ModelMaterialPolicyError> {
    let mut diagnostics = Vec::new();
    for op in &policy.ops {
        apply_policy_op(op, model, &mut diagnostics)?;
    }
    apply_unbound_fallback(policy, model, &mut diagnostics)?;
    Ok(ModelMaterialPolicyReport { diagnostics })
}

fn apply_policy_op(
    op: &ModelMaterialPolicyOp,
    model: &mut Model,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    match op {
        ModelMaterialPolicyOp::PatchNamed {
            name,
            patch,
            optional,
        } => apply_patch_named(model, name, patch, *optional, diagnostics),
        ModelMaterialPolicyOp::RemapToNamed {
            targets,
            material_name,
        } => apply_remap_to_named(model, targets, material_name, diagnostics),
        ModelMaterialPolicyOp::RemapUsersOfNamed { from_name, to_name } => {
            apply_remap_users_of_named(model, from_name, to_name, diagnostics)
        }
        ModelMaterialPolicyOp::RemapNamedMeshesToNamed {
            mesh_names,
            material_name,
        } => apply_remap_named_meshes_to_named(model, mesh_names, material_name, diagnostics),
        ModelMaterialPolicyOp::AddAndRemap { material, targets } => {
            apply_add_and_remap(model, material, targets, diagnostics)
        }
        ModelMaterialPolicyOp::AddAndRemapUsersOfNamed {
            from_name,
            material,
        } => apply_add_and_remap_users_of_named(model, from_name, material, diagnostics),
        ModelMaterialPolicyOp::AddAndRemapNamedMeshes {
            material,
            mesh_names,
        } => apply_add_and_remap_named_meshes(model, material, mesh_names, diagnostics),
    }
}

fn apply_patch_named(
    model: &mut Model,
    name: &str,
    patch: &MaterialFactorPatch,
    optional: bool,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    match material_index_by_name(model, name) {
        Ok(index) => {
            let before = model.materials()[index.get()].clone();
            let after = patch.apply_to(before);
            model.replace_material(index, after)?;
            diagnostics.push(ImportDiagnostic {
                code: "material-policy-patched".to_owned(),
                message: format!("patched material `{name}` factors via explicit policy"),
                severity: ImportDiagnosticSeverity::Info,
            });
            Ok(())
        }
        Err(ModelMaterialPolicyError::MissingNamedMaterial { name }) if optional => {
            diagnostics.push(ImportDiagnostic {
                code: "material-policy-patch-skipped".to_owned(),
                message: format!(
                    "optional material patch skipped: `{name}` not present in import"
                ),
                severity: ImportDiagnosticSeverity::Warning,
            });
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn apply_remap_to_named(
    model: &mut Model,
    targets: &[MeshPrimitiveRef],
    material_name: &str,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let index = material_index_by_name(model, material_name)?;
    for target in targets {
        model.set_primitive_material(target.mesh, target.primitive, index)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-remapped".to_owned(),
        message: format!(
            "remapped {} primitive(s) to material `{material_name}`",
            targets.len()
        ),
        severity: ImportDiagnosticSeverity::Info,
    });
    Ok(())
}

fn apply_remap_users_of_named(
    model: &mut Model,
    from_name: &str,
    to_name: &str,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let from = material_index_by_name(model, from_name)?;
    let to = material_index_by_name(model, to_name)?;
    let targets = primitives_using_material(model, from);
    for target in &targets {
        model.set_primitive_material(target.mesh, target.primitive, to)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-remapped-users".to_owned(),
        message: format!(
            "remapped {} primitive(s) from `{from_name}` to `{to_name}`",
            targets.len()
        ),
        severity: ImportDiagnosticSeverity::Info,
    });
    Ok(())
}

fn apply_remap_named_meshes_to_named(
    model: &mut Model,
    mesh_names: &[String],
    material_name: &str,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let index = material_index_by_name(model, material_name)?;
    let targets = primitives_of_named_meshes(model, mesh_names)?;
    for target in &targets {
        model.set_primitive_material(target.mesh, target.primitive, index)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-remapped-named-meshes".to_owned(),
        message: format!(
            "remapped {} primitive(s) on {} named mesh(es) to material `{material_name}`",
            targets.len(),
            mesh_names.len()
        ),
        severity: ImportDiagnosticSeverity::Info,
    });
    Ok(())
}

fn apply_add_and_remap(
    model: &mut Model,
    material: &Material,
    targets: &[MeshPrimitiveRef],
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let name = material
        .name()
        .unwrap_or("unnamed-policy-material")
        .to_owned();
    let index = model.add_material(material.clone())?;
    for target in targets {
        model.set_primitive_material(target.mesh, target.primitive, index)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-added".to_owned(),
        message: format!(
            "added material `{name}` and remapped {} primitive(s)",
            targets.len()
        ),
        severity: ImportDiagnosticSeverity::Info,
    });
    Ok(())
}

fn apply_add_and_remap_users_of_named(
    model: &mut Model,
    from_name: &str,
    material: &Material,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let from = material_index_by_name(model, from_name)?;
    let targets = primitives_using_material(model, from);
    let name = material
        .name()
        .unwrap_or("unnamed-policy-material")
        .to_owned();
    let index = model.add_material(material.clone())?;
    for target in &targets {
        model.set_primitive_material(target.mesh, target.primitive, index)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-added-users".to_owned(),
        message: format!(
            "added material `{name}` and remapped {} user(s) of `{from_name}`",
            targets.len()
        ),
        severity: ImportDiagnosticSeverity::Info,
    });
    Ok(())
}

fn apply_add_and_remap_named_meshes(
    model: &mut Model,
    material: &Material,
    mesh_names: &[String],
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let targets = primitives_of_named_meshes(model, mesh_names)?;
    let name = material
        .name()
        .unwrap_or("unnamed-policy-material")
        .to_owned();
    let index = model.add_material(material.clone())?;
    for target in &targets {
        model.set_primitive_material(target.mesh, target.primitive, index)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-added-named-meshes".to_owned(),
        message: format!(
            "added material `{name}` and remapped {} primitive(s) on {} named mesh(es)",
            targets.len(),
            mesh_names.len()
        ),
        severity: ImportDiagnosticSeverity::Info,
    });
    Ok(())
}

fn apply_unbound_fallback(
    policy: &ModelMaterialPolicy,
    model: &mut Model,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<(), ModelMaterialPolicyError> {
    let Some(fallback) = &policy.unbound_primitive_fallback else {
        return Ok(());
    };
    let unbound = unbound_primitives(model);
    if unbound.is_empty() {
        return Ok(());
    }
    let name = fallback
        .name()
        .unwrap_or("unnamed-unbound-fallback")
        .to_owned();
    let index = model.add_material(fallback.clone())?;
    for target in &unbound {
        model.set_primitive_material(target.mesh, target.primitive, index)?;
    }
    diagnostics.push(ImportDiagnostic {
        code: "material-policy-unbound-fallback".to_owned(),
        message: format!(
            "assigned explicit fallback `{name}` to {} unbound primitive(s)",
            unbound.len()
        ),
        severity: ImportDiagnosticSeverity::Warning,
    });
    Ok(())
}

fn material_index_by_name(
    model: &Model,
    name: &str,
) -> Result<MaterialIndex, ModelMaterialPolicyError> {
    model
        .materials()
        .iter()
        .position(|material| material.name() == Some(name))
        .map(MaterialIndex::new)
        .ok_or_else(|| ModelMaterialPolicyError::MissingNamedMaterial {
            name: name.to_owned(),
        })
}

fn primitives_of_named_meshes(
    model: &Model,
    mesh_names: &[String],
) -> Result<Vec<MeshPrimitiveRef>, ModelMaterialPolicyError> {
    let mut targets = Vec::new();
    for name in mesh_names {
        let mesh = model
            .meshes()
            .iter()
            .position(|mesh| mesh.name() == Some(name.as_str()))
            .ok_or_else(|| ModelMaterialPolicyError::MissingNamedMesh { name: name.clone() })?;
        for primitive in 0..model.meshes()[mesh].primitives().len() {
            targets.push(MeshPrimitiveRef { mesh, primitive });
        }
    }
    Ok(targets)
}

fn primitives_using_material(model: &Model, material: MaterialIndex) -> Vec<MeshPrimitiveRef> {
    let mut users = Vec::new();
    for (mesh, mesh_value) in model.meshes().iter().enumerate() {
        for (primitive, primitive_value) in mesh_value.primitives().iter().enumerate() {
            if primitive_value.material() == Some(material) {
                users.push(MeshPrimitiveRef { mesh, primitive });
            }
        }
    }
    users
}

fn unbound_primitives(model: &Model) -> Vec<MeshPrimitiveRef> {
    let mut unbound = Vec::new();
    for (mesh, mesh_value) in model.meshes().iter().enumerate() {
        for (primitive, primitive_value) in mesh_value.primitives().iter().enumerate() {
            if primitive_value.material().is_none() {
                unbound.push(MeshPrimitiveRef { mesh, primitive });
            }
        }
    }
    unbound
}

fn material_has_textures(material: &Material) -> bool {
    material.base_color_texture().is_some()
        || material.normal_texture().is_some()
        || material.metallic_roughness_texture().is_some()
        || material.emissive_texture().is_some()
        || material.specular_glossiness().is_some_and(|workflow| {
            workflow.diffuse_texture().is_some() || workflow.specular_glossiness_texture().is_some()
        })
}

/// One material slot and the primitives currently bound to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelMaterialUsageEntry {
    /// Material slot index.
    pub index: MaterialIndex,
    /// Optional source/debug name.
    pub name: Option<String>,
    /// Whether any texture map is bound.
    pub has_textures: bool,
    /// Mesh/primitive users in source order.
    pub users: Vec<MeshPrimitiveRef>,
}

/// High-level inventory of material bindings on a [`Model`].
///
/// Use this to answer “which meshes still use `material_0`?” without walking
/// low-level mesh storage by hand.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelMaterialUsage {
    materials: Vec<ModelMaterialUsageEntry>,
    unbound: Vec<MeshPrimitiveRef>,
}

impl ModelMaterialUsage {
    /// Per-material usage rows in material-index order.
    #[must_use]
    pub fn materials(&self) -> &[ModelMaterialUsageEntry] {
        &self.materials
    }

    /// Primitives with no material binding.
    #[must_use]
    pub fn unbound(&self) -> &[MeshPrimitiveRef] {
        &self.unbound
    }

    /// Returns users of the first material with the given name.
    #[must_use]
    pub fn users_of_named(&self, name: &str) -> Option<&[MeshPrimitiveRef]> {
        self.materials
            .iter()
            .find(|entry| entry.name.as_deref() == Some(name))
            .map(|entry| entry.users.as_slice())
    }

    /// Formats a stable multi-line summary for logs and demos.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for entry in &self.materials {
            let label = entry
                .name
                .as_deref()
                .map_or_else(|| format!("#{}", entry.index.get()), ToOwned::to_owned);
            let textures = if entry.has_textures {
                "textured"
            } else {
                "factor-only"
            };
            let _ = writeln!(
                out,
                "material `{label}` ({textures}): {} primitive(s)",
                entry.users.len()
            );
            for user in &entry.users {
                let _ = writeln!(out, "  - mesh {} primitive {}", user.mesh, user.primitive);
            }
        }
        if !self.unbound.is_empty() {
            let _ = writeln!(out, "unbound: {} primitive(s)", self.unbound.len());
            for user in &self.unbound {
                let _ = writeln!(out, "  - mesh {} primitive {}", user.mesh, user.primitive);
            }
        }
        out
    }
}

impl Model {
    /// Builds a high-level material usage inventory for diagnostics and tools.
    #[must_use]
    pub fn material_usage(&self) -> ModelMaterialUsage {
        let mut materials = Vec::with_capacity(self.materials().len());
        for (index, material) in self.materials().iter().enumerate() {
            let index = MaterialIndex::new(index);
            materials.push(ModelMaterialUsageEntry {
                index,
                name: material.name().map(ToOwned::to_owned),
                has_textures: material_has_textures(material),
                users: primitives_using_material(self, index),
            });
        }
        ModelMaterialUsage {
            materials,
            unbound: unbound_primitives(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mesh, MeshPrimitive};

    fn sample_model() -> Model {
        let prim_a = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("triangle")
            .with_material(MaterialIndex::new(0));
        let prim_b = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2]).expect("triangle");
        let mesh_a = Mesh::new(Some("a".to_owned()), vec![prim_a]).expect("mesh");
        let mesh_b = Mesh::new(Some("b".to_owned()), vec![prim_b]).expect("mesh");
        Model::new(
            vec![mesh_a, mesh_b],
            vec![
                Material::new()
                    .with_name("material_0")
                    .with_base_color_factor([1.0, 1.0, 1.0, 1.0]),
                Material::new().with_name("metal_gray"),
            ],
            Vec::new(),
        )
        .expect("valid model")
    }

    #[test]
    fn policy_forces_body_mat_single_sided_against_yaw_zfight() {
        let prim = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("triangle")
            .with_material(MaterialIndex::new(0));
        let mesh = Mesh::new(Some("body".to_owned()), vec![prim]).expect("mesh");
        let mut model = Model::new(
            vec![mesh],
            vec![
                Material::new()
                    .with_name("body_mat")
                    .with_double_sided(true),
            ],
            Vec::new(),
        )
        .expect("model");
        assert!(model.materials()[0].double_sided());
        ModelMaterialPolicy::new()
            .patch_named(
                "body_mat",
                MaterialFactorPatch::new().with_double_sided(false),
            )
            .apply(&mut model)
            .expect("policy");
        assert!(
            !model.materials()[0].double_sided(),
            "body_mat must be single-sided so yaw cannot z-fight UV islands"
        );
    }

    #[test]
    fn policy_patches_remaps_and_assigns_unbound_fallback_atomically() {
        let mut model = sample_model();
        let policy = ModelMaterialPolicy::new()
            .patch_named(
                "material_0",
                MaterialFactorPatch::new()
                    .with_base_color_factor([0.04, 0.06, 0.10, 1.0])
                    .with_metallic_roughness(0.05, 0.82)
                    .with_double_sided(true),
            )
            .remap_meshes_to_named([0], "metal_gray")
            .add_and_remap_meshes(
                Material::new()
                    .with_name("yuyib.recovered_red_neon")
                    .with_emissive_factor([2.4, 0.03, 0.12]),
                [0],
            )
            .with_unbound_primitive_fallback(
                Material::new()
                    .with_name("yuyib.unbound_fallback")
                    .with_base_color_factor([0.2, 0.2, 0.2, 1.0]),
            );

        let report = policy.apply(&mut model).expect("policy applies");
        assert_eq!(
            model.materials()[0].base_color_factor(),
            [0.04, 0.06, 0.10, 1.0]
        );
        assert!(model.materials()[0].double_sided());
        assert_eq!(
            model.meshes()[0].primitives()[0].material(),
            Some(MaterialIndex::new(2))
        );
        assert_eq!(
            model.meshes()[1].primitives()[0].material(),
            Some(MaterialIndex::new(3))
        );
        assert!(report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "material-policy-unbound-fallback"));
    }

    #[test]
    fn optional_named_patch_skips_missing_material() {
        let mut model = sample_model();
        let before = model.clone();
        let report = ModelMaterialPolicy::new()
            .patch_named_optional(
                "missing_quirk",
                MaterialFactorPatch::new().with_metallic_roughness(0.0, 0.5),
            )
            .patch_named(
                "material_0",
                MaterialFactorPatch::new().with_metallic_roughness(0.1, 0.9),
            )
            .apply(&mut model)
            .expect("optional miss must not fail policy");
        assert_ne!(
            model.materials()[0].metallic_factor(),
            before.materials()[0].metallic_factor()
        );
        assert!(report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "material-policy-patch-skipped"));
    }

    #[test]
    fn missing_named_material_leaves_model_unchanged() {
        let mut model = sample_model();
        let before = model.clone();
        let error = ModelMaterialPolicy::new()
            .remap_meshes_to_named([0], "missing")
            .apply(&mut model)
            .expect_err("missing name fails");
        assert_eq!(
            error,
            ModelMaterialPolicyError::MissingNamedMaterial {
                name: "missing".to_owned()
            }
        );
        assert_eq!(model, before);
    }

    #[test]
    fn named_mesh_remaps_prefer_labels_over_indices() {
        let mut model = sample_model();
        let report = ModelMaterialPolicy::new()
            .remap_named_meshes_to_named(["a"], "metal_gray")
            .add_and_remap_named_meshes(
                Material::new()
                    .with_name("yuyib.named_neon")
                    .with_emissive_factor([1.0, 0.2, 0.1]),
                ["b"],
            )
            .apply(&mut model)
            .expect("named mesh policy applies");
        assert_eq!(
            model.meshes()[0].primitives()[0].material(),
            Some(MaterialIndex::new(1))
        );
        assert_eq!(
            model.meshes()[1].primitives()[0].material(),
            Some(MaterialIndex::new(2))
        );
        assert!(report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "material-policy-remapped-named-meshes"));
        assert!(report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "material-policy-added-named-meshes"));

        let before = model.clone();
        let error = ModelMaterialPolicy::new()
            .remap_named_meshes_to_named(["missing_mesh"], "metal_gray")
            .apply(&mut model)
            .expect_err("missing mesh name fails");
        assert_eq!(
            error,
            ModelMaterialPolicyError::MissingNamedMesh {
                name: "missing_mesh".to_owned()
            }
        );
        assert_eq!(model, before);
    }

    #[test]
    fn material_usage_and_remap_users_of_named_are_high_level() {
        let prim_a = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("triangle")
            .with_material(MaterialIndex::new(0));
        let prim_b = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("triangle")
            .with_material(MaterialIndex::new(0));
        let prim_c = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("triangle")
            .with_material(MaterialIndex::new(1));
        let mut model = Model::new(
            vec![
                Mesh::new(Some("a".to_owned()), vec![prim_a]).expect("mesh"),
                Mesh::new(Some("b".to_owned()), vec![prim_b]).expect("mesh"),
                Mesh::new(Some("c".to_owned()), vec![prim_c]).expect("mesh"),
            ],
            vec![
                Material::new().with_name("material_0"),
                Material::new().with_name("metal_gray"),
            ],
            Vec::new(),
        )
        .expect("valid model");

        let usage = model.material_usage();
        assert_eq!(usage.users_of_named("material_0").map(<[_]>::len), Some(2));
        assert!(usage.summary().contains("material `material_0`"));

        let report = ModelMaterialPolicy::new()
            .add_and_remap_users_of_named(
                "material_0",
                Material::new()
                    .with_name("yuyib.demo_neon")
                    .with_emissive_factor([2.0, 0.1, 0.1]),
            )
            .apply(&mut model)
            .expect("remap users");
        assert_eq!(
            model.meshes()[0].primitives()[0].material(),
            Some(MaterialIndex::new(2))
        );
        assert_eq!(
            model.meshes()[1].primitives()[0].material(),
            Some(MaterialIndex::new(2))
        );
        assert_eq!(
            model.meshes()[2].primitives()[0].material(),
            Some(MaterialIndex::new(1))
        );
        assert!(report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "material-policy-added-users"));
        assert_eq!(
            model
                .material_usage()
                .users_of_named("material_0")
                .map(<[_]>::len),
            Some(0)
        );
    }
}
