//! Embeds the locally built Monaco/Vite distribution into the native Editor binary.

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let ui_dist = manifest.join("../../editor-ui/dist");
    println!("cargo:rerun-if-changed={}", ui_dist.display());
    let mut files = Vec::new();
    visit(&ui_dist, &ui_dist, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(
        files.iter().any(|(path, _)| path == "index.html"),
        "Editor UI is not built. Run `npm install` and `npm run build` in editor-ui first."
    );

    let mut generated = String::from(
        "/// Locally built Editor UI assets embedded at compile time.\n\
         pub const EMBEDDED_EDITOR_ASSETS: &[(&str, &[u8])] = &[\n",
    );
    for (logical, absolute) in files {
        writeln!(
            generated,
            "    ({logical:?}, include_bytes!({absolute:?})),",
            absolute = absolute.to_string_lossy()
        )
        .expect("write embedded asset declaration");
    }
    generated.push_str("];\n");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("output directory")).join("editor_assets.rs");
    fs::write(output, generated).expect("write embedded Editor asset table");
}

fn visit(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    let entries = fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "could not read Editor UI directory {}: {error}",
            directory.display()
        )
    });
    for entry in entries {
        let entry = entry.expect("read Editor UI entry");
        let path = entry.path();
        if path.is_dir() {
            visit(root, &path, files);
        } else if path.is_file() && path.extension().is_none_or(|extension| extension != "map") {
            let logical = path
                .strip_prefix(root)
                .expect("Editor UI file remains below dist")
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push((logical, path));
        }
    }
}
