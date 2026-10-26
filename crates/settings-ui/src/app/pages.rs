//! The pages of the newer modules: Mouse, Displays, Clipboard History, Notch
//! Shelf, Color Picker, Text Extractor, File Tools and Window Switcher, plus
//! the Display Control bridge to the running app.
//!
//! **Displays and the separate settings process.** The settings window never
//! opens a DDC channel or a gamma table itself: a second `DisplayControl`
//! here would restore the gamma tables (undoing the app's software dimming)
//! when the window closes, and two processes writing DDC to the same monitor
//! race. Instead the running app writes `display-status.json` (displays and
//! their current values, every 2 s) and applies the values this window writes
//! to `display-set.json` (polled every 150 ms). Per-display options
//! (excluded displays, forced software dimming) and the key / OSD settings are
//! ordinary config edits.

use super::*;

use color_picker::{ActivationAction, CustomFormat, Rgb};
use mouse_tuning::{ButtonAction, ButtonMapping, DeviceKind, PointerSettings};

/// The color the format previews use.
const SAMPLE: Rgb = Rgb::new(0x3A, 0x86, 0xFF);

/// Feature ids for `display_local`.
const F_BRIGHT: u8 = 0;
const F_CONTRAST: u8 = 1;
const F_VOLUME: u8 = 2;

impl SettingsApp {
    // -----------------------------------------------------------------------
    // Mouse

    pub(super) fn mouse_tuning(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let ax = self.perms.map(|p| p.0);
        let devices = self.devices.get_or_insert_with(list_devices_deduped).clone();
        let mut refresh = false;
        let rec = &mut self.recording;
        let m = &mut self.app.cfg.mouse_tuning;

        pointer_card(
            ui,
            "Mouse",
            if mac { "Applies to every mouse. Changes are undone when PowerMac quits or this page is switched off." } else { "Windows has one pointer setting for every device, so this covers trackpads too." },
            &mut m.mouse,
        );
        if mac {
            pointer_card(ui, "Trackpad", "Applies to the built-in trackpad and Magic Trackpads.", &mut m.trackpad);
        }

        card_with(
            ui,
            "Devices",
            Some(if mac {
                "Give one mouse or trackpad settings of its own; the rest follow the cards above."
            } else {
                "Per-device settings are only available on macOS."
            }),
            |ui| {
                if !mac {
                    block(ui, |ui| hint(ui, "Windows applies one pointer setting to every device."));
                    return;
                }
                if devices.is_empty() {
                    block(ui, |ui| hint(ui, "No mice or trackpads found."));
                }
                for (i, dev) in devices.iter().enumerate() {
                    let pos = m.devices.iter().position(|o| o.matches(dev));
                    let mut own = pos.is_some();
                    let glyph = if dev.kind == DeviceKind::Trackpad { icon::LAPTOP } else { icon::MOUSE_SIMPLE };
                    let kind = if dev.kind == DeviceKind::Trackpad { "Trackpad" } else { "Mouse" };
                    let mut desc = kind.to_string();
                    if !dev.transport.is_empty() {
                        desc.push_str(&format!(" · {}", dev.transport));
                    }
                    if dev.vendor_id != 0 || dev.product_id != 0 {
                        desc.push_str(&format!(" · {:04X}:{:04X}", dev.vendor_id, dev.product_id));
                    }
                    let name = if dev.name.is_empty() { kind.to_string() } else { dev.name.clone() };
                    let tint = Color32::from_rgb(0xA8, 0x55, 0xF7);
                    icon_row(ui, glyph, tint, &name, Some(&desc), |ui| {
                        ui.label(RichText::new("Own settings").size(12.0).color(pal(ui).text2));
                        toggle_labeled(ui, &mut own, &format!("Own settings for {name}"));
                    });
                    match (own, pos) {
                        (true, None) => m.devices.push(dev.new_override()),
                        (false, Some(p)) => {
                            m.devices.remove(p);
                        }
                        _ => {}
                    }
                    let Some(o) = m.devices.iter_mut().find(|o| o.matches(dev)) else { continue };
                    ui.push_id(("dev", i), |ui| {
                        row(ui, "Acceleration", Some(&format!("For {name}")), |ui| {
                            segmented(
                                ui,
                                &mut o.disable_acceleration,
                                &[(None, "Like the others"), (Some(true), "Off"), (Some(false), "On")],
                            )
                        });
                        let mut set = o.speed.is_some();
                        row(ui, "Pointer speed", Some(&format!("For {name}")), |ui| {
                            toggle_labeled(ui, &mut set, "Own speed");
                            if set {
                                let mut v = o.speed.unwrap_or(0.5);
                                percent_slider(ui, &mut v);
                                o.speed = Some(v);
                            }
                        });
                        if !set {
                            o.speed = None;
                        }
                    });
                }
                block(ui, |ui| {
                    if icon_text_button(ui, icon::ARROWS_CLOCKWISE, "Refresh devices").clicked() {
                        refresh = true;
                    }
                });
            },
        );

        card_with(
            ui,
            "Buttons",
            Some("What the middle and side buttons do. The left and right buttons are never changed."),
            |ui| {
                if mac {
                    toggle_row(
                        ui,
                        "Side buttons go back and forward",
                        Some("In Safari, Finder, Chrome, Xcode and most other apps (sent as Cmd+[ and Cmd+])."),
                        &mut m.buttons.side_buttons_navigation,
                    );
                }
                for (button, label) in MAPPABLE_BUTTONS {
                    let pos = m.buttons.mappings.iter().position(|x| x.button == button);
                    let mut action = pos.map(|p| m.buttons.mappings[p].action.clone()).unwrap_or_default();
                    let before = action.clone();
                    let natural = match button {
                        mouse_tuning::button::BACK if mac && m.buttons.side_buttons_navigation => "Default (back)",
                        mouse_tuning::button::FORWARD if mac && m.buttons.side_buttons_navigation => "Default (forward)",
                        _ => "Default",
                    };
                    row(ui, label, None, |ui| {
                        let shown = if action == ButtonAction::Default { natural } else { action.label() };
                        egui::ComboBox::from_id_salt(("button-action", button))
                            .selected_text(shown)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.set_min_width(220.0);
                                for a in ButtonAction::all() {
                                    let same = std::mem::discriminant(&a) == std::mem::discriminant(&action);
                                    let text = if a == ButtonAction::Default { natural } else { a.label() };
                                    if ui.selectable_label(same, text).clicked() && !same {
                                        action = a;
                                    }
                                }
                            });
                    });
                    if let ButtonAction::Keystroke { keys } = &mut action {
                        row(ui, "Shortcut to send", Some(&format!("When {} is pressed", label.to_lowercase())), |ui| {
                            hotkey_edit(ui, rec, Id::unique(("button-keys", button)), keys, RecMode::Combo)
                        });
                        if let Err(e) = mouse_tuning::keys::parse_combo(keys) {
                            if !keys.trim().is_empty() {
                                error_label(ui, &e);
                                ui.add_space(8.0);
                            }
                        }
                    }
                    if action != before {
                        match (action, pos) {
                            (ButtonAction::Default, Some(p)) => {
                                m.buttons.mappings.remove(p);
                            }
                            (ButtonAction::Default, None) => {}
                            (a, Some(p)) => m.buttons.mappings[p].action = a,
                            (a, None) => m.buttons.mappings.push(ButtonMapping { button, action: a }),
                        }
                    }
                }
                if mac {
                    if let Some(ok) = ax {
                        row(
                            ui,
                            "Accessibility",
                            Some("Needed to change what the buttons do (pointer settings work without it)."),
                            |ui| permission_control(ui, ok, "Privacy_Accessibility"),
                        );
                    }
                }
            },
        );
        if refresh {
            self.devices = None;
        }
    }

    // -----------------------------------------------------------------------
    // Displays

    pub(super) fn displays(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        self.scaling_banners(ui);
        let on = self.app.cfg.display_control.enabled;
        let status = self.display_status.clone().filter(|s| s.is_fresh() && s.running);
        let mut sets: Vec<(String, u8, u8)> = Vec::new();
        let mut mute: Option<(String, bool)> = None;
        let local = &self.display_local;
        let dc = &mut self.app.cfg.display_control;
        card_with(
            ui,
            "Displays",
            Some("External monitors are controlled over DDC/CI, Apple displays directly; the rest can be dimmed in software."),
            |ui| {
                let Some(st) = &status else {
                    block(ui, |ui| {
                        hint(
                            ui,
                            if on {
                                "Your displays and their sliders appear here while PowerMac is running."
                            } else {
                                "Display control is off. Switch it on above to control your displays."
                            },
                        )
                    });
                    return;
                };
                if st.displays.is_empty() {
                    block(ui, |ui| hint(ui, "No displays found."));
                }
                for (i, d) in st.displays.iter().enumerate() {
                    ui.push_id(("display", i), |ui| {
                        let mut o = dc.override_for(&d.id);
                        let glyph = if d.is_builtin { icon::LAPTOP } else { icon::MONITOR };
                        let method = match d.brightness_method {
                            display_control::BrightnessMethod::Ddc => "DDC/CI",
                            display_control::BrightnessMethod::Native => "Native brightness",
                            display_control::BrightnessMethod::Software => "Software dimming",
                            display_control::BrightnessMethod::None => "No brightness control",
                        };
                        let mut desc = if d.is_builtin { format!("Built-in · {method}") } else { method.to_string() };
                        if let Some(src) = d.input_source_name() {
                            desc.push_str(&format!(" · {src}"));
                        }
                        desc.push_str(&format!(" · {}×{}", d.bounds.width, d.bounds.height));
                        icon_row(ui, glyph, Color32::from_rgb(0xF5, 0xB2, 0x00), &d.name, Some(&desc), |ui| {
                            toggle_labeled(ui, &mut o.enabled, &format!("Control {}", d.name))
                        });
                        let value = |f: u8, v: Option<u8>| local.get(&(d.id.clone(), f)).map(|x| x.0).or(v);
                        ui.add_enabled_ui(o.enabled, |ui| {
                            if let Some(mut b) = value(F_BRIGHT, d.brightness) {
                                if row(ui, "Brightness", None, |ui| slider(ui, &mut b, 0..=100, " %")).changed() {
                                    sets.push((d.id.clone(), F_BRIGHT, b));
                                }
                            }
                            if let Some(mut c) = value(F_CONTRAST, d.contrast) {
                                if row(ui, "Contrast", None, |ui| slider(ui, &mut c, 0..=100, " %")).changed() {
                                    sets.push((d.id.clone(), F_CONTRAST, c));
                                }
                            }
                            if let Some(mut v) = value(F_VOLUME, d.volume) {
                                let mut muted = d.muted.unwrap_or(false);
                                let r = row(ui, "Volume", Some("The monitor's own speakers."), |ui| {
                                    slider(ui, &mut v, 0..=100, " %")
                                });
                                if r.changed() {
                                    sets.push((d.id.clone(), F_VOLUME, v));
                                }
                                if toggle_row(ui, "Mute", None, &mut muted).changed() {
                                    mute = Some((d.id.clone(), muted));
                                }
                            }
                            if !d.is_builtin && d.brightness_method != display_control::BrightnessMethod::Native {
                                toggle_row(
                                    ui,
                                    "Dim in software",
                                    Some("Use the color table instead of the monitor's own brightness (for monitors whose DDC brightness misbehaves)."),
                                    &mut o.force_software_dim,
                                );
                            }
                        });
                        if o == display_control::DisplayOverride::default() {
                            dc.displays.remove(&d.id);
                        } else if dc.displays.get(&d.id) != Some(&o) {
                            dc.displays.insert(d.id.clone(), o);
                        }
                    });
                }
            },
        );
        for (id, f, v) in sets {
            self.display_set(&id, f, v);
        }
        if let Some((id, m)) = mute {
            let mut r = self.display_pending.take().filter(|r| r.id == id).unwrap_or_else(|| pm_config::DisplayRequest::new(&id));
            r.muted = Some(m);
            self.display_pending = Some(r);
            self.flush_display_request(true);
        }

        self.scaling_cards(ui);
        let keys_active = status.as_ref().map(|s| s.keys_active);
        let rec = &mut self.recording;
        let d = &mut self.app;
        let dc = &mut d.cfg.display_control;
        card_with(ui, "Keyboard", Some("The keys act on the display under the pointer."), |ui| {
            if cfg!(target_os = "macos") {
                toggle_row(
                    ui,
                    "Brightness keys control external displays",
                    Some("On the built-in display the keys keep working as usual."),
                    &mut dc.take_over_brightness_keys,
                );
            }
            toggle_row(
                ui,
                "Volume keys control monitor speakers",
                Some("Only on displays with DDC volume; otherwise the keys change the Mac's volume."),
                &mut dc.take_over_volume_keys,
            );
            row(ui, "Step per key press", Some("Hold Option+Shift with a key for a quarter step."), |ui| {
                slider(ui, &mut dc.step, 1..=50, " %")
            });
            toggle_row(ui, "Show the level on screen", None, &mut dc.show_osd);
            if let Some(active) = keys_active {
                if dc.take_over_brightness_keys || dc.take_over_volume_keys {
                    row(ui, "Keys", Some("Taking over the keys needs Accessibility permission."), |ui| {
                        status_pill(ui, active, if active { "Active" } else { "Not active" });
                        if !active && ui.button("Open Settings…").clicked() {
                            crate::platform::open_privacy_pane("Privacy_Accessibility");
                        }
                    });
                }
            }
        });
        card_with(ui, "Shortcuts", Some("Optional shortcuts for the display under the pointer."), |ui| {
            for (i, name) in DC_HOTKEYS.iter().enumerate() {
                row(ui, name, None, |ui| {
                    hotkey_edit(ui, rec, Id::unique(("dc-hk", i)), &mut d.display_hotkeys[i], RecMode::Combo)
                });
                if let Err(e) = parse_hk(&d.display_hotkeys[i]) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
            }
        });
        let dc = &mut d.cfg.display_control;
        card(ui, "Dimming", |ui| {
            toggle_row(
                ui,
                "Dim displays without hardware control",
                Some("Darkens them through their color table, which is put back when PowerMac quits."),
                &mut dc.software_dim_fallback,
            );
        });
    }

    /// Ask the running app to set `feature` of display `id` to `v`.
    fn display_set(&mut self, id: &str, feature: u8, v: u8) {
        self.display_local.insert((id.to_string(), feature), (v, Instant::now()));
        let mut r = match self.display_pending.take() {
            Some(r) if r.id == id => r,
            Some(other) => {
                self.display_pending = Some(other);
                self.flush_display_request(true);
                pm_config::DisplayRequest::new(id)
            }
            None => pm_config::DisplayRequest::new(id),
        };
        match feature {
            F_BRIGHT => r.brightness = Some(v),
            F_CONTRAST => r.contrast = Some(v),
            _ => r.volume = Some(v),
        }
        self.display_pending = Some(r);
    }

    /// Write the pending display request (at most every 70 ms unless `force`).
    pub(super) fn flush_display_request(&mut self, force: bool) {
        if self.display_pending.is_none() {
            return;
        }
        if !force && self.display_written.elapsed() < Duration::from_millis(70) {
            return;
        }
        if let Some(mut r) = self.display_pending.take() {
            r.at = pm_config::DisplayRequest::new("").at;
            if let Err(e) = r.write_for(&self.config_path) {
                log::warn!("display request: {e:#}");
            }
            self.display_written = Instant::now();
        }
    }

    // -----------------------------------------------------------------------
    // Clipboard History

    pub(super) fn clipboard_history(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let clip_count = self.clip_count;
        let config_path = self.config_path.clone();
        let rec = &mut self.recording;
        let new_app = self.new_app.entry("clipboard").or_default();
        let running = &mut self.running_apps;
        let c = &mut self.app.cfg.clipboard_history;
        let mut cleared = false;

        card(ui, "Picker", |ui| {
            row(ui, "Shortcut", Some("Opens the history from any app."), |ui| {
                hotkey_edit(ui, rec, Id::unique("clip-hk"), &mut c.hotkey, RecMode::Combo)
            });
            if !c.hotkey.trim().is_empty() {
                if let Err(e) = parse_shortcut(&c.hotkey) {
                    error_label(ui, &e);
                    ui.add_space(8.0);
                }
            }
            toggle_row(
                ui,
                "Paste the item you choose",
                Some("Into the app you were in. Otherwise it's only copied."),
                &mut c.paste_on_select,
            );
            row(ui, "Open the picker", None, |ui| {
                use clipboard_history::PickerPosition as P;
                segmented(ui, &mut c.picker_position, &[(P::Mouse, "At the pointer"), (P::Center, "In the center")])
            });
        });
        card(ui, "History", |ui| {
            toggle_row(ui, "Pause recording", Some("The history stays available; new copies aren't added."), &mut c.paused);
            toggle_row(
                ui,
                "Keep the history after quitting",
                Some("Saved in the clipboard folder next to config.json. When off it lives in memory only, and the saved history is deleted."),
                &mut c.persist,
            );
            row(ui, "Items to keep", Some("Pinned items don't count and are always kept."), |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut c.max_items).range(1..=5000))
            });
            row(ui, "Forget items after", Some("0 keeps them until the limit above pushes them out."), |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut c.max_age_days).range(0..=3650).suffix(" days"))
            });
            row(
                ui,
                "Stored now",
                Some(&match clip_count {
                    Some(n) => plural(n, "item"),
                    None => "Shown while PowerMac is running with the history on.".to_string(),
                }),
                |ui| {
                    let id = Id::unique("clip-confirm-clear");
                    let confirm: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
                    ui.add_enabled_ui(clip_count.is_some_and(|n| n > 0), |ui| {
                        if confirm {
                            if ui.button("Cancel").clicked() {
                                ui.data_mut(|d| d.remove::<bool>(id));
                            }
                            if ui
                                .add(egui::Button::new(RichText::new("Clear unpinned items").color(Color32::WHITE)).fill(pal(ui).error))
                                .clicked()
                            {
                                cleared = true;
                                ui.data_mut(|d| d.remove::<bool>(id));
                            }
                        } else if icon_text_button(ui, icon::TRASH, "Clear history…").clicked() {
                            ui.data_mut(|d| d.insert_temp(id, true));
                        }
                    });
                },
            );
        });
        card(ui, "What's recorded", |ui| {
            toggle_row(ui, "Formatted text", Some("Keeps the HTML / RTF of copied text; plain text is always kept."), &mut c.capture_rich_text);
            toggle_row(ui, "Images", Some("Stored as PNG with a thumbnail."), &mut c.capture_images);
            toggle_row(ui, "Files", Some("Their paths, to paste them again in Finder."), &mut c.capture_files);
            row(ui, "Skip items larger than", None, |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut c.max_item_mb).range(1..=500).suffix(" MB"))
            });
        });
        card_with(
            ui,
            "Privacy",
            Some(if mac {
                "Nothing copied from these apps is recorded. Use app names (1Password) or bundle ids (com.1password.1password)."
            } else {
                "Nothing copied from these apps is recorded. Use app or exe names (KeePass.exe)."
            }),
            |ui| {
                toggle_row(
                    ui,
                    "Skip passwords and other hidden items",
                    Some("Password managers mark what they copy as concealed or transient."),
                    &mut c.skip_concealed,
                );
                apps_list(ui, &mut c.excluded_apps, new_app, running, if mac { "App name or bundle id" } else { "App.exe" });
            },
        );
        if cleared {
            let dir = clipboard_history::data_dir_for_config(&config_path);
            if let Some(cl) = clipboard_history::ipc::Client::connect(&dir) {
                match cl.call_ok(&clipboard_history::ipc::Request::Clear { keep_pinned: true }) {
                    Ok(r) => {
                        self.clip_count = Some(r.total);
                        self.toast = Some((Instant::now(), "History cleared".into()));
                    }
                    Err(e) => log::warn!("clipboard history: {e:#}"),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Notch Shelf

    pub(super) fn notch_shelf(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let notch = self.has_notch;
        let n = &mut self.app.cfg.notch_shelf;
        if !notch {
            callout(
                ui,
                icon::INFO,
                "This Mac has no notch",
                "Turn on the virtual notch below to get the shelf as a small pill at the top center of the screen.",
            );
        }
        card(ui, "Opening", |ui| {
            toggle_row(ui, "Open when the pointer rests on the notch", None, &mut n.expand_on_hover);
            ui.add_enabled_ui(n.expand_on_hover, |ui| {
                row(ui, "Wait before opening", None, |ui| {
                    ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut n.hover_delay_ms).range(0..=2000).suffix(" ms"))
                });
            });
            toggle_row(ui, "Open when the notch is clicked", None, &mut n.expand_on_click);
            toggle_row(
                ui,
                "Open while dragging something to the top",
                Some("Files, links, text and images can then be dropped on the shelf."),
                &mut n.expand_on_drag,
            );
            row(ui, "Close after the pointer leaves", None, |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut n.collapse_delay_ms).range(0..=5000).suffix(" ms"))
            });
        });
        card(ui, "Screens", |ui| {
            row(ui, "Show the shelf on", None, |ui| {
                use notch_shelf::ScreenMode as S;
                segmented(ui, &mut n.screens, &[(S::NotchOnly, "The notch screen"), (S::All, "Every screen")])
            });
            toggle_row(
                ui,
                "Virtual notch on screens without one",
                Some("A small pill at the top center that works like the notch."),
                &mut n.virtual_notch_on_non_notch_screens,
            );
        });
        card(ui, "Items", |ui| {
            toggle_row(
                ui,
                "Keep temporary files",
                Some("Copies files dropped from temporary places (like screenshot previews) into the shelf so they don't disappear."),
                &mut n.copy_temporary_files,
            );
            toggle_row(ui, "Remove items once they're dragged out", None, &mut n.remove_after_drag_out);
            row(ui, "Shelf width", None, |ui| slider(ui, &mut n.panel_width, 380..=1400, " pt"));
        });
        card(ui, "Extras", |ui| {
            toggle_row(ui, "Haptic feedback on drop", Some("A tick on the trackpad."), &mut n.haptics);
            toggle_row(
                ui,
                "Show the clipboard as the first item",
                Some("macOS may ask whether PowerMac may read the clipboard."),
                &mut n.show_clipboard,
            );
            toggle_row(
                ui,
                "Show the battery level when charging starts or stops",
                None,
                &mut n.battery_indicator,
            );
        });
        self.notch_widgets(ui);
    }

    // -----------------------------------------------------------------------
    // Color Picker

    pub(super) fn color_picker(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let rec = &mut self.recording;
        let d = &mut self.app;
        let new_format = &mut self.new_format;
        let c = &mut d.cfg.color_picker.settings;

        card(ui, "Activation", |ui| {
            row(ui, "Shortcut", None, |ui| hotkey_edit(ui, rec, Id::unique("cp-hk"), &mut d.color_hotkey, RecMode::Combo));
            if let Err(e) = parse_hk(&d.color_hotkey) {
                error_label(ui, &e);
                ui.add_space(8.0);
            }
            row(ui, "When pressed", None, |ui| {
                combo(
                    ui,
                    "cp-activation",
                    &mut c.activation,
                    &[
                        (ActivationAction::PickThenEditor, "Picks a color, then opens the editor"),
                        (ActivationAction::PickThenCopy, "Picks a color and copies it"),
                        (ActivationAction::OpenEditor, "Opens the editor"),
                    ],
                )
            });
        });

        let formats = c.all_formats();
        card_with(ui, "Copied format", Some("A picked color is copied to the clipboard in this format."), |ui| {
            let preview = c.copied_text(SAMPLE);
            row(ui, "Copy as", Some(&preview), |ui| {
                swatch(ui, SAMPLE);
                egui::ComboBox::from_id_salt("cp-copied")
                    .selected_text(c.copied_format.clone())
                    .width(180.0)
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        for (name, t) in &formats {
                            let r = ui.selectable_label(c.copied_format == *name, name);
                            if r.on_hover_text(color_picker::format_color(SAMPLE, t)).clicked() {
                                c.copied_format = name.clone();
                            }
                        }
                    });
            });
            toggle_row(ui, "Show the color's name in the editor", Some("Like “Light blue”."), &mut c.show_color_name);
            row(ui, "Colors in the history", None, |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut c.history_limit).range(1..=100))
            });
        });

        card_with(
            ui,
            "Formats in the editor",
            Some("Switch formats on to list them in the editor; use the arrows to change their order. Previews use the sample color."),
            |ui| {
                let mut order: Vec<(String, String)> =
                    c.visible_formats.iter().filter_map(|n| formats.iter().find(|(f, _)| f == n).cloned()).collect();
                order.extend(formats.iter().filter(|(n, _)| !c.visible_formats.contains(n)).cloned());
                let mut action: Option<(String, i32)> = None;
                let mut toggled: Option<(String, bool)> = None;
                for (name, t) in &order {
                    let vis_pos = c.visible_formats.iter().position(|n| n == name);
                    let mut on = vis_pos.is_some();
                    let preview = color_picker::format_color(SAMPLE, t);
                    row(ui, name, Some(&preview), |ui| {
                        if let Some(p) = vis_pos {
                            ui.add_enabled_ui(p > 0, |ui| {
                                if icon_button(ui, icon::ARROW_UP, "Move up").clicked() {
                                    action = Some((name.clone(), -1));
                                }
                            });
                            ui.add_enabled_ui(p + 1 < c.visible_formats.len(), |ui| {
                                if icon_button(ui, icon::ARROW_DOWN, "Move down").clicked() {
                                    action = Some((name.clone(), 1));
                                }
                            });
                        }
                        if toggle_labeled(ui, &mut on, &format!("Show {name}")).changed() {
                            toggled = Some((name.clone(), on));
                        }
                    });
                }
                if let Some((name, dir)) = action {
                    if let Some(p) = c.visible_formats.iter().position(|n| *n == name) {
                        let q = (p as i32 + dir).clamp(0, c.visible_formats.len() as i32 - 1) as usize;
                        c.visible_formats.swap(p, q);
                    }
                }
                match toggled {
                    Some((name, true)) => c.visible_formats.push(name),
                    Some((name, false)) => c.visible_formats.retain(|n| *n != name),
                    None => {}
                }
            },
        );

        card_with(
            ui,
            "Custom formats",
            Some("Templates use PowerToys' syntax: %Re, %Gr and %Bl are red, green and blue; add a formatter like x (two hex digits) or f (0–1), e.g. #%ReX%GrX%BlX."),
            |ui| {
                let mut remove = None;
                let mut renamed: Option<(String, String)> = None;
                for (i, f) in c.custom_formats.iter_mut().enumerate() {
                    block(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            swatch(ui, SAMPLE);
                            let old = f.name.clone();
                            ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Name").desired_width(120.0));
                            if f.name != old {
                                renamed = Some((old, f.name.clone()));
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut f.format)
                                    .hint_text("#%ReX%GrX%BlX")
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(220.0),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(30.0);
                            let p = pal(ui);
                            ui.label(RichText::new("→").color(p.text3));
                            ui.label(
                                RichText::new(color_picker::format_color(SAMPLE, &f.format))
                                    .monospace()
                                    .color(p.text2),
                            );
                        });
                    });
                }
                if let Some((old, new)) = renamed {
                    for n in c.visible_formats.iter_mut().filter(|n| **n == old) {
                        *n = new.clone();
                    }
                    if c.copied_format == old {
                        c.copied_format = new;
                    }
                }
                if let Some(i) = remove {
                    let gone = c.custom_formats.remove(i).name;
                    if c.template(&gone).is_none() {
                        c.visible_formats.retain(|n| *n != gone);
                        if c.copied_format == gone {
                            c.copied_format = "HEX".into();
                        }
                    }
                }
                if c.custom_formats.is_empty() {
                    block(ui, |ui| hint(ui, "No custom formats yet."));
                }
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.add(egui::TextEdit::singleline(&mut new_format.0).hint_text("Name").desired_width(120.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut new_format.1)
                                .hint_text("Template, e.g. %ReX%GrX%BlX")
                                .font(egui::TextStyle::Monospace)
                                .desired_width(220.0),
                        );
                        let ok = !new_format.0.trim().is_empty() && !new_format.1.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new(theme::icon_job(icon::PLUS, "Add", 13.0, pal(ui).text))).clicked() {
                            let name = new_format.0.trim().to_string();
                            c.custom_formats.push(CustomFormat { name: name.clone(), format: new_format.1.trim().to_string() });
                            if !c.visible_formats.contains(&name) {
                                c.visible_formats.push(name);
                            }
                            *new_format = Default::default();
                        }
                    });
                    if !new_format.1.is_empty() {
                        ui.horizontal(|ui| {
                            let p = pal(ui);
                            ui.label(RichText::new("Preview →").size(12.0).color(p.text3));
                            ui.label(RichText::new(color_picker::format_color(SAMPLE, &new_format.1)).monospace().color(p.text2));
                        });
                    }
                });
                ui.collapsing(RichText::new("Template parameters").size(12.5), |ui| {
                    let p = pal(ui);
                    egui::Grid::new("cp-params").num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
                        for (i, (code, _, desc)) in color_picker::formats::PARAMETERS.iter().enumerate() {
                            ui.label(RichText::new(format!("%{code}")).monospace().color(p.text));
                            ui.label(RichText::new(*desc).size(12.0).color(p.text2));
                            if i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.add_space(6.0);
                    let fm: Vec<String> =
                        color_picker::formats::FORMATTERS.iter().map(|(c, d)| format!("{c} = {d}")).collect();
                    hint(ui, &format!("Formatters: {}. %Na is the color's name.", fm.join(", ")));
                });
            },
        );
        for e in validate_color_formats(c) {
            warn_label(ui, &e);
        }
    }

    // -----------------------------------------------------------------------
    // Text Extractor

    pub(super) fn text_extractor(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let granted = self.screen_recording;
        let langs = self.languages.get_or_insert_with(text_extractor::supported_languages).clone();
        let rec = &mut self.recording;
        let d = &mut self.app;
        let t = &mut d.cfg.text_extractor.settings;
        card(ui, "Activation", |ui| {
            row(ui, "Shortcut", Some("Then drag over the text. Esc or a right click cancels."), |ui| {
                hotkey_edit(ui, rec, Id::unique("ocr-hk"), &mut d.ocr_hotkey, RecMode::Combo)
            });
            if let Err(e) = parse_hk(&d.ocr_hotkey) {
                error_label(ui, &e);
                ui.add_space(8.0);
            }
            toggle_row(ui, "Show a confirmation", Some("“Copied 42 words”, or why nothing was found."), &mut t.show_toast);
        });
        card(ui, "Recognition", |ui| {
            row(
                ui,
                "Languages",
                Some(if t.languages.is_empty() {
                    "Detected automatically. Add languages to prefer them, in order."
                } else {
                    "Tried in this order."
                }),
                |ui| {
                    let mut remove = None;
                    for (i, l) in t.languages.iter().enumerate() {
                        let p = pal(ui);
                        egui::Frame::new()
                            .fill(p.accent_soft)
                            .corner_radius(11.0)
                            .inner_margin(egui::Margin { left: 9, right: 4, top: 2, bottom: 2 })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    ui.label(RichText::new(lang_name(l)).size(12.0).color(p.text));
                                    if ui
                                        .add(egui::Button::new(theme::icon_text(icon::X).size(11.0).color(p.text2)).frame(false))
                                        .on_hover_text("Remove")
                                        .clicked()
                                    {
                                        remove = Some(i);
                                    }
                                });
                            });
                    }
                    if let Some(i) = remove {
                        t.languages.remove(i);
                    }
                    let left: Vec<&String> = langs.iter().filter(|l| !t.languages.contains(l)).collect();
                    egui::ComboBox::from_id_salt("ocr-add-lang")
                        .selected_text(if t.languages.is_empty() { "Add a language…" } else { "Add…" })
                        .width(150.0)
                        .height(320.0)
                        .show_ui(ui, |ui| {
                            ui.set_min_width(200.0);
                            if left.is_empty() {
                                ui.weak("No more languages");
                            }
                            for l in left {
                                if ui.selectable_label(false, lang_name(l)).clicked() {
                                    t.languages.push(l.clone());
                                }
                            }
                        });
                },
            );
            toggle_row(ui, "Join lines into one", Some("Line breaks become spaces."), &mut t.single_line);
            if mac {
                toggle_row(
                    ui,
                    "Correct words with the language model",
                    Some("Better for prose; turn it off for codes and identifiers."),
                    &mut t.language_correction,
                );
            }
        });
        if mac {
            card_with(ui, "Permission", Some("Text in image files and on the clipboard can be read without it: powermac ocr <file> or powermac ocr --clipboard."), |ui| {
                row(ui, "Screen Recording", Some("Needed to read text from the screen."), |ui| {
                    status_pill(ui, granted, if granted { "Granted" } else { "Not granted" });
                    if !granted {
                        if ui.button("Ask…").on_hover_text("Shows the macOS permission prompt (only once)").clicked() {
                            text_extractor::request_screen_recording_permission();
                        }
                        if ui.button("Open Settings…").clicked() {
                            crate::platform::open_privacy_pane("Privacy_ScreenCapture");
                        }
                    }
                });
            });
        }
    }

    // -----------------------------------------------------------------------
    // File Tools

    pub(super) fn file_tools(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let installed = *self.finder_installed.get_or_insert_with(file_tools::finder_integration_installed);
        let needs_update = *self.finder_needs_update.get_or_insert_with(file_tools::finder_integration_needs_update);
        let mut toggle = None;
        let where_ = if mac { "Finder" } else { "Explorer" };
        card_with(
            ui,
            &format!("{where_} integration"),
            Some(if mac {
                "Adds Rename with PowerRename, New file here, Open in Terminal, Open in Editor, Copy Path and Resize Images to Quick Actions when you right-click files or folders in Finder."
            } else {
                "Adds Rename with PowerRename, New file here, Open in Terminal, Open in Editor, Copy Path and Resize Images to the right-click menu in Explorer."
            }),
            |ui| {
                row(
                    ui,
                    if mac { "Quick Actions" } else { "Context menu" },
                    Some("They run this copy of PowerMac."),
                    |ui| {
                        if needs_update {
                            status_pill(ui, false, "Some are missing");
                            if primary_button(ui, if mac { "Update Finder actions" } else { "Update Explorer menu" }).clicked() {
                                toggle = Some(true);
                            }
                            return;
                        }
                        status_pill(ui, installed, if installed { "Installed" } else { "Not installed" });
                        if installed {
                            if ui.button("Remove").clicked() {
                                toggle = Some(false);
                            }
                        } else if primary_button(ui, "Install").clicked() {
                            toggle = Some(true);
                        }
                    },
                );
                if let Some(e) = &self.finder_error {
                    error_label(ui, e);
                    ui.add_space(8.0);
                }
                row(ui, "From a terminal", Some("powermac rename <files…>  ·  powermac new --dir <folder>  ·  powermac resize <pictures…>  ·  powermac finder <action>"), |_| {});
            },
        );
        if let Some(install) = toggle {
            let r = if install {
                std::env::current_exe()
                    .map_err(anyhow::Error::from)
                    .and_then(|exe| file_tools::install_finder_integration(&exe).map(|_| ()))
            } else {
                file_tools::uninstall_finder_integration()
            };
            self.finder_error = r.err().map(|e| format!("{e:#}"));
            self.finder_installed = Some(file_tools::finder_integration_installed());
            self.finder_needs_update = Some(file_tools::finder_integration_needs_update());
        }

        let ft = &mut self.ft;
        card_with(ui, "PowerRename", Some("Search and replace in the names of many files at once, with regular expressions and previews."), |ui| {
            toggle_row(
                ui,
                "Remember the last search and options",
                Some("Otherwise every window starts empty."),
                &mut ft.rename.remember_options,
            );
            toggle_row(ui, "List only the items that will change", None, &mut ft.rename.show_only_changed);
            row(ui, "Recent searches kept", Some("0 turns the list off."), |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut ft.rename.mru_size).range(0..=50))
            });
        });
        let np = &mut ft.new_plus;
        let dir = file_tools::newplus::templates_dir_for(np);
        card_with(ui, "New+", Some("Create files and folders from your own templates: anything in the templates folder can be copied into a folder with “New file here”."), |ui| {
            row(ui, "Templates folder", Some(&dir.display().to_string()), |ui| {
                if icon_text_button(ui, icon::FOLDER_OPEN, "Open").clicked() {
                    crate::platform::open_folder(&dir);
                }
            });
            row(ui, "Use another folder", Some("Leave empty for the default."), |ui| {
                let mut text = np.templates_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                let r = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .hint_text(file_tools::newplus::default_templates_dir().map(|p| p.display().to_string()).unwrap_or_default())
                        .desired_width(260.0),
                );
                if r.changed() {
                    np.templates_dir = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
                }
            });
            toggle_row(ui, "Hide file extensions in the list", None, &mut np.hide_extensions);
            toggle_row(
                ui,
                "Hide leading numbers",
                Some("“01. Notes.txt” is listed and created as “Notes.txt”, so numbers can set the order."),
                &mut np.hide_starting_digits,
            );
            toggle_row(
                ui,
                "Fill in variables",
                Some("$YYYY, $MM, $DD, $PARENT_FOLDER_NAME and environment variables in names and text files."),
                &mut np.replace_variables,
            );
            toggle_row(ui, &format!("Select the new item in {where_}"), None, &mut np.reveal_after_create);
            toggle_row(ui, "Open the new item", None, &mut np.open_after_create);
        });
        self.finder_cards(ui);
    }

    // -----------------------------------------------------------------------
    // Window Switcher

    pub(super) fn window_switcher(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let alt_tab = self.alt_tab_running;
        let alt_tab_keys = self.alt_tab_hotkeys.clone();
        let ax = self.perms.map(|p| p.0);
        let screen = self.screen_recording;
        let rec = &mut self.recording;
        let new_app = self.new_app.entry("switcher").or_default();
        let running = &mut self.running_apps;
        let d = &mut self.app;
        let w = &mut d.cfg.window_switcher;
        if alt_tab && w.enabled {
            let clash = alt_tab_keys.iter().find(|h| window_switcher::conflicts::clashes(w, h));
            if let Some(h) = clash {
                callout(
                    ui,
                    icon::WARNING,
                    &format!("AltTab is running and also uses {h}"),
                    "Both would react to the same shortcut. Quit AltTab, or give one of them another shortcut.",
                );
            }
        }
        let system = if mac { "Cmd+Tab" } else { "Alt+Tab" };
        replace_banner(
            ui,
            &format!("Replace {system}"),
            if mac {
                "Cmd+Tab opens this switcher instead of the macOS app switcher: hold Cmd and press Tab to move on, \
                 release Cmd to switch to that window. The macOS switcher is off only while PowerMac runs, \
                 and comes back when you turn this off or quit (even after a crash)."
            } else {
                "Alt+Tab opens this switcher instead of the Windows one."
            },
            &mut w.replace_cmd_tab,
        );
        card(ui, "Shortcuts", |ui| {
            if w.replace_cmd_tab {
                row(
                    ui,
                    "Switch between windows",
                    Some(if mac {
                        "Hold Cmd, press Tab to move on, release to switch. Cmd+Shift+Tab goes back."
                    } else {
                        "Hold Alt, press Tab to move on, release to switch. Alt+Shift+Tab goes back."
                    }),
                    |ui| keycaps(ui, system),
                );
                row(
                    ui,
                    "Also with",
                    Some("A second shortcut for the same switcher. Leave it empty to use only the one above."),
                    |ui| hotkey_edit(ui, rec, Id::unique("ws-trigger"), &mut d.switch_trigger, RecMode::Combo),
                );
            } else {
                row(
                    ui,
                    "Switch between windows",
                    Some("Hold the modifier, press the key to move on, release to switch. Add Shift to go back."),
                    |ui| hotkey_edit(ui, rec, Id::unique("ws-trigger"), &mut d.switch_trigger, RecMode::Combo),
                );
            }
            if let Err(e) = parse_hk(&d.switch_trigger) {
                error_label(ui, &e);
                ui.add_space(8.0);
            }
            row(ui, "Switch between the front app's windows", None, |ui| {
                hotkey_edit(ui, rec, Id::unique("ws-app-trigger"), &mut d.switch_app_trigger, RecMode::Combo)
            });
            if let Err(e) = parse_hk(&d.switch_app_trigger) {
                error_label(ui, &e);
                ui.add_space(8.0);
            }
            if mac && w.replace_cmd_tab {
                toggle_row(
                    ui,
                    "Cmd+` switches the front app's windows too",
                    Some("Instead of macOS's “Move focus to next window”."),
                    &mut w.replace_cmd_backtick,
                );
            }
            row(
                ui,
                "Delay before it appears",
                Some("A quicker tap switches straight to the previous window."),
                |ui| slider(ui, &mut w.show_delay_ms, 0..=500, " ms"),
            );
        });
        if mac {
            card_with(
                ui,
                "Preview",
                Some("While the switcher is open, the window you're about to switch to is shown where it is on screen."),
                |ui| {
                    toggle_row(
                        ui,
                        "Preview the selected window",
                        Some("Minimized windows and windows on other desktops are shown in the middle of the screen."),
                        &mut w.preview_selected_window,
                    );
                    ui.add_enabled_ui(w.preview_selected_window, |ui| {
                        toggle_row(ui, "Dim the rest of the screen", None, &mut w.dim_background);
                        ui.add_enabled_ui(w.dim_background, |ui| {
                            row(ui, "Dimming", None, |ui| percent_slider(ui, &mut w.dim_opacity));
                        });
                    });
                    if w.preview_selected_window && !screen {
                        hint(ui, "Without Screen Recording the preview shows the window's outline, app icon and title.");
                    }
                },
            );
        }
        card(ui, "Windows shown", |ui| {
            toggle_row(ui, "Minimized windows", None, &mut w.show_minimized);
            toggle_row(ui, "Windows of hidden apps", None, &mut w.show_hidden);
            toggle_row(ui, if mac { "Windows on other Spaces" } else { "Windows on other desktops" }, None, &mut w.show_other_spaces);
            toggle_row(ui, "Only windows on the screen with the pointer", None, &mut w.only_current_screen);
            toggle_row(
                ui,
                "Window previews",
                Some(if mac { "Needs Screen Recording; without it the switcher shows app icons." } else { "Live thumbnails of the windows." }),
                &mut w.thumbnails,
            );
        });
        card(ui, "While it's open", |ui| {
            if mac && w.replace_cmd_tab {
                hint(ui, "With Cmd+Tab, these work with Cmd still held (Cmd+Q, Cmd+W, …) — on the selected window, not the front one.");
            }
            for (keys, what) in [
                ("Q", "Quit the app"),
                ("W", "Close the window"),
                ("M", "Minimize or restore"),
                ("H", "Hide or show the app"),
                ("F", "Full screen"),
                ("Esc", "Cancel"),
            ] {
                row(ui, what, None, |ui| keycaps(ui, keys));
            }
        });
        card_with(
            ui,
            "Excluded apps",
            Some(if mac {
                "Windows of these apps are never listed. Use bundle ids (com.apple.Safari) or app names (Safari)."
            } else {
                "Windows of these apps are never listed. Use exe names (notepad.exe)."
            }),
            |ui| apps_list(ui, &mut w.excluded_apps, new_app, running, if mac { "com.example.App" } else { "app.exe" }),
        );
        if mac {
            card_with(ui, "Permissions", Some("Grant these to PowerMac.app in System Settings → Privacy & Security."), |ui| {
                if let Some(ok) = ax {
                    row(ui, "Accessibility", Some("Needed for the shortcut and to bring windows to the front."), |ui| {
                        permission_control(ui, ok, "Privacy_Accessibility")
                    });
                }
                row(ui, "Screen Recording", Some("Needed for window previews."), |ui| {
                    permission_control(ui, screen, "Privacy_ScreenCapture")
                });
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers

/// Pointing devices, one per model.
fn list_devices_deduped() -> Vec<mouse_tuning::DeviceInfo> {
    let mut v: Vec<mouse_tuning::DeviceInfo> = Vec::new();
    for d in mouse_tuning::list_devices() {
        if !v.iter().any(|x| x.same_model(&d)) {
            v.push(d);
        }
    }
    v
}

/// Acceleration and speed for one device class.
fn pointer_card(ui: &mut Ui, title: &str, desc: &str, s: &mut PointerSettings) {
    card_with(ui, title, Some(desc), |ui| {
        toggle_row(
            ui,
            "Turn off acceleration",
            Some("The pointer moves as far as you move the device, however fast you move it."),
            &mut s.disable_acceleration,
        );
        let mut set = s.speed.is_some();
        toggle_row(ui, "Set the pointer speed", Some("Otherwise the speed from System Settings is kept."), &mut set);
        if set {
            let mut v = s.speed.unwrap_or(0.5);
            let desc = if s.disable_acceleration {
                format!("{:.2}× the device's own speed.", mouse_tuning::linear_speed_factor(v))
            } else {
                "Same scale as Tracking speed in System Settings.".to_string()
            };
            row(ui, "Speed", Some(&desc), |ui| percent_slider(ui, &mut v));
            s.speed = Some(v);
        } else {
            s.speed = None;
        }
    });
}

/// A status pill plus "Open Settings…" when not granted.
pub(super) fn permission_control(ui: &mut Ui, ok: bool, pane: &str) {
    status_pill(ui, ok, if ok { "Granted" } else { "Not granted" });
    if !ok && ui.button("Open Settings…").clicked() {
        crate::platform::open_privacy_pane(pane);
    }
}

/// A small color square.
fn swatch(ui: &mut Ui, c: Rgb) {
    let p = pal(ui);
    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    ui.painter().rect(
        r,
        6.0,
        Color32::from_rgb(c.r, c.g, c.b),
        Stroke::new(1.0, p.card_border),
        StrokeKind::Inside,
    );
}

/// A tinted note above the cards.
/// A prominent on/off setting at the top of a page: an accent-tinted card
/// with an icon, title, explanation and a switch.
fn replace_banner(ui: &mut Ui, title: &str, text: &str, on: &mut bool) {
    let p = pal(ui);
    let c = p.accent;
    ui.add_space(16.0);
    egui::Frame::new()
        .fill(theme::mix(p.card, c, if *on { if p.dark { 0.20 } else { 0.10 } } else if p.dark { 0.08 } else { 0.04 }))
        .stroke(Stroke::new(1.0, theme::mix(p.card_border, c, if *on { 0.55 } else { 0.25 })))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            reset_rows(ui);
            let (_, label) = icon_row(ui, icon::COMMAND, c, title, Some(text), |ui| toggle_labeled(ui, on, title));
            if label.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && ui.is_enabled() {
                *on = !*on;
            }
        });
}

pub(super) fn callout(ui: &mut Ui, glyph: &str, title: &str, text: &str) {
    let p = pal(ui);
    let c = if glyph == icon::WARNING { Color32::from_rgb(0xE8, 0x9A, 0x00) } else { p.accent };
    ui.add_space(16.0);
    egui::Frame::new()
        .fill(theme::mix(p.card, c, if p.dark { 0.16 } else { 0.08 }))
        .stroke(Stroke::new(1.0, theme::mix(p.card_border, c, 0.4)))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon_text(glyph).size(18.0).color(c));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).font(theme::font_semibold(13.5)).color(p.text));
                    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text2)).wrap());
                });
            });
        });
}

pub(super) fn apps_list_pub(
    ui: &mut Ui,
    list: &mut Vec<String>,
    new_app: &mut String,
    running: &mut Option<Vec<(String, String)>>,
    placeholder: &str,
) {
    apps_list(ui, list, new_app, running, placeholder)
}

/// An editable list of app names / bundle ids with an "Add" field and (on
/// macOS) a menu of the running apps.
fn apps_list(
    ui: &mut Ui,
    list: &mut Vec<String>,
    new_app: &mut String,
    running: &mut Option<Vec<(String, String)>>,
    placeholder: &str,
) {
    let mut remove = None;
    for (i, a) in list.iter().enumerate() {
        if row(ui, a, None, |ui| icon_button(ui, icon::TRASH, "Remove").clicked()) {
            remove = Some(i);
        }
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    block(ui, |ui| {
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(new_app).desired_width(240.0).hint_text(placeholder));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (icon_text_button(ui, icon::PLUS, "Add").clicked() || enter) && !new_app.trim().is_empty() {
                add_excluded(list, new_app.trim());
                new_app.clear();
            }
            if cfg!(target_os = "macos") {
                let resp = ui.menu_button(theme::icon_job(icon::APP_WINDOW, "Running apps…", 13.0, pal(ui).text), |ui| {
                    let apps = running.get_or_insert_with(crate::platform::running_apps);
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for (name, id) in apps.iter() {
                            let taken = list.iter().any(|a| a.eq_ignore_ascii_case(id) || a.eq_ignore_ascii_case(name));
                            if ui
                                .add_enabled(!taken, egui::Button::new(format!("{name}  —  {id}")).frame(false))
                                .clicked()
                            {
                                add_excluded(list, id);
                                ui.close();
                            }
                        }
                        if apps.is_empty() {
                            ui.weak("No running apps found.");
                        }
                    });
                });
                if resp.response.clicked() {
                    *running = None; // refresh the list each time it opens
                }
            }
        });
    });
}

/// "English (en-US)" for common BCP-47 tags; the tag itself otherwise.
fn lang_name(tag: &str) -> String {
    let base = tag.split(['-', '_']).next().unwrap_or(tag).to_ascii_lowercase();
    let name = match base.as_str() {
        "en" => "English",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "sv" => "Swedish",
        "da" => "Danish",
        "no" | "nb" => "Norwegian",
        "fi" => "Finnish",
        "pl" => "Polish",
        "cs" => "Czech",
        "ro" => "Romanian",
        "tr" => "Turkish",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "el" => "Greek",
        "ar" | "ars" => "Arabic",
        "he" => "Hebrew",
        "hi" => "Hindi",
        "th" => "Thai",
        "vi" => "Vietnamese",
        "id" => "Indonesian",
        "ms" => "Malay",
        "ja" => "Japanese",
        "ko" => "Korean",
        "zh" if tag.contains("Hant") => "Chinese, Traditional",
        "zh" => "Chinese, Simplified",
        "yue" => "Cantonese",
        _ => return tag.to_string(),
    };
    format!("{name} ({tag})")
}
