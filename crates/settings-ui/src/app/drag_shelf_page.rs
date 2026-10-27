//! The Drag Shelf page (Windows & Desktop): shaking to open a shelf and
//! its sensitivity, what shelves do with their items, and the shortcut.

use super::pages::callout;
use super::*;
use drag_shelf::{Sensitivity, ShakeThresholds};

/// What a sensitivity level means, in words.
pub(super) fn sensitivity_text(s: Sensitivity, custom: &ShakeThresholds) -> String {
    let t = s.thresholds(custom);
    let how = match s {
        Sensitivity::Low => "A firm shake: few accidental shelves.",
        Sensitivity::Medium => "A quick shake. Right for most people.",
        Sensitivity::High => "A small wiggle is enough.",
        Sensitivity::Custom => "Your own thresholds below.",
    };
    format!("{how} {} reversals in {} ms, {} pt in all.", t.reversals, t.window_ms, t.min_distance_px)
}

impl SettingsApp {
    pub(super) fn drag_shelf_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        if let Some(why) = drag_shelf::unsupported_reason() {
            callout(
                ui,
                icon::INFO,
                "Drag Shelf is macOS-only for now",
                &format!("Drag shelves are {why} yet. These settings are kept in the config, for your Macs."),
            );
        }
        let rec = &mut self.recording;
        let d = &mut self.app;
        let c = &mut d.cfg.drag_shelf;
        card_with(
            ui,
            "Shake to open",
            Some("While dragging files, links, text or images, shake the pointer left and right: a shelf opens under it. Drop the items, go wherever they need to go — another window, Space or folder — and drag them out of the shelf there."),
            |ui| {
                toggle_row(ui, "Open a shelf when you shake while dragging", Some("Moving windows or selecting text never opens one."), &mut c.shake_to_open);
                ui.add_enabled_ui(c.shake_to_open, |ui| {
                    let desc = sensitivity_text(c.sensitivity, &c.custom_shake);
                    row(ui, "Sensitivity", Some(&desc), |ui| {
                        segmented(
                            ui,
                            &mut c.sensitivity,
                            &[(Sensitivity::Low, "Low"), (Sensitivity::Medium, "Medium"), (Sensitivity::High, "High"), (Sensitivity::Custom, "Custom")],
                        )
                    });
                    if c.sensitivity == Sensitivity::Custom {
                        let t = &mut c.custom_shake;
                        row(ui, "Changes of direction", Some("Left-right reversals that make a shake."), |ui| slider(ui, &mut t.reversals, 1..=10, ""));
                        row(ui, "Within", Some("All of them in this time."), |ui| slider(ui, &mut t.window_ms, 100..=3000, " ms"));
                        row(ui, "Shortest stroke", Some("Smaller movements are jitter, not a change of direction."), |ui| slider(ui, &mut t.min_segment_px, 4..=400, " pt"));
                        row(ui, "Distance", Some("The strokes must add up to this much."), |ui| slider(ui, &mut t.min_distance_px, 10..=4000, " pt"));
                    }
                });
            },
        );
        card(ui, "Shelves", |ui| {
            toggle_row(
                ui,
                "Remove items once they're dragged out",
                Some("Dropping items somewhere takes them off the shelf. The files themselves are never touched."),
                &mut c.remove_after_drag_out,
            );
            toggle_row(ui, "Close a shelf when it's empty", None, &mut c.auto_close);
            ui.add_enabled_ui(c.auto_close, |ui| {
                row(ui, "Close after", None, |ui| slider(ui, &mut c.auto_close_delay_ms, 0..=10_000, " ms"));
            });
            row(ui, "Shelves open at most", Some("Shaking again opens another shelf, up to this many."), |ui| slider(ui, &mut c.max_shelves, 1..=20, ""));
            toggle_row(
                ui,
                "Keep temporary files",
                Some("Copies files dropped from temporary places (like screenshot previews) into the shelf so they don't disappear."),
                &mut c.copy_temporary_files,
            );
            toggle_row(ui, "Haptic feedback", Some("A tick on the trackpad when a shelf opens and when items land on it."), &mut c.haptics);
        });
        card_with(
            ui,
            "Shortcut",
            Some("Open an empty shelf at the pointer from anywhere. Not set by default; the menu bar icon also has New drag shelf."),
            |ui| {
                row(ui, "New shelf", None, |ui| {
                    hotkey_edit(ui, rec, Id::unique("ds-hk"), &mut d.drag_shelf_hotkey, RecMode::Combo);
                });
                if let Err(e) = parse_hk(&d.drag_shelf_hotkey) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
            },
        );
        card(ui, "Using a shelf", |ui| {
            for (title, text) in [
                ("Drag out", "Drag the stack to take everything, or click it to see every item and drag just one."),
                ("Actions", "The ··· button and right-click: Copy to…, Move to…, Open, Show in Finder, Share (AirDrop), Remove, Clear."),
                ("Move it", "Drag a shelf by its background. Shelves are on every Space and over full-screen apps."),
            ] {
                row(ui, title, Some(text), |_| {});
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_descriptions() {
        let c = ShakeThresholds::default();
        assert!(sensitivity_text(Sensitivity::Medium, &c).contains("3 reversals in 650 ms, 240 pt"));
        let custom = ShakeThresholds { reversals: 7, ..c };
        assert!(sensitivity_text(Sensitivity::Custom, &custom).contains("7 reversals"));
        assert!(sensitivity_text(Sensitivity::Low, &c).starts_with("A firm shake"));
    }
}
