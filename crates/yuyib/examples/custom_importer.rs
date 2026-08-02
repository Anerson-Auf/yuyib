//! Compile-time importer plugin: typed low-level dispatch and high-level loading.

use std::{error::Error, fmt, sync::Arc, thread};

use yuyib::prelude::*;

#[derive(Debug, Eq, PartialEq)]
struct DialogueAsset {
    lines: Vec<String>,
}

#[derive(Debug)]
enum DialogueImportError {
    Utf8,
    MissingHeader,
    TooManyLines { actual: usize, maximum: usize },
}

impl fmt::Display for DialogueImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utf8 => formatter.write_str("dialogue is not UTF-8"),
            Self::MissingHeader => formatter.write_str("expected YDLG header"),
            Self::TooManyLines { actual, maximum } => {
                write!(
                    formatter,
                    "dialogue has {actual} lines, maximum is {maximum}"
                )
            }
        }
    }
}

impl Error for DialogueImportError {}

struct DialogueImporter {
    max_lines: usize,
}

impl AssetImporter<DialogueAsset> for DialogueImporter {
    type Error = DialogueImportError;

    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new("example.dialogue", "1.0.0")
            .with_extension("ydlg")
            .with_media_type("text/x-yuyib-dialogue")
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.prefix.starts_with(b"YDLG\n") {
            ImportMatch::Exact
        } else if probe.extension == Some("ydlg") {
            ImportMatch::Possible
        } else {
            ImportMatch::Unsupported
        }
    }

    fn import(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<DialogueAsset>, Self::Error> {
        let text = std::str::from_utf8(source.bytes()).map_err(|_| DialogueImportError::Utf8)?;
        let body = text
            .strip_prefix("YDLG\n")
            .ok_or(DialogueImportError::MissingHeader)?;
        let lines = body.lines().map(str::to_owned).collect::<Vec<_>>();
        if lines.len() > self.max_lines {
            return Err(DialogueImportError::TooManyLines {
                actual: lines.len(),
                maximum: self.max_lines,
            });
        }

        let mut output = ImporterOutput::new(DialogueAsset { lines });
        output.cpu_bytes = u64::try_from(body.len()).ok();
        output.diagnostics.push(ImportDiagnostic {
            code: "example-format".to_owned(),
            message: "YDLG is a tutorial format, not a shipping format".to_owned(),
            severity: ImportDiagnosticSeverity::Info,
        });
        Ok(output)
    }
}

fn make_registry() -> Result<ImporterRegistry<DialogueAsset>, Box<dyn Error>> {
    let mut registry = ImporterRegistry::default();
    registry.register(DialogueImporter { max_lines: 128 })?;
    Ok(registry)
}

fn main() -> Result<(), Box<dyn Error>> {
    let bytes = b"YDLG\nHello\nTyped importer plugin";

    // Low-level: deterministic synchronous dispatch. Call this in any worker
    // selected by the host when AssetServer ownership is not desired.
    let registry = make_registry()?;
    let imported = registry.import(ImportSource::new("intro.ydlg", bytes))?;
    println!(
        "low-level: importer={} lines={:?}",
        imported.importer.id, imported.asset.lines
    );

    // High-level: the same plugin runs on AssetServer's bounded task pool and
    // publishes into the same stable handle returned while it is still loading.
    let registry = Arc::new(make_registry()?);
    let mut assets = Assets::new();
    let mut server = AssetServer::<DialogueAsset, ImportError>::new(TaskPoolConfig::new(1, 8)?)?;
    let handle = server.try_import_bytes(
        &mut assets,
        registry,
        "Intro dialogue",
        OwnedImportSource::new("intro.ydlg", bytes.to_vec()),
    )?;
    assert_eq!(assets.state(handle), Some(AssetState::Loading));

    for _ in 0..10_000 {
        let update = server.update(&mut assets)?;
        if update.ready.contains(&handle) {
            let dialogue = assets.get(handle).expect("ready handle has a value");
            let metadata = assets.metadata(handle).expect("ready handle has metadata");
            println!(
                "high-level: lines={} importer={} diagnostics={}",
                dialogue.lines.len(),
                metadata.importer_version.as_deref().unwrap_or("unknown"),
                metadata.diagnostics.len()
            );
            return Ok(());
        }
        if update.failed.contains(&handle) {
            return Err(format!("import failed: {:?}", server.failure(handle)).into());
        }
        thread::yield_now();
    }

    Err("background importer did not complete".into())
}
