//! Safe Source 1 VMT `$basetexture` resolution without GPU upload.
//!
//! [`Source1MaterialResolver`] canonicalizes declared `materials` and texture
//! roots, accepts a parsed [`VmtMaterial`] or a local `.vmt` relative path, and
//! resolves/decodes one RGBA8 VTF base texture. It rejects absolute paths,
//! URI-like references, traversal and canonical symlink escapes.
//!
//! This boundary intentionally has no VPK support, bump/PBR binding, Source 2
//! parsing, cache, or GPU resource ownership. `Patch` VMTs are followed for
//! `$basetexture`: an included material is loaded first, then `insert` and
//! `replace` properties from the patch override it.

#![forbid(unsafe_code)]

use std::{
    error::Error,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use yuyib_vmt::{VmtBlock, VmtMaterial, parse};
use yuyib_vtf::{VtfError, VtfHighResFormat, decode};

/// Decoded Source 1 base texture ready for a later RGBA8 GPU uploader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source1BaseTexture {
    /// Canonical local VTF path under the declared texture root.
    pub path: PathBuf,
    /// Pixel width.
    pub width: u16,
    /// Pixel height.
    pub height: u16,
    /// Original VTF high-resolution format before conversion to RGBA8.
    pub source_format: VtfHighResFormat,
    /// Tightly packed RGBA8 pixels in row-major order.
    pub rgba8: Vec<u8>,
}

/// Authored base-texture references resolved from one VMT include chain.
///
/// `second` is populated by terrain shaders such as
/// `WorldVertexTransition`; its blend factor comes from BSP displacement
/// vertex alpha rather than from the VTF image alpha channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source1MaterialTextureReferences {
    /// Texture selected at a blend weight of zero.
    pub first: String,
    /// Optional texture selected at a blend weight of one.
    pub second: Option<String>,
}

/// Canonical-root Source 1 material resolver.
#[derive(Clone, Debug)]
pub struct Source1MaterialResolver {
    materials_root: PathBuf,
    texture_root: PathBuf,
}

impl Source1MaterialResolver {
    /// Canonicalizes two existing directory roots.
    ///
    /// # Errors
    ///
    /// Returns [`Source1AssetError::Root`] when a declared root is absent or not a directory.
    pub fn new(
        materials_root: impl AsRef<Path>,
        texture_root: impl AsRef<Path>,
    ) -> Result<Self, Source1AssetError> {
        Ok(Self {
            materials_root: canonical_directory(materials_root.as_ref(), "materials")?,
            texture_root: canonical_directory(texture_root.as_ref(), "textures")?,
        })
    }

    /// Resolves a parsed VMT's `$basetexture` and decodes its VTF payload.
    ///
    /// # Errors
    ///
    /// Returns structured path, I/O and VTF decode errors. A VMT with no base texture is not silently white.
    pub fn resolve(&self, material: &VmtMaterial) -> Result<Source1BaseTexture, Source1AssetError> {
        let authored = material
            .base_texture()
            .ok_or(Source1AssetError::MissingBaseTexture)?;
        let path = self.resolve_texture_path(authored)?;
        let bytes = fs::read(&path).map_err(|source| Source1AssetError::Read {
            path: path.clone(),
            source,
        })?;
        let image = decode(&bytes).map_err(Source1AssetError::Vtf)?;
        Ok(Source1BaseTexture {
            path,
            width: image.width(),
            height: image.height(),
            source_format: image.source_format(),
            rgba8: image.pixels_rgba8().to_vec(),
        })
    }

    /// Reads and parses one `.vmt` path relative to the declared materials root, then resolves it.
    ///
    /// # Errors
    ///
    /// Returns [`Source1AssetError`] for unsafe paths, I/O, UTF-8, VMT parsing or VTF decoding.
    pub fn resolve_vmt_path(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<Source1BaseTexture, Source1AssetError> {
        let path = self.resolve_material_path(relative_path.as_ref())?;
        self.resolve_vmt_file(&path, 0)
    }

    /// Reads a loose VMT and resolves its authored base-texture references.
    ///
    /// Patch includes are followed with the same bounded, root-confined policy
    /// as [`Self::resolve_vmt_path`]. Both `$basetexture` and
    /// `$basetexture2` overrides from `insert`/`replace` blocks are honoured.
    ///
    /// # Errors
    ///
    /// Returns [`Source1AssetError`] for unsafe paths, I/O, UTF-8/VMT parsing,
    /// missing `$basetexture`, or an excessive Patch include chain.
    pub fn resolve_vmt_texture_references(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<Source1MaterialTextureReferences, Source1AssetError> {
        let path = self.resolve_material_path(relative_path.as_ref())?;
        self.resolve_vmt_references_file(&path, 0)
    }

    /// Resolves and decodes one authored `$basetexture` reference directly.
    ///
    /// This is used by container formats such as BSP when the VMT is embedded
    /// in the container but its referenced VTF is supplied by the declared
    /// external texture root.
    ///
    /// # Errors
    ///
    /// Returns structured path, I/O or VTF decode failures.
    pub fn resolve_texture_reference(
        &self,
        authored: &str,
    ) -> Result<Source1BaseTexture, Source1AssetError> {
        self.resolve_texture_authored(authored)
    }

    fn resolve_vmt_file(
        &self,
        path: &Path,
        patch_depth: usize,
    ) -> Result<Source1BaseTexture, Source1AssetError> {
        if patch_depth >= 16 {
            return Err(Source1AssetError::PatchDepthExceeded);
        }
        let text = fs::read_to_string(&path).map_err(|source| Source1AssetError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let material = parse(&text).map_err(Source1AssetError::Vmt)?;
        if material.shader().eq_ignore_ascii_case("patch") {
            let included = material
                .block()
                .property("include")
                .ok_or(Source1AssetError::PatchMissingInclude)?;
            let included_path = self.resolve_material_path(Path::new(included))?;
            if let Some(base_texture) = patch_base_texture(material.block()) {
                return self.resolve_texture_authored(base_texture);
            }
            return self.resolve_vmt_file(&included_path, patch_depth + 1);
        }
        self.resolve(&material)
    }

    fn resolve_vmt_references_file(
        &self,
        path: &Path,
        patch_depth: usize,
    ) -> Result<Source1MaterialTextureReferences, Source1AssetError> {
        if patch_depth >= 16 {
            return Err(Source1AssetError::PatchDepthExceeded);
        }
        let text = fs::read_to_string(path).map_err(|source| Source1AssetError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let material = parse(&text).map_err(Source1AssetError::Vmt)?;
        if material.shader().eq_ignore_ascii_case("patch") {
            let included = material
                .block()
                .property("include")
                .ok_or(Source1AssetError::PatchMissingInclude)?;
            let included_path = self.resolve_material_path(Path::new(included))?;
            let mut references =
                self.resolve_vmt_references_file(&included_path, patch_depth + 1)?;
            if let Some(first) = patch_texture_property(material.block(), "$basetexture") {
                references.first = first.to_owned();
            }
            if let Some(second) = patch_texture_property(material.block(), "$basetexture2") {
                references.second = Some(second.to_owned());
            }
            return Ok(references);
        }
        Ok(Source1MaterialTextureReferences {
            first: material
                .base_texture()
                .ok_or(Source1AssetError::MissingBaseTexture)?
                .to_owned(),
            second: material.base_texture2().map(str::to_owned),
        })
    }

    fn resolve_material_path(&self, relative: &Path) -> Result<PathBuf, Source1AssetError> {
        let relative = strip_materials_prefix(relative);
        let mut relative = relative.to_path_buf();
        if relative.extension().is_none() {
            relative.set_extension("vmt");
        }
        let path = resolve_local(&self.materials_root, &relative, "VMT")?;
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("vmt"))
        {
            return Err(Source1AssetError::WrongExtension {
                path,
                expected: "vmt",
            });
        }
        Ok(path)
    }

    fn resolve_texture_path(&self, authored: &str) -> Result<PathBuf, Source1AssetError> {
        self.resolve_texture_path_value(authored)
    }

    fn resolve_texture_authored(
        &self,
        authored: &str,
    ) -> Result<Source1BaseTexture, Source1AssetError> {
        let path = self.resolve_texture_path_value(authored)?;
        let bytes = fs::read(&path).map_err(|source| Source1AssetError::Read {
            path: path.clone(),
            source,
        })?;
        let image = decode(&bytes).map_err(Source1AssetError::Vtf)?;
        Ok(Source1BaseTexture {
            path,
            width: image.width(),
            height: image.height(),
            source_format: image.source_format(),
            rgba8: image.pixels_rgba8().to_vec(),
        })
    }

    fn resolve_texture_path_value(&self, authored: &str) -> Result<PathBuf, Source1AssetError> {
        if authored.contains("://") {
            return Err(Source1AssetError::UnsafePath {
                value: authored.into(),
            });
        }
        let mut relative =
            strip_materials_prefix(Path::new(&authored.replace('\\', "/"))).to_path_buf();
        if relative.extension().is_none() {
            relative.set_extension("vtf");
        }
        let path = resolve_local(&self.texture_root, &relative, "$basetexture")?;
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("vtf"))
        {
            return Err(Source1AssetError::WrongExtension {
                path,
                expected: "vtf",
            });
        }
        Ok(path)
    }
}

fn patch_base_texture(block: &VmtBlock) -> Option<&str> {
    for block in block.blocks() {
        if block.name().eq_ignore_ascii_case("replace")
            || block.name().eq_ignore_ascii_case("insert")
        {
            if let Some(texture) = block.property("$basetexture") {
                return Some(texture);
            }
        }
    }
    None
}

fn patch_texture_property<'a>(block: &'a VmtBlock, property: &str) -> Option<&'a str> {
    block
        .blocks()
        .iter()
        .filter(|block| {
            block.name().eq_ignore_ascii_case("replace")
                || block.name().eq_ignore_ascii_case("insert")
        })
        .find_map(|block| block.property(property))
}

fn strip_materials_prefix(path: &Path) -> &Path {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return path;
    };
    if first.to_string_lossy().eq_ignore_ascii_case("materials") {
        components.as_path()
    } else {
        path
    }
}

fn canonical_directory(path: &Path, label: &'static str) -> Result<PathBuf, Source1AssetError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| Source1AssetError::Root { label, source })?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(Source1AssetError::RootNotDirectory { label })
    }
}

fn resolve_local(
    root: &Path,
    relative: &Path,
    label: &'static str,
) -> Result<PathBuf, Source1AssetError> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Source1AssetError::UnsafePath {
            value: relative.display().to_string(),
        });
    }
    let candidate = root.join(relative);
    let canonical = fs::canonicalize(&candidate).map_err(|source| Source1AssetError::Read {
        path: candidate,
        source,
    })?;
    if canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(Source1AssetError::EscapesRoot {
            label,
            path: canonical,
        })
    }
}

/// Source1 resolver failure.
#[derive(Debug)]
pub enum Source1AssetError {
    /// A declared root could not be canonicalized.
    Root {
        /// Human-readable root role.
        label: &'static str,
        /// Underlying canonicalization failure.
        source: std::io::Error,
    },
    /// A declared root was not a directory.
    RootNotDirectory {
        /// Human-readable root role.
        label: &'static str,
    },
    /// Authored input was absolute, URI-like or used traversal.
    UnsafePath {
        /// Rejected authored path.
        value: String,
    },
    /// A path canonicalized outside its declared root.
    EscapesRoot {
        /// Human-readable root role.
        label: &'static str,
        /// Canonical path outside that root.
        path: PathBuf,
    },
    /// File operation failed.
    Read {
        /// Attempted local path.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// A local path had an unexpected extension.
    WrongExtension {
        /// Resolved local path.
        path: PathBuf,
        /// Required extension without its dot.
        expected: &'static str,
    },
    /// VMT omitted `$basetexture`.
    MissingBaseTexture,
    /// VMT parse failed.
    Vmt(yuyib_vmt::VmtParseError),
    /// VTF decode failed.
    Vtf(VtfError),
    /// A Patch VMT did not name an included material.
    PatchMissingInclude,
    /// A Patch include chain was deeper than the bounded resolver permits.
    PatchDepthExceeded,
}
impl fmt::Display for Source1AssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root { label, source } => {
                write!(formatter, "cannot resolve {label} root: {source}")
            }
            Self::RootNotDirectory { label } => {
                write!(formatter, "{label} root is not a directory")
            }
            Self::UnsafePath { value } => write!(formatter, "unsafe Source1 asset path: {value}"),
            Self::EscapesRoot { label, path } => write!(
                formatter,
                "{label} path escapes declared root: {}",
                path.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::WrongExtension { path, expected } => {
                write!(formatter, "{} must use .{expected}", path.display())
            }
            Self::MissingBaseTexture => formatter.write_str("VMT has no $basetexture"),
            Self::Vmt(source) => write!(formatter, "cannot parse VMT: {source}"),
            Self::Vtf(source) => write!(formatter, "cannot decode VTF: {source}"),
            Self::PatchMissingInclude => formatter.write_str("Patch VMT has no include"),
            Self::PatchDepthExceeded => formatter.write_str("Patch VMT include depth exceeds 16"),
        }
    }
}
impl Error for Source1AssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Root { source, .. } | Self::Read { source, .. } => Some(source),
            Self::Vmt(source) => Some(source),
            Self::Vtf(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> Source1MaterialResolver {
        let root =
            std::env::temp_dir().join(format!("yuyib-source1-assets-{}", std::process::id()));
        let materials = root.join("materials");
        let textures = root.join("textures");
        fs::create_dir_all(&materials).expect("materials directory");
        fs::create_dir_all(&textures).expect("textures directory");
        Source1MaterialResolver::new(materials, textures).expect("canonical roots")
    }

    #[test]
    fn rejects_traversal_and_uri_base_textures() {
        let resolver = resolver();
        let traversal =
            parse("LightmappedGeneric { \"$basetexture\" \"../outside\" }").expect("VMT");
        assert!(matches!(
            resolver.resolve(&traversal),
            Err(Source1AssetError::UnsafePath { .. })
        ));
        let uri = parse("LightmappedGeneric { \"$basetexture\" \"https://example.invalid/a\" }")
            .expect("VMT");
        assert!(matches!(
            resolver.resolve(&uri),
            Err(Source1AssetError::UnsafePath { .. })
        ));
    }

    #[test]
    fn missing_base_texture_is_explicit() {
        let resolver = resolver();
        let material = parse("LightmappedGeneric { \"$surfaceprop\" \"metal\" }").expect("VMT");
        assert!(matches!(
            resolver.resolve(&material),
            Err(Source1AssetError::MissingBaseTexture)
        ));
    }

    #[test]
    fn loose_patch_retains_both_world_vertex_transition_textures() {
        let root = std::env::temp_dir().join(format!(
            "yuyib-source1-assets-references-{}",
            std::process::id()
        ));
        let materials = root.join("materials");
        let textures = root.join("textures");
        fs::create_dir_all(materials.join("terrain")).expect("materials directory");
        fs::create_dir_all(&textures).expect("textures directory");
        fs::write(
            materials.join("terrain/base.vmt"),
            r#"WorldVertexTransition {
                "$basetexture" "terrain/grass"
                "$basetexture2" "terrain/dirt"
            }"#,
        )
        .expect("base VMT");
        fs::write(
            materials.join("terrain/override.vmt"),
            r#"Patch {
                "include" "terrain/base"
                "replace" { "$basetexture2" "terrain/rock" }
            }"#,
        )
        .expect("patch VMT");
        let resolver =
            Source1MaterialResolver::new(&materials, &textures).expect("canonical roots");

        let references = resolver
            .resolve_vmt_texture_references("terrain/override")
            .expect("two texture references");

        assert_eq!(references.first, "terrain/grass");
        assert_eq!(references.second.as_deref(), Some("terrain/rock"));
    }
}
