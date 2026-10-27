//! The Automation page (Mouse & Keyboard): hotkeys
//! (`keyboardManager.hotkeys`), mouse buttons (`mouseTuning.buttons`, the
//! shared actions via `ButtonAction::all_with_automation`) and trackpad
//! gestures (`gestures`), all editing the same kind of action
//! (`keyboard_manager::automation::Action`) with one editor.
//!
//! The Test button runs `test_action` on a background thread in this
//! process (shell actions are waited for, so their output can be shown).
//! Recording a gesture goes through the app, which reads the trackpad:
//! `gestures-request.json` (`Record`, renewed every 2 s while recording)
//! and `gestures-status.json` (the gestures it saw).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use super::*;
use super::pages::{callout, permission_control};

use gestures::{Gesture, GestureBinding};
use keyboard_manager::automation::{Action, Interpreter, NotifyWhen, Outcome, ShellAction};
use keyboard_manager::RunProgram;
use mouse_tuning::{ButtonAction, ButtonMapping};
use pm_config::bridge::{self, GesturesRequest, GesturesStatus};

const AUTO_TINT: Color32 = Color32::from_rgb(0x7D, 0x5B, 0xF6);

/// A Test button's result.
#[derive(Debug, Clone)]
pub(super) enum TestState {
    Running,
    Done { ok: bool, summary: String, stdout: String, stderr: String },
}

pub(super) type Tests = Arc<Mutex<BTreeMap<String, TestState>>>;

/// Automation page state.
#[derive(Default)]
pub(super) struct AutomationUi {
    tests: Tests,
    /// Expanded editors ("hk:0", "btn:3", "g:1").
    open: BTreeSet<String>,
    gestures: Option<GesturesStatus>,
    /// Recording: when it started (ms since the epoch) and which binding
    /// gets the gesture (`None`: a new one).
    recording: Option<(u64, Option<usize>)>,
    record_written: Option<Instant>,
    /// Running apps for the "only in" menus.
    apps: Option<(Vec<(String, String)>, Instant)>,
    /// Tests pin the gesture status.
    pinned: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl SettingsApp {
    pub(super) fn poll_automation(&mut self) {
        if !self.auto.pinned {
            self.auto.gestures = bridge::read_fresh(&self.config_path, bridge::GESTURES_STATUS_FILE, bridge::STATUS_FRESH_MS);
        }
        if self.page == Page::Automation && self.auto.apps.as_ref().is_none_or(|(_, t)| t.elapsed() >= Duration::from_secs(5)) {
            self.auto.apps = Some((crate::platform::running_apps(), Instant::now()));
        }
    }

    pub(super) fn tick_automation(&mut self, ctx: &egui::Context) {
        if self.auto.tests.lock().map(|t| t.values().any(|s| matches!(s, TestState::Running))).unwrap_or(false) {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        let Some((since, target)) = self.auto.recording else { return };
        if self.page != Page::Automation {
            self.stop_recording();
            return;
        }
        if self.auto.record_written.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            self.auto.record_written = Some(Instant::now());
            let _ = bridge::write(&self.config_path, bridge::GESTURES_REQUEST_FILE, &GesturesRequest::Record { on: true });
        }
        if !self.auto.pinned {
            self.auto.gestures = bridge::read_fresh(&self.config_path, bridge::GESTURES_STATUS_FILE, bridge::STATUS_FRESH_MS);
        }
        let got = self.auto.gestures.as_ref().and_then(|s| s.recorded.iter().rev().find(|r| r.at >= since)).and_then(|r| r.gesture.parse::<Gesture>().ok());
        if let Some(g) = got {
            let b = &mut self.app.cfg.gestures.bindings;
            match target.and_then(|i| b.get_mut(i)) {
                Some(binding) => binding.gesture = g,
                None => {
                    b.push(GestureBinding::new(g, Action::Shortcut { keys: String::new() }));
                    let i = b.len() - 1;
                    self.auto.open.insert(format!("g:{i}"));
                }
            }
            self.toast = Some((Instant::now(), format!("Recorded: {}", g.label())));
            self.stop_recording();
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn start_recording(&mut self, target: Option<usize>) {
        self.auto.recording = Some((now_ms(), target));
        self.auto.record_written = Some(Instant::now());
        let _ = bridge::write(&self.config_path, bridge::GESTURES_REQUEST_FILE, &GesturesRequest::Record { on: true });
    }

    fn stop_recording(&mut self) {
        if self.auto.recording.take().is_some() {
            let _ = bridge::write(&self.config_path, bridge::GESTURES_REQUEST_FILE, &GesturesRequest::Record { on: false });
        }
    }

    /// For tests: show this gesture status (nothing is read).
    #[cfg(test)]
    pub fn set_gestures_for_test(&mut self, st: Option<GesturesStatus>, open: &[&str]) {
        self.auto.gestures = st;
        self.auto.pinned = true;
        self.auto.open = open.iter().map(|s| s.to_string()).collect();
    }

    /// For tests: a finished Test result under the editor `key`.
    #[cfg(test)]
    pub fn set_test_result_for_test(&mut self, key: &str, ok: bool, summary: &str, stdout: &str) {
        if let Ok(mut t) = self.auto.tests.lock() {
            t.insert(key.into(), TestState::Done { ok, summary: summary.into(), stdout: stdout.into(), stderr: String::new() });
        }
    }

    pub(super) fn automation_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let ax = self.perms.map(|p| p.0);
        let apps = self.auto.apps.as_ref().map(|(a, _)| a.clone()).unwrap_or_default();
        let tests = self.auto.tests.clone();
        let running = self.app_status.is_some();

        // ---- Hotkeys -----------------------------------------------------
        let km_on = self.app.cfg.keyboard_manager.enabled;
        let mut turn_on_km = false;
        {
            let rec = &mut self.recording;
            let open = &mut self.auto.open;
            let rows = &mut self.app.hotkeys;
            card_with(
                ui,
                "Hotkeys",
                Some("A key (F13) or a shortcut (Ctrl+Option+B) that runs an action. A remap of the same keys in Keyboard Manager wins."),
                |ui| {
                    if !km_on {
                        block(ui, |ui| {
                            ui.horizontal(|ui| {
                                warn_label(ui, "Hotkeys run while Keyboard Manager is on.");
                                if ui.button("Turn it on").clicked() {
                                    turn_on_km = true;
                                }
                            });
                        });
                    }
                    if rows.is_empty() {
                        block(ui, |ui| hint(ui, "No hotkeys yet."));
                    }
                    let mut remove = None;
                    for (i, h) in rows.iter_mut().enumerate() {
                        let key = format!("hk:{i}");
                        ui.push_id(&key, |ui| {
                            let is_open = open.contains(&key);
                            let title = action_summary(&h.action);
                            let desc = if h.app.trim().is_empty() { "In every app".to_string() } else { format!("Only in {}", h.app.trim()) };
                            icon_row(ui, action_glyph(&h.action), AUTO_TINT, &title, Some(&desc), |ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                    remove = Some(i);
                                }
                                if icon_button(ui, if is_open { icon::CARET_DOWN } else { icon::PENCIL_SIMPLE }, if is_open { "Done" } else { "Edit" }).clicked() && !open.remove(&key) {
                                    open.insert(key.clone());
                                }
                                toggle_labeled(ui, &mut h.enabled, &format!("Hotkey {}", i + 1));
                                hotkey_edit(ui, rec, Id::unique(("hk-trigger", i)), &mut h.from, RecMode::Combo);
                            });
                            if let Err(e) = (!h.is_blank()).then(|| h.hotkey()).transpose() {
                                error_label(ui, &e);
                                ui.add_space(6.0);
                            }
                            if is_open {
                                sub_panel(ui, |ui| {
                                    app_scope_row(ui, &mut h.app, &apps, "Only shortcuts can be limited to one app.");
                                    action_editor(ui, &key, &mut h.action, rec, &tests, true);
                                });
                            }
                        });
                    }
                    if let Some(i) = remove {
                        rows.remove(i);
                        open.retain(|k| !k.starts_with("hk:"));
                    }
                    block(ui, |ui| {
                        if icon_text_button(ui, icon::PLUS, "Add hotkey").clicked() {
                            rows.push(HotkeyRow::new());
                            open.insert(format!("hk:{}", rows.len() - 1));
                        }
                    });
                    if mac {
                        if let Some(ok) = ax {
                            row(ui, "Accessibility", Some("Hotkeys need it to see and swallow the keys."), |ui| permission_control(ui, ok, "Privacy_Accessibility"));
                        }
                    }
                },
            );
        }
        if turn_on_km {
            self.app.cfg.keyboard_manager.enabled = true;
        }

        // ---- Mouse buttons -----------------------------------------------
        let mt_on = self.app.cfg.mouse_tuning.enabled;
        let mut turn_on_mt = false;
        {
            let rec = &mut self.recording;
            let open = &mut self.auto.open;
            let m = &mut self.app.cfg.mouse_tuning;
            card_with(ui, "Mouse buttons", Some("The middle and side buttons can run any of these actions too (the Mouse page has the basic choices)."), |ui| {
                if !mt_on {
                    block(ui, |ui| {
                        ui.horizontal(|ui| {
                            warn_label(ui, "Button actions run while the Mouse module is on.");
                            if ui.button("Turn it on").clicked() {
                                turn_on_mt = true;
                            }
                        });
                    });
                }
                for (button, label) in MAPPABLE_BUTTONS {
                    let key = format!("btn:{button}");
                    let pos = m.buttons.mappings.iter().position(|x| x.button == button);
                    let mut action = pos.map(|p| m.buttons.mappings[p].action.clone()).unwrap_or_default();
                    let before = action.clone();
                    row(ui, label, None, |ui| {
                        egui::ComboBox::from_id_salt(("auto-button", button)).selected_text(action.label()).width(220.0).show_ui(ui, |ui| {
                            ui.set_min_width(220.0);
                            for (i, a) in ButtonAction::all_with_automation().into_iter().enumerate() {
                                if i == ButtonAction::all().len() {
                                    ui.separator();
                                }
                                let same = button_kind(&a) == button_kind(&action);
                                if ui.selectable_label(same, a.label()).clicked() && !same {
                                    action = a;
                                }
                            }
                        });
                    });
                    match &mut action {
                        ButtonAction::Keystroke { keys } => {
                            row(ui, "Shortcut to send", Some(&format!("When {} is pressed", label.to_lowercase())), |ui| {
                                hotkey_edit(ui, rec, Id::unique(("auto-button-keys", button)), keys, RecMode::Combo)
                            });
                        }
                        ButtonAction::Action(a) => {
                            let is_open = open.contains(&key);
                            let title = action_summary(a);
                            let (_, l) = icon_row(ui, action_glyph(a), AUTO_TINT, &title, Some("Click Edit to change the details"), |ui| {
                                if ui.button(if is_open { "Done" } else { "Edit" }).clicked() && !open.remove(&key) {
                                    open.insert(key.clone());
                                }
                            });
                            let _ = l;
                            if is_open {
                                ui.push_id(&key, |ui| sub_panel(ui, |ui| action_editor(ui, &key, a, rec, &tests, false)));
                            }
                        }
                        _ => {}
                    }
                    if action != before {
                        match (action.normalized(), pos) {
                            (ButtonAction::Default, Some(p)) => {
                                m.buttons.mappings.remove(p);
                            }
                            (ButtonAction::Default, None) => {}
                            (a, Some(p)) => m.buttons.mappings[p].action = a,
                            (a, None) => m.buttons.mappings.push(ButtonMapping { button, action: a }),
                        }
                    }
                }
            });
        }
        if turn_on_mt {
            self.app.cfg.mouse_tuning.enabled = true;
        }

        // ---- Trackpad gestures -------------------------------------------
        let gst = self.auto.gestures.clone();
        let recording = self.auto.recording;
        let mut record: Option<Option<usize>> = None;
        let mut stop = false;
        {
            let rec = &mut self.recording;
            let open = &mut self.auto.open;
            let g = &mut self.app.cfg.gestures;
            card_with(
                ui,
                "Trackpad gestures",
                Some("Three-, four- and five-finger swipes, taps and clicks, and pinches. Touches are only read, never taken away from macOS."),
                |ui| {
                    if !mac {
                        block(ui, |ui| hint(ui, "Trackpad gestures are only available on macOS."));
                        return;
                    }
                    toggle_row(ui, "Gestures", None, &mut g.enabled);
                    let (glyph, tint, text, desc) = match (&gst, running) {
                        (_, false) => (icon::INFO, pal(ui).text3, "PowerMac isn't running".to_string(), "Gestures work while PowerMac runs.".to_string()),
                        (None, true) if g.enabled => (icon::INFO, pal(ui).text3, "Starting…".to_string(), String::new()),
                        (None, true) => (icon::POWER, pal(ui).text3, "Off".to_string(), String::new()),
                        (Some(s), true) => match s.state.as_str() {
                            "running" => (
                                icon::CHECK_CIRCLE,
                                pal(ui).ok,
                                if s.recording { "Recording".to_string() } else { "Running".to_string() },
                                s.clicks_error.clone().map(|e| format!("{} · taps and clicks: {e}", s.detail)).unwrap_or_else(|| s.detail.clone()),
                            ),
                            "unavailable" => (icon::WARNING_CIRCLE, pal(ui).error, "Unavailable".to_string(), s.detail.clone()),
                            _ if g.bindings.is_empty() => (icon::POWER, pal(ui).text3, "Idle".to_string(), "Nothing is bound, so the trackpad isn't read.".to_string()),
                            _ => (icon::POWER, pal(ui).text3, "Off".to_string(), String::new()),
                        },
                    };
                    icon_row(ui, glyph, tint, &text, (!desc.is_empty()).then_some(desc.as_str()), |ui| {
                        let can_record = running && g.enabled && gst.as_ref().is_some_and(|s| s.state != "unavailable");
                        if recording.is_some() {
                            if ui.button("Stop").clicked() {
                                stop = true;
                            }
                            ui.spinner();
                            ui.label(RichText::new("Do the gesture on the trackpad…").color(pal(ui).text2));
                        } else if ui
                            .add_enabled(can_record, egui::Button::new(theme::icon_job(icon::RECORD, "Record gesture", 13.0, pal(ui).text)))
                            .on_disabled_hover_text("Needs PowerMac running with gestures on")
                            .clicked()
                        {
                            record = Some(None);
                        }
                    });
                    ui.add_enabled_ui(g.enabled, |ui| {
                        toggle_row(
                            ui,
                            "Keep bound clicks from the app",
                            Some("A physical click that triggers a bound N-finger click doesn't also click where the pointer is (needs Accessibility)."),
                            &mut g.swallow_clicks,
                        );
                        if g.bindings.is_empty() {
                            block(ui, |ui| hint(ui, "No gestures yet. Record one, or add one and pick it from the list."));
                        }
                        let mut remove = None;
                        let n = g.bindings.len();
                        for i in 0..n {
                            let dup = g.bindings[..i].iter().any(|o| o.gesture == g.bindings[i].gesture && o.app == g.bindings[i].app && o.enabled);
                            let b = &mut g.bindings[i];
                            let key = format!("g:{i}");
                            ui.push_id(&key, |ui| {
                                let is_open = open.contains(&key);
                                let mut desc = action_summary(&b.action);
                                if let Some(a) = b.app.as_deref().filter(|a| !a.trim().is_empty()) {
                                    desc.push_str(&format!(" · only in {a}"));
                                }
                                icon_row(ui, gesture_glyph(&b.gesture), AUTO_TINT, &b.gesture.label(), Some(&desc), |ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                        remove = Some(i);
                                    }
                                    if icon_button(ui, if is_open { icon::CARET_DOWN } else { icon::PENCIL_SIMPLE }, if is_open { "Done" } else { "Edit" }).clicked() && !open.remove(&key) {
                                        open.insert(key.clone());
                                    }
                                    toggle_labeled(ui, &mut b.enabled, &b.gesture.label());
                                    if b.gesture.may_conflict_with_macos() {
                                        tag_pill(ui, Color32::from_rgb(0xE8, 0x9A, 0x00), icon::WARNING, "macOS uses it too");
                                    }
                                });
                                if dup {
                                    warn_label(ui, "Bound twice: the first one wins.");
                                    ui.add_space(6.0);
                                }
                                if is_open {
                                    row(ui, "Gesture", None, |ui| {
                                        if ui.add_enabled(recording.is_none() && running, egui::Button::new("Record…")).clicked() {
                                            record = Some(Some(i));
                                        }
                                        egui::ComboBox::from_id_salt("gesture").selected_text(b.gesture.label()).width(220.0).show_ui(ui, |ui| {
                                            ui.set_min_width(220.0);
                                            for gg in Gesture::all() {
                                                ui.selectable_value(&mut b.gesture, gg, gg.label());
                                            }
                                        });
                                    });
                                    if b.gesture.may_conflict_with_macos() {
                                        block(ui, |ui| {
                                            hint(ui, "macOS has its own gesture here (System Settings → Trackpad, or Accessibility → Pointer Control for three-finger drag). Both happen; turn the macOS one off if you only want yours.");
                                            if ui.button("Open Trackpad settings…").clicked() {
                                                open_trackpad_settings();
                                            }
                                        });
                                    }
                                    let mut app = b.app.clone().unwrap_or_default();
                                    sub_panel(ui, |ui| {
                                        app_scope_row(ui, &mut app, &apps, "Bind “Do nothing” in an app to turn a global gesture off there.");
                                        action_editor(ui, &key, &mut b.action, rec, &tests, true);
                                    });
                                    b.app = (!app.trim().is_empty()).then_some(app);
                                }
                            });
                        }
                        if let Some(i) = remove {
                            g.bindings.remove(i);
                            open.retain(|k| !k.starts_with("g:"));
                        }
                        block(ui, |ui| {
                            if icon_text_button(ui, icon::PLUS, "Add gesture").clicked() {
                                let next = Gesture::all().into_iter().find(|x| !g.bindings.iter().any(|b| b.gesture == *x)).unwrap_or(Gesture::Tap { fingers: 3 });
                                g.bindings.push(GestureBinding::new(next, Action::Shortcut { keys: String::new() }));
                                open.insert(format!("g:{}", g.bindings.len() - 1));
                            }
                        });
                        for w in g.validate() {
                            warn_label(ui, &w);
                            ui.add_space(4.0);
                        }
                    });
                },
            );
        }
        if stop {
            self.stop_recording();
        }
        if let Some(t) = record {
            self.start_recording(t);
        }
        if !running {
            callout(ui, icon::INFO, "PowerMac isn't running", "Hotkeys, buttons and gestures work while PowerMac runs. The Test buttons work now.");
        }
    }
}

fn button_kind(a: &ButtonAction) -> String {
    match a {
        ButtonAction::Action(x) => format!("action:{}", x.label()),
        other => format!("{:?}", std::mem::discriminant(other)),
    }
}

fn gesture_glyph(g: &Gesture) -> &'static str {
    match g {
        Gesture::Swipe { .. } => glyph5::HAND_SWIPE_LEFT,
        Gesture::Tap { .. } | Gesture::Click { .. } => glyph5::HAND_TAP,
        Gesture::Pinch { .. } => icon::ARROWS_LEFT_RIGHT,
    }
}

fn action_glyph(a: &Action) -> &'static str {
    match a {
        Action::Disabled => icon::X,
        Action::Shortcut { .. } => icon::KEYBOARD,
        Action::Text { .. } => icon::TEXT_AA,
        Action::Run(_) => icon::ROCKET_LAUNCH,
        Action::OpenUri { .. } => icon::LINK,
        Action::PastePlainText => icon::CLIPBOARD_TEXT,
        Action::Shell(_) => glyph5::TERMINAL,
    }
}

/// "Shell: make", "Press Cmd+W", "Open https://…".
pub(super) fn action_summary(a: &Action) -> String {
    let cut = |s: &str, n: usize| if s.chars().count() > n { format!("{}…", s.chars().take(n - 1).collect::<String>()) } else { s.to_string() };
    match a {
        Action::Disabled => "Do nothing".into(),
        Action::Shortcut { keys } if keys.trim().is_empty() => "Press a shortcut (not set)".into(),
        Action::Shortcut { keys } => format!("Press {keys}"),
        Action::Text { text } => format!("Type “{}”", cut(text.lines().next().unwrap_or(""), 30)),
        Action::Run(r) if r.path.trim().is_empty() => "Open a program (not set)".into(),
        Action::Run(r) => format!("Open {}", std::path::Path::new(r.path.trim()).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| r.path.clone())),
        Action::OpenUri { uri } if uri.trim().is_empty() => "Open a URL (not set)".into(),
        Action::OpenUri { uri } => format!("Open {}", cut(uri.trim(), 40)),
        Action::PastePlainText => "Paste as plain text".into(),
        Action::Shell(s) => format!("Shell: {}", s.display_name()),
    }
}

/// "Only in" text field with a menu of running apps.
fn app_scope_row(ui: &mut Ui, app: &mut String, apps: &[(String, String)], desc: &str) {
    row(ui, "Only in", Some(desc), |ui| {
        egui::ComboBox::from_id_salt("app-scope").selected_text("").width(24.0).show_ui(ui, |ui| {
            ui.set_min_width(240.0);
            if ui.selectable_label(app.trim().is_empty(), "Every app").clicked() {
                app.clear();
            }
            for (name, id) in apps {
                if ui.selectable_label(app == id, name).on_hover_text(id).clicked() {
                    *app = id.clone();
                }
            }
        });
        ui.add(egui::TextEdit::singleline(app).hint_text("Every app (or a bundle id)").desired_width(220.0))
    });
}

fn kind_index(a: &Action) -> usize {
    match a {
        Action::Disabled => 0,
        Action::Shortcut { .. } => 1,
        Action::Text { .. } => 2,
        Action::Run(_) => 3,
        Action::OpenUri { .. } => 4,
        Action::PastePlainText => 5,
        Action::Shell(_) => 6,
    }
}

/// The shared action editor: kind, its fields, and a Test button with the
/// outcome (and a shell action's captured output) under it.
/// A tinted, indented panel for an expanded editor under its row.
fn sub_panel(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    ui.add_space(2.0);
    egui::Frame::new()
        .fill(theme::mix(p.card, AUTO_TINT, if p.dark { 0.07 } else { 0.035 }))
        .stroke(Stroke::new(1.0, theme::mix(p.card_border, AUTO_TINT, 0.25)))
        .corner_radius(10.0)
        .inner_margin(egui::Margin { left: 14, right: 12, top: 2, bottom: 2 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            reset_rows(ui);
            add(ui);
        });
    ui.add_space(8.0);
    continue_rows(ui);
}

pub(super) fn action_editor(ui: &mut Ui, key: &str, a: &mut Action, rec: &mut Option<Id>, tests: &Tests, show_kind: bool) {
    let mut kind = kind_index(a);
    if show_kind {
    row(ui, "Action", None, |ui| {
        egui::ComboBox::from_id_salt(("action-kind", key)).selected_text(a.label()).width(220.0).show_ui(ui, |ui| {
            ui.set_min_width(220.0);
            for (i, x) in Action::all().into_iter().enumerate() {
                ui.selectable_value(&mut kind, i, x.label());
            }
        });
    });
    }
    if kind != kind_index(a) {
        *a = Action::all().swap_remove(kind);
    }
    match a {
        Action::Disabled => block(ui, |ui| hint(ui, "The trigger is swallowed and nothing happens.")),
        Action::PastePlainText => block(ui, |ui| hint(ui, "Pastes the clipboard without formatting (Keyboard Manager's paste settings).")),
        Action::Shortcut { keys } => {
            row(ui, "Keys to press", None, |ui| hotkey_edit(ui, rec, Id::unique(("action-keys", key)), keys, RecMode::Combo));
            if !keys.trim().is_empty() {
                if let Err(e) = keyboard_manager::keys::parse_combo(keys) {
                    error_label(ui, &e);
                    ui.add_space(6.0);
                }
            }
        }
        Action::Text { text } => {
            row(ui, "Text to type", Some("A new line presses Return."), |ui| ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(300.0)));
        }
        Action::OpenUri { uri } => {
            row(ui, "URL, file or folder", None, |ui| ui.add(egui::TextEdit::singleline(uri).hint_text("https://example.com").desired_width(300.0)));
        }
        Action::Run(r) => run_fields(ui, r),
        Action::Shell(s) => shell_fields(ui, key, s),
    }
    for e in a.validate() {
        if !matches!(a, Action::Shortcut { .. }) {
            warn_label(ui, &e);
            ui.add_space(4.0);
        }
    }
    // Test
    let state = tests.lock().ok().and_then(|t| t.get(key).cloned());
    let busy = matches!(state, Some(TestState::Running));
    block(ui, |ui| {
        ui.horizontal(|ui| {
            let r = ui
                .add_enabled(!busy && *a != Action::Disabled, egui::Button::new(theme::icon_job(glyph5::PLAY_CIRCLE, "Test", 13.0, pal(ui).text)))
                .on_hover_text(if a.injects_input() { "Runs it now; keys go to this window" } else { "Runs it now (shell commands are waited for)" });
            if r.clicked() {
                let action = a.clone();
                let tests = tests.clone();
                let k = key.to_string();
                if let Ok(mut t) = tests.lock() {
                    t.insert(k.clone(), TestState::Running);
                }
                std::thread::spawn(move || {
                    let o = keyboard_manager::automation::test_action(&action);
                    let (stdout, stderr) = match &o {
                        Outcome::Finished(r) => (r.stdout.clone(), r.stderr.clone()),
                        _ => (String::new(), String::new()),
                    };
                    let done = TestState::Done { ok: o.is_ok(), summary: o.summary(), stdout, stderr };
                    if let Ok(mut t) = tests.lock() {
                        t.insert(k, done);
                    }
                });
            }
            if busy {
                ui.spinner();
                hint(ui, "Running…");
            }
            if let Some(TestState::Done { ok, summary, .. }) = &state {
                if *ok {
                    ok_label(ui, summary);
                } else {
                    error_label(ui, summary);
                }
            }
        });
        if let Some(TestState::Done { stdout, stderr, .. }) = &state {
            let text = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
                (true, true) => None,
                (false, true) => Some(stdout.trim_end().to_string()),
                (true, false) => Some(stderr.trim_end().to_string()),
                (false, false) => Some(format!("{}\n--- stderr ---\n{}", stdout.trim_end(), stderr.trim_end())),
            };
            if let Some(t) = text {
                ui.add_space(6.0);
                let p = pal(ui);
                egui::Frame::new().fill(if p.dark { Color32::from_gray(0x1A) } else { Color32::from_gray(0xF4) }).corner_radius(6.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().id_salt(("test-out", key)).max_height(140.0).show(ui, |ui| {
                        ui.label(RichText::new(t).monospace().size(11.5).color(p.text));
                    });
                });
            }
        }
    });
}

fn run_fields(ui: &mut Ui, r: &mut RunProgram) {
    row(ui, "Program", Some("An app (/Applications/Safari.app) or an executable."), |ui| {
        ui.add(egui::TextEdit::singleline(&mut r.path).hint_text("/Applications/Notes.app").desired_width(300.0))
    });
    let mut args = r.args.join("\n");
    row(ui, "Arguments", Some("One per line."), |ui| {
        if ui.add(egui::TextEdit::multiline(&mut args).desired_rows(1).desired_width(300.0)).changed() {
            r.args = args.lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
        }
    });
    let mut wd = r.working_dir.clone().unwrap_or_default();
    row(ui, "Working folder", None, |ui| {
        if ui.add(egui::TextEdit::singleline(&mut wd).hint_text("Home folder").desired_width(300.0)).changed() {
            r.working_dir = (!wd.trim().is_empty()).then(|| wd.clone());
        }
    });
}

fn interp_index(i: &Interpreter, all: &[Interpreter]) -> usize {
    all.iter().position(|x| std::mem::discriminant(x) == std::mem::discriminant(i)).unwrap_or(0)
}

fn shell_fields(ui: &mut Ui, key: &str, s: &mut ShellAction) {
    row(ui, "Name", Some("For notifications and the log file (optional)."), |ui| ui.add(egui::TextEdit::singleline(&mut s.name).hint_text("Optional").desired_width(220.0)));
    let mut script = s.script_path.is_some();
    row(ui, "Runs", None, |ui| segmented(ui, &mut script, &[(false, "A command"), (true, "A script file")]));
    if script != s.script_path.is_some() {
        s.script_path = script.then(String::new);
    }
    match &mut s.script_path {
        Some(p) => {
            row(ui, "Script", None, |ui| ui.add(egui::TextEdit::singleline(p).hint_text("~/bin/backup.sh").desired_width(300.0)));
        }
        None => {
            block(ui, |ui| {
                ui.label(RichText::new("Command").font(theme::font_semibold(13.0)).color(pal(ui).text));
                ui.add_space(4.0);
                ui.add(egui::TextEdit::multiline(&mut s.command).code_editor().desired_rows(3).desired_width(f32::INFINITY).hint_text("say \"Hello\""));
            });
        }
    }
    let all = Interpreter::all();
    let mut ii = interp_index(&s.interpreter, &all);
    row(ui, "Interpreter", None, |ui| {
        egui::ComboBox::from_id_salt(("interp", key)).selected_text(s.interpreter.label()).width(160.0).show_ui(ui, |ui| {
            for (i, x) in all.iter().enumerate() {
                ui.selectable_value(&mut ii, i, x.label());
            }
        });
    });
    if ii != interp_index(&s.interpreter, &all) {
        s.interpreter = all[ii].clone();
    }
    if let Interpreter::Custom { program, args } = &mut s.interpreter {
        row(ui, "Program", Some("Run as: program args… <command or script> <arguments…>"), |ui| {
            ui.add(egui::TextEdit::singleline(program).hint_text("/usr/local/bin/node").desired_width(220.0))
        });
        let mut a = args.join(" ");
        row(ui, "Its arguments", Some("Separated by spaces."), |ui| {
            if ui.add(egui::TextEdit::singleline(&mut a).desired_width(220.0)).changed() {
                *args = a.split_whitespace().map(str::to_string).collect();
            }
        });
    }
    let mut args = s.args.join("\n");
    row(ui, "Arguments", Some("One per line ($1, $2… in shells)."), |ui| {
        if ui.add(egui::TextEdit::multiline(&mut args).desired_rows(1).desired_width(300.0)).changed() {
            s.args = args.lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
        }
    });
    let mut wd = s.working_dir.clone().unwrap_or_default();
    row(ui, "Working folder", None, |ui| {
        if ui.add(egui::TextEdit::singleline(&mut wd).hint_text("Home folder").desired_width(300.0)).changed() {
            s.working_dir = (!wd.trim().is_empty()).then(|| wd.clone());
        }
    });
    env_table(ui, key, &mut s.env);
    row(ui, "Run", Some("Waiting lets a notification show the result; a background command keeps running on its own."), |ui| {
        segmented(ui, &mut s.wait, &[(false, "In the background"), (true, "Wait for it")])
    });
    let mut secs = s.timeout_ms.map(|ms| ms as f64 / 1000.0).unwrap_or(0.0);
    let default_note = if s.wait { "0 = the default (30 s when waiting)" } else { "0 = no limit" };
    row(ui, "Time limit", Some(default_note), |ui| {
        if ui.add_sized(vec2(84.0, 24.0), egui::DragValue::new(&mut secs).range(0.0..=86_400.0).speed(0.5).suffix(" s")).changed() {
            s.timeout_ms = (secs > 0.0).then(|| (secs * 1000.0).round() as u64);
        }
    });
    row(ui, "Notify", None, |ui| {
        segmented(ui, &mut s.notify, &[(NotifyWhen::Never, "Never"), (NotifyWhen::OnFailure, "On failure"), (NotifyWhen::Always, "Always")])
    });
    let log = keyboard_manager::automation::default_log_dir().map(|d| s.log_path(&d));
    row(ui, "Log output", log.as_ref().map(|p| p.display().to_string()).as_deref(), |ui| {
        let exists = log.as_ref().is_some_and(|p| p.exists());
        if ui.add_enabled(exists, egui::Button::new("Open log")).on_disabled_hover_text("No log yet").clicked() {
            if let Some(p) = &log {
                open_file(p);
            }
        }
        toggle_labeled(ui, &mut s.log_output, "Log output")
    });
}

fn env_table(ui: &mut Ui, key: &str, env: &mut BTreeMap<String, String>) {
    let id = Id::unique(("env-new", key));
    row(ui, "Environment", Some("Added to (or replacing) the inherited variables."), |ui| {
        let mut draft: (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        if ui.add_enabled(!draft.0.trim().is_empty(), egui::Button::new("Add")).clicked() {
            env.insert(draft.0.trim().to_string(), draft.1.clone());
            draft = Default::default();
        }
        ui.add(egui::TextEdit::singleline(&mut draft.1).hint_text("value").desired_width(120.0));
        ui.add(egui::TextEdit::singleline(&mut draft.0).hint_text("NAME").desired_width(100.0));
        ui.data_mut(|d| d.insert_temp(id, draft));
    });
    if env.is_empty() {
        return;
    }
    let mut remove = None;
    block(ui, |ui| {
        for (k, v) in env.iter_mut() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(k).monospace().size(12.0));
                ui.label("=");
                ui.add(egui::TextEdit::singleline(v).desired_width(220.0).font(egui::TextStyle::Monospace));
                if icon_button(ui, icon::X, "Remove").clicked() {
                    remove = Some(k.clone());
                }
            });
        }
    });
    if let Some(k) = remove {
        env.remove(&k);
    }
}

fn open_file(p: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = p;
}

fn open_trackpad_settings() {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.Trackpad-Settings.extension").spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_and_kinds() {
        assert_eq!(action_summary(&Action::Shortcut { keys: "Cmd+W".into() }), "Press Cmd+W");
        assert_eq!(action_summary(&Action::Shell(ShellAction { command: "make all".into(), ..Default::default() })), "Shell: make all");
        assert!(action_summary(&Action::Run(RunProgram { path: "/Applications/Notes.app".into(), args: vec![], working_dir: None })).ends_with("Notes.app"));
        for (i, a) in Action::all().iter().enumerate() {
            assert_eq!(kind_index(a), i);
        }
        assert_ne!(button_kind(&ButtonAction::Action(Action::PastePlainText)), button_kind(&ButtonAction::Action(Action::Text { text: String::new() })));
        assert_eq!(button_kind(&ButtonAction::Keystroke { keys: "a".into() }), button_kind(&ButtonAction::Keystroke { keys: "b".into() }));
    }

    /// The Test button's runner: a shell action's output comes back.
    #[cfg(unix)]
    #[test]
    fn test_action_captures_output() {
        let a = Action::Shell(ShellAction { command: "echo hello-from-test".into(), interpreter: Interpreter::Sh, log_output: false, ..Default::default() });
        let o = keyboard_manager::automation::test_action(&a);
        assert!(o.is_ok(), "{}", o.summary());
        assert!(o.summary().contains("hello-from-test"), "{}", o.summary());
    }
}
