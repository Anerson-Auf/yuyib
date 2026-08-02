//! Development commands that keep the published Yuyib documentation coherent.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use yuyib_gltf::{ImportOptions, import_scene_path, import_scene_path_with_options};

const PUBLIC_CRATES: &[&str] = &[
    "yuyib",
    "yuyib-app",
    "yuyib-assets",
    "yuyib-audio",
    "yuyib-character-3d",
    "yuyib-core",
    "yuyib-ecs",
    "yuyib-game",
    "yuyib-game-2d",
    "yuyib-game-3d",
    "yuyib-gameplay",
    "yuyib-gltf",
    "yuyib-image",
    "yuyib-input",
    "yuyib-model",
    "yuyib-model-assets",
    "yuyib-net",
    "yuyib-physics",
    "yuyib-platform",
    "yuyib-render",
    "yuyib-render-2d",
    "yuyib-render-3d",
    "yuyib-render-texture",
    "yuyib-scene",
    "yuyib-shader",
    "yuyib-source1",
    "yuyib-source1-scene",
    "yuyib-tasks",
    "yuyib-vmf",
    "yuyib-vmf-model",
    "yuyib-vmt",
    "yuyib-vtf",
    "yuyib-ui",
    "yuyib-ui-render",
    "yuyib-ui-text",
    "yuyib-ui-text-render",
    "yuyib-webview",
    "yuyib-source1-assets",
    "yuyib-2d",
];

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("docs") => build_docs(),
        Some("gltf-fixtures") => audit_gltf_fixtures(arguments.next()),
        Some("help" | "--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(command) => Err(format!(
            "unknown xtask command `{command}`; run `cargo run -p xtask -- help`"
        )
        .into()),
    }
}

fn print_help() {
    println!(
        "Yuyib workspace commands:\n  cargo run -p xtask -- docs                     Build wiki and embedded Rust API reference.\n  cargo run -p xtask -- gltf-fixtures [path]    Audit known GLB fixtures or one exact path."
    );
}

fn audit_gltf_fixtures(path: Option<String>) -> Result<(), Box<dyn Error>> {
    let workspace = workspace_root()?;
    let paths = match path {
        Some(path) => vec![resolve_fixture_path(&workspace, Path::new(&path))],
        None => known_gltf_fixture_paths(&workspace)?,
    };
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-utf8-path>");
        match import_scene_path(&path) {
            Ok(asset) => print_strict_gltf_audit(name, &asset),
            Err(error) => {
                match import_scene_path_with_options(&path, ImportOptions::skeletal_preview()) {
                    Ok(asset) => println!(
                        "PREVIEW OK {name} strict_error={error:?} skipped_primitives={} morph_primitives={} morph_tracks={} meshes={} skins={} animations={}",
                        asset.report().skipped_primitive_count(),
                        asset.scene.morph_primitives().len(),
                        asset
                            .scene
                            .animations()
                            .iter()
                            .map(|clip| clip.morph_tracks().len())
                            .sum::<usize>(),
                        asset.model.meshes().len(),
                        asset.scene.skins().len(),
                        asset.scene.animations().len(),
                    ),
                    Err(preview_error) => {
                        println!("ERR {name} strict={error}; skeletal_preview={preview_error}");
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "A flat audit row keeps every material capability visible in CI output."
)]
fn print_strict_gltf_audit(name: &str, asset: &yuyib_gltf::ImportedAsset) {
    let primitives = asset
        .model
        .meshes()
        .iter()
        .map(|mesh| mesh.primitives().len())
        .sum::<usize>();
    let tangents = asset
        .model
        .meshes()
        .iter()
        .flat_map(yuyib_model::Mesh::primitives)
        .filter(|primitive| primitive.tangents().is_some())
        .count();
    let materials = asset.model.materials();
    let base_textures = materials
        .iter()
        .filter(|material| material.base_color_texture().is_some())
        .count();
    let normal_maps = materials
        .iter()
        .filter(|material| material.normal_texture().is_some())
        .count();
    let metallic_roughness_maps = materials
        .iter()
        .filter(|material| material.metallic_roughness_texture().is_some())
        .count();
    let emissive_maps = materials
        .iter()
        .filter(|material| material.emissive_texture().is_some())
        .count();
    let complete_pbr_texture_sets = materials
        .iter()
        .filter(|material| {
            material.base_color_texture().is_some()
                && material.normal_texture().is_some()
                && material.metallic_roughness_texture().is_some()
        })
        .count();
    let nonzero_emissive_without_map = materials
        .iter()
        .filter(|material| {
            material.emissive_texture().is_none()
                && material
                    .emissive_factor()
                    .iter()
                    .any(|value| value.abs() > f32::EPSILON)
        })
        .count();
    let non_opaque_materials = materials
        .iter()
        .filter(|material| material.alpha_mode() != yuyib_model::AlphaMode::Opaque)
        .count();
    let alpha_mask_materials = materials
        .iter()
        .filter(|material| matches!(material.alpha_mode(), yuyib_model::AlphaMode::Mask { .. }))
        .count();
    let alpha_blend_materials = materials
        .iter()
        .filter(|material| material.alpha_mode() == yuyib_model::AlphaMode::Blend)
        .count();
    println!(
        "OK {name} meshes={} primitives={primitives} tangents={tangents} materials={} base_textures={base_textures} normal_maps={normal_maps} metallic_roughness_maps={metallic_roughness_maps} emissive_maps={emissive_maps} complete_pbr_texture_sets={complete_pbr_texture_sets} nonzero_emissive_without_map={nonzero_emissive_without_map} non_opaque_materials={non_opaque_materials} alpha_mask_materials={alpha_mask_materials} alpha_blend_materials={alpha_blend_materials} scenes={} nodes={} cameras={} directional_lights={}",
        asset.model.meshes().len(),
        materials.len(),
        asset.scene.scenes().len(),
        asset.scene.nodes().len(),
        asset.scene.cameras().len(),
        asset.scene.directional_lights().len(),
    );
}

fn known_gltf_fixture_paths(workspace: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let fixture_root = workspace.join("for_tests");
    let mut paths = fs::read_dir(fixture_root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("glb"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn resolve_fixture_path(workspace: &Path, supplied: &Path) -> PathBuf {
    if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        workspace.join(supplied)
    }
}

fn build_docs() -> Result<(), Box<dyn Error>> {
    let workspace = workspace_root()?;
    let docs_site = workspace.join("docs/site");
    let api_target = workspace.join("target/docs-api");
    let site_output = docs_site.join("book");
    let published_api = site_output.join("api");

    remove_directory_if_present(&api_target)?;
    run_cargo_doc(&workspace, &api_target)?;
    let rustdoc_output = find_rustdoc_output(&api_target)?;
    run_command(Command::new("mdbook").arg("build").arg(&docs_site))?;

    remove_directory_if_present(&published_api)?;
    copy_directory_contents(&rustdoc_output, &published_api)?;
    ensure_generated_file(&published_api.join("yuyib/index.html"))?;
    ensure_generated_file(&published_api.join("yuyib_app/struct.ApplicationWebViewHandle.html"))?;

    println!(
        "Documentation built: {}",
        site_output.join("index.html").display()
    );
    println!(
        "Embedded Rust API: {}",
        published_api.join("yuyib/index.html").display()
    );
    Ok(())
}

fn ensure_generated_file(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_file() {
        return Ok(());
    }
    Err(format!(
        "documentation pipeline did not generate required API page `{}`",
        path.display()
    )
    .into())
}

fn workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask must live directly inside the workspace root".into())
}

fn run_cargo_doc(workspace: &Path, api_target: &Path) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", api_target)
        .arg("doc")
        .arg("--no-deps")
        .arg("--all-features");

    for package in PUBLIC_CRATES {
        command.arg("--package").arg(package);
    }

    run_command(&mut command)
}

fn run_command(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status()?;
    ensure_success(&program, status)
}

fn ensure_success(program: &str, status: ExitStatus) -> Result<(), Box<dyn Error>> {
    if status.success() {
        return Ok(());
    }

    Err(format!("`{program}` exited with {status}").into())
}

fn remove_directory_if_present(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn copy_directory_contents(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if !source.is_dir() {
        return Err(format!("Rustdoc output directory is missing: {}", source.display()).into());
    }

    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        copy_path(&source_path, &destination_path)?;
    }
    Ok(())
}

fn find_rustdoc_output(api_target: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let direct_output = api_target.join("doc");
    if direct_output.is_dir() {
        return Ok(direct_output);
    }

    let mut target_outputs = fs::read_dir(api_target)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("doc"))
        .filter(|candidate| candidate.is_dir());

    let Some(output) = target_outputs.next() else {
        return Err(format!(
            "Rustdoc output directory is missing under {}",
            api_target.display()
        )
        .into());
    };
    if target_outputs.next().is_some() {
        return Err(format!(
            "multiple Rustdoc output directories found under {}; set one Cargo target",
            api_target.display()
        )
        .into());
    }
    Ok(output)
}

fn copy_path(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if source.is_dir() {
        fs::create_dir_all(destination)?;
        copy_directory_contents(source, destination)?;
    } else if source.is_file() {
        fs::copy(source, destination)?;
    } else {
        return Err(format!("unsupported path in Rustdoc output: {}", source.display()).into());
    }
    Ok(())
}
