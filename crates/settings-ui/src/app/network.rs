//! The Network page (Security & Network): live per-app connections, the
//! firewall rules model (`net-rules.json`) with a rules editor and Little
//! Snitch `.lsrules` import, and an audit of what the rules would do.
//!
//! The monitor is read-only (libproc / the IP Helper tables, like `lsof -i`
//! and `netstat`) and safe to run here, so this window takes its own
//! snapshots on a background thread every 1.5 s while the page is open —
//! the app isn't involved. Nothing is blocked: enforcing rules needs a
//! Network Extension content filter (an entitlement only Apple grants), and
//! no system firewall setting is ever changed.

use std::collections::BTreeSet;
use std::sync::mpsc;

use super::*;
use super::pages::callout;

use net_monitor::rules::{self, AuditEntry, DirectionMatch, Ports};
use net_monitor::{Action, AppConnections, AppMatch, Direction, Protocol, RemoteMatch, Rule, RuleSet, Scope, Snapshot};

const NET_TINT: Color32 = Color32::from_rgb(0x00, 0x8C, 0xA8);
const DOCS_URL: &str = "https://github.com/finepointcgi/PowerMac/blob/master/docs/features/network-monitor.md";
/// How often the connections are re-read while the page is open.
const REFRESH: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum NetView {
    #[default]
    Connections,
    Rules,
    Audit,
}

/// A rule being edited (new, or an existing one by id).
#[derive(Debug, Clone, PartialEq)]
struct RuleEdit {
    id: Option<String>,
    action: Action,
    app: String,
    remote: String,
    ports: String,
    protocol: Option<Protocol>,
    direction: DirectionMatch,
    high_priority: bool,
    note: String,
    error: Option<String>,
}

impl RuleEdit {
    fn new(app: &str, action: Action) -> Self {
        RuleEdit {
            id: None,
            action,
            app: app.to_string(),
            remote: "any".into(),
            ports: "any".into(),
            protocol: None,
            direction: DirectionMatch::Outgoing,
            high_priority: false,
            note: String::new(),
            error: None,
        }
    }

    fn of(r: &Rule) -> Self {
        RuleEdit {
            id: Some(r.id.clone()),
            action: r.action,
            app: match &r.app {
                AppMatch::Any => String::new(),
                AppMatch::Bundle(p) | AppMatch::Path(p) => p.display().to_string(),
                AppMatch::Name(n) => n.clone(),
            },
            remote: match &r.remote {
                RemoteMatch::Any => "any".into(),
                RemoteMatch::LocalNetwork => "local-net".into(),
                other => other.label(),
            },
            ports: if r.ports.is_any() { "any".into() } else { r.ports.label() },
            protocol: r.protocol,
            direction: r.direction,
            high_priority: r.high_priority,
            note: r.note.clone(),
            error: None,
        }
    }

    /// The rule, or what's wrong with the fields.
    fn build(&self, base: Option<&Rule>) -> Result<Rule, String> {
        let remote = RemoteMatch::parse(&self.remote).map_err(|e| format!("Remote: {e:#}"))?;
        let ports = Ports::parse(&self.ports).map_err(|e| format!("Ports: {e:#}"))?;
        let mut r = base.cloned().unwrap_or_default();
        r.action = self.action;
        r.app = app_match(&self.app);
        r.remote = remote;
        r.ports = ports;
        r.protocol = self.protocol;
        r.direction = self.direction;
        r.high_priority = self.high_priority;
        r.note = self.note.trim().to_string();
        Ok(r)
    }
}

/// `/Applications/Safari.app` → bundle, `/usr/bin/curl` → path, `curl` →
/// name, empty / "any" → any app (as `powermac net rules add --app`).
fn app_match(s: &str) -> AppMatch {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("any") {
        AppMatch::Any
    } else if s.starts_with('/') {
        AppMatch::for_path(std::path::Path::new(s))
    } else {
        AppMatch::Name(s.to_string())
    }
}

/// Network page state.
#[derive(Default)]
pub(super) struct NetUi {
    snap: Option<Snapshot>,
    snap_at: Option<Instant>,
    rx: Option<mpsc::Receiver<Result<Snapshot, String>>>,
    error: Option<String>,
    rules: Option<RuleSet>,
    rules_mtime: Option<SystemTime>,
    rules_error: Option<String>,
    filter: String,
    view: NetView,
    /// Expanded apps (by key).
    open: BTreeSet<String>,
    listeners: bool,
    editor: Option<RuleEdit>,
    import_path: String,
    note: Option<Result<String, String>>,
    audit_all: bool,
    /// Tests pin the snapshot.
    pinned: bool,
}

impl SettingsApp {
    fn rules_dir(&self) -> std::path::PathBuf {
        pm_config::data_dir_for(&self.config_path)
    }

    pub(super) fn poll_network(&mut self) {
        if self.page != Page::Network {
            return;
        }
        let dir = self.rules_dir();
        let m = mtime(&RuleSet::path_in(&dir));
        if self.net.rules.is_none() || (m != self.net.rules_mtime && self.net.editor.is_none()) {
            self.net.rules_mtime = m;
            match RuleSet::load(&dir) {
                Ok(r) => {
                    self.net.rules = Some(r);
                    self.net.rules_error = None;
                }
                Err(e) => {
                    self.net.rules_error = Some(format!("{e:#}"));
                    self.net.rules.get_or_insert_with(RuleSet::default);
                }
            }
        }
    }

    pub(super) fn tick_network(&mut self, ctx: &egui::Context) {
        if self.page != Page::Network || self.net.pinned {
            return;
        }
        if let Some(rx) = &self.net.rx {
            match rx.try_recv() {
                Ok(r) => {
                    self.net.rx = None;
                    match r {
                        Ok(s) => {
                            self.net.snap = Some(s);
                            self.net.error = None;
                        }
                        Err(e) => self.net.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.net.rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.net.rx.is_none() && self.net.snap_at.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.net.snap_at = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            self.net.rx = Some(rx);
            let _ = std::thread::Builder::new().name("net-snapshot".into()).spawn(move || {
                let _ = tx.send(net_monitor::snapshot().map_err(|e| format!("{e:#}")));
            });
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    /// For tests: show this snapshot and these rules (nothing is read).
    #[cfg(test)]
    pub fn set_network_for_test(&mut self, snap: Snapshot, rules: RuleSet, view: u8) {
        self.net.snap = Some(snap);
        self.net.rules = Some(rules);
        self.net.pinned = true;
        self.net.editor = None;
        if view == 3 {
            self.net.editor = self.net.rules.as_ref().and_then(|r| r.rules.get(2)).map(RuleEdit::of);
        }
        self.net.view = match view {
            1 | 3 => NetView::Rules,
            2 => NetView::Audit,
            _ => NetView::Connections,
        };
        if let Some(s) = &self.net.snap {
            if let Some(a) = s.by_app().first() {
                self.net.open.insert(a.key.clone());
            }
        }
    }

    fn save_rules(&mut self) {
        let dir = self.rules_dir();
        if let Some(r) = &self.net.rules {
            match r.save(&dir) {
                Ok(()) => {
                    self.net.rules_mtime = mtime(&RuleSet::path_in(&dir));
                    self.net.rules_error = None;
                    self.toast = Some((Instant::now(), "Saved".into()));
                }
                Err(e) => self.net.rules_error = Some(format!("{e:#}")),
            }
        }
    }

    pub(super) fn network_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        // The banner: nothing is enforced.
        let p = pal(ui);
        let c = Color32::from_rgb(0xE8, 0x9A, 0x00);
        ui.add_space(16.0);
        egui::Frame::new()
            .fill(theme::mix(p.card, c, if p.dark { 0.16 } else { 0.08 }))
            .stroke(Stroke::new(1.0, theme::mix(p.card_border, c, 0.4)))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    ui.label(theme::icon_text(glyph5::SHIELD_WARNING).size(20.0).color(c));
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Monitoring only — nothing is blocked").font(theme::font_semibold(14.0)).color(p.text));
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "The connection list is read-only and the rules below are a model: PowerMac checks them against live traffic and shows what they would do. \
                                     Blocking needs a Network Extension content filter, which requires an entitlement only Apple's developer program grants. \
                                     No system firewall setting is changed.",
                                )
                                .size(12.0)
                                .color(p.text2),
                            )
                            .wrap(),
                        );
                        ui.hyperlink_to(RichText::new("How the monitor and the rules work").size(12.0), DOCS_URL);
                    });
                });
            });
        ui.add_space(14.0);
        let mut view = self.net.view;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            segmented(ui, &mut view, &[(NetView::Connections, "Connections"), (NetView::Rules, "Rules"), (NetView::Audit, "Audit")]);
            if view != NetView::Rules {
                ui.add_space(10.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.net.filter)
                        .hint_text("Filter apps, addresses, ports")
                        .desired_width(260.0),
                );
            }
        });
        self.net.view = view;
        match view {
            NetView::Connections => self.net_connections(ui),
            NetView::Rules => self.net_rules(ui),
            NetView::Audit => self.net_audit(ui),
        }
    }

    fn net_connections(&mut self, ui: &mut Ui) {
        let Some(snap) = self.net.snap.clone() else {
            card(ui, "Connections", |ui| {
                block(ui, |ui| match &self.net.error {
                    Some(e) => error_label(ui, e),
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            hint(ui, "Reading the connections…");
                        });
                    }
                })
            });
            return;
        };
        let apps = snap.by_app();
        let q = self.net.filter.trim().to_lowercase();
        let listeners = self.net.listeners;
        let shown: Vec<&AppConnections> = apps
            .iter()
            .filter(|a| listeners || a.active() > 0)
            .filter(|a| q.is_empty() || app_matches_filter(a, &q))
            .collect();
        let total: usize = apps.iter().map(|a| a.active()).sum();
        let sub = format!(
            "{} with {} — updated every 1.5 s.{}",
            plural(apps.iter().filter(|a| a.active() > 0).count(), "app"),
            plural(total, "connection"),
            if snap.inaccessible > 0 {
                format!(
                    " {} {} of other users or the system weren't readable (run `sudo powermac net list` to see them).",
                    snap.inaccessible,
                    if snap.inaccessible == 1 { "process" } else { "processes" }
                )
            } else {
                String::new()
            }
        );
        let mut new_rule: Option<RuleEdit> = None;
        let open = &mut self.net.open;
        card_with(ui, "Connections", Some(&sub), |ui| {
            toggle_row(ui, "Show listening and unconnected sockets", None, &mut self.net.listeners);
            if shown.is_empty() {
                block(ui, |ui| hint(ui, if q.is_empty() { "No connections right now." } else { "Nothing matches the filter." }));
            }
            for a in shown {
                ui.push_id(("app", &a.key), |ui| {
                    let is_open = open.contains(&a.key) || !q.is_empty();
                    let mut desc = plural(a.active(), "connection");
                    if a.listeners() > 0 {
                        desc.push_str(&format!(", {} listening", a.listeners()));
                    }
                    let scopes = app_scopes(a);
                    let (_, label) = icon_row(ui, if is_open { icon::CARET_DOWN } else { icon::CARET_RIGHT }, NET_TINT, &a.name, Some(&desc), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if ui.small_button("Rule…").on_hover_text("Write a rule for this app").clicked() {
                            let app = a.bundle.as_ref().or(a.path.as_ref()).map(|p| p.display().to_string()).unwrap_or_else(|| a.name.clone());
                            new_rule = Some(RuleEdit::new(&app, Action::Deny));
                        }
                        for s in scopes.iter().rev() {
                            scope_badge(ui, *s);
                        }
                    });
                    if label.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && q.is_empty() && !open.remove(&a.key) {
                        open.insert(a.key.clone());
                    }
                    if is_open {
                        block(ui, |ui| {
                            for c in a.connections.iter().filter(|c| listeners || c.socket.remote.is_some()) {
                                connection_line(ui, c);
                            }
                        });
                    }
                });
            }
        });
        if let Some(e) = new_rule {
            self.net.editor = Some(e);
            self.net.view = NetView::Rules;
        }
    }

    fn net_rules(&mut self, ui: &mut Ui) {
        let mut changed = false;
        let mut edit: Option<RuleEdit> = None;
        let mut remove: Option<String> = None;
        let rules = self.net.rules.get_or_insert_with(RuleSet::default);
        card_with(
            ui,
            "Rules",
            Some("The most specific matching rule decides, as in Little Snitch; between equally specific rules deny beats ask beats allow. Stored in net-rules.json (also edited by `powermac net rules`)."),
            |ui| {
                let before = rules.default_action;
                row(ui, "When no rule matches", Some("Allow: a list of apps to block. Deny: an allow-list."), |ui| {
                    segmented(ui, &mut rules.default_action, &[(Action::Allow, "Allow"), (Action::Ask, "Ask"), (Action::Deny, "Deny")])
                });
                changed |= rules.default_action != before;
                if rules.rules.is_empty() {
                    block(ui, |ui| hint(ui, "No rules yet. Add one, or write one from an app on the Connections tab."));
                }
                for r in rules.rules.iter_mut() {
                    let rid = r.id.clone();
                    ui.push_id(("rule", rid), |ui| {
                        let mut desc = format!("{} · {}", r.id, rule_detail(r));
                        if let Some(s) = &r.source {
                            desc.push_str(&format!(" · from {s}"));
                        }
                        if !r.note.is_empty() {
                            desc.push_str(&format!(" · {}", r.note));
                        }
                        let title = format!("{} {} {}", r.app.label(), arrow(r.direction), r.remote.label());
                        icon_row(ui, action_glyph(r.action), action_color(ui, r.action), &title, Some(&desc), |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                remove = Some(r.id.clone());
                            }
                            if icon_button(ui, icon::PENCIL_SIMPLE, "Edit").clicked() {
                                edit = Some(RuleEdit::of(r));
                            }
                            if toggle_labeled(ui, &mut r.enabled, &title).changed() {
                                changed = true;
                            }
                            action_pill(ui, r.action);
                        });
                    });
                }
                block(ui, |ui| {
                    ui.horizontal(|ui| {
                        if icon_text_button(ui, icon::PLUS, "Add rule").clicked() {
                            edit = Some(RuleEdit::new("", Action::Deny));
                        }
                    });
                });
            },
        );
        if let Some(id) = remove {
            rules.remove(&id);
            changed = true;
        }
        if let Some(e) = edit {
            self.net.editor = Some(e);
        }
        if changed {
            self.save_rules();
        }
        self.rule_editor_card(ui);

        // Import
        let busy = self.busy.contains("lsrules-choose");
        let mut choose = false;
        let mut import = false;
        card_with(ui, "Import Little Snitch rules", Some("A rule group (.lsrules, the JSON format of Little Snitch's rule group subscriptions). Remotes the model can't express are listed as skipped."), |ui| {
            row(ui, "File", None, |ui| {
                if ui.add_enabled(!self.net.import_path.trim().is_empty(), egui::Button::new("Import")).clicked() {
                    import = true;
                }
                if cfg!(target_os = "macos") && ui.add_enabled(!busy, egui::Button::new("Choose…")).clicked() {
                    choose = true;
                }
                ui.add(egui::TextEdit::singleline(&mut self.net.import_path).hint_text("~/Downloads/rules.lsrules").desired_width(240.0));
            });
            match &self.net.note {
                Some(Ok(m)) => block(ui, |ui| ok_label(ui, m)),
                Some(Err(e)) => block(ui, |ui| error_label(ui, e)),
                None => {}
            }
            if let Some(e) = &self.net.rules_error {
                block(ui, |ui| error_label(ui, e));
            }
        });
        if let Some(Ok(p)) = self.notes.remove("lsrules-choose") {
            if !p.is_empty() {
                self.net.import_path = p;
            }
        }
        if choose {
            self.run_background("lsrules-choose", choose_lsrules);
        }
        if import {
            self.net.note = Some(self.import_lsrules());
        }
    }

    fn import_lsrules(&mut self) -> Result<String, String> {
        let p = self.net.import_path.trim();
        let path = match p.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rest)).unwrap_or_else(|| p.into()),
            None => std::path::PathBuf::from(p),
        };
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let imp = rules::import_lsrules(&text).map_err(|e| format!("{e:#}"))?;
        let n = imp.rules.len();
        let set = self.net.rules.get_or_insert_with(RuleSet::default);
        for r in imp.rules {
            set.add(r);
        }
        self.save_rules();
        let mut msg = format!("Imported {} from “{}”.", plural(n, "rule"), imp.name);
        if !imp.skipped.is_empty() {
            msg.push_str(&format!(" Skipped {}: {}", imp.skipped.len(), imp.skipped.iter().take(3).cloned().collect::<Vec<_>>().join("; ")));
            if imp.skipped.len() > 3 {
                msg.push('…');
            }
        }
        Ok(msg)
    }

    fn rule_editor_card(&mut self, ui: &mut Ui) {
        let Some(mut e) = self.net.editor.take() else { return };
        let apps: Vec<(String, String)> = self
            .net
            .snap
            .as_ref()
            .map(|s| {
                s.by_app().into_iter().map(|a| (a.name.clone(), a.bundle.or(a.path).map(|p| p.display().to_string()).unwrap_or(a.name))).collect()
            })
            .unwrap_or_default();
        let mut save = false;
        let mut cancel = false;
        let title = if e.id.is_some() { "Edit rule" } else { "New rule" };
        card_with(ui, title, None, |ui| {
            row(ui, "Action", None, |ui| segmented(ui, &mut e.action, &[(Action::Allow, "Allow"), (Action::Ask, "Ask"), (Action::Deny, "Deny")]));
            row(ui, "App", Some("An app bundle, an executable path, or a name (curl). Empty: any app."), |ui| {
                egui::ComboBox::from_id_salt("rule-app-pick").selected_text("").width(24.0).show_ui(ui, |ui| {
                    ui.set_min_width(280.0);
                    if ui.selectable_label(e.app.is_empty(), "Any app").clicked() {
                        e.app.clear();
                    }
                    for (name, path) in &apps {
                        if ui.selectable_label(false, name).on_hover_text(path).clicked() {
                            e.app = path.clone();
                        }
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut e.app).hint_text("Any app").desired_width(260.0))
            });
            row(ui, "Remote", Some("any, local-net, 1.2.3.4, 10.0.0.0/8, api.example.com or *.example.com."), |ui| {
                ui.add(egui::TextEdit::singleline(&mut e.remote).desired_width(260.0))
            });
            row(ui, "Ports", Some("Remote ports: any, 443, 80, 443 or 8000-8100."), |ui| ui.add(egui::TextEdit::singleline(&mut e.ports).desired_width(160.0)));
            row(ui, "Protocol", None, |ui| segmented(ui, &mut e.protocol, &[(None, "Both"), (Some(Protocol::Tcp), "TCP"), (Some(Protocol::Udp), "UDP")]));
            row(ui, "Direction", None, |ui| {
                segmented(ui, &mut e.direction, &[(DirectionMatch::Outgoing, "Outgoing"), (DirectionMatch::Incoming, "Incoming"), (DirectionMatch::Both, "Both")])
            });
            toggle_row(ui, "High priority", Some("Wins over every regular rule."), &mut e.high_priority);
            row(ui, "Note", None, |ui| ui.add(egui::TextEdit::singleline(&mut e.note).desired_width(260.0)));
            if let Some(err) = &e.error {
                block(ui, |ui| error_label(ui, err));
            }
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if primary_button(ui, if e.id.is_some() { "Save rule" } else { "Add rule" }).clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if let Ok(r) = e.build(None) {
                        hint(ui, &r.summary());
                    }
                });
            });
        });
        if cancel {
            return;
        }
        if save {
            let set = self.net.rules.get_or_insert_with(RuleSet::default);
            let base = e.id.as_ref().and_then(|id| set.rules.iter().find(|r| &r.id == id)).cloned();
            match e.build(base.as_ref()) {
                Ok(r) => {
                    match &e.id {
                        Some(id) => {
                            if let Some(slot) = set.get_mut(id) {
                                *slot = r;
                            }
                        }
                        None => {
                            set.add(r);
                        }
                    }
                    self.save_rules();
                    return;
                }
                Err(err) => e.error = Some(err),
            }
        }
        self.net.editor = Some(e);
    }

    fn net_audit(&mut self, ui: &mut Ui) {
        let (Some(snap), Some(rules)) = (self.net.snap.clone(), self.net.rules.clone()) else {
            card(ui, "Audit", |ui| block(ui, |ui| hint(ui, "Reading the connections…")));
            return;
        };
        let entries = rules.audit(&snap);
        let q = self.net.filter.trim().to_lowercase();
        let blocked: Vec<&AuditEntry> = entries.iter().filter(|e| e.verdict.action != Action::Allow).collect();
        let shown: Vec<&AuditEntry> = entries
            .iter()
            .filter(|e| self.net.audit_all || e.verdict.action != Action::Allow)
            .filter(|e| q.is_empty() || e.app.to_lowercase().contains(&q) || net_monitor::describe(&e.connection.socket).to_lowercase().contains(&q))
            .collect();
        let sub = format!(
            "What the rules would do to the {} open right now: {} denied, {} asked about. Host and domain rules only decide connections whose host name is known, which the socket table doesn't have.",
            plural(entries.len(), "connection"),
            blocked.iter().filter(|e| e.verdict.action == Action::Deny).count(),
            blocked.iter().filter(|e| e.verdict.action == Action::Ask).count(),
        );
        card_with(ui, "Audit", Some(&sub), |ui| {
            toggle_row(ui, "Show allowed connections too", None, &mut self.net.audit_all);
            if shown.is_empty() {
                block(ui, |ui| {
                    if blocked.is_empty() && !self.net.audit_all {
                        ok_label(ui, "The rules wouldn't deny or ask about anything open right now.");
                    } else {
                        hint(ui, "Nothing matches the filter.");
                    }
                });
            }
            for (i, e) in shown.iter().enumerate() {
                ui.push_id(("audit", i), |ui| {
                    let rule = match &e.verdict.rule {
                        Some(id) => rules.rules.iter().find(|r| &r.id == id).map(|r| format!("rule {id}: {}", r.summary())).unwrap_or_else(|| format!("rule {id}")),
                        None => format!("default ({})", rules.default_action.label()),
                    };
                    let desc = format!("{} · {rule}", net_monitor::describe(&e.connection.socket));
                    icon_row(ui, action_glyph(e.verdict.action), action_color(ui, e.verdict.action), &e.app, Some(&desc), |ui| {
                        action_pill(ui, e.verdict.action);
                        if let Some(s) = e.connection.socket.remote_scope() {
                            scope_badge(ui, s);
                        }
                    });
                });
            }
        });
        if rules.rules.is_empty() {
            callout(ui, icon::INFO, "No rules yet", "Everything is allowed by default. Add rules on the Rules tab to see what they would block.");
        }
    }
}

fn app_matches_filter(a: &AppConnections, q: &str) -> bool {
    a.name.to_lowercase().contains(q)
        || a.connections.iter().any(|c| net_monitor::describe(&c.socket).to_lowercase().contains(q) || c.process.to_lowercase().contains(q))
}

/// The distinct scopes of an app's remote ends (internet first).
fn app_scopes(a: &AppConnections) -> Vec<Scope> {
    let mut v: Vec<Scope> = a.connections.iter().filter_map(|c| c.socket.remote_scope()).collect();
    v.sort_by_key(|s| match s {
        Scope::Internet => 0,
        Scope::LocalNetwork => 1,
        Scope::Loopback => 2,
    });
    v.dedup();
    v
}

fn scope_badge(ui: &mut Ui, s: Scope) {
    let (c, glyph) = match s {
        Scope::Internet => (Color32::from_rgb(0xE8, 0x7A, 0x00), glyph5::GLOBE),
        Scope::LocalNetwork => (Color32::from_rgb(0x0A, 0x84, 0xFF), icon::NETWORK),
        Scope::Loopback => (pal(ui).text2, icon::ARROW_COUNTER_CLOCKWISE),
    };
    tag_pill(ui, c, glyph, s.label());
}

fn connection_line(ui: &mut Ui, c: &net_monitor::Connection) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let dir = match c.direction {
            Direction::Outgoing => "↗",
            Direction::Incoming => "↘",
        };
        ui.label(RichText::new(dir).size(12.0).color(p.text2));
        ui.label(RichText::new(net_monitor::describe(&c.socket)).monospace().size(11.5).color(p.text));
        if let Some(s) = c.socket.remote_scope() {
            ui.label(RichText::new(s.label()).size(11.0).color(p.text3));
        }
        ui.label(RichText::new(&c.process).size(11.0).color(p.text3));
    });
}

fn arrow(d: DirectionMatch) -> &'static str {
    match d {
        DirectionMatch::Outgoing => "→",
        DirectionMatch::Incoming => "←",
        DirectionMatch::Both => "↔",
    }
}

fn rule_detail(r: &Rule) -> String {
    let mut v = vec![r.ports.label()];
    v.push(r.protocol.map(|p| p.label().to_string()).unwrap_or_else(|| "TCP and UDP".into()));
    if r.high_priority {
        v.push("high priority".into());
    }
    v.join(", ")
}

fn action_glyph(a: Action) -> &'static str {
    match a {
        Action::Allow => icon::CHECK_CIRCLE,
        Action::Ask => icon::INFO,
        Action::Deny => icon::X,
    }
}

fn action_color(ui: &Ui, a: Action) -> Color32 {
    let p = pal(ui);
    match a {
        Action::Allow => p.ok,
        Action::Ask => Color32::from_rgb(0xE8, 0x9A, 0x00),
        Action::Deny => p.error,
    }
}

fn action_pill(ui: &mut Ui, a: Action) {
    let c = action_color(ui, a);
    let text = match a {
        Action::Allow => "Allow",
        Action::Ask => "Ask",
        Action::Deny => "Deny",
    };
    tag_pill(ui, c, action_glyph(a), text);
}

fn choose_lsrules() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "POSIX path of (choose file with prompt \"Choose a Little Snitch rule group (.lsrules)\")"])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
    #[cfg(not(target_os = "macos"))]
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_edit_round_trip() {
        let mut e = RuleEdit::new("/Applications/Safari.app", Action::Deny);
        e.remote = "*.example.com".into();
        e.ports = "80, 443".into();
        e.protocol = Some(Protocol::Tcp);
        let r = e.build(None).unwrap();
        assert_eq!(r.app, AppMatch::Bundle("/Applications/Safari.app".into()));
        assert_eq!(r.remote, RemoteMatch::Domain("example.com".into()));
        assert_eq!(r.ports.label(), "80, 443");
        let back = RuleEdit::of(&r);
        assert_eq!((back.app.as_str(), back.remote.as_str(), back.ports.as_str()), ("/Applications/Safari.app", "*.example.com", "80, 443"));
        assert_eq!(back.build(Some(&r)).unwrap(), r);
        assert_eq!(app_match(""), AppMatch::Any);
        assert_eq!(app_match("curl"), AppMatch::Name("curl".into()));
        let mut bad = RuleEdit::new("", Action::Allow);
        bad.ports = "eighty".into();
        assert!(bad.build(None).unwrap_err().starts_with("Ports"));
        bad.ports = "any".into();
        bad.remote = "not a host!".into();
        assert!(bad.build(None).unwrap_err().starts_with("Remote"));
        let mut lan = RuleEdit::new("", Action::Allow);
        lan.remote = "local-net".into();
        assert_eq!(RuleEdit::of(&lan.build(None).unwrap()).remote, "local-net");
    }
}
