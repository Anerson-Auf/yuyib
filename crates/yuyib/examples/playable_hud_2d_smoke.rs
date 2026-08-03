//! Thin UI smoke: [`pause_overlay_tree`] + [`ApplicationUi::with_active_flag`].
//!
//! No window. Proves the pause tree builds and the active flag gates
//! [`ApplicationUi::is_active`].
//!
//! ```text
//! cargo run -p yuyib --example playable_hud_2d_smoke --features "app,ui"
//! ```

use std::{cell::Cell, error::Error, rc::Rc};

use yuyib::app::{ApplicationUi, pause_overlay_tree};

fn main() -> Result<(), Box<dyn Error>> {
    let tree = pause_overlay_tree("Paused", "Esc — resume")?;
    let active = Rc::new(Cell::new(false));
    let ui = ApplicationUi::new(tree).with_active_flag(Rc::clone(&active));
    if ui.is_active() {
        return Err("playable_hud_2d_smoke: expected inactive while flag is false".into());
    }
    active.set(true);
    if !ui.is_active() {
        return Err("playable_hud_2d_smoke: expected active while flag is true".into());
    }
    println!(
        "playable_hud_2d_smoke OK: pause tree widgets + active flag toggle"
    );
    Ok(())
}
