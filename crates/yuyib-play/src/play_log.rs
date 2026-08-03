//! Line-oriented Play logging that flushes through Editor process pipes.
//!
//! Windows Play is spawned with piped stderr + `CREATE_NO_WINDOW`. Without an
//! explicit flush, interaction messages can sit in the CRT buffer until exit —
//! so Diagnostics / the parent console look empty during Play.

use std::io::{Write, stderr};

/// Prints one line to stderr and flushes (visible in Editor Diagnostics `source=play`).
pub fn play_log(message: impl AsRef<str>) {
    let message = message.as_ref();
    eprintln!("{message}");
    let _ = stderr().flush();
}
