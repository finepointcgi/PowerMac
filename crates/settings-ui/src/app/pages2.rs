//! Pages and cards of the newest modules: Resolution & HiDPI (on the
//! Displays page), Sound, Dock Previews, Text Expansion, the Caps
//! Lock card's neighbours, the link to the Notch Widgets page and the Finder basics /
//! Image Resizer cards of File Tools.
//!
//! Like Displays, the modules that need the running app (Display Scaling,
//! the audio taps) are reached through status / request files
//! (`pm_config::bridge`): the settings window never switches display
//! modes or installs taps itself. Slow one-off actions with an admin or
//! file prompt (installing a display override, choosing an espanso file)
//! run on a background thread so the window keeps drawing.

use super::*;
use super::pages::{apps_list_pub, callout, permission_control};

use audio_control::{AudioDevice, CapturePermission, DeviceKind, DeviceMatch};
use pm_config::bridge::{self, AudioRequest, MenuBarRequest, ScalingRequest};

/// Accent of the Sound page's rows.
const SOUND_TINT: Color32 = Color32::from_rgb(0xFF, 0x37, 0x5F);
const DISPLAY_TINT: Color32 = Color32::from_rgb(0xF5, 0xB2, 0x00);

impl SettingsApp {
    // -----------------------------------------------------------------------
    // Polling and background work

    /// Re-read the status files of the newer modules (once a second).
    pub(super) fn poll_new_modules(&mut self) {
        use pm_config::bridge::*;
        let p = &self.config_path;
        self.scaling_status = read_fresh(p, SCALING_STATUS_FILE, STATUS_FRESH_MS);
        self.audio_status = read_fresh(p, AUDIO_STATUS_FILE, STATUS_FRESH_MS);
        self.menubar_status = read_fresh(p, MENUBAR_STATUS_FILE, STATUS_FRESH_MS);
        self.notch_status = read_fresh(p, NOTCH_STATUS_FILE, STATUS_FRESH_MS);
        if self.page == Page::MenuBar
            && self.app.cfg.menu_bar.enabled
            && self.menubar_list_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(4))
        {
            // Keeps the app listing the items while this page is open.
            self.menubar_list_at = Some(Instant::now());
            let _ = write(p, MENUBAR_REQUEST_FILE, &MenuBarRequest::List);
        }
        self.audio_app_local.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(4));
        if self.page == Page::Sound && !self.audio_running() {
            // Read the devices here while the app doesn't run Sound.
            if self.audio_local.is_none() {
                self.audio_local = Some((
                    audio_control::list_devices(DeviceKind::Output).unwrap_or_default(),
                    audio_control::list_devices(DeviceKind::Input).unwrap_or_default(),
                ));
            }
        }
    }

    fn audio_running(&self) -> bool {
        self.audio_status.as_ref().is_some_and(|s| s.running)
    }

    /// Run `f` on another thread; its result shows under the card that
    /// started it (`what`) once it's done.
    pub(super) fn run_background(&mut self, what: &'static str, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if !self.busy.insert(what) {
            return;
        }
        self.notes.remove(what);
        let out = self.background.clone();
        std::thread::spawn(move || {
            let r = f();
            out.lock().unwrap_or_else(|e| e.into_inner()).push((what, r));
        });
    }

    /// Pick up finished background actions.
    pub(super) fn collect_background(&mut self, ctx: &egui::Context) {
        let done: Vec<_> = std::mem::take(&mut *self.background.lock().unwrap_or_else(|e| e.into_inner()));
        for (what, r) in done {
            self.busy.remove(what);
            if what == "espanso-choose" {
                if let Ok(path) = &r {
                    if !path.is_empty() {
                        self.espanso_path = path.clone();
                    }
                }
                continue;
            }
            self.notes.insert(what, r);
        }
        if !self.busy.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn note(&self, ui: &mut Ui, what: &str) {
        match self.notes.get(what) {
            Some(Ok(m)) if !m.is_empty() => {
                ok_label(ui, m);
                ui.add_space(8.0);
            }
            Some(Err(e)) => {
                error_label(ui, e);
                ui.add_space(8.0);
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Displays: Resolution & HiDPI

    /// "Keep this resolution?" banners, at the top of the Displays page.
    pub(super) fn scaling_banners(&mut self, ui: &mut Ui) {
        let Some(st) = self.scaling_status.clone().filter(|s| s.running) else { return };
        let answered_id = Id::unique("scaling-answered");
        let answered: Vec<u64> = ui.data(|d| d.get_temp(answered_id)).unwrap_or_default();
        for p in st.pending.iter().filter(|p| !answered.contains(&p.token.0)) {
            let name = st.displays.iter().find(|d| d.id == p.display_id).map(|d| d.name.clone()).unwrap_or_else(|| p.display_id.clone());
            if let Some(keep) = confirm_banner(ui, &name, p) {
                let req = if keep { ScalingRequest::Confirm { token: p.token } } else { ScalingRequest::Revert { token: p.token } };
                if let Err(e) = bridge::write(&self.config_path, bridge::SCALING_REQUEST_FILE, &req) {
                    log::warn!("display scaling request: {e:#}");
                }
                ui.data_mut(|d| d.get_temp_mut_or_default::<Vec<u64>>(answered_id).push(p.token.0));
            }
        }
    }

    /// The Resolution & HiDPI cards (one per external display), after the
    /// Displays card.
    pub(super) fn scaling_cards(&mut self, ui: &mut Ui) {
        let status = self.scaling_status.clone().filter(|s| s.running);
        let mac = cfg!(target_os = "macos");
        let mut apply: Option<(String, display_scaling::Mode)> = None;
        let mut install: Option<display_scaling::OverridePlist> = None;
        let mut remove_override: Option<(u32, u32)> = None;
        let busy_override = self.busy.contains("override");
        let new_res = self.new_device.entry("custom-res").or_default();
        let ds = &mut self.app.cfg.display_scaling;
        let timeout = ds.revert_timeout().as_secs();
        card_with(
            ui,
            "Resolution & HiDPI",
            Some(if mac {
                "Every resolution your displays offer, and HiDPI for sharp text on non-Retina monitors (a 1440p display can look like 1920×1080 at Retina sharpness)."
            } else {
                "Every resolution your displays offer. Windows scales text itself (Settings → Display → Scale)."
            }),
            |ui| {
                toggle_row(
                    ui,
                    "Manage resolutions",
                    Some("Keeps the resolutions and HiDPI settings of your external displays applied while PowerMac runs. Switching it off puts every display back."),
                    &mut ds.enabled,
                );
                if !ds.enabled {
                    return;
                }
                row(ui, "Undo a new resolution after", Some("Unless you keep it: a way back from a resolution the display can't show."), |ui| {
                    slider(ui, &mut ds.revert_timeout_secs, 5..=120, " s")
                });
                match &status {
                    None => block(ui, |ui| hint(ui, "Your displays and their resolutions appear here while PowerMac is running.")),
                    Some(st) => {
                        if mac && !st.hidpi_supported {
                            block(ui, |ui| hint(ui, "HiDPI on external displays isn't available on this version of macOS; resolutions can still be changed."));
                        }
                        if !st.displays.iter().any(|d| !d.is_builtin) {
                            block(ui, |ui| hint(ui, "No external displays connected. The built-in display's sizes are in System Settings → Displays."));
                        }
                        if let Some(e) = &st.error {
                            error_label(ui, e);
                            ui.add_space(8.0);
                        }
                    }
                }
            },
        );
        let displays: Vec<display_scaling::DisplayModes> = match (&status, ds.enabled) {
            (Some(st), true) => st.displays.iter().filter(|d| !d.is_builtin).cloned().collect(),
            _ => Vec::new(),
        };
        for (i, d) in displays.iter().enumerate() {
            ui.push_id(("scaling", i), |ui| {
                let key = display_scaling::logic::match_config(&d.id, d.vendor, d.model, &ds.displays)
                    .map(|(k, _)| k.clone())
                    .unwrap_or_else(|| d.id.clone());
                let mut dc = ds.displays.get(&key).cloned().unwrap_or_default();
                card(ui, &format!("{} — resolution", d.name), |ui| {
                    let mut desc = Vec::new();
                    if let Some(n) = d.native {
                        desc.push(format!("{}×{} panel", n.width, n.height));
                    }
                    if d.hidpi_active {
                        desc.push("HiDPI on".into());
                    } else if d.native_hidpi {
                        desc.push("Retina".into());
                    }
                    if let Some(s) = d.scale_percent.filter(|_| !mac) {
                        desc.push(format!("scale {s} %"));
                    }
                    icon_row(ui, icon::MONITOR, DISPLAY_TINT, &d.name, Some(&desc.join(" · ")), |_| {});
                    let rdesc = format!("You get {timeout} seconds to keep a new resolution; otherwise it goes back.");
                    row(ui, "Resolution", Some(&rdesc), |ui| {
                        if let Some(m) = mode_picker(ui, d) {
                            apply = Some((d.id.clone(), m));
                        }
                    });
                    if mac && d.hidpi_capable && !d.native_hidpi {
                        toggle_row(
                            ui,
                            "HiDPI",
                            Some("Draws at twice the resolution and scales down: sharper text. PowerMac adds a virtual display that this one mirrors while it's on."),
                            &mut dc.hidpi_enabled,
                        );
                        if dc.hidpi_enabled {
                            row(ui, "Looks like", Some("Bigger sizes fit more on screen; the panel's own size is the sharpest."), |ui| {
                                let mut items: Vec<(Option<display_scaling::Resolution>, String)> =
                                    vec![(None, match d.native { Some(n) => format!("{}×{} (panel size)", n.width, n.height), None => "Panel size".into() })];
                                items.extend(d.suggested_hidpi.iter().map(|r| (Some(*r), format!("{}×{}", r.width, r.height))));
                                let cur = items.iter().find(|(v, _)| *v == dc.target_logical_resolution).map(|(_, t)| t.clone()).unwrap_or_else(|| "?".into());
                                egui::ComboBox::from_id_salt("hidpi-target").selected_text(cur).width(200.0).height(320.0).show_ui(ui, |ui| {
                                    for (v, t) in &items {
                                        ui.selectable_value(&mut dc.target_logical_resolution, *v, t);
                                    }
                                });
                            });
                            row(ui, "Custom sizes", Some("Extra “looks like” sizes, as width×height (e.g. 2200×1238)."), |ui| {
                                let mut remove = None;
                                for (j, r) in dc.custom_resolutions.iter().enumerate() {
                                    let p = pal(ui);
                                    egui::Frame::new()
                                        .fill(p.accent_soft)
                                        .corner_radius(11.0)
                                        .inner_margin(egui::Margin { left: 9, right: 4, top: 2, bottom: 2 })
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.spacing_mut().item_spacing.x = 2.0;
                                                ui.label(RichText::new(format!("{}×{}", r.width, r.height)).size(12.0).color(p.text));
                                                if ui.add(egui::Button::new(theme::icon_text(icon::X).size(11.0).color(p.text2)).frame(false)).on_hover_text("Remove").clicked() {
                                                    remove = Some(j);
                                                }
                                            });
                                        });
                                }
                                if let Some(j) = remove {
                                    dc.custom_resolutions.remove(j);
                                }
                                let r = ui.add(egui::TextEdit::singleline(new_res).hint_text("2200×1238").desired_width(90.0));
                                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let parsed = parse_resolution(new_res);
                                let add = ui.add_enabled(parsed.is_some(), egui::Button::new("Add")).clicked() || enter;
                                if let Some(r) = parsed.filter(|_| add) {
                                    if !dc.custom_resolutions.contains(&r) {
                                        dc.custom_resolutions.push(r);
                                    }
                                    new_res.clear();
                                }
                            });
                        }
                    }
                    if mac && !d.native_hidpi && d.vendor != 0 {
                        let installed = display_scaling::override_installed(d.vendor, d.model);
                        row(
                            ui,
                            "Permanent HiDPI override",
                            Some("Alternative to the virtual display: a display override file in /Library/Displays makes macOS itself offer HiDPI sizes, even without PowerMac. Needs an administrator password; takes effect after you reconnect the display or restart."),
                            |ui| {
                                if installed {
                                    status_pill(ui, true, "Installed");
                                    if ui.add_enabled(!busy_override, egui::Button::new("Remove (admin)…")).clicked() {
                                        remove_override = Some((d.vendor, d.model));
                                    }
                                } else if ui.add_enabled(!busy_override, egui::Button::new("Install permanent HiDPI override (admin)…")).clicked() {
                                    let mut sizes: Vec<display_scaling::Resolution> = d.suggested_hidpi.clone();
                                    sizes.extend(dc.custom_resolutions.iter().copied().filter(|r| !d.suggested_hidpi.contains(r)));
                                    install = Some(display_scaling::generate_override(d.vendor, d.model, &d.name, &sizes));
                                }
                            },
                        );
                    }
                });
                if dc == display_scaling::DisplayConfig::default() {
                    ds.displays.remove(&key);
                } else if ds.displays.get(&key) != Some(&dc) {
                    ds.displays.insert(key, dc);
                }
            });
        }
        self.note(ui, "override");
        if let Some((id, mode)) = apply {
            let req = ScalingRequest::Apply { display_id: id, mode, permanent: false };
            if let Err(e) = bridge::write(&self.config_path, bridge::SCALING_REQUEST_FILE, &req) {
                log::warn!("display scaling request: {e:#}");
            }
        }
        if let Some(o) = install {
            self.run_background("override", move || {
                display_scaling::install_override_with_admin(&o)
                    .map(|()| "Override installed. Reconnect the display (or restart) to get the new sizes.".to_string())
                    .map_err(|e| format!("{e:#}"))
            });
        }
        if let Some((v, m)) = remove_override {
            self.run_background("override", move || {
                display_scaling::remove_override_with_admin(v, m).map(|()| "Override removed.".to_string()).map_err(|e| format!("{e:#}"))
            });
        }
    }

    // -----------------------------------------------------------------------
    // Sound

    pub(super) fn sound(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let running = self.audio_running();
        let status = self.audio_status.clone().filter(|s| s.running);
        let (outputs, inputs) = match &status {
            Some(s) => (s.outputs.clone(), s.inputs.clone()),
            None => self.audio_local.clone().unwrap_or_default(),
        };
        let on = self.app.cfg.audio.enabled;
        let mut set_default: Option<(DeviceKind, String)> = None;
        let mut refresh = false;

        if on && !running {
            callout(
                ui,
                icon::INFO,
                "PowerMac isn't running",
                "The devices below are read directly. The rules, the device switching and per-app volume work while PowerMac runs.",
            );
        }
        for (kind, title, list) in [(DeviceKind::Output, "Output", &outputs), (DeviceKind::Input, "Input", &inputs)] {
            card_with(
                ui,
                title,
                Some(if kind == DeviceKind::Output { "Where sound plays. Click Use to switch." } else { "The microphone apps record from." }),
                |ui| {
                    if list.is_empty() {
                        block(ui, |ui| hint(ui, "No devices found."));
                    }
                    for (i, d) in list.iter().enumerate() {
                        ui.push_id((title, i), |ui| {
                            let glyph = if kind == DeviceKind::Output { icon::SPEAKER_HIGH } else { icon::RECORD };
                            let mut desc = d.transport.label().to_string();
                            if let Some(v) = d.volume {
                                desc.push_str(&format!(" · volume {:.0} %", v * 100.0));
                            }
                            if d.muted == Some(true) {
                                desc.push_str(" · muted");
                            }
                            icon_row(ui, glyph, SOUND_TINT, &d.name, Some(&desc), |ui| {
                                if d.is_default {
                                    tag_pill(ui, pal(ui).ok, icon::CHECK, "In use");
                                } else if ui.button("Use").clicked() {
                                    set_default = Some((kind, d.uid.clone()));
                                }
                            });
                        });
                    }
                    if !running {
                        block(ui, |ui| {
                            if icon_text_button(ui, icon::ARROWS_CLOCKWISE, "Refresh").clicked() {
                                refresh = true;
                            }
                        });
                    }
                },
            );
        }

        let a = &mut self.app.cfg.audio;
        let new_device = &mut self.new_device;
        for (key, title, sub, which) in [
            ("prio-out", "Preferred outputs", "When devices come and go, the first one here that is connected becomes the output. Leave empty to let macOS decide.", 0),
            ("prio-in", "Preferred microphones", "The first one here that is connected becomes the input.", 1),
            ("never", "Never switch to automatically", "When the system switches to one of these by itself (AirPods connecting, a display with speakers), PowerMac switches back at once. Choosing one yourself still works.", 2),
        ] {
            let devices: &[AudioDevice] = if which == 1 { &inputs } else { &outputs };
            let list = match which {
                0 => &mut a.rules.outputs,
                1 => &mut a.rules.inputs,
                _ => &mut a.block_auto_switch_to,
            };
            let text = new_device.entry(key).or_default();
            card_with(ui, title, Some(sub), |ui| {
                ui.add_enabled_ui(a.enabled, |ui| device_rule_list(ui, key, list, devices, text, which != 2));
            });
        }
        card(ui, "Microphone with Bluetooth headphones", |ui| {
            ui.add_enabled_ui(a.enabled, |ui| {
                toggle_row(
                    ui,
                    "Keep the built-in microphone",
                    Some("When Bluetooth headphones connect, the Mac's own microphone stays the input, so the headphones keep playing in high quality instead of switching to the call (headset) mode."),
                    &mut a.keep_builtin_mic_with_bluetooth,
                );
            });
        });

        // Per-app volume
        let supported = status.as_ref().map(|s| s.per_app_supported).unwrap_or_else(audio_control::per_app_volume_supported);
        let permission = status.as_ref().map(|s| s.capture_permission).unwrap_or_else(audio_control::capture_permission);
        let apps = status.as_ref().map(|s| s.apps.clone()).unwrap_or_default();
        let local = &self.audio_app_local;
        let mut level_req: Vec<AudioRequest> = Vec::new();
        card_with(
            ui,
            "Per-app volume (experimental)",
            Some(if mac {
                "A volume for each app, up to 150 %. Uses Core Audio process taps (macOS 14.4 or later), which need the System Audio Recording permission."
            } else {
                "A volume for each app, like the Windows volume mixer."
            }),
            |ui| {
                ui.add_enabled_ui(a.enabled && supported, |ui| {
                    toggle_row(ui, "Per-app volume", None, &mut a.per_app_volume);
                });
                if !supported {
                    block(ui, |ui| hint(ui, "Needs macOS 14.4 or later."));
                    return;
                }
                if mac {
                    row(ui, "System Audio Recording", Some("Lets PowerMac route each app's sound through its volume control. Nothing is recorded."), |ui| {
                        let (ok, text) = match permission {
                            CapturePermission::Granted => (true, "Granted"),
                            CapturePermission::Denied => (false, "Denied"),
                            CapturePermission::NotDetermined => (false, "Not asked yet"),
                            CapturePermission::NotApplicable => (true, "Not needed"),
                        };
                        status_pill(ui, ok, text);
                        if permission == CapturePermission::NotDetermined
                            && ui.add_enabled(running, egui::Button::new("Ask…")).on_hover_text("Shows the macOS permission prompt (PowerMac must be running)").clicked()
                        {
                            level_req.push(AudioRequest::RequestPermission);
                        }
                        if permission == CapturePermission::Denied && ui.button("Open Settings…").clicked() {
                            crate::platform::open_privacy_pane("Privacy_ScreenCapture");
                        }
                    });
                }
                if !a.per_app_volume {
                    return;
                }
                if !running {
                    block(ui, |ui| hint(ui, "Apps playing sound appear here while PowerMac is running."));
                } else if apps.is_empty() {
                    block(ui, |ui| hint(ui, "No app is playing sound right now. Apps appear here when they do."));
                }
                for (i, app) in apps.iter().enumerate() {
                    ui.push_id(("app", i), |ui| {
                        let saved = a.app_volumes.get(&app.bundle_id).copied().unwrap_or_default();
                        let mut pct = local.get(&app.bundle_id).map(|(v, _)| *v).unwrap_or(app.volume);
                        let mut pct_u = (pct * 100.0).round().clamp(0.0, 150.0) as u32;
                        let mut muted = app.muted || saved.muted;
                        let desc = format!("{}{}", app.bundle_id, if app.playing { " · playing" } else { "" });
                        icon_row(ui, icon::SPEAKER_HIGH, SOUND_TINT, &app.name, Some(&desc), |ui| {
                            let r = slider(ui, &mut pct_u, 0..=150, " %");
                            if r.changed() {
                                pct = pct_u as f32 / 100.0;
                                level_req.push(AudioRequest::AppVolume { bundle_id: app.bundle_id.clone(), volume: pct });
                            }
                            let glyph = if muted { icon::SPEAKER_X } else { icon::SPEAKER_HIGH };
                            if icon_button(ui, glyph, if muted { "Unmute" } else { "Mute" }).clicked() {
                                muted = !muted;
                                level_req.push(AudioRequest::AppMute { bundle_id: app.bundle_id.clone(), muted });
                            }
                        });
                    });
                }
                let gone: Vec<String> = a.app_volumes.keys().filter(|b| !apps.iter().any(|x| &&x.bundle_id == b)).cloned().collect();
                if !gone.is_empty() {
                    block(ui, |ui| hint(ui, "Saved levels of apps that aren't playing now:"));
                    let mut reset = None;
                    for b in &gone {
                        let l = a.app_volumes[b];
                        let what = if l.muted { "muted".to_string() } else { format!("{:.0} %", l.volume * 100.0) };
                        if row(ui, b, Some(&what), |ui| ui.button("Reset").clicked()) {
                            reset = Some(b.clone());
                        }
                    }
                    if let Some(b) = reset {
                        a.app_volumes.remove(&b);
                    }
                }
            },
        );
        if let Some(e) = status.as_ref().and_then(|s| s.error.clone()) {
            warn_label(ui, &e);
        }
        for r in level_req {
            if let AudioRequest::AppVolume { bundle_id, volume } = &r {
                self.audio_app_local.insert(bundle_id.clone(), (*volume, Instant::now()));
                // Saved by the app too; keep the draft in step so a later save
                // doesn't put the old level back.
                let l = self.app.cfg.audio.app_volumes.entry(bundle_id.clone()).or_default();
                l.volume = *volume;
                if l.is_neutral() {
                    self.app.cfg.audio.app_volumes.remove(bundle_id);
                }
            }
            if let AudioRequest::AppMute { bundle_id, muted } = &r {
                let l = self.app.cfg.audio.app_volumes.entry(bundle_id.clone()).or_default();
                l.muted = *muted;
                if l.is_neutral() {
                    self.app.cfg.audio.app_volumes.remove(bundle_id);
                }
            }
            self.queue_audio_request(r);
        }
        if let Some((kind, uid)) = set_default {
            if running {
                self.queue_audio_request(AudioRequest::SetDefault { kind, uid });
                self.flush_audio_request(true);
            } else {
                if let Err(e) = audio_control::set_default_device(kind, &uid) {
                    log::warn!("sound: {e:#}");
                }
                refresh = true;
            }
        }
        if refresh {
            self.audio_local = None;
            self.poll_new_modules();
        }
    }

    fn queue_audio_request(&mut self, r: AudioRequest) {
        // Slider drags replace each other; anything else goes out at once.
        let same = matches!((&self.audio_pending, &r),
            (Some(AudioRequest::AppVolume { bundle_id: a, .. }), AudioRequest::AppVolume { bundle_id: b, .. }) if a == b);
        if !same {
            self.flush_audio_request(true);
        }
        let urgent = !matches!(r, AudioRequest::AppVolume { .. });
        self.audio_pending = Some(r);
        if urgent {
            self.flush_audio_request(true);
        }
    }

    /// Write the pending audio request (at most every 70 ms unless `force`).
    pub(super) fn flush_audio_request(&mut self, force: bool) {
        if self.audio_pending.is_none() || (!force && self.audio_written.elapsed() < Duration::from_millis(70)) {
            return;
        }
        if let Some(r) = self.audio_pending.take() {
            if let Err(e) = bridge::write(&self.config_path, bridge::AUDIO_REQUEST_FILE, &r) {
                log::warn!("sound request: {e:#}");
            }
            self.audio_written = Instant::now();
        }
    }

    // -----------------------------------------------------------------------
    // Menu Bar

    pub(super) fn menu_bar(&mut self, ui: &mut Ui) {
        use menubar_manager::Section;
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let status = self.menubar_status.clone().filter(|s| s.running);
        let mut req: Option<MenuBarRequest> = None;
        if !mac {
            callout(ui, icon::INFO, "Not needed on Windows", "The notification area already hides icons behind its ^ button; drag icons in or out of it to arrange them.");
        }
        if let Some(st) = &status {
            if !st.hidden_by_notch.is_empty() {
                let names: Vec<String> = st.hidden_by_notch.iter().map(item_name).collect();
                callout(
                    ui,
                    icon::WARNING,
                    "Some icons are behind the notch",
                    &format!("{} can't be seen. Move icons you don't need to Hidden below, or turn on “Use the bar under the menu bar”.", names.join(", ")),
                );
            }
            if let Some(issue) = &st.layout_issue {
                callout(ui, icon::WARNING, "PowerMac's separators are out of order", issue);
            }
        }
        let m = &mut self.app.cfg.menu_bar;
        card_with(
            ui,
            "How it works",
            Some("PowerMac adds a chevron (‹) to the menu bar. Icons left of its separator are hidden until you click the chevron; Option-click also shows the always-hidden ones. Cmd-drag icons across the separators to arrange them, or use the buttons below."),
            |ui| {
                toggle_row(ui, "Always-hidden section", Some("A second separator: icons left of it only show on Option-click."), &mut m.always_hidden_section);
                toggle_row(ui, "Show on hover", Some("Point at an empty part of the menu bar to show the hidden icons."), &mut m.show_on_hover);
                ui.add_enabled_ui(m.show_on_hover, |ui| {
                    row(ui, "Wait before showing", None, |ui| {
                        ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut m.hover_delay_ms).range(0..=3000).suffix(" ms"))
                    });
                });
                toggle_row(ui, "Hide again automatically", None, &mut m.auto_rehide);
                ui.add_enabled_ui(m.auto_rehide, |ui| {
                    row(ui, "After", Some("Without menu bar activity."), |ui| slider(ui, &mut m.rehide_delay_secs, 1..=120, " s"));
                    toggle_row(ui, "Also when you switch apps", None, &mut m.rehide_on_focus_change);
                });
            },
        );
        card(ui, "Appearance", |ui| {
            toggle_row(
                ui,
                "Use a bar under the menu bar",
                Some("Shows the hidden icons in a floating bar instead of in the menu bar. Other apps' icons need Screen Recording on macOS 15 and older."),
                &mut m.use_floating_bar,
            );
            ui.add_enabled_ui(!m.use_floating_bar, |ui| {
                toggle_row(ui, "Use the bar when icons would end up behind the notch", None, &mut m.notch_fallback_to_bar);
            });
            toggle_row(ui, "Show the separators", Some("Thin lines between the sections while they're shown."), &mut m.show_separators);
            toggle_row(ui, "Animate", None, &mut m.animate);
        });
        let on = m.enabled;
        let always_hidden = m.always_hidden_section;
        card_with(
            ui,
            "Icons",
            Some(if mac && status.as_ref().is_some_and(|s| s.fitted_mode) {
                "Move an icon to another section. On macOS 26 you can also Cmd-drag it across the separators."
            } else {
                "Move an icon to another section, or Cmd-drag it across the separators."
            }),
            |ui| {
                let Some(st) = &status else {
                    block(ui, |ui| hint(ui, if on { "The menu bar icons appear here while PowerMac is running." } else { "Turn the menu bar manager on above to arrange the icons." }));
                    return;
                };
                if !st.accessibility {
                    row(ui, "Accessibility", Some("Needed to move icons from here (and to list other apps' icons on macOS 26)."), |ui| {
                        permission_control(ui, false, "Privacy_Accessibility")
                    });
                }
                if !st.listed {
                    block(ui, |ui| hint(ui, "Listing the icons…"));
                    return;
                }
                let sections: &[(Section, &str)] = if always_hidden {
                    &[(Section::Visible, "Visible"), (Section::Hidden, "Hidden"), (Section::AlwaysHidden, "Always hidden")]
                } else {
                    &[(Section::Visible, "Visible"), (Section::Hidden, "Hidden")]
                };
                for (sec, title) in sections {
                    let items: Vec<&menubar_manager::MenuBarItem> = st.items.iter().filter(|i| i.section == *sec && !i.own).collect();
                    block(ui, |ui| {
                        ui.label(RichText::new(format!("{title} · {}", items.len())).font(theme::font_semibold(13.0)).color(pal(ui).text));
                    });
                    for (i, it) in items.iter().enumerate() {
                        ui.push_id((title, i), |ui| {
                            let mut desc = it.bundle_id.clone().unwrap_or_else(|| it.owner.clone());
                            if it.system {
                                desc.push_str(" · system");
                            }
                            if st.hidden_by_notch.iter().any(|h| h.id == it.id) {
                                desc.push_str(" · behind the notch");
                            }
                            row(ui, &item_name(it), Some(&desc), |ui| {
                                ui.add_enabled_ui(st.accessibility, |ui| {
                                    for (to, _) in sections.iter() {
                                        let label = match to {
                                            Section::Visible => "Show",
                                            Section::Hidden => "Hide",
                                            Section::AlwaysHidden => "Always hide",
                                        };
                                        if to != sec && ui.button(label).on_hover_text(format!("Move it to {}", match to {
                                            Section::Visible => "the visible icons",
                                            Section::Hidden => "the hidden icons",
                                            Section::AlwaysHidden => "the always-hidden icons",
                                        })).clicked() {
                                            req = Some(MenuBarRequest::Move { item_id: it.id.clone(), section: *to });
                                        }
                                    }
                                });
                            });
                        });
                    }
                    if items.is_empty() {
                        block(ui, |ui| hint(ui, "None."));
                    }
                }
                if let Some(e) = &st.error {
                    error_label(ui, e);
                    ui.add_space(8.0);
                }
            },
        );
        if let Some(r) = req {
            if let Err(e) = bridge::write(&self.config_path, bridge::MENUBAR_REQUEST_FILE, &r) {
                log::warn!("menu bar request: {e:#}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Dock Previews

    pub(super) fn dock_previews(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let perms = dock_previews::permissions();
        let d = &mut self.app.cfg.dock_previews;
        if !mac {
            callout(ui, icon::INFO, "Not needed on Windows", "The taskbar already shows window thumbnails when you point at an app.");
        }
        card(ui, "Previews", |ui| {
            row(ui, "Show after", Some("How long the pointer rests on an icon first."), |ui| slider(ui, &mut d.hover_delay_ms, 0..=2000, " ms"));
            row(ui, "Preview size", None, |ui| {
                let items: Vec<(dock_previews::PreviewSize, &str)> = dock_previews::PreviewSize::ALL.iter().map(|s| (*s, s.label())).collect();
                segmented(ui, &mut d.preview_size, &items)
            });
            toggle_row(ui, "Also for apps with one window", Some("Otherwise only apps with two or more windows get a preview."), &mut d.show_single_window);
        });
        card(ui, "In the preview", |ui| {
            for (keys, what) in [
                ("Click", "Bring that window forward"),
                ("W", "Close the window under the pointer"),
                ("M", "Minimize or restore it"),
                ("Scroll", "Go through the windows"),
                ("Esc", "Close the preview"),
            ] {
                row(ui, what, None, |ui| keycaps(ui, keys));
            }
        });
        card(ui, "Clicking the Dock", |ui| {
            toggle_row(
                ui,
                "Click the front app's icon to minimize or switch windows",
                Some("Like the Windows taskbar: one window is minimized, several are cycled through. Changes what a Dock click does, so it's off by default."),
                &mut d.click_to_cycle,
            );
        });
        if mac {
            card_with(ui, "Permissions", Some("Grant these to PowerMac.app in System Settings → Privacy & Security."), |ui| {
                row(ui, "Accessibility", Some("Needed to read the Dock and bring windows forward."), |ui| permission_control(ui, perms.accessibility, "Privacy_Accessibility"));
                row(ui, "Screen Recording", Some("Needed for window thumbnails; without it the previews show app icons and titles."), |ui| {
                    permission_control(ui, perms.screen_recording, "Privacy_ScreenCapture")
                });
            });
        }
    }

    // -----------------------------------------------------------------------
    // Text Expansion

    pub(super) fn text_expander(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let ax = self.perms.map(|p| p.0);
        let open = &mut self.snippet_open;
        let t = &mut self.app.cfg.text_expander;
        card_with(
            ui,
            "Snippets",
            Some("Type the abbreviation anywhere and it is replaced as soon as it's complete. Starting abbreviations with ; keeps them from firing in normal words."),
            |ui| {
                let mut remove = None;
                let mut toggle_open = None;
                for (i, sn) in t.snippets.iter_mut().enumerate() {
                    ui.push_id(("snippet", i), |ui| {
                        block(ui, |ui| {
                            let p = pal(ui);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                toggle_labeled(ui, &mut sn.enabled, &format!("Snippet {}", i + 1));
                                ui.add(
                                    egui::TextEdit::singleline(&mut sn.trigger)
                                        .hint_text(";abbr")
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(110.0),
                                );
                                ui.label(theme::icon_text(icon::CARET_RIGHT).size(13.0).color(p.text3));
                                let w = (ui.available_width() - 92.0).max(120.0);
                                ui.add(
                                    egui::TextEdit::multiline(&mut sn.replacement)
                                        .hint_text("What it becomes")
                                        .desired_rows(1)
                                        .desired_width(w),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                        remove = Some(i);
                                    }
                                    let is_open = *open == Some(i);
                                    if icon_button(ui, if is_open { icon::CARET_DOWN } else { icon::SLIDERS_HORIZONTAL }, "Options").clicked() {
                                        toggle_open = Some(i);
                                    }
                                });
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(52.0);
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let preview = text_expander::expand_preview(sn);
                                let shown: String = preview.lines().next().unwrap_or("").chars().take(70).collect();
                                let more = preview.lines().count() > 1 || preview.chars().count() > 70;
                                ui.label(RichText::new("Preview").size(11.5).color(p.text3));
                                ui.label(RichText::new(format!("{shown}{}", if more { " …" } else { "" })).size(12.0).color(p.text2));
                                for chip in snippet_chips(sn) {
                                    tag_pill(ui, p.accent, icon::INFO, &chip);
                                }
                            });
                            if *open == Some(i) {
                                ui.add_space(4.0);
                                let mut any_case = !sn.case_sensitive;
                                if toggle_row(ui, "Any case", Some("“;Addr” capitalizes the result, “;ADDR” writes it in capitals."), &mut any_case).changed() {
                                    sn.case_sensitive = !any_case;
                                }
                                toggle_row(ui, "Whole word only", Some("Expands when a space, Return or punctuation follows, and only at the start of a word."), &mut sn.word_boundary);
                                toggle_row(ui, "Paste instead of typing", Some("Faster for long text; the clipboard is put back afterwards."), &mut sn.paste);
                                row(ui, "Only in these apps", Some("Bundle ids or app names, separated by commas. Empty: everywhere."), |ui| {
                                    let mut text = sn.apps.join(", ");
                                    if ui.add(egui::TextEdit::singleline(&mut text).hint_text("com.apple.mail").desired_width(240.0)).changed() {
                                        sn.apps = text.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
                                    }
                                });
                                row(ui, "Note", None, |ui| {
                                    ui.add(egui::TextEdit::singleline(&mut sn.label).desired_width(240.0))
                                });
                            }
                        });
                    });
                }
                if let Some(i) = toggle_open {
                    *open = if *open == Some(i) { None } else { Some(i) };
                }
                if let Some(i) = remove {
                    t.snippets.remove(i);
                    *open = None;
                }
                if t.snippets.is_empty() {
                    block(ui, |ui| hint(ui, "No snippets yet."));
                }
                block(ui, |ui| {
                    if icon_text_button(ui, icon::PLUS, "Add snippet").clicked() {
                        t.snippets.push(text_expander::Snippet::new("", ""));
                        *open = Some(t.snippets.len() - 1);
                    }
                });
                ui.collapsing(RichText::new("Variables").size(12.5), |ui| {
                    let p = pal(ui);
                    egui::Grid::new("te-vars").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                        for (v, what) in [
                            ("{date}", "today, 2026-10-26"),
                            ("{date:%d.%m.%Y}", "today in your format (strftime)"),
                            ("{time}", "now, 14:05"),
                            ("{clipboard}", "the text on the clipboard"),
                            ("{cursor}", "where the caret ends up"),
                            ("{uuid}", "a new UUID"),
                            ("{random:6}", "a number from 1 to 6 ({random:10-99} for a range)"),
                            ("{{ and }}", "literal braces"),
                        ] {
                            ui.label(RichText::new(v).monospace().color(p.text));
                            ui.label(RichText::new(what).size(12.0).color(p.text2));
                            ui.end_row();
                        }
                    });
                });
            },
        );
        for w in t.validate() {
            warn_label(ui, &w);
        }
        let te_on = t.enabled;

        // espanso import
        let busy = self.busy.contains("espanso-choose");
        let mut import = false;
        let mut choose = false;
        card_with(
            ui,
            "Import from espanso",
            Some("Adds the matches of an espanso match file (match/base.yml) to your snippets. Forms, scripts and regex triggers are skipped."),
            |ui| {
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.espanso_path).hint_text("~/Library/Application Support/espanso/match/base.yml").desired_width(330.0));
                        if mac && ui.add_enabled(!busy, egui::Button::new("Choose…")).clicked() {
                            choose = true;
                        }
                        if ui.add_enabled(!self.espanso_path.trim().is_empty(), egui::Button::new("Import")).clicked() {
                            import = true;
                        }
                    });
                    match &self.espanso_result {
                        Some((true, m)) => ok_label(ui, m),
                        Some((false, m)) => error_label(ui, m),
                        None => {}
                    }
                });
            },
        );
        if choose {
            self.run_background("espanso-choose", choose_file);
        }
        if import {
            self.espanso_result = Some(import_espanso(&self.espanso_path, &mut self.app.cfg.text_expander));
        }

        let new_app = self.new_app.entry("text-expander").or_default();
        let running = &mut self.running_apps;
        let t = &mut self.app.cfg.text_expander;
        card(ui, "Typing", |ui| {
            toggle_row(ui, "Backspace undoes an expansion", Some("Right after it, Backspace puts the abbreviation back."), &mut t.undo_with_backspace);
            row(ui, "Paste replacements longer than", Some("Typing very long text is slow; these go through the clipboard instead."), |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut t.paste_threshold).range(1..=10_000).suffix(" chars"))
            });
            row(ui, "Put the clipboard back after", None, |ui| {
                ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut t.clipboard_restore_delay_ms).range(50..=5_000).suffix(" ms"))
            });
        });
        card_with(
            ui,
            "Excluded apps",
            Some("Nothing expands in these apps. Password managers are listed by default; nothing is read while a password field has focus either."),
            |ui| {
                toggle_row(ui, "Also in terminals", Some("Terminal, iTerm, Warp, kitty, Ghostty, Windows Terminal, …"), &mut t.exclude_terminals);
                apps_list_pub(ui, &mut t.excluded_apps, new_app, running, if mac { "com.example.App" } else { "app.exe" });
            },
        );
        if mac {
            if let Some(ok) = ax {
                card(ui, "Permission", |ui| {
                    row(ui, "Accessibility", Some("Needed to see what you type and to type the replacement."), |ui| permission_control(ui, ok, "Privacy_Accessibility"));
                });
            }
        }
        let _ = te_on;
    }

    // -----------------------------------------------------------------------
    // Notch Shelf: a link to the Notch Widgets page

    pub(super) fn notch_widgets(&mut self, ui: &mut Ui) {
        let n = &self.app.cfg.notch_shelf;
        let pages = n.page_list();
        let on = n.widgets.iter().filter(|w| w.enabled && w.kind != notch_shelf::WidgetKind::Unknown).count();
        let mut go = false;
        card_with(
            ui,
            "Widgets",
            Some("The open notch's Home view: clocks, scripts, web data, Shortcuts, launchers and more, on pages you can swipe between."),
            |ui| {
                let desc = format!(
                    "{} on {}",
                    plural(on, "widget"),
                    if pages.len() == 1 { "one page".to_string() } else { plural(pages.len(), "page") }
                );
                let (clicked, label) = icon_row(ui, icon::LAYOUT, Color32::from_rgb(0xAF, 0x52, 0xDE), "Notch Widgets", Some(&desc), |ui| {
                    primary_button(ui, "Edit widgets…").clicked()
                });
                go = clicked || label.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
            },
        );
        if go {
            self.go(Page::NotchWidgets);
        }
    }

    // -----------------------------------------------------------------------
    // File Tools: Finder basics and Image Resizer

    pub(super) fn finder_cards(&mut self, ui: &mut Ui) {
        use file_tools::finder::apps::AppKind;
        let mac = cfg!(target_os = "macos");
        let (terms, eds) = self
            .finder_apps
            .get_or_insert_with(|| {
                (file_tools::finder::apps::installed_apps(AppKind::Terminal), file_tools::finder::apps::installed_apps(AppKind::Editor))
            })
            .clone();
        let f = &mut self.ft.finder;
        let where_ = if mac { "Finder" } else { "Explorer" };
        card_with(
            ui,
            &format!("{where_} basics"),
            Some("Used by the Quick Actions (Open in Terminal, Open in Editor, Copy Path), the tray, and powermac finder."),
            |ui| {
                for (label, kind, list, value) in [
                    ("Terminal", AppKind::Terminal, &terms, &mut f.terminal_app),
                    ("Editor", AppKind::Editor, &eds, &mut f.editor_app),
                ] {
                    row(ui, label, Some(&format!("Default: {}", file_tools::finder::apps::app_name(file_tools::finder::apps::default_app(kind)))), |ui| {
                        let cur = if value.is_empty() { "Automatic".to_string() } else { file_tools::finder::apps::app_name(value) };
                        egui::ComboBox::from_id_salt(("finder-app", label)).selected_text(cur).width(200.0).show_ui(ui, |ui| {
                            ui.selectable_value(value, String::new(), "Automatic");
                            for a in list.iter() {
                                ui.selectable_value(value, a.id.clone(), &a.name);
                            }
                            if list.is_empty() {
                                ui.weak("None of the known apps found");
                            }
                        });
                    });
                }
                row(ui, "Copy Path copies", Some(&format!("e.g. {}", copy_example(f.path_style))), |ui| {
                    let items: Vec<(file_tools::CopyStyle, &str)> = file_tools::CopyStyle::ALL.iter().map(|s| (*s, s.label())).collect();
                    combo(ui, "copy-style", &mut f.path_style, &items)
                });
                if mac {
                    toggle_row(
                        ui,
                        "Use the front Finder window",
                        Some("Lets powermac finder and keyboard shortcuts act on Finder's selection or front window when no files are given. macOS asks once for permission to control Finder (Automation)."),
                        &mut f.use_front_finder_window,
                    );
                }
                toggle_row(ui, "Eject all: include disk images", Some("Mounted .dmg files."), &mut f.eject_disk_images);
                toggle_row(ui, "Eject all: include network volumes", Some("SMB and AFP shares."), &mut f.eject_network_volumes);
                row(ui, "From a terminal", Some("powermac finder open-terminal · copy-path · eject-all · show-hidden · who-uses <path>"), |_| {});
            },
        );

        let r = &mut self.ft.resize;
        card_with(
            ui,
            "Image Resizer",
            Some("Resize pictures from the “Resize Images” Quick Action or powermac resize. These are the sizes it offers and how it saves."),
            |ui| {
                use file_tools::resize::{OutputFormat, ResizeFit, ResizeSize, ResizeUnit};
                let mut remove = None;
                for (i, s) in r.presets.iter_mut().enumerate() {
                    ui.push_id(("preset", i), |ui| {
                        block(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.add(egui::TextEdit::singleline(&mut s.name).hint_text("Name").desired_width(100.0));
                                egui::ComboBox::from_id_salt("fit").selected_text(s.fit.label()).width(78.0).show_ui(ui, |ui| {
                                    for v in [ResizeFit::Fit, ResizeFit::Fill, ResizeFit::Stretch] {
                                        ui.selectable_value(&mut s.fit, v, v.label());
                                    }
                                });
                                ui.add_sized(vec2(64.0, 24.0), egui::DragValue::new(&mut s.width).range(0.0..=100_000.0).max_decimals(2));
                                ui.label(RichText::new("×").color(pal(ui).text3));
                                ui.add_enabled_ui(!(s.unit == ResizeUnit::Percent && s.fit != ResizeFit::Stretch), |ui| {
                                    ui.add_sized(vec2(64.0, 24.0), egui::DragValue::new(&mut s.height).range(0.0..=100_000.0).max_decimals(2));
                                });
                                egui::ComboBox::from_id_salt("unit").selected_text(s.unit.short()).width(56.0).show_ui(ui, |ui| {
                                    for v in ResizeUnit::ALL {
                                        ui.selectable_value(&mut s.unit, v, v.label());
                                    }
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                        remove = Some(i);
                                    }
                                    ui.label(RichText::new(s.describe()).size(12.0).color(pal(ui).text2));
                                });
                            });
                        });
                    });
                }
                if let Some(i) = remove {
                    r.presets.remove(i);
                    if r.selected >= r.presets.len() && r.selected > 0 {
                        r.selected = r.selected.min(r.presets.len());
                    }
                }
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        if icon_text_button(ui, icon::PLUS, "Add size").clicked() {
                            r.presets.push(ResizeSize::new(&format!("Size {}", r.presets.len() + 1), ResizeFit::Fit, 1280.0, 720.0, ResizeUnit::Pixel));
                        }
                        hint(ui, "0 = automatic (keeps the aspect ratio).");
                    });
                });
                let o = &mut r.options;
                toggle_row(ui, "Make pictures smaller but not larger", None, &mut o.shrink_only);
                toggle_row(ui, "Ignore the orientation of pictures", Some("A landscape size also fits portrait pictures the other way round."), &mut o.ignore_orientation);
                toggle_row(
                    ui,
                    "Replace the originals",
                    Some(if mac { "The originals go to the Trash. Otherwise new files are made next to them." } else { "Otherwise new files are made next to them." }),
                    &mut o.replace,
                );
                ui.add_enabled_ui(!o.replace, |ui| {
                    row(ui, "File name", Some("%1 original name · %2 size name · %3×%4 chosen size · %5×%6 actual size"), |ui| {
                        ui.add(egui::TextEdit::singleline(&mut o.file_name_format).font(egui::TextStyle::Monospace).desired_width(160.0))
                    });
                });
                row(ui, "Save as", None, |ui| {
                    let items: Vec<(OutputFormat, &str)> = file_tools::resize::available_formats().into_iter().map(|f| (f, f.label())).collect();
                    combo(ui, "resize-format", &mut o.format, &items)
                });
                row(ui, "Quality", Some("JPEG, HEIC and WebP."), |ui| slider(ui, &mut o.jpeg_quality, 1..=100, " %"));
                toggle_row(ui, "Keep metadata", Some("EXIF, IPTC and XMP: camera, date, captions."), &mut o.keep_metadata);
                ui.add_enabled_ui(o.keep_metadata, |ui| {
                    toggle_row(ui, "Remove the location", None, &mut o.remove_gps);
                });
                toggle_row(ui, "Keep the original's modification date", None, &mut o.keep_date_modified);
                for e in validate_resize(r) {
                    error_label(ui, &e);
                    ui.add_space(6.0);
                }
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers

/// The "Keep this resolution?" banner; `Some(true)` = keep, `Some(false)` = revert.
fn confirm_banner(ui: &mut Ui, display: &str, p: &display_scaling::PendingChange) -> Option<bool> {
    let pal_ = pal(ui);
    let c = pal_.accent;
    let mut answer = None;
    ui.add_space(16.0);
    egui::Frame::new()
        .fill(theme::mix(pal_.card, c, if pal_.dark { 0.18 } else { 0.09 }))
        .stroke(Stroke::new(1.0, theme::mix(pal_.card_border, c, 0.45)))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon_text(icon::MONITOR).size(20.0).color(c));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new("Keep this resolution?").font(theme::font_semibold(14.0)).color(pal_.text));
                    ui.label(
                        RichText::new(format!(
                            "{display} is now {}. It goes back to {} in {} s.",
                            p.applied.label(),
                            p.previous.label(),
                            p.seconds_left
                        ))
                        .size(12.5)
                        .color(pal_.text2),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, "Keep").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Revert").clicked() {
                        answer = Some(false);
                    }
                });
            });
        });
    answer
}

/// A dropdown of the display's modes, HiDPI ones first; returns the mode
/// picked (when it differs from the current one).
fn mode_picker(ui: &mut Ui, d: &display_scaling::DisplayModes) -> Option<display_scaling::Mode> {
    let mut picked = None;
    let cur = d.current;
    let text = cur.map(|m| m.label()).unwrap_or_else(|| "Unknown".into());
    egui::ComboBox::from_id_salt("mode").selected_text(text).width(250.0).height(380.0).show_ui(ui, |ui| {
        ui.set_min_width(250.0);
        let mut modes = d.modes.clone();
        modes.sort_by(|a, b| (b.width * b.height).cmp(&(a.width * a.height)).then(b.refresh.total_cmp(&a.refresh)));
        for (hidpi, title) in [(true, "HiDPI"), (false, "Standard")] {
            let group: Vec<&display_scaling::Mode> = modes.iter().filter(|m| m.hidpi == hidpi).collect();
            if group.is_empty() {
                continue;
            }
            ui.label(RichText::new(title).font(theme::font_semibold(12.0)).color(pal(ui).text2));
            for m in group {
                let sel = cur.is_some_and(|c| display_scaling::logic::same_mode(&c, m));
                let mut label = format!("{} × {}", m.width, m.height);
                if m.refresh > 0.0 {
                    label.push_str(&format!("  ·  {} Hz", (m.refresh * 100.0).round() / 100.0));
                }
                if d.native.is_some_and(|n| n.width == m.pixel_width && n.height == m.pixel_height && !m.hidpi) {
                    label.push_str("  (native)");
                }
                if ui.selectable_label(sel, label).clicked() && !sel {
                    picked = Some(*m);
                }
            }
            ui.add_space(4.0);
        }
    });
    picked
}

/// `"2200x1238"`, `"2200×1238"`, `"2200 x 1238"`.
pub(crate) fn parse_resolution(s: &str) -> Option<display_scaling::Resolution> {
    let t = s.trim().to_lowercase().replace('×', "x");
    let (w, h) = t.split_once('x')?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    ((320..=8192).contains(&w) && (200..=8192).contains(&h)).then_some(display_scaling::Resolution { width: w, height: h })
}

/// An ordered list of device rules, with "add a connected device" and
/// "add by name".
fn device_rule_list(ui: &mut Ui, key: &str, list: &mut Vec<DeviceMatch>, devices: &[AudioDevice], text: &mut String, ordered: bool) {
    let mut action: Option<(usize, i32)> = None;
    for (i, m) in list.iter().enumerate() {
        ui.push_id((key, i), |ui| {
            let connected = devices.iter().any(|d| m.matches(d));
            let label = if m.name.is_empty() { m.uid.clone() } else { m.name.clone() };
            let desc = match (m.uid.is_empty(), connected) {
                (true, true) => "Any device whose name contains this · connected",
                (true, false) => "Any device whose name contains this",
                (false, true) => "Connected",
                (false, false) => "Not connected",
            };
            let label = if ordered { format!("{}. {label}", i + 1) } else { label };
            row(ui, &label, Some(desc), |ui| {
                if ordered {
                    ui.add_enabled_ui(i > 0, |ui| {
                        if icon_button(ui, icon::ARROW_UP, "Higher priority").clicked() {
                            action = Some((i, -1));
                        }
                    });
                    ui.add_enabled_ui(i + 1 < list.len(), |ui| {
                        if icon_button(ui, icon::ARROW_DOWN, "Lower priority").clicked() {
                            action = Some((i, 1));
                        }
                    });
                }
                if icon_button(ui, icon::TRASH, "Remove").clicked() {
                    action = Some((i, 0));
                }
            });
        });
    }
    match action {
        Some((i, 0)) => {
            list.remove(i);
        }
        Some((i, d)) => {
            let j = (i as i32 + d).clamp(0, list.len() as i32 - 1) as usize;
            list.swap(i, j);
        }
        None => {}
    }
    if list.is_empty() {
        block(ui, |ui| hint(ui, "Empty."));
    }
    block(ui, |ui| {
        ui.horizontal(|ui| {
            let free: Vec<&AudioDevice> = devices.iter().filter(|d| !list.iter().any(|m| m.matches_uid(d))).collect();
            ui.add_enabled_ui(!free.is_empty(), |ui| {
                egui::ComboBox::from_id_salt(("add-dev", key)).selected_text("Add a connected device…").width(210.0).show_ui(ui, |ui| {
                    for d in free {
                        if ui.selectable_label(false, &d.name).clicked() {
                            list.push(DeviceMatch::uid(d.uid.clone(), d.name.clone()));
                        }
                    }
                });
            });
            let r = ui.add(egui::TextEdit::singleline(text).hint_text("or part of a name").desired_width(170.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!text.trim().is_empty(), egui::Button::new("Add")).clicked() || enter) && !text.trim().is_empty() {
                let name = text.trim().to_string();
                if !list.iter().any(|m| m.uid.is_empty() && m.name.eq_ignore_ascii_case(&name)) {
                    list.push(DeviceMatch::name(name));
                }
                text.clear();
            }
        });
    });
}

/// Short labels for a snippet's non-default options.
fn snippet_chips(s: &text_expander::Snippet) -> Vec<String> {
    let mut v = Vec::new();
    if !s.case_sensitive {
        v.push("Any case".to_string());
    }
    if s.word_boundary {
        v.push("Whole word".to_string());
    }
    if s.paste {
        v.push("Pasted".to_string());
    }
    if !s.apps.is_empty() {
        v.push(format!("Only in {}", s.apps.join(", ")));
    }
    if !s.label.is_empty() {
        v.push(s.label.clone());
    }
    v
}

fn copy_example(style: file_tools::CopyStyle) -> String {
    let home = if cfg!(windows) { std::path::PathBuf::from(r"C:\Users\you") } else { std::path::PathBuf::from("/Users/you") };
    let p = home.join("Documents").join("Q3 report.pdf");
    file_tools::finder::paths::format_path_with(&p, style, Some(&home), false)
}

/// A native "choose file" dialog (macOS, through AppleScript): the POSIX
/// path, or "" when cancelled.
fn choose_file() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "POSIX path of (choose file with prompt \"Choose an espanso match file\" of type {\"yml\", \"yaml\", \"public.yaml\", \"public.text\"})"])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
    #[cfg(not(target_os = "macos"))]
    Ok(String::new())
}

/// Import an espanso match file into `cfg`: (ok, message).
fn import_espanso(path: &str, cfg: &mut text_expander::Config) -> (bool, String) {
    let p = path.trim();
    let p = match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rest)).unwrap_or_else(|| p.into()),
        None => std::path::PathBuf::from(p),
    };
    let yaml = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) => return (false, format!("{}: {e}", p.display())),
    };
    let (snippets, skipped) = text_expander::espanso::import(&yaml);
    let mut added = 0;
    let mut dup = 0;
    for s in snippets {
        if cfg.snippets.iter().any(|x| x.trigger == s.trigger && x.apps == s.apps) {
            dup += 1;
        } else {
            cfg.snippets.push(s);
            added += 1;
        }
    }
    let mut msg = format!("Added {added} snippet{}", if added == 1 { "" } else { "s" });
    if dup > 0 {
        msg.push_str(&format!(", {dup} already there"));
    }
    if !skipped.is_empty() {
        msg.push_str(&format!(", skipped {}", skipped.len()));
    }
    msg.push('.');
    (added > 0 || dup > 0, msg)
}


/// "Wi-Fi", "Dropbox", "Control Center (2)".
fn item_name(i: &menubar_manager::MenuBarItem) -> String {
    match &i.title {
        Some(t) if !t.trim().is_empty() && !t.eq_ignore_ascii_case(&i.owner) => format!("{} — {t}", i.owner),
        _ => i.owner.clone(),
    }
}
