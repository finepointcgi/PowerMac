//! Color editing for Mouse Utilities colors (kept out of pm-ui so it stays
//! independent of the module crates).

use eframe::egui::{Color32, RichText, Ui};

use crate::theme::pal;
use crate::widgets::swatch_picker;

/// Color picker for a Mouse Utilities color (with or without alpha).
pub fn color_edit(ui: &mut Ui, c: &mut mouse_utils::Color, alpha: bool) {
    let mut col = Color32::from_rgba_unmultiplied(c.r, c.g, c.b, if alpha { c.a } else { 255 });
    if swatch_picker(ui, &mut col, alpha).changed() {
        let [r, g, b, a] = col.to_srgba_unmultiplied();
        *c = mouse_utils::Color::argb(if alpha { a } else { c.a }, r, g, b);
    }
    let p = pal(ui);
    ui.label(RichText::new(c.to_string()).monospace().color(p.text2));
}

