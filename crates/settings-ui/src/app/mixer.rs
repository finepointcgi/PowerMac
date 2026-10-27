//! Sound → App routing (a card on the Sound page) and the Mixer page.
//!
//! Like the rest of Sound, routing and the mixer run in the app (process
//! taps and the private aggregate device need its permissions); this window
//! edits `audio.appRoutes` / `audio.mixer` in `config.json` and, for live
//! fader / mute / solo / send / EQ / limiter changes, also sends the whole
//! mixer as an `AudioRequest::SetMixer` (applied without rebuilding the
//! device, at most every 70 ms while dragging). The app reports strip and
//! route states in `audio-status.json` (`AudioStatus::mixer`).
//!
//! Meters: while the Mixer page is open this window renews
//! `audio-watch.json` every second with `meters: true`; the app then writes
//! `audio-meters.json` about 20 times a second, which is read here at the
//! same rate. Nothing runs at 20 Hz while the page is closed.

use super::*;
use super::pages::{callout, permission_control};

use audio_control::mixer::dsp::biquad::{response_db, BandKind, Coeffs, MAX_BANDS};
use audio_control::{
    AppRoute, AudioApp, AudioDevice, CapturePermission, DeviceMatch, EqBand, EqConfig, Effects, LimiterConfig, MeterReading, Meters, MixOutput,
    MixSource, Mixer, MixerState, RouteStatus, SourceKind, StripState, Transport,
};
use pm_config::bridge::{self, AudioRequest, AudioWatch};

const SOUND_TINT: Color32 = Color32::from_rgb(0xFF, 0x37, 0x5F);
/// Fader range (dB).
const FADER_MIN: f32 = -60.0;
const FADER_MAX: f32 = 12.0;

/// Mixer page / routing card state.
#[derive(Default)]
pub(super) struct MixerUi {
    /// Latest meters (with when they were read).
    meters: Option<Meters>,
    meters_read: Option<Instant>,
    /// When `audio-watch.json` was last renewed.
    watch_written: Option<Instant>,
    /// The strip whose effects are edited: `src:<id>` / `out:<id>`.
    selected: Option<String>,
    /// The mixer as last sent to (or seen from) the app.
    last_sent: Option<Mixer>,
    /// Running apps (name, bundle id) for the app pickers, with when.
    running_apps: Option<(Vec<(String, String)>, Instant)>,
    /// Tests pin the meters (no file reads).
    pinned: bool,
}

/// What a strip's controls asked for.
#[derive(Default)]
struct StripOut {
    select: bool,
    remove: bool,
}

impl SettingsApp {
    // -----------------------------------------------------------------------
    // Polling (shared entry points for the routing / mixer, battery, network
    // and automation pages)

    /// Once a second.
    pub(super) fn poll_int5(&mut self) {
        self.poll_mixer();
        self.poll_battery();
        self.poll_network();
        self.poll_automation();
    }

    /// Every frame.
    pub(super) fn tick_int5(&mut self, ctx: &egui::Context) {
        self.tick_mixer(ctx);
        self.tick_network(ctx);
        self.tick_automation(ctx);
    }

    fn poll_mixer(&mut self) {
        let sound_page = matches!(self.page, Page::Sound | Page::Mixer);
        if sound_page && self.mixer_ui.watch_written.is_none_or(|t| t.elapsed() >= Duration::from_millis(900)) {
            self.mixer_ui.watch_written = Some(Instant::now());
            let w = AudioWatch { meters: self.page == Page::Mixer };
            if let Err(e) = bridge::write(&self.config_path, bridge::AUDIO_WATCH_FILE, &w) {
                log::debug!("audio watch: {e:#}");
            }
        }
        if sound_page && self.mixer_ui.running_apps.as_ref().is_none_or(|(_, t)| t.elapsed() >= Duration::from_secs(5)) {
            self.mixer_ui.running_apps = Some((crate::platform::running_apps(), Instant::now()));
        }
    }

    fn tick_mixer(&mut self, ctx: &egui::Context) {
        // Live mixer edits go to the app at once (config.json follows).
        let cur = &self.app.cfg.audio.mixer;
        match &self.mixer_ui.last_sent {
            None => self.mixer_ui.last_sent = Some(cur.clone()),
            Some(last) if last != cur => {
                let cur = cur.clone();
                // Only edits made here (not a config reloaded from disk).
                if self.app_file.changed_at.is_some() && self.audio_status.as_ref().is_some_and(|s| s.running) {
                    self.queue_mixer_request(AudioRequest::SetMixer { mixer: cur.clone() });
                }
                self.mixer_ui.last_sent = Some(cur);
            }
            _ => {}
        }
        if self.page != Page::Mixer || self.mixer_ui.pinned {
            return;
        }
        let running = self.audio_status.as_ref().is_some_and(|s| s.running && s.mixer.as_ref().is_some_and(|m| m.running));
        if !running {
            self.mixer_ui.meters = None;
            return;
        }
        if self.mixer_ui.meters_read.is_none_or(|t| t.elapsed() >= audio_control::METER_INTERVAL) {
            self.mixer_ui.meters_read = Some(Instant::now());
            self.mixer_ui.meters = bridge::read_fresh::<Meters>(&self.config_path, bridge::AUDIO_METERS_FILE, 1_000);
        }
        ctx.request_repaint_after(audio_control::METER_INTERVAL);
    }

    /// Mixer requests replace each other while dragging (the newest wins).
    fn queue_mixer_request(&mut self, r: AudioRequest) {
        if !matches!(self.audio_pending, Some(AudioRequest::SetMixer { .. })) {
            self.flush_audio_request(true);
        }
        self.audio_pending = Some(r);
    }

    fn apps_for_pickers(&self, playing: &[AudioApp]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = playing.iter().map(|a| (a.name.clone(), a.bundle_id.clone())).collect();
        if let Some((running, _)) = &self.mixer_ui.running_apps {
            for (n, b) in running {
                if !b.is_empty() && !v.iter().any(|(_, x)| x == b) {
                    v.push((n.clone(), b.clone()));
                }
            }
        }
        v
    }

    // -----------------------------------------------------------------------
    // Sound → App routing

    /// The "App routing" card on the Sound page.
    pub(super) fn routing_card(&mut self, ui: &mut Ui) {
        let mac = cfg!(target_os = "macos");
        let status = self.audio_status.clone().filter(|s| s.running);
        let running = status.is_some();
        let state: Option<MixerState> = status.as_ref().and_then(|s| s.mixer.clone());
        let outputs: Vec<AudioDevice> = match &status {
            Some(s) => s.outputs.clone(),
            None => self.audio_local.as_ref().map(|(o, _)| o.clone()).unwrap_or_default(),
        };
        let playing = status.as_ref().map(|s| s.apps.clone()).unwrap_or_default();
        let pick = self.apps_for_pickers(&playing);
        let permission = status.as_ref().map(|s| s.capture_permission).unwrap_or_else(audio_control::capture_permission);
        let a = &mut self.app.cfg.audio;
        let mut add_app: Option<(String, String)> = None;
        let mut remove: Option<String> = None;
        card_with(
            ui,
            "App routing",
            Some(if mac {
                "Send an app to other speakers or a virtual device, instead of (or as well as) the normal output. macOS 14.4 or later, through Core Audio process taps."
            } else {
                "Send an app to another output device (Windows' own per-app device setting)."
            }),
            |ui| {
                ui.add_enabled_ui(a.enabled, |ui| {
                    if mac {
                        row(
                            ui,
                            "System Audio Recording",
                            Some("Routing reads each app's sound through a process tap. Nothing is recorded."),
                            |ui| capture_permission_control(ui, permission),
                        );
                    }
                    if a.app_routes.is_empty() {
                        block(ui, |ui| hint(ui, "No app is routed. Add one below: its sound then plays on the devices you choose."));
                    }
                    let keys: Vec<String> = a.app_routes.keys().cloned().collect();
                    for bundle in keys {
                        let Some(route) = a.app_routes.get_mut(&bundle) else { continue };
                        let st = state.as_ref().and_then(|m| m.routes.iter().find(|r| r.bundle_id == bundle));
                        ui.push_id(("route", &bundle), |ui| {
                            if route_rows(ui, &bundle, route, st, &outputs, mac) {
                                remove = Some(bundle.clone());
                            }
                        });
                    }
                    block(ui, |ui| {
                        ui.horizontal(|ui| {
                            let taken: Vec<&String> = a.app_routes.keys().collect();
                            let choices: Vec<&(String, String)> = pick.iter().filter(|(_, b)| !taken.contains(&b)).collect();
                            egui::ComboBox::from_id_salt("route-add")
                                .selected_text(RichText::new("Add an app…").color(pal(ui).text2))
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    ui.set_min_width(260.0);
                                    if choices.is_empty() {
                                        ui.label(RichText::new("No running apps found").color(pal(ui).text2));
                                    }
                                    for (name, b) in choices {
                                        let playing_now = playing.iter().any(|p| &p.bundle_id == b && p.playing);
                                        let label = if playing_now { format!("{name}  ·  playing") } else { name.clone() };
                                        if ui.selectable_label(false, label).on_hover_text(b).clicked() {
                                            add_app = Some((name.clone(), b.clone()));
                                        }
                                    }
                                });
                            if !running {
                                hint(ui, "Apps playing sound are listed first while PowerMac runs.");
                            }
                        });
                    });
                    if let Some(st) = &state {
                        for e in &st.errors {
                            warn_label(ui, e);
                            ui.add_space(6.0);
                        }
                    }
                });
            },
        );
        if let Some((name, b)) = add_app {
            let target = outputs.iter().find(|d| d.is_default).or(outputs.first()).map(|d| DeviceMatch::uid(&d.uid, &d.name));
            let mut r = AppRoute { name, ..Default::default() };
            r.outputs.extend(target);
            self.app.cfg.audio.app_routes.insert(b, r);
        }
        if let Some(b) = remove {
            self.app.cfg.audio.app_routes.remove(&b);
        }
    }

    // -----------------------------------------------------------------------
    // Mixer page

    pub(super) fn mixer_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        let mac = cfg!(target_os = "macos");
        let status = self.audio_status.clone().filter(|s| s.running);
        let running = status.is_some();
        let state: MixerState = status.as_ref().and_then(|s| s.mixer.clone()).unwrap_or_default();
        let (outputs, inputs) = match &status {
            Some(s) => (s.outputs.clone(), s.inputs.clone()),
            None => self.audio_local.clone().unwrap_or_default(),
        };
        if self.audio_local.is_none() && !running {
            self.audio_local = Some((
                audio_control::list_devices(audio_control::DeviceKind::Output).unwrap_or_default(),
                audio_control::list_devices(audio_control::DeviceKind::Input).unwrap_or_default(),
            ));
        }
        let playing = status.as_ref().map(|s| s.apps.clone()).unwrap_or_default();
        let pick = self.apps_for_pickers(&playing);
        let permission = status.as_ref().map(|s| s.capture_permission).unwrap_or_else(audio_control::capture_permission);
        let meters = self.mixer_ui.meters.clone().unwrap_or_default();
        let sound_on = self.app.cfg.audio.enabled;

        if !sound_on {
            callout(ui, icon::INFO, "Sound is off", "The mixer runs inside the Sound module. Turn Sound on to use it.");
            ui.add_space(4.0);
            if icon_text_button(ui, icon::POWER, "Turn on Sound").clicked() {
                self.app.cfg.audio.enabled = true;
            }
        } else if !running {
            callout(ui, icon::INFO, "PowerMac isn't running", "Changes are saved and take effect when PowerMac runs. Meters and strip states appear while it does.");
        }
        self.mixer_status_card(ui, &state, permission, running, mac);

        // ---- strips ------------------------------------------------------
        let m = &mut self.app.cfg.audio.mixer;
        let sel = &mut self.mixer_ui.selected;
        let mut add_source: Option<MixSource> = None;
        let mut add_output: Option<MixOutput> = None;
        card_with(
            ui,
            "Sources",
            Some("Apps, microphones and virtual devices' loopback. Faders set each source's level into every output it is sent to."),
            |ui| {
                ui.add_enabled_ui(m.enabled, |ui| {
                    block(ui, |ui| {
                        if m.sources.is_empty() {
                            hint(ui, "No sources yet. Add an app, a microphone or a virtual device.");
                            ui.add_space(6.0);
                        }
                        let mut remove = None;
                        strips_row(ui, "src-strips", |ui| {
                            for (i, s) in m.sources.iter_mut().enumerate() {
                                let key = format!("src:{}", s.id);
                                let st = state.sources.iter().find(|x| x.id == s.id).map(|x| &x.state);
                                let meter = meters.sources.iter().find(|x| x.id == s.id);
                                let (glyph, kind) = source_kind_label(&s.kind);
                                let fx = s.effects.eq.enabled || s.effects.limiter.enabled;
                                let out = ui
                                    .push_id(("src", i), |ui| {
                                        strip(ui, glyph, &s.name, kind, &mut s.gain_db, &mut s.muted, Some(&mut s.solo), meter, st, sel.as_deref() == Some(&key), fx)
                                    })
                                    .inner;
                                if out.select {
                                    *sel = if sel.as_deref() == Some(&key) { None } else { Some(key.clone()) };
                                }
                                if out.remove {
                                    remove = Some(i);
                                }
                            }
                        });
                        if let Some(i) = remove {
                            let id = m.sources.remove(i).id;
                            m.sends.retain(|x| x.source != id);
                        }
                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            add_source = add_source_menu(ui, m, &pick, &inputs);
                        });
                    });
                });
            },
        );
        card_with(
            ui,
            "Outputs",
            Some("Devices the mix plays on. The first connected output's clock drives the mixer; other devices are kept in step by Core Audio."),
            |ui| {
                ui.add_enabled_ui(m.enabled, |ui| {
                    block(ui, |ui| {
                        if m.outputs.is_empty() {
                            hint(ui, "No outputs yet. Add the speakers or a virtual device to send the mix to.");
                            ui.add_space(6.0);
                        }
                        let mut remove = None;
                        strips_row(ui, "out-strips", |ui| {
                            for (i, o) in m.outputs.iter_mut().enumerate() {
                                let key = format!("out:{}", o.id);
                                let st = state.outputs.iter().find(|x| x.id == o.id).map(|x| &x.state);
                                let meter = meters.outputs.iter().find(|x| x.id == o.id);
                                let fx = o.effects.eq.enabled || o.effects.limiter.enabled;
                                let sub = device_short(&o.device, &outputs);
                                let out = ui
                                    .push_id(("out", i), |ui| {
                                        strip(ui, icon::SPEAKER_HIGH, &o.name, &sub, &mut o.gain_db, &mut o.muted, None, meter, st, sel.as_deref() == Some(&key), fx)
                                    })
                                    .inner;
                                if out.select {
                                    *sel = if sel.as_deref() == Some(&key) { None } else { Some(key.clone()) };
                                }
                                if out.remove {
                                    remove = Some(i);
                                }
                            }
                        });
                        if let Some(i) = remove {
                            let id = m.outputs.remove(i).id;
                            m.sends.retain(|x| x.output != id);
                        }
                        ui.add_space(8.0);
                        add_output = add_output_menu(ui, m, &outputs);
                    });
                });
            },
        );
        if let Some(s) = add_source {
            let id = s.id.clone();
            // New sources go to every output at unity.
            for o in m.outputs.iter().map(|o| o.id.clone()).collect::<Vec<_>>() {
                m.set_send(&id, &o, 0.0, false);
            }
            m.sources.push(s);
        }
        if let Some(o) = add_output {
            let id = o.id.clone();
            if m.outputs.is_empty() {
                for s in m.sources.iter().map(|s| s.id.clone()).collect::<Vec<_>>() {
                    m.set_send(&s, &id, 0.0, false);
                }
            }
            m.outputs.push(o);
        }

        // ---- send matrix -------------------------------------------------
        card_with(
            ui,
            "Sends",
            Some("Which source plays on which output. Click a crosspoint to connect it; drag its value to set the send level (dB)."),
            |ui| {
                ui.add_enabled_ui(m.enabled, |ui| block(ui, |ui| send_matrix(ui, m)));
            },
        );

        // ---- effects of the selected strip -------------------------------
        let rate = state.sample_rate.unwrap_or(48_000.0);
        let selected = self.mixer_ui.selected.clone();
        let src_idx = selected.as_deref().and_then(|k| k.strip_prefix("src:")).and_then(|id| m.sources.iter().position(|s| s.id == id));
        let out_idx = selected.as_deref().and_then(|k| k.strip_prefix("out:")).and_then(|id| m.outputs.iter().position(|o| o.id == id));
        let sub = "In order: input trim, EQ, then the limiter (outputs: trim, EQ, master, limiter).";
        if let Some(i) = src_idx {
            let s = &mut m.sources[i];
            let title = format!("Effects · {}", if s.name.is_empty() { "Untitled" } else { &s.name });
            card_with(ui, &title, Some(sub), |ui| {
                source_options(ui, s);
                effects_rows(ui, &mut s.effects, rate);
            });
        } else if let Some(i) = out_idx {
            let o = &mut m.outputs[i];
            let title = format!("Effects · {}", if o.name.is_empty() { "Untitled" } else { &o.name });
            card_with(ui, &title, Some(sub), |ui| {
                row(ui, "Name", None, |ui| ui.add(egui::TextEdit::singleline(&mut o.name).desired_width(200.0)));
                row(ui, "First channel", Some("The stereo pair written starts here (1 = channels 1–2)."), |ui| {
                    let mut ch = o.first_channel + 1;
                    if ui.add(egui::DragValue::new(&mut ch).range(1..=64)).changed() {
                        o.first_channel = ch - 1;
                    }
                });
                effects_rows(ui, &mut o.effects, rate);
            });
        } else if !m.sources.is_empty() || !m.outputs.is_empty() {
            card(ui, "Effects", |ui| block(ui, |ui| hint(ui, "Click FX on a strip to edit its EQ and limiter.")));
        }
    }

    fn mixer_status_card(&mut self, ui: &mut Ui, st: &MixerState, permission: CapturePermission, running: bool, mac: bool) {
        let m = &self.app.cfg.audio.mixer;
        let wants_apps = m.sources.iter().any(|s| matches!(s.kind, SourceKind::App { .. }));
        let wants_mic = m.sources.iter().any(|s| matches!(s.kind, SourceKind::Input { .. }));
        let reasons: Vec<String> = st
            .sources
            .iter()
            .chain(st.outputs.iter())
            .filter_map(|s| match &s.state {
                StripState::Unavailable { reason } | StripState::Waiting { reason } => Some(format!("{}: {reason}", s.name)),
                _ => None,
            })
            .collect();
        let rate_problem = reasons.iter().chain(st.errors.iter()).any(|r| r.contains("Audio MIDI Setup") || r.contains("sample rate"));
        card_with(ui, "Status", None, |ui| {
            let (glyph, tint, text, desc) = if !m.enabled {
                (icon::POWER, pal(ui).text3, "Off".to_string(), "Turn the mixer on above.".to_string())
            } else if !running {
                (icon::INFO, pal(ui).text3, "Waiting for PowerMac".to_string(), "The mixer runs while PowerMac runs.".to_string())
            } else if st.running {
                let rate = st.sample_rate.map(|r| format!("{:.1} kHz", r / 1000.0)).unwrap_or_default();
                let clock = st.clock_device.clone().map(|c| self.device_name(&c)).unwrap_or_default();
                (
                    icon::CHECK_CIRCLE,
                    pal(ui).ok,
                    "Running".to_string(),
                    format!("{rate}{}{clock}", if clock.is_empty() { "" } else { ", clock: " }),
                )
            } else if !st.supported {
                (icon::WARNING_CIRCLE, pal(ui).error, "Unavailable".to_string(), "The mixer isn't available on this system.".to_string())
            } else {
                (icon::WARNING, pal(ui).warn, "Waiting".to_string(), "Nothing to mix yet: add a source and an output that are available.".to_string())
            };
            icon_row(ui, glyph, tint, &text, Some(&desc), |ui| {
                if st.running {
                    ui.label(RichText::new(format!("{} callbacks", st.callbacks)).size(11.5).color(pal(ui).text3));
                }
            });
            if !reasons.is_empty() || !st.errors.is_empty() {
                block(ui, |ui| {
                    for r in reasons.iter().chain(st.errors.iter()) {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(theme::icon_text(icon::WARNING).size(13.0).color(pal(ui).warn));
                            ui.add(egui::Label::new(RichText::new(r).size(12.5).color(pal(ui).text2)).wrap());
                        });
                    }
                });
            }
            if rate_problem && mac {
                row(
                    ui,
                    "Sample rates differ",
                    Some("Every device in the mix must run at the same rate. Set them all to the same Format (e.g. 48,000 Hz) in Audio MIDI Setup."),
                    |ui| {
                        if icon_text_button(ui, icon::ARROW_SQUARE_OUT, "Open Audio MIDI Setup").clicked() {
                            open_audio_midi_setup();
                        }
                    },
                );
            }
            if mac {
                row(
                    ui,
                    "System Audio Recording",
                    Some(if wants_apps { "App sources are read through process taps. Nothing is recorded." } else { "Needed for app sources and routes." }),
                    |ui| capture_permission_control(ui, permission),
                );
                row(
                    ui,
                    "Microphone",
                    Some(if wants_mic {
                        "Input-device sources need it; macOS asks the first time the mixer opens a microphone."
                    } else {
                        "Needed only for microphone and interface sources."
                    }),
                    |ui| {
                        if ui.button("Open Settings…").clicked() {
                            crate::platform::open_privacy_pane("Privacy_Microphone");
                        }
                    },
                );
            }
        });
    }

    fn device_name(&self, uid: &str) -> String {
        let st = self.audio_status.as_ref();
        st.and_then(|s| s.outputs.iter().chain(s.inputs.iter()).find(|d| d.uid == uid).map(|d| d.name.clone())).unwrap_or_else(|| uid.to_string())
    }

    /// For tests: show these meters (and don't read the file).
    #[cfg(test)]
    pub fn set_meters_for_test(&mut self, m: Meters) {
        self.mixer_ui.meters = Some(m);
        self.mixer_ui.pinned = true;
    }

    /// For tests: select a strip's effects (`src:<id>` / `out:<id>`).
    #[cfg(test)]
    pub fn select_strip_for_test(&mut self, key: Option<&str>) {
        self.mixer_ui.selected = key.map(str::to_string);
    }
}

// ---------------------------------------------------------------------------
// Routing rows

/// One route's rows; returns whether "Remove" was clicked.
fn route_rows(ui: &mut Ui, bundle: &str, r: &mut AppRoute, st: Option<&RouteStatus>, outputs: &[AudioDevice], mac: bool) -> bool {
    let name = if r.name.is_empty() { bundle.to_string() } else { r.name.clone() };
    let mut remove = false;
    let (state_text, state_ok) = match st.map(|s| (&s.state, s.active)) {
        Some((_, true)) => ("Routing now".to_string(), Some(true)),
        Some((StripState::Waiting { reason }, _)) => (format!("Waiting: {reason}"), None),
        Some((StripState::Unavailable { reason }, _)) => (format!("Unavailable: {reason}"), Some(false)),
        Some((StripState::Off, _)) | None => (if r.enabled { "Starts with PowerMac".to_string() } else { "Off".to_string() }, None),
        Some((StripState::Active, _)) => ("Routing now".to_string(), Some(true)),
    };
    let desc = format!("{bundle} · {state_text}");
    icon_row(ui, icon::SWAP, SOUND_TINT, &name, Some(&desc), |ui| {
        if icon_button(ui, icon::TRASH, "Remove this route").clicked() {
            remove = true;
        }
        toggle_labeled(ui, &mut r.enabled, &format!("Route {name}"));
        match state_ok {
            Some(true) => tag_pill(ui, pal(ui).ok, icon::CHECK, "Active"),
            Some(false) => tag_pill(ui, pal(ui).error, icon::WARNING_CIRCLE, "Unavailable"),
            None => {}
        }
    });
    ui.add_enabled_ui(r.enabled, |ui| {
        row(ui, "Plays on", Some("One or more devices (speakers, headphones, PowerMac virtual devices)."), |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut drop = None;
                for (i, d) in r.outputs.iter().enumerate() {
                    let avail = st.and_then(|s| s.targets.get(i)).map(|t| t.available).unwrap_or(true);
                    let label = device_label(d, outputs);
                    let p = pal(ui);
                    let c = if avail { p.accent } else { p.text3 };
                    let resp = egui::Frame::new()
                        .fill(c.gamma_multiply(if p.dark { 0.22 } else { 0.12 }))
                        .corner_radius(10.0)
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(RichText::new(&label).size(12.0).color(c));
                                ui.add(egui::Button::new(theme::icon_text(icon::X).size(11.0).color(c)).frame(false)).clicked()
                            })
                            .inner
                        });
                    if resp.inner {
                        drop = Some(i);
                    }
                    if !avail {
                        resp.response.on_hover_text("Not connected");
                    }
                }
                if let Some(i) = drop {
                    r.outputs.remove(i);
                }
                let choices: Vec<&AudioDevice> = outputs.iter().filter(|d| !r.outputs.iter().any(|m| m.matches(d))).collect();
                if !choices.is_empty() {
                    egui::ComboBox::from_id_salt(("route-target", bundle))
                        .selected_text(theme::icon_text(icon::PLUS).size(12.0))
                        .width(36.0)
                        .show_ui(ui, |ui| {
                            ui.set_min_width(240.0);
                            for d in choices {
                                if ui.selectable_label(false, format!("{}  ·  {}", d.name, d.transport.label())).clicked() {
                                    r.outputs.push(DeviceMatch::uid(&d.uid, &d.name));
                                }
                            }
                        });
                }
            });
        });
        if mac {
            row(ui, "Normal output", Some("Only there: the app is heard on the devices above only. Also: it keeps playing where it did too."), |ui| {
                segmented(ui, &mut r.keep_default, &[(false, "Only there"), (true, "Also keep it")])
            });
            row(ui, "Level", Some("Extra gain on the routed sound."), |ui| db_slider(ui, &mut r.gain_db, -24.0..=12.0));
        }
        if let Some(msg) = st.and_then(|s| s.message.as_deref()) {
            hint(ui, msg);
            ui.add_space(6.0);
        }
    });
    remove
}

fn device_label(d: &DeviceMatch, devices: &[AudioDevice]) -> String {
    devices
        .iter()
        .find(|x| d.matches(x))
        .map(|x| x.name.clone())
        .or_else(|| (!d.name.is_empty()).then(|| d.name.clone()))
        .unwrap_or_else(|| d.uid.clone())
}

fn device_short(d: &DeviceMatch, devices: &[AudioDevice]) -> String {
    let n = device_label(d, devices);
    if n.chars().count() > 18 {
        format!("{}…", n.chars().take(17).collect::<String>())
    } else {
        n
    }
}

pub(super) fn capture_permission_control(ui: &mut Ui, permission: CapturePermission) {
    let (ok, text) = match permission {
        CapturePermission::Granted => (true, "Granted"),
        CapturePermission::Denied => (false, "Denied"),
        CapturePermission::NotDetermined => (false, "Not asked yet"),
        CapturePermission::NotApplicable => (true, "Not needed"),
    };
    if matches!(permission, CapturePermission::Granted | CapturePermission::NotApplicable) {
        status_pill(ui, ok, text);
    } else {
        permission_control(ui, false, "Privacy_AudioCapture");
        ui.label(RichText::new(text).size(12.0).color(pal(ui).text2));
    }
}

fn open_audio_midi_setup() {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").args(["-b", "com.apple.audio.AudioMIDISetup"]).spawn();
}

// ---------------------------------------------------------------------------
// Strips

fn source_kind_label(k: &SourceKind) -> (&'static str, &'static str) {
    match k {
        SourceKind::App { .. } => (icon::APP_WINDOW, "App"),
        SourceKind::Input { .. } => (glyph5::MICROPHONE, "Input"),
        SourceKind::Loopback { .. } => (glyph5::WAVEFORM, "Loopback"),
    }
}

/// A horizontally scrolling row of strips.
fn strips_row(ui: &mut Ui, salt: &str, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::horizontal().id_salt(salt).auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        });
    });
}

const STRIP_W: f32 = 96.0;
const FADER_H: f32 = 148.0;

/// One channel strip: name, state dot, meter + fader, dB value, mute /
/// solo, FX and remove.
#[allow(clippy::too_many_arguments)]
fn strip(
    ui: &mut Ui,
    glyph: &str,
    name: &str,
    sub: &str,
    gain_db: &mut f32,
    muted: &mut bool,
    solo: Option<&mut bool>,
    meter: Option<&MeterReading>,
    state: Option<&StripState>,
    selected: bool,
    fx_on: bool,
) -> StripOut {
    let p = pal(ui);
    let mut out = StripOut::default();
    let border = if selected { p.accent } else { p.card_border };
    egui::Frame::new()
        .fill(if selected { theme::mix(p.card, p.accent, if p.dark { 0.14 } else { 0.06 }) } else { theme::mix(p.card, p.control, 0.5) })
        .stroke(Stroke::new(if selected { 1.5 } else { 1.0 }, border))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 8))
        .show(ui, |ui| {
            ui.set_width(STRIP_W - 16.0);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(theme::icon_text(glyph).size(13.0).color(SOUND_TINT));
                    let (dot, tip) = match state {
                        Some(StripState::Active) => (p.ok, "Running".to_string()),
                        Some(StripState::Waiting { reason }) => (p.warn, format!("Waiting: {reason}")),
                        Some(StripState::Unavailable { reason }) => (p.error, format!("Unavailable: {reason}")),
                        Some(StripState::Off) | None => (p.text3, "Not running".to_string()),
                    };
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (r, resp) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.0, dot);
                        resp.on_hover_text(tip);
                    });
                });
                let shown = if name.is_empty() { "Untitled" } else { name };
                let short = shown.to_string();
                ui.add(egui::Label::new(RichText::new(short).font(theme::font_semibold(12.5)).color(p.text)).truncate()).on_hover_text(shown);
                ui.add(egui::Label::new(RichText::new(sub).size(10.5).color(p.text2)).truncate());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_space(6.0);
                    meter_bars(ui, meter, *muted);
                    ui.spacing_mut().slider_width = FADER_H;
                    let mut v = gain_db.clamp(FADER_MIN, FADER_MAX);
                    let r = ui.add(egui::Slider::new(&mut v, FADER_MIN..=FADER_MAX).vertical().show_value(false));
                    if r.changed() {
                        *gain_db = if v <= FADER_MIN + 0.05 { audio_control::mixer::dsp::MIN_DB } else { (v * 2.0).round() / 2.0 };
                    }
                    if r.double_clicked() {
                        *gain_db = 0.0;
                    }
                });
                let text = if *gain_db <= FADER_MIN { "−∞ dB".to_string() } else { format!("{:+.1} dB", gain_db) };
                ui.label(RichText::new(text).size(11.5).color(p.text2).monospace());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let w = if solo.is_some() { 24.0 } else { 52.0 };
                    if toggle_chip(ui, "M", *muted, p.error, w).on_hover_text("Mute").clicked() {
                        *muted = !*muted;
                    }
                    if let Some(s) = solo {
                        if toggle_chip(ui, "S", *s, Color32::from_rgb(0xF5, 0xB2, 0x00), 24.0).on_hover_text("Solo: only soloed sources are heard").clicked() {
                            *s = !*s;
                        }
                    }
                    if toggle_chip(ui, "FX", fx_on || selected, p.accent, 24.0).on_hover_text("EQ and limiter").clicked() {
                        out.select = true;
                    }
                });
                if ui.add(egui::Button::new(RichText::new("Remove").size(11.0).color(p.text2)).frame(false)).clicked() {
                    out.remove = true;
                }
            });
        });
    out
}

/// A small square toggle button ("M", "S", "FX").
fn toggle_chip(ui: &mut Ui, text: &str, on: bool, c: Color32, w: f32) -> egui::Response {
    let p = pal(ui);
    let (fill, tc) = if on { (c, Color32::WHITE) } else { (p.control, p.text2) };
    ui.add(
        egui::Button::new(RichText::new(text).font(theme::font_semibold(11.0)).color(tc))
            .fill(fill)
            .stroke(Stroke::new(1.0, if on { c } else { p.control_border }))
            .corner_radius(6.0)
            .min_size(vec2(w, 22.0)),
    )
}

/// dBFS → 0..1 on the meter scale (−60 … 0 dB).
fn meter_pos(db: f32) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Two meter bars (left / right): RMS filled, peak as a line, a red cap
/// once it clipped.
fn meter_bars(ui: &mut Ui, m: Option<&MeterReading>, muted: bool) {
    let p = pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(20.0, FADER_H), Sense::hover());
    let painter = ui.painter_at(rect.expand(1.0));
    let bar_w = 8.0;
    for ch in 0..2 {
        let x = rect.left() + ch as f32 * (bar_w + 4.0);
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(bar_w, rect.height()));
        painter.rect_filled(r, 3.0, if p.dark { Color32::from_gray(0x22) } else { Color32::from_gray(0xE4) });
        let Some(m) = m else { continue };
        let rms = meter_pos(m.rms_db()[ch]);
        let peak = meter_pos(m.peak_db()[ch]);
        // Green below −12 dB, yellow to −3, red above (segments of one bar).
        let c = |pos: f32| {
            if pos > 57.0 / 60.0 {
                Color32::from_rgb(0xFF, 0x45, 0x3A)
            } else if pos > 48.0 / 60.0 {
                Color32::from_rgb(0xFF, 0xC4, 0x00)
            } else {
                Color32::from_rgb(0x30, 0xD1, 0x58)
            }
        };
        for (lo, hi) in [(0.0f32, 48.0 / 60.0), (48.0 / 60.0, 57.0 / 60.0), (57.0 / 60.0, 1.0)] {
            if rms <= lo {
                break;
            }
            let top = r.bottom() - r.height() * rms.min(hi);
            let bottom = r.bottom() - r.height() * lo;
            let seg = Rect::from_min_max(pos2(r.left(), top), pos2(r.right(), bottom));
            painter.rect_filled(seg, 2.0, if muted { p.text3 } else { c((lo + hi) / 2.0) });
        }
        let py = r.bottom() - r.height() * peak;
        painter.hline(r.x_range(), py, Stroke::new(2.0, if muted { p.text3 } else { c(peak) }));
        if m.clips > 0 {
            painter.rect_filled(Rect::from_min_size(r.left_top(), vec2(bar_w, 4.0)), 2.0, Color32::from_rgb(0xFF, 0x45, 0x3A));
        }
    }
    // 0 / −12 / −24 / −48 dB ticks.
    for db in [0.0f32, -12.0, -24.0, -48.0] {
        let y = rect.bottom() - rect.height() * meter_pos(db);
        painter.hline(rect.right() - 2.0..=rect.right(), y, Stroke::new(1.0, p.text3));
    }
    if let Some(m) = m {
        let pk = m.peak_db();
        let text = |d: f32| if d.is_finite() { format!("{d:.1}") } else { "−∞".into() };
        let mut tip = format!("Peak {} / {} dBFS", text(pk[0]), text(pk[1]));
        if m.limiter_reduction_db > 0.05 {
            tip.push_str(&format!("\nLimiter −{:.1} dB", m.limiter_reduction_db));
        }
        if m.clips > 0 {
            tip.push_str(&format!("\nClipped {} time{}", m.clips, if m.clips == 1 { "" } else { "s" }));
        }
        resp.on_hover_text(tip);
    }
}

fn unique_id(prefix: &str, taken: impl Fn(&str) -> bool) -> String {
    (1..).map(|n| format!("{prefix}{n}")).find(|id| !taken(id)).expect("an id is free")
}

fn add_source_menu(ui: &mut Ui, m: &Mixer, apps: &[(String, String)], inputs: &[AudioDevice]) -> Option<MixSource> {
    let mut out = None;
    let new_id = || unique_id("src", |id| m.sources.iter().any(|s| s.id == id));
    let p = pal(ui);
    egui::ComboBox::from_id_salt("mix-add-app")
        .selected_text(RichText::new("Add an app…").color(p.text2))
        .width(170.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(240.0);
            if apps.is_empty() {
                ui.label(RichText::new("No running apps found").color(pal(ui).text2));
            }
            for (name, b) in apps {
                if ui.selectable_label(false, name).on_hover_text(b).clicked() {
                    out = Some(MixSource { id: new_id(), name: name.clone(), kind: SourceKind::App { bundle_id: b.clone() }, ..Default::default() });
                }
            }
        });
    let (virt, real): (Vec<&AudioDevice>, Vec<&AudioDevice>) = inputs.iter().partition(|d| is_virtual(d));
    egui::ComboBox::from_id_salt("mix-add-input")
        .selected_text(RichText::new("Add a microphone…").color(p.text2))
        .width(170.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(240.0);
            if real.is_empty() {
                ui.label(RichText::new("No input devices").color(pal(ui).text2));
            }
            for d in &real {
                if ui.selectable_label(false, format!("{}  ·  {}", d.name, d.transport.label())).clicked() {
                    out = Some(MixSource {
                        id: new_id(),
                        name: d.name.clone(),
                        kind: SourceKind::Input { device: DeviceMatch::uid(&d.uid, &d.name) },
                        mono: d.channels == 1,
                        ..Default::default()
                    });
                }
            }
        });
    egui::ComboBox::from_id_salt("mix-add-loop")
        .selected_text(RichText::new("Add a virtual device…").color(p.text2))
        .width(170.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(240.0);
            if virt.is_empty() {
                ui.label(RichText::new("No virtual devices (install PowerMac Audio on the Sound page)").color(pal(ui).text2));
            }
            for d in &virt {
                if ui.selectable_label(false, &d.name).on_hover_text("What apps play to this device").clicked() {
                    out = Some(MixSource { id: new_id(), name: d.name.clone(), kind: SourceKind::Loopback { device: DeviceMatch::uid(&d.uid, &d.name) }, ..Default::default() });
                }
            }
        });
    out
}

fn is_virtual(d: &AudioDevice) -> bool {
    d.transport == Transport::Virtual || d.uid.starts_with("PowerMacAudio") || d.name.contains("BlackHole")
}

fn add_output_menu(ui: &mut Ui, m: &Mixer, outputs: &[AudioDevice]) -> Option<MixOutput> {
    let mut out = None;
    let choices: Vec<&AudioDevice> = outputs.iter().filter(|d| !m.outputs.iter().any(|o| o.device.matches(d))).collect();
    egui::ComboBox::from_id_salt("mix-add-out")
        .selected_text(RichText::new("Add an output…").color(pal(ui).text2))
        .width(200.0)
        .show_ui(ui, |ui| {
            ui.set_min_width(260.0);
            if choices.is_empty() {
                ui.label(RichText::new("Every output is already in the mix").color(pal(ui).text2));
            }
            for d in choices {
                if ui.selectable_label(false, format!("{}  ·  {}", d.name, d.transport.label())).clicked() {
                    out = Some(MixOutput {
                        id: unique_id("out", |id| m.outputs.iter().any(|o| o.id == id)),
                        name: d.name.clone(),
                        device: DeviceMatch::uid(&d.uid, &d.name),
                        ..Default::default()
                    });
                }
            }
        });
    out
}

/// The sources × outputs grid of crosspoints.
fn send_matrix(ui: &mut Ui, m: &mut Mixer) {
    let p = pal(ui);
    if m.sources.is_empty() || m.outputs.is_empty() {
        hint(ui, "Add at least one source and one output.");
        return;
    }
    let outs: Vec<(String, String)> = m.outputs.iter().map(|o| (o.id.clone(), o.name.clone())).collect();
    let srcs: Vec<(String, String)> = m.sources.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
    egui::ScrollArea::horizontal().id_salt("send-matrix").auto_shrink([false, true]).show(ui, |ui| {
        egui::Grid::new("send-grid").spacing(vec2(10.0, 8.0)).min_col_width(96.0).show(ui, |ui| {
            ui.label("");
            for (_, name) in &outs {
                ui.label(RichText::new(short(name, 14)).font(theme::font_semibold(12.0)).color(p.text)).on_hover_text(name);
            }
            ui.end_row();
            for (sid, sname) in &srcs {
                ui.label(RichText::new(short(sname, 16)).size(12.5).color(p.text)).on_hover_text(sname);
                for (oid, oname) in &outs {
                    let send = m.send(sid, oid).cloned();
                    ui.push_id((sid, oid), |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let on = send.as_ref().is_some_and(|s| !s.muted);
                            let r = crosspoint(ui, on, send.is_some());
                            if r.clicked() {
                                match &send {
                                    None => m.set_send(sid, oid, 0.0, false),
                                    Some(s) if s.muted => m.set_send(sid, oid, s.gain_db, false),
                                    Some(_) => m.remove_send(sid, oid),
                                }
                            }
                            r.on_hover_text(format!("{sname} → {oname}"));
                            if let Some(s) = m.sends.iter_mut().find(|s| &s.source == sid && &s.output == oid) {
                                ui.add_sized(
                                    vec2(58.0, 22.0),
                                    egui::DragValue::new(&mut s.gain_db).range(-60.0..=12.0).speed(0.25).fixed_decimals(1).suffix(" dB"),
                                );
                            }
                        });
                    });
                }
                ui.end_row();
            }
        });
    });
}

/// A round crosspoint button (filled when connected).
fn crosspoint(ui: &mut Ui, on: bool, exists: bool) -> egui::Response {
    let p = pal(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    let c = r.center();
    let hover = resp.hovered() && ui.is_enabled();
    if on {
        ui.painter().circle_filled(c, 9.0, SOUND_TINT);
        ui.painter().circle_filled(c, 3.0, Color32::WHITE);
    } else {
        ui.painter().circle(c, 8.5, if hover { p.control_hover } else { p.control }, Stroke::new(1.5, if exists { SOUND_TINT.gamma_multiply(0.6) } else { p.control_border }));
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

/// A slider in dB with a value field.
fn db_slider(ui: &mut Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>) -> egui::Response {
    ui.spacing_mut().slider_width = 150.0;
    let mut r = ui.add(egui::Slider::new(v, range.clone()).show_value(false));
    let d = ui.add_sized(vec2(72.0, 24.0), egui::DragValue::new(v).range(range).speed(0.1).fixed_decimals(1).suffix(" dB"));
    if d.changed() {
        r.mark_changed();
    }
    r
}

/// Source-only options in the effects card.
fn source_options(ui: &mut Ui, s: &mut MixSource) {
    row(ui, "Name", None, |ui| ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(200.0)));
    match &s.kind {
        SourceKind::App { bundle_id } => {
            let b = bundle_id.clone();
            row(ui, "App", Some(&b), |ui| {
                ui.label(RichText::new("Process tap").size(12.0).color(pal(ui).text2));
            });
            toggle_row(ui, "Keep it on its normal output too", Some("Off: the mixer takes the app over and it is heard only through the mix."), &mut s.keep_original);
        }
        SourceKind::Input { .. } | SourceKind::Loopback { .. } => {
            row(ui, "First channel", Some("The stereo pair starts here (1 = channels 1–2)."), |ui| {
                let mut ch = s.first_channel + 1;
                if ui.add(egui::DragValue::new(&mut ch).range(1..=64)).changed() {
                    s.first_channel = ch - 1;
                }
            });
            toggle_row(ui, "Mono", Some("Read one channel and play it on both sides (most microphones)."), &mut s.mono);
        }
    }
}

// ---------------------------------------------------------------------------
// EQ and limiter

fn band_kind_label(k: BandKind) -> &'static str {
    match k {
        BandKind::Peak => "Bell",
        BandKind::LowShelf => "Low shelf",
        BandKind::HighShelf => "High shelf",
        BandKind::LowPass => "Low-pass",
        BandKind::HighPass => "High-pass",
        BandKind::Notch => "Notch",
    }
}

const BAND_KINDS: [BandKind; 6] = [BandKind::Peak, BandKind::LowShelf, BandKind::HighShelf, BandKind::LowPass, BandKind::HighPass, BandKind::Notch];

fn band_color(i: usize) -> Color32 {
    const C: [Color32; 5] = [
        Color32::from_rgb(0xFF, 0x37, 0x5F),
        Color32::from_rgb(0xFF, 0x9F, 0x0A),
        Color32::from_rgb(0x30, 0xD1, 0x58),
        Color32::from_rgb(0x0A, 0x84, 0xFF),
        Color32::from_rgb(0xBF, 0x5A, 0xF2),
    ];
    C[i % C.len()]
}

fn eq_editor(ui: &mut Ui, eq: &mut EqConfig, rate: f64) {
    toggle_row(ui, "Equalizer", Some("Up to 10 parametric bands. Drag a point on the curve, or edit the numbers below."), &mut eq.enabled);
    if !eq.enabled {
        return;
    }
    if eq.bands.is_empty() {
        *eq = EqConfig::flat(5);
    }
    block(ui, |ui| response_curve(ui, eq, rate));
    let mut remove = None;
    for (i, b) in eq.bands.iter_mut().enumerate() {
        ui.push_id(("band", i), |ui| {
            row(ui, &format!("Band {}", i + 1), None, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if icon_button(ui, icon::TRASH, "Remove this band").clicked() {
                    remove = Some(i);
                }
                let gain_used = matches!(b.kind, BandKind::Peak | BandKind::LowShelf | BandKind::HighShelf);
                ui.add_enabled_ui(gain_used, |ui| {
                    ui.add_sized(vec2(64.0, 22.0), egui::DragValue::new(&mut b.gain_db).range(-24.0..=24.0).speed(0.1).fixed_decimals(1).suffix(" dB"));
                });
                ui.add_sized(vec2(56.0, 22.0), egui::DragValue::new(&mut b.q).range(0.1..=24.0).speed(0.01).fixed_decimals(2).prefix("Q "));
                let speed = b.freq_hz.max(10.0) * 0.005;
                ui.add_sized(
                    vec2(76.0, 22.0),
                    egui::DragValue::new(&mut b.freq_hz)
                        .range(10.0..=22_000.0)
                        .speed(speed)
                        .custom_formatter(|v, _| if v >= 1000.0 { format!("{:.2} kHz", v / 1000.0) } else { format!("{v:.0} Hz") }),
                );
                egui::ComboBox::from_id_salt("kind").selected_text(band_kind_label(b.kind)).width(96.0).show_ui(ui, |ui| {
                    for k in BAND_KINDS {
                        ui.selectable_value(&mut b.kind, k, band_kind_label(k));
                    }
                });
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.5, if b.enabled { band_color(i) } else { pal(ui).text3 });
                toggle_labeled(ui, &mut b.enabled, &format!("Band {}", i + 1));
            });
        });
    }
    if let Some(i) = remove {
        eq.bands.remove(i);
    }
    block(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.add_enabled(eq.bands.len() < MAX_BANDS, egui::Button::new("Add band")).clicked() {
                let f = eq.bands.last().map(|b| (b.freq_hz * 1.6).min(16_000.0)).unwrap_or(1000.0);
                eq.bands.push(EqBand { freq_hz: f, ..Default::default() });
            }
            if ui.button("Reset to flat").clicked() {
                *eq = EqConfig::flat(eq.bands.len().clamp(3, MAX_BANDS));
            }
        });
    });
}

/// x position (0..1) of a frequency on the 20 Hz – 20 kHz log axis.
fn freq_x(f: f64) -> f32 {
    (((f.max(20.0)).log10() - 20f64.log10()) / (20_000f64.log10() - 20f64.log10())) as f32
}

fn x_freq(x: f32) -> f64 {
    10f64.powf(20f64.log10() + x.clamp(0.0, 1.0) as f64 * (20_000f64.log10() - 20f64.log10()))
}

const CURVE_DB: f32 = 18.0;

/// The EQ's response from 20 Hz to 20 kHz, with a draggable point per band.
fn response_curve(ui: &mut Ui, eq: &mut EqConfig, rate: f64) {
    let p = pal(ui);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 150.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 8.0, if p.dark { Color32::from_gray(0x1E) } else { Color32::from_gray(0xF6) });
    let y_of = |db: f32| rect.center().y - (db / CURVE_DB).clamp(-1.0, 1.0) * (rect.height() / 2.0 - 8.0);
    // Grid: octave-ish frequencies and ±6 / ±12 dB.
    for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
        let x = rect.left() + rect.width() * freq_x(f);
        painter.vline(x, rect.y_range(), Stroke::new(1.0, p.separator));
        let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
        painter.text(pos2(x + 3.0, rect.bottom() - 3.0), Align2::LEFT_BOTTOM, label, theme::font(9.5), p.text3);
    }
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let y = y_of(db);
        painter.hline(rect.x_range(), y, Stroke::new(if db == 0.0 { 1.2 } else { 1.0 }, if db == 0.0 { p.control_border } else { p.separator }));
        painter.text(pos2(rect.left() + 4.0, y - 1.0), Align2::LEFT_BOTTOM, format!("{db:+}"), theme::font(9.5), p.text3);
    }
    let coeffs: Vec<Coeffs> =
        eq.bands.iter().filter(|b| b.enabled).map(|b| Coeffs::design(b.kind, rate, b.freq_hz as f64, b.gain_db as f64, b.q as f64)).collect();
    let n = (rect.width() as usize).max(2);
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let x = i as f32 / n as f32;
            let db = response_db(&coeffs, x_freq(x), rate) as f32;
            pos2(rect.left() + x * rect.width(), y_of(db))
        })
        .collect();
    // Area under the curve, then the line.
    for w2 in pts.windows(2) {
        let quad = vec![w2[0], w2[1], pos2(w2[1].x, y_of(0.0)), pos2(w2[0].x, y_of(0.0))];
        painter.add(egui::Shape::convex_polygon(quad, SOUND_TINT.gamma_multiply(0.12), Stroke::NONE));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(2.0, SOUND_TINT)));
    for (i, b) in eq.bands.iter_mut().enumerate() {
        let gain_used = matches!(b.kind, BandKind::Peak | BandKind::LowShelf | BandKind::HighShelf);
        let c = pos2(rect.left() + rect.width() * freq_x(b.freq_hz as f64), y_of(if gain_used { b.gain_db } else { 0.0 }));
        let id = ui.scope_id().with(("eq-point", i));
        let hit = Rect::from_center_size(c, vec2(16.0, 16.0));
        let r = ui.interact(hit, id, Sense::drag());
        if r.dragged() {
            if let Some(pos) = r.interact_pointer_pos() {
                b.freq_hz = x_freq((pos.x - rect.left()) / rect.width()) as f32;
                if gain_used {
                    let db = (rect.center().y - pos.y) / (rect.height() / 2.0 - 8.0) * CURVE_DB;
                    b.gain_db = (db * 2.0).round() / 2.0;
                }
            }
        }
        let col = if b.enabled { band_color(i) } else { p.text3 };
        painter.circle(c, if r.hovered() || r.dragged() { 7.0 } else { 5.5 }, col, Stroke::new(1.5, Color32::WHITE));
        r.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(format!("Band {}: {} Hz", i + 1, b.freq_hz.round()));
    }
}

fn effects_rows(ui: &mut Ui, fx: &mut Effects, rate: f64) {
    row(ui, "Input trim", None, |ui| db_slider(ui, &mut fx.gain_db, -24.0..=24.0));
    eq_editor(ui, &mut fx.eq, rate);
    limiter_editor(ui, &mut fx.limiter);
}

fn limiter_editor(ui: &mut Ui, l: &mut LimiterConfig) {
    toggle_row(ui, "Limiter", Some("A look-ahead brickwall limiter: nothing gets louder than the ceiling."), &mut l.enabled);
    ui.add_enabled_ui(l.enabled, |ui| {
        row(ui, "Ceiling", Some("dBFS"), |ui| db_slider(ui, &mut l.ceiling_db, -60.0..=0.0));
        row(ui, "Release", None, |ui| slider(ui, &mut l.release_ms, 1.0..=1000.0, " ms"));
        row(ui, "Look-ahead", Some("Adds this much latency to the strip."), |ui| slider(ui, &mut l.lookahead_ms, 0.0..=20.0, " ms"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales() {
        assert_eq!(meter_pos(f32::NEG_INFINITY), 0.0);
        assert_eq!(meter_pos(0.0), 1.0);
        assert!((meter_pos(-30.0) - 0.5).abs() < 1e-6);
        assert!((freq_x(20.0)).abs() < 1e-6 && (freq_x(20_000.0) - 1.0).abs() < 1e-6);
        assert!((x_freq(freq_x(1000.0)) - 1000.0).abs() < 0.01);
        assert_eq!(unique_id("src", |id| id == "src1" || id == "src2"), "src3");
        assert_eq!(short("PowerMac Audio Mix 1", 10), "PowerMac …");
        let d = AudioDevice { uid: "PowerMacAudio:main".into(), name: "PowerMac Audio".into(), ..Default::default() };
        assert!(is_virtual(&d));
        assert_eq!(device_label(&DeviceMatch::uid("PowerMacAudio:main", "Old"), &[d]), "PowerMac Audio");
        assert_eq!(device_label(&DeviceMatch::name("AirPods"), &[]), "AirPods");
    }
}
