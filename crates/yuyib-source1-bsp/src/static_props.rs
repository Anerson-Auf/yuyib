//! Static-prop instance and `StudioModel` sidecar asset resolution.

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use yuyib_bsp::{Bsp, BspError, BspStaticPropError, BspStaticPropLump};
use yuyib_source1::Source1StudioModelFiles;

/// Backward-compatible name for a BSP static prop's StudioModel sidecars.
pub type Source1StaticPropModelFiles = Source1StudioModelFiles;

/// Asset lookup policy for Source static-prop `StudioModel` files.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Source1StaticPropAssetOptions {
    /// Optional loose Source content root containing the `models` directory.
    /// Embedded BSP PAKFILE entries always take precedence.
    pub external_content_root: Option<PathBuf>,
}

/// Static-prop instances plus one resolved real model family per dictionary item.
#[derive(Clone, Debug, PartialEq)]
pub struct Source1StaticPropAssets {
    /// Typed instance dictionary and transforms from the BSP `sprp` lump.
    pub lump: BspStaticPropLump,
    /// MDL/VVD/VTX families in exactly the same order as `lump.model_names`.
    pub models: Vec<Source1StaticPropModelFiles>,
}

/// Failure while resolving real `StudioModel` files for static props.
#[derive(Debug)]
pub enum Source1StaticPropAssetError {
    /// The BSP static-prop directory/payload was malformed.
    StaticProps(BspStaticPropError),
    /// Embedded PAKFILE lookup failed.
    Pak(BspError),
    /// A model path could escape the configured loose content root.
    UnsafeModelPath {
        /// Rejected path from the BSP dictionary.
        path: String,
    },
    /// A required `StudioModel` sidecar is absent from both PAKFILE and loose content.
    MissingModelFile {
        /// Static-prop model owning the missing file.
        model: String,
        /// Sidecar kind (`MDL`, `VVD` or `VTX`).
        kind: &'static str,
        /// Canonical relative candidates checked in order.
        tried: Vec<String>,
    },
    /// A loose content file exists but could not be read.
    ExternalRead {
        /// Exact file that failed.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
}

impl fmt::Display for Source1StaticPropAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaticProps(source) => source.fmt(formatter),
            Self::Pak(source) => source.fmt(formatter),
            Self::UnsafeModelPath { path } => {
                write!(formatter, "unsafe Source static-prop model path {path:?}")
            }
            Self::MissingModelFile { model, kind, tried } => write!(
                formatter,
                "Source static-prop model {model} is missing {kind}; tried {}",
                tried.join(", ")
            ),
            Self::ExternalRead { path, source } => {
                write!(
                    formatter,
                    "cannot read Source model asset {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for Source1StaticPropAssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StaticProps(source) => Some(source),
            Self::Pak(source) => Some(source),
            Self::ExternalRead { source, .. } => Some(source),
            Self::UnsafeModelPath { .. } | Self::MissingModelFile { .. } => None,
        }
    }
}

impl From<BspStaticPropError> for Source1StaticPropAssetError {
    fn from(source: BspStaticPropError) -> Self {
        Self::StaticProps(source)
    }
}

/// Resolves the optional BSP static-prop lump and every referenced real model family.
///
/// Embedded files win over the optional loose Source content root. VTX lookup
/// prefers the DirectX 9 optimized mesh, then generic, DirectX 8 and software
/// variants. Missing sidecars are errors: this function never manufactures
/// placeholder geometry.
///
/// # Errors
///
/// Returns typed BSP/PAK, unsafe-path, missing-sidecar or filesystem failures.
pub fn load_static_prop_assets(
    bsp: &Bsp,
    options: &Source1StaticPropAssetOptions,
) -> Result<Option<Source1StaticPropAssets>, Source1StaticPropAssetError> {
    let Some(lump) = bsp.static_props()? else {
        return Ok(None);
    };
    let embedded = bsp
        .pak_files_by_extension(&["mdl", "vvd", "vtx"])
        .map_err(Source1StaticPropAssetError::Pak)?
        .into_iter()
        .map(|file| (normalize_archive(&file.path), Arc::from(file.bytes)))
        .collect::<HashMap<_, _>>();

    let mut models = Vec::with_capacity(lump.model_names.len());
    for model_path in &lump.model_names {
        let mdl_path = validate_relative(model_path)?;
        let base = mdl_path.strip_suffix(".mdl").unwrap_or(mdl_path.as_str());
        let vvd_path = format!("{base}.vvd");
        let vtx_candidates = [
            format!("{base}.dx90.vtx"),
            format!("{base}.vtx"),
            format!("{base}.dx80.vtx"),
            format!("{base}.sw.vtx"),
        ];
        let mdl = resolve_one(
            &mdl_path,
            model_path,
            "MDL",
            &embedded,
            options.external_content_root.as_deref(),
        )?;
        let vvd = resolve_one(
            &vvd_path,
            model_path,
            "VVD",
            &embedded,
            options.external_content_root.as_deref(),
        )?;
        let (vtx_path, vtx) = resolve_candidates(
            &vtx_candidates,
            model_path,
            "VTX",
            &embedded,
            options.external_content_root.as_deref(),
        )?;
        models.push(Source1StaticPropModelFiles {
            model_path: mdl_path,
            mdl,
            vvd,
            vtx,
            vtx_path,
        });
    }
    Ok(Some(Source1StaticPropAssets { lump, models }))
}

fn resolve_one(
    path: &str,
    model: &str,
    kind: &'static str,
    embedded: &HashMap<String, Arc<[u8]>>,
    root: Option<&Path>,
) -> Result<Arc<[u8]>, Source1StaticPropAssetError> {
    resolve_candidates(&[path.to_owned()], model, kind, embedded, root).map(|(_, bytes)| bytes)
}

fn resolve_candidates(
    candidates: &[String],
    model: &str,
    kind: &'static str,
    embedded: &HashMap<String, Arc<[u8]>>,
    root: Option<&Path>,
) -> Result<(String, Arc<[u8]>), Source1StaticPropAssetError> {
    for candidate in candidates {
        if let Some(bytes) = embedded.get(candidate) {
            return Ok((candidate.clone(), bytes.clone()));
        }
    }
    if let Some(root) = root {
        for candidate in candidates {
            let path = root.join(candidate.replace('/', std::path::MAIN_SEPARATOR_STR));
            match std::fs::read(&path) {
                Ok(bytes) => return Ok((candidate.clone(), Arc::from(bytes))),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Source1StaticPropAssetError::ExternalRead { path, source });
                }
            }
        }
    }
    Err(Source1StaticPropAssetError::MissingModelFile {
        model: model.to_owned(),
        kind,
        tried: candidates.to_vec(),
    })
}

fn validate_relative(path: &str) -> Result<String, Source1StaticPropAssetError> {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let candidate = Path::new(&normalized);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Source1StaticPropAssetError::UnsafeModelPath {
            path: path.to_owned(),
        });
    }
    Ok(normalized)
}

fn normalize_archive(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_model_paths_before_loose_lookup() {
        for path in ["../outside.mdl", "models/../outside.mdl", "C:/outside.mdl"] {
            assert!(matches!(
                validate_relative(path),
                Err(Source1StaticPropAssetError::UnsafeModelPath { .. })
            ));
        }
        assert_eq!(
            validate_relative(r"Models\Props\Tree.MDL").expect("safe model"),
            "models/props/tree.mdl"
        );
    }

    #[test]
    fn vtx_candidate_order_prefers_dx90() {
        let embedded = HashMap::from([
            ("models/tree.vtx".to_owned(), Arc::from([1_u8])),
            ("models/tree.dx90.vtx".to_owned(), Arc::from([9_u8])),
        ]);
        let candidates = [
            "models/tree.dx90.vtx".to_owned(),
            "models/tree.vtx".to_owned(),
        ];
        let (path, bytes) =
            resolve_candidates(&candidates, "models/tree.mdl", "VTX", &embedded, None)
                .expect("embedded VTX");
        assert_eq!(path, "models/tree.dx90.vtx");
        assert_eq!(bytes.as_ref(), [9]);
    }

    #[test]
    fn missing_assets_are_explicit() {
        assert!(matches!(
            resolve_one(
                "models/tree.vvd",
                "models/tree.mdl",
                "VVD",
                &HashMap::new(),
                None
            ),
            Err(Source1StaticPropAssetError::MissingModelFile { kind: "VVD", .. })
        ));
    }
}
