//! Native skeletal walk-cycle preview for the sci-fi girl GLB fixture.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example animated_girl_preview
//! ```
//!
//! The skeletal walk clip and the separate 29-target cloth morph track are
//! sampled every frame. Cloth positions are updated through the character
//! renderer's preview morph path.

#[path = "velina_skeletal_preview.rs"]
#[allow(dead_code, reason = "the shared file is also a standalone example")]
mod character_preview;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    character_preview::run(true)
}
