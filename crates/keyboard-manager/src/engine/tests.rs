use super::*;
use crate::config::{KeyRemap, PastePlainText, ShortcutRemap};
use crate::keys::Key;

fn d(vk: u32) -> Action {
    Action::down(vk)
}
fn u(vk: u32) -> Action {
    Action::up(vk)
}

fn sc(s: &str) -> Shortcut {
    s.parse().unwrap()
}

fn cfg(keys: &[(u32, Target)], shortcuts: &[(&str, Target, Option<&str>)]) -> Config {
    Config {
        enabled: true,
        remap_keys: keys
            .iter()
            .map(|(f, t)| KeyRemap {
                from: Key(*f),
                to: t.clone(),
            })
            .collect(),
        remap_shortcuts: shortcuts
            .iter()
            .map(|(f, t, app)| ShortcutRemap {
                from: sc(f),
                to: t.clone(),
                app: app.map(String::from),
            })
            .collect(),
        paste_plain_text: PastePlainText {
            enabled: false,
            ..Default::default()
        },
        windows_style: Default::default(),
    }
}

/// Engine plus a simulated OS that applies outcomes, checking invariants.
struct Harness {
    e: Engine,
    os: BTreeSet<u32>,
    app: Vec<String>,
    app_queries: usize,
    log: Vec<String>,
}

impl Harness {
    fn new(c: Config) -> Self {
        Self::with_opts(
            c,
            EngineOptions {
                dummy_key: false,
                windows_style: true,
            },
        )
    }
    fn with_opts(c: Config, o: EngineOptions) -> Self {
        Harness {
            e: Engine::new(&c, o),
            os: BTreeSet::new(),
            app: vec![],
            app_queries: 0,
            log: vec![],
        }
    }

    fn ev(&mut self, vk: u32, down: bool) -> Outcome {
        let app = self.app.clone();
        let q = &mut self.app_queries;
        let out = self.e.process(vk, down, &mut || {
            *q += 1;
            app.clone()
        });
        self.log.push(format!(
            "{} {} -> {:?}",
            crate::keys::key_name(vk),
            if down { "down" } else { "up" },
            out
        ));
        match &out {
            Outcome::Pass => self.apply(vk, down),
            Outcome::Replace(acts) => {
                for a in acts {
                    if let Action::Key { vk, down } = *a {
                        if vk != VK_DUMMY {
                            self.apply(vk, down);
                        }
                    }
                }
            }
        }
        assert_eq!(&self.os, self.e.os_keys(), "engine os view diverged");
        out
    }

    fn apply(&mut self, vk: u32, down: bool) {
        if down {
            self.os.insert(vk);
        } else {
            assert!(
                self.os.remove(&vk),
                "key-up for {vk:#x} which the OS does not have down"
            );
        }
    }

    fn down(&mut self, vk: u32) -> Outcome {
        self.ev(vk, true)
    }
    fn up(&mut self, vk: u32) -> Outcome {
        self.ev(vk, false)
    }
    /// Press and release, return both outcomes.
    fn tap(&mut self, vk: u32) -> (Outcome, Outcome) {
        (self.down(vk), self.up(vk))
    }

    fn assert_clean(&self) {
        assert!(
            self.os.is_empty(),
            "stuck keys: {:x?}\n{}",
            self.os,
            self.log.join("\n")
        );
        assert!(self.e.physical_keys().is_empty());
        assert!(!self.e.shortcut_active());
    }
}

fn rep(a: Vec<Action>) -> Outcome {
    Outcome::Replace(a)
}

const PASS: Outcome = Outcome::Pass;

// ---------------------------------------------------------------------------
// Single key remaps
// ---------------------------------------------------------------------------

#[test]
fn no_config_passes_everything() {
    let mut h = Harness::new(Config::default());
    for vk in [VK_LCONTROL, VK_C, VK_LWIN, VK_RSHIFT, VK_CAPITAL] {
        assert_eq!(h.down(vk), PASS);
    }
    for vk in [VK_C, VK_LWIN, VK_RSHIFT, VK_LCONTROL, VK_CAPITAL] {
        assert_eq!(h.up(vk), PASS);
    }
    assert_eq!(h.app_queries, 0);
    h.assert_clean();
}

#[test]
fn capslock_to_escape() {
    let mut h = Harness::new(cfg(&[(VK_CAPITAL, Target::Key(VK_ESCAPE))], &[]));
    assert_eq!(h.down(VK_CAPITAL), rep(vec![d(VK_ESCAPE)]));
    assert_eq!(h.down(VK_CAPITAL), rep(vec![d(VK_ESCAPE)]), "auto-repeat");
    assert_eq!(h.up(VK_CAPITAL), rep(vec![u(VK_ESCAPE)]));
    // other keys untouched
    assert_eq!(h.tap(VK_A), (PASS, PASS));
    h.assert_clean();
}

#[test]
fn swap_option_and_command() {
    let mut h = Harness::new(cfg(
        &[
            (VK_LMENU, Target::Key(VK_LWIN)),
            (VK_LWIN, Target::Key(VK_LMENU)),
            (VK_RMENU, Target::Key(VK_RWIN)),
            (VK_RWIN, Target::Key(VK_RMENU)),
        ],
        &[],
    ));
    // Physical Option+C -> Cmd+C
    assert_eq!(h.down(VK_LMENU), rep(vec![d(VK_LWIN)]));
    assert_eq!(h.tap(VK_C), (PASS, PASS));
    assert_eq!(h.up(VK_LMENU), rep(vec![u(VK_LWIN)]));
    // Physical Cmd -> Option
    assert_eq!(h.down(VK_RWIN), rep(vec![d(VK_RMENU)]));
    assert_eq!(h.up(VK_RWIN), rep(vec![u(VK_RMENU)]));
    h.assert_clean();
}

#[test]
fn generic_source_covers_both_sides() {
    let mut h = Harness::new(cfg(&[(VK_CONTROL, Target::Key(VK_LWIN))], &[]));
    assert_eq!(h.down(VK_RCONTROL), rep(vec![d(VK_LWIN)]));
    assert_eq!(h.up(VK_RCONTROL), rep(vec![u(VK_LWIN)]));
    // Generic target "Win" (either) injects the left key
    let mut h = Harness::new(cfg(&[(VK_CAPITAL, Target::Key(VK_WIN_BOTH))], &[]));
    assert_eq!(h.down(VK_CAPITAL), rep(vec![d(VK_LWIN)]));
    assert_eq!(h.up(VK_CAPITAL), rep(vec![u(VK_LWIN)]));
}

#[test]
fn disabled_key() {
    let mut h = Harness::new(cfg(
        &[
            (VK_INSERT, Target::Disabled),
            (VK_F1, Target::Key(VK_DISABLED)),
        ],
        &[],
    ));
    assert_eq!(h.tap(VK_INSERT), (rep(vec![]), rep(vec![])));
    assert_eq!(h.down(VK_F1), rep(vec![]));
    assert_eq!(h.down(VK_F1), rep(vec![]));
    assert_eq!(h.up(VK_F1), rep(vec![]));
    h.assert_clean();
}

#[test]
fn key_to_shortcut() {
    let mut h = Harness::new(cfg(&[(VK_F5, Target::Shortcut(sc("Ctrl+Shift+T")))], &[]));
    assert_eq!(
        h.down(VK_F5),
        rep(vec![d(VK_LCONTROL), d(VK_LSHIFT), d(VK_T)])
    );
    assert_eq!(
        h.down(VK_F5),
        rep(vec![d(VK_T)]),
        "repeat only repeats the action key"
    );
    assert_eq!(
        h.up(VK_F5),
        rep(vec![u(VK_T), u(VK_LSHIFT), u(VK_LCONTROL)])
    );
    h.assert_clean();

    // Modifier already held by the user is not pressed/released again.
    assert_eq!(h.down(VK_LCONTROL), PASS);
    assert_eq!(h.down(VK_F5), rep(vec![d(VK_LSHIFT), d(VK_T)]));
    assert_eq!(h.up(VK_F5), rep(vec![u(VK_T), u(VK_LSHIFT)]));
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn key_to_shortcut_released_after_user_modifier() {
    // User presses F5 (-> Ctrl+Shift+T), then presses and holds LCtrl, releases F5.
    let mut h = Harness::new(cfg(&[(VK_F5, Target::Shortcut(sc("Ctrl+T")))], &[]));
    assert_eq!(h.down(VK_F5), rep(vec![d(VK_LCONTROL), d(VK_T)]));
    assert_eq!(h.down(VK_LCONTROL), PASS);
    // LCtrl is physically held, so it must stay down.
    assert_eq!(h.up(VK_F5), rep(vec![u(VK_T)]));
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn key_to_text() {
    let mut h = Harness::new(cfg(&[(VK_F13, Target::Text("hi".into()))], &[]));
    assert_eq!(h.down(VK_F13), rep(vec![Action::Text("hi".into())]));
    assert_eq!(h.up(VK_F13), rep(vec![]));
    h.assert_clean();
}

#[test]
fn key_to_side_actions_fire_once_per_press() {
    let run = RunProgram {
        path: "/bin/true".into(),
        args: vec![],
        working_dir: None,
    };
    let mut h = Harness::new(cfg(
        &[
            (VK_F7, Target::Run(run.clone())),
            (VK_F8, Target::OpenUri("https://x".into())),
            (VK_F9, Target::PastePlainText),
        ],
        &[],
    ));
    assert_eq!(h.down(VK_F7), rep(vec![Action::Run(run)]));
    assert_eq!(h.down(VK_F7), rep(vec![]));
    assert_eq!(h.up(VK_F7), rep(vec![]));
    assert_eq!(
        h.tap(VK_F8).0,
        rep(vec![Action::OpenUri("https://x".into())])
    );
    assert_eq!(h.tap(VK_F9).0, rep(vec![Action::PastePlainText]));
    h.assert_clean();
}

#[test]
fn config_change_while_key_held_releases_what_was_pressed() {
    let mut h = Harness::new(cfg(&[(VK_CAPITAL, Target::Key(VK_ESCAPE))], &[]));
    assert_eq!(h.down(VK_CAPITAL), rep(vec![d(VK_ESCAPE)]));
    h.e.set_config(&cfg(&[(VK_CAPITAL, Target::Key(VK_LCONTROL))], &[]));
    assert_eq!(h.up(VK_CAPITAL), rep(vec![u(VK_ESCAPE)]));
    assert_eq!(h.down(VK_CAPITAL), rep(vec![d(VK_LCONTROL)]));
    h.e.set_config(&Config::default());
    assert_eq!(h.up(VK_CAPITAL), rep(vec![u(VK_LCONTROL)]));
    assert_eq!(h.tap(VK_CAPITAL), (PASS, PASS));
    h.assert_clean();
}

#[test]
fn key_held_before_start_passes_its_release() {
    let mut h = Harness::new(cfg(&[(VK_CAPITAL, Target::Key(VK_ESCAPE))], &[]));
    h.os.insert(VK_CAPITAL); // pretend the OS has it down already
    assert_eq!(h.up(VK_CAPITAL), PASS);
    h.assert_clean();
}

// ---------------------------------------------------------------------------
// Shortcut -> shortcut
// ---------------------------------------------------------------------------

#[test]
fn ctrl_c_to_cmd_c() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    assert_eq!(h.down(VK_LCONTROL), PASS);
    assert_eq!(h.down(VK_C), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_C)]));
    // Same key out as in: delivered as-is (the backend fixes its modifier
    // flags on macOS), the modifier state stays Cmd.
    assert_eq!(h.down(VK_C), PASS, "repeat");
    assert_eq!(h.up(VK_C), PASS);
    // tap again while Ctrl still held -> Cmd+C again, modifiers untouched
    assert_eq!(h.down(VK_C), PASS);
    assert!(h.e.os_keys().contains(&VK_LWIN) && !h.e.os_keys().contains(&VK_LCONTROL));
    assert_eq!(h.up(VK_C), PASS);
    assert_eq!(h.up(VK_LCONTROL), rep(vec![u(VK_LWIN)]));
    h.assert_clean();
    // without Ctrl, C is untouched
    assert_eq!(h.tap(VK_C), (PASS, PASS));
}

#[test]
fn ctrl_left_to_option_left_keeps_shared_modifiers() {
    let mut h = Harness::new(cfg(
        &[],
        &[(
            "Ctrl+Shift+Left",
            Target::Shortcut(sc("Option+Shift+Left")),
            None,
        )],
    ));
    h.down(VK_RSHIFT);
    h.down(VK_LCONTROL);
    // RShift is shared, so it stays; LCtrl is swapped for LOption.
    assert_eq!(
        h.down(VK_LEFT),
        rep(vec![u(VK_LCONTROL), d(VK_LMENU), d(VK_LEFT)])
    );
    assert_eq!(h.up(VK_LEFT), PASS);
    assert_eq!(h.up(VK_LCONTROL), rep(vec![u(VK_LMENU)]));
    assert_eq!(h.up(VK_RSHIFT), PASS);
    h.assert_clean();
}

#[test]
fn shared_either_modifier_uses_side_user_holds() {
    let mut h = Harness::new(cfg(
        &[],
        &[("Ctrl+Alt+K", Target::Shortcut(sc("Ctrl+Shift+K")), None)],
    ));
    h.down(VK_RCONTROL);
    h.down(VK_LMENU);
    assert_eq!(h.down(VK_K), rep(vec![u(VK_LMENU), d(VK_LSHIFT), d(VK_K)]));
    assert_eq!(h.up(VK_K), PASS);
    assert_eq!(h.up(VK_LMENU), rep(vec![u(VK_LSHIFT)]));
    assert_eq!(h.up(VK_RCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn modifier_released_before_action_key() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    h.down(VK_LCONTROL);
    h.down(VK_C);
    // Release Ctrl first: target action and target modifier are released.
    assert_eq!(h.up(VK_LCONTROL), rep(vec![u(VK_C), u(VK_LWIN)]));
    // The physical C release is swallowed (already released at OS level).
    assert_eq!(h.up(VK_C), rep(vec![]));
    h.assert_clean();
}

#[test]
fn modifier_released_restores_other_held_modifiers() {
    // Win+Ctrl+A -> Ctrl+V: release Ctrl first, Win is still held and must be
    // pressed again so the OS matches the keyboard.
    let mut h = Harness::with_opts(
        cfg(&[], &[("Win+Ctrl+A", Target::Shortcut(sc("Ctrl+V")), None)]),
        EngineOptions {
            dummy_key: true,
            windows_style: true,
        },
    );
    h.down(VK_LWIN);
    h.down(VK_LCONTROL);
    assert_eq!(
        h.down(VK_A),
        rep(vec![d(VK_DUMMY), u(VK_DUMMY), u(VK_LWIN), d(VK_V)]),
        "dummy key before releasing Win"
    );
    assert_eq!(h.up(VK_A), rep(vec![u(VK_V)]));
    assert_eq!(
        h.up(VK_LCONTROL),
        rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_DUMMY), u(VK_DUMMY)]),
        "Win pressed again, followed by a dummy so its release won't open Start"
    );
    assert_eq!(h.up(VK_LWIN), PASS);
    h.assert_clean();
}

#[test]
fn modifier_repeat_while_active_is_swallowed() {
    // Windows auto-repeats held modifiers; a repeat of the suppressed Ctrl must
    // not re-press it.
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    h.down(VK_LCONTROL);
    h.down(VK_C);
    h.up(VK_C);
    assert_eq!(h.down(VK_LCONTROL), rep(vec![]));
    assert_eq!(h.down(VK_LCONTROL), rep(vec![]));
    // Other side of an either-side modifier is swallowed too...
    assert_eq!(h.down(VK_RCONTROL), rep(vec![]));
    // ...and releasing it ends the remap.
    assert_eq!(h.up(VK_RCONTROL), rep(vec![u(VK_LWIN), d(VK_LCONTROL)]));
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn shared_modifier_repeat_is_forwarded() {
    let mut h = Harness::new(cfg(
        &[],
        &[("Ctrl+Shift+Z", Target::Shortcut(sc("Ctrl+Y")), None)],
    ));
    h.down(VK_LCONTROL);
    h.down(VK_LSHIFT);
    assert_eq!(h.down(VK_Z), rep(vec![u(VK_LSHIFT), d(VK_Y)]));
    assert_eq!(h.down(VK_LCONTROL), PASS, "shared modifier repeat passes");
    assert_eq!(
        h.down(VK_LSHIFT),
        rep(vec![]),
        "suppressed modifier repeat is swallowed"
    );
    assert_eq!(h.up(VK_Z), rep(vec![u(VK_Y)]));
    assert_eq!(h.up(VK_LSHIFT), rep(vec![]));
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn other_key_pressed_during_remap_reverts_to_physical() {
    let mut h = Harness::new(cfg(
        &[],
        &[("Ctrl+Left", Target::Shortcut(sc("Option+Left")), None)],
    ));
    h.down(VK_LCONTROL);
    assert_eq!(
        h.down(VK_LEFT),
        rep(vec![u(VK_LCONTROL), d(VK_LMENU), d(VK_LEFT)])
    );
    // Shift joins: back to Ctrl, then Shift goes through.
    assert_eq!(
        h.down(VK_LSHIFT),
        rep(vec![u(VK_LEFT), u(VK_LMENU), d(VK_LCONTROL), d(VK_LSHIFT)])
    );
    // Left auto-repeat now is Ctrl+Shift+Left (not remapped): passes.
    assert_eq!(h.down(VK_LEFT), PASS);
    assert_eq!(h.up(VK_LEFT), PASS);
    assert_eq!(h.up(VK_LSHIFT), PASS);
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn other_key_triggers_another_remap() {
    let mut h = Harness::new(cfg(
        &[],
        &[
            ("Ctrl+Left", Target::Shortcut(sc("Option+Left")), None),
            ("Ctrl+Right", Target::Shortcut(sc("Option+Right")), None),
        ],
    ));
    h.down(VK_LCONTROL);
    h.down(VK_LEFT);
    h.up(VK_LEFT);
    // Right while Ctrl still held: revert, then the other remap fires.
    assert_eq!(
        h.down(VK_RIGHT),
        rep(vec![
            u(VK_LMENU),
            d(VK_LCONTROL),
            u(VK_LCONTROL),
            d(VK_LMENU),
            d(VK_RIGHT)
        ])
    );
    assert_eq!(h.up(VK_RIGHT), PASS);
    assert_eq!(h.up(VK_LCONTROL), rep(vec![u(VK_LMENU)]));
    h.assert_clean();
}

#[test]
fn shortcut_fired_by_auto_repeat_of_held_key() {
    // D held first, then Alt: D's auto-repeat now matches Alt+D. The D the OS
    // already has down must be released, or it would be stuck.
    let mut h = Harness::new(cfg(&[], &[("Alt+D", Target::Shortcut(sc("Cmd+D")), None)]));
    assert_eq!(h.down(VK_D), PASS);
    assert_eq!(h.down(VK_LMENU), PASS);
    assert_eq!(
        h.down(VK_D),
        rep(vec![u(VK_D), u(VK_LMENU), d(VK_LWIN), d(VK_D)])
    );
    assert_eq!(h.up(VK_D), PASS);
    assert_eq!(h.up(VK_LMENU), rep(vec![u(VK_LWIN)]));
    h.assert_clean();
}

#[test]
fn extra_modifier_prevents_match() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    h.down(VK_LCONTROL);
    h.down(VK_LSHIFT);
    assert_eq!(h.down(VK_C), PASS);
    h.up(VK_C);
    h.up(VK_LSHIFT);
    h.up(VK_LCONTROL);
    h.assert_clean();
}

#[test]
fn sided_source_modifier() {
    let mut h = Harness::new(cfg(
        &[],
        &[("RCtrl+C", Target::Shortcut(sc("Cmd+C")), None)],
    ));
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_C), PASS);
    h.up(VK_C);
    h.up(VK_LCONTROL);
    h.down(VK_RCONTROL);
    assert_eq!(h.down(VK_C), rep(vec![u(VK_RCONTROL), d(VK_LWIN), d(VK_C)]));
    h.up(VK_C);
    h.up(VK_RCONTROL);
    h.assert_clean();
}

#[test]
fn other_non_modifier_held_prevents_shortcut_match() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    h.down(VK_A);
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_C), PASS);
    h.up(VK_C);
    h.up(VK_A);
    h.up(VK_LCONTROL);
    h.assert_clean();
}

#[test]
fn single_key_remap_feeds_shortcut_layer() {
    // CapsLock -> LCtrl, and Ctrl+C -> Cmd+C: CapsLock+C gives Cmd+C.
    let mut h = Harness::new(cfg(
        &[(VK_CAPITAL, Target::Key(VK_LCONTROL))],
        &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)],
    ));
    assert_eq!(h.down(VK_CAPITAL), rep(vec![d(VK_LCONTROL)]));
    assert_eq!(h.down(VK_C), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_C)]));
    assert_eq!(h.up(VK_C), PASS);
    assert_eq!(h.up(VK_CAPITAL), rep(vec![u(VK_LWIN)]));
    h.assert_clean();
}

#[test]
fn swapped_modifiers_with_shortcut_remap() {
    // Option<->Cmd swap plus Cmd+Left -> Ctrl+A (as on mac "home").
    let mut h = Harness::new(cfg(
        &[
            (VK_LMENU, Target::Key(VK_LWIN)),
            (VK_LWIN, Target::Key(VK_LMENU)),
        ],
        &[("Cmd+Left", Target::Shortcut(sc("Ctrl+A")), None)],
    ));
    // Physical Option acts as Cmd, so Option+Left -> Ctrl+A
    assert_eq!(h.down(VK_LMENU), rep(vec![d(VK_LWIN)]));
    assert_eq!(
        h.down(VK_LEFT),
        rep(vec![u(VK_LWIN), d(VK_LCONTROL), d(VK_A)])
    );
    assert_eq!(h.up(VK_LEFT), rep(vec![u(VK_A)]));
    assert_eq!(h.up(VK_LMENU), rep(vec![u(VK_LCONTROL)]));
    // Physical Cmd acts as Option: Cmd+Left -> Option+Left, unremapped.
    assert_eq!(h.down(VK_LWIN), rep(vec![d(VK_LMENU)]));
    assert_eq!(h.down(VK_LEFT), PASS);
    assert_eq!(h.up(VK_LEFT), PASS);
    assert_eq!(h.up(VK_LWIN), rep(vec![u(VK_LMENU)]));
    h.assert_clean();
}

// ---------------------------------------------------------------------------
// Shortcut -> key / disabled / text / programs
// ---------------------------------------------------------------------------

#[test]
fn shortcut_to_key() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+Alt+B", Target::Key(VK_BACK), None)]));
    h.down(VK_LCONTROL);
    h.down(VK_LMENU);
    assert_eq!(
        h.down(VK_B),
        rep(vec![u(VK_LMENU), u(VK_LCONTROL), d(VK_BACK)])
    );
    assert_eq!(h.down(VK_B), rep(vec![d(VK_BACK)]));
    assert_eq!(h.up(VK_B), rep(vec![u(VK_BACK)]));
    assert_eq!(
        h.up(VK_LMENU),
        rep(vec![d(VK_LCONTROL)]),
        "Ctrl still held -> restored"
    );
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn shortcut_to_modifier_key_combines_with_next_key() {
    // Alt+D -> Win; holding Alt+D and pressing A gives Win+A (PowerToys behaviour).
    let mut h = Harness::new(cfg(&[], &[("Alt+D", Target::Key(VK_LWIN), None)]));
    h.down(VK_LMENU);
    assert_eq!(h.down(VK_D), rep(vec![u(VK_LMENU), d(VK_LWIN)]));
    assert_eq!(h.down(VK_A), PASS);
    assert_eq!(h.up(VK_A), PASS);
    assert_eq!(h.up(VK_D), rep(vec![u(VK_LWIN)]));
    // Alt was suppressed, so its release has nothing to undo.
    assert_eq!(h.up(VK_LMENU), rep(vec![]));
    h.assert_clean();
}

#[test]
fn shortcut_disabled() {
    let mut h = Harness::new(cfg(&[], &[("Cmd+Q", Target::Disabled, None)]));
    assert_eq!(h.down(VK_LWIN), PASS);
    assert_eq!(h.down(VK_Q), rep(vec![]));
    assert_eq!(h.down(VK_Q), rep(vec![]));
    assert_eq!(h.up(VK_Q), rep(vec![]));
    assert_eq!(h.up(VK_LWIN), PASS);
    h.assert_clean();
    // Q alone is fine
    assert_eq!(h.tap(VK_Q), (PASS, PASS));
}

#[test]
fn shortcut_disabled_emits_dummy_on_windows() {
    let mut h = Harness::with_opts(
        cfg(&[], &[("Win+D", Target::Disabled, None)]),
        EngineOptions {
            dummy_key: true,
            windows_style: true,
        },
    );
    h.down(VK_LWIN);
    assert_eq!(h.down(VK_D), rep(vec![d(VK_DUMMY), u(VK_DUMMY)]));
    h.up(VK_D);
    assert_eq!(h.up(VK_LWIN), PASS);
    h.assert_clean();
}

#[test]
fn shortcut_disabled_then_other_key() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+Q", Target::Disabled, None)]));
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_Q), rep(vec![]));
    // Ctrl+W while Q held: the remap ends, W goes through with Ctrl.
    assert_eq!(h.down(VK_W), PASS);
    assert_eq!(h.up(VK_W), PASS);
    assert_eq!(
        h.up(VK_Q),
        rep(vec![]),
        "Q's down was swallowed so its up is too"
    );
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn shortcut_to_text() {
    let mut h = Harness::new(cfg(
        &[],
        &[("Ctrl+Shift+D", Target::Text("2026-10-25".into()), None)],
    ));
    h.down(VK_LCONTROL);
    h.down(VK_LSHIFT);
    assert_eq!(
        h.down(VK_D),
        rep(vec![
            u(VK_LSHIFT),
            u(VK_LCONTROL),
            Action::Text("2026-10-25".into())
        ]),
        "modifiers released before typing"
    );
    assert_eq!(h.up(VK_D), rep(vec![]));
    assert_eq!(
        h.down(VK_D),
        rep(vec![Action::Text("2026-10-25".into())]),
        "again while held"
    );
    assert_eq!(h.up(VK_D), rep(vec![]));
    assert_eq!(h.up(VK_LSHIFT), rep(vec![d(VK_LCONTROL)]));
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

#[test]
fn shortcut_to_run_and_uri() {
    let run = RunProgram {
        path: "/Applications/Calculator.app".into(),
        args: vec![],
        working_dir: None,
    };
    let mut h = Harness::new(cfg(
        &[],
        &[
            ("Ctrl+Alt+C", Target::Run(run.clone()), None),
            (
                "Ctrl+Alt+G",
                Target::OpenUri("https://github.com".into()),
                None,
            ),
        ],
    ));
    h.down(VK_LCONTROL);
    h.down(VK_LMENU);
    assert_eq!(h.down(VK_C), rep(vec![Action::Run(run.clone())]));
    assert_eq!(h.down(VK_C), rep(vec![]), "auto-repeat does not relaunch");
    assert_eq!(h.up(VK_C), rep(vec![]));
    assert_eq!(
        h.down(VK_C),
        rep(vec![Action::Run(run)]),
        "a new press does"
    );
    h.up(VK_C);
    // switching to another remapped shortcut while modifiers stay held
    assert_eq!(
        h.down(VK_G),
        rep(vec![Action::OpenUri("https://github.com".into())])
    );
    h.up(VK_G);
    assert_eq!(h.up(VK_LMENU), PASS);
    assert_eq!(h.up(VK_LCONTROL), PASS);
    h.assert_clean();
}

// ---------------------------------------------------------------------------
// App-specific remaps
// ---------------------------------------------------------------------------

#[test]
fn app_specific_remap() {
    let mut h = Harness::new(cfg(
        &[],
        &[
            (
                "Ctrl+W",
                Target::Shortcut(sc("Cmd+W")),
                Some("com.apple.Safari"),
            ),
            ("Ctrl+T", Target::Shortcut(sc("Cmd+T")), Some("chrome.exe")),
            ("Ctrl+T", Target::Shortcut(sc("Cmd+N")), None),
        ],
    ));
    h.app = vec!["com.apple.Terminal".into(), "Terminal".into()];
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_W), PASS, "not in Safari");
    assert_eq!(h.app_queries, 1);
    h.up(VK_W);
    // app-specific does not apply -> global fallback
    assert_eq!(h.down(VK_T), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_N)]));
    h.up(VK_T);
    h.up(VK_LCONTROL);
    h.assert_clean();

    h.app = vec!["COM.APPLE.SAFARI".into()];
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_W), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_W)]));
    h.up(VK_W);
    h.up(VK_LCONTROL);

    // Windows exe names: ".exe" optional, case-insensitive, full paths ok
    h.app = vec!["C:\\Program Files\\Google\\Chrome.EXE".into()];
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_T), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_T)]));
    h.up(VK_T);
    h.up(VK_LCONTROL);
    h.assert_clean();

    // No candidate -> app provider not consulted
    let q = h.app_queries;
    h.tap(VK_A);
    h.down(VK_LCONTROL);
    h.tap(VK_A);
    h.up(VK_LCONTROL);
    assert_eq!(h.app_queries, q);
}

#[test]
fn app_specific_wins_over_global() {
    let mut h = Harness::new(cfg(
        &[],
        &[
            ("Ctrl+K", Target::Shortcut(sc("Cmd+K")), None),
            ("Ctrl+K", Target::Disabled, Some("code")),
        ],
    ));
    h.app = vec!["Code.exe".into()];
    h.down(VK_LCONTROL);
    assert_eq!(h.down(VK_K), rep(vec![]));
    h.up(VK_K);
    h.up(VK_LCONTROL);
    h.assert_clean();
}

// ---------------------------------------------------------------------------
// Paste as plain text hotkey
// ---------------------------------------------------------------------------

#[test]
fn paste_plain_text_hotkey() {
    let mut c = cfg(&[], &[]);
    c.paste_plain_text = PastePlainText {
        enabled: true,
        hotkey: sc("Cmd+Option+Shift+V"),
        ..Default::default()
    };
    let mut h = Harness::new(c);
    h.down(VK_LWIN);
    h.down(VK_LMENU);
    h.down(VK_LSHIFT);
    assert_eq!(
        h.down(VK_V),
        rep(vec![
            u(VK_LSHIFT),
            u(VK_LMENU),
            u(VK_LWIN),
            Action::PastePlainText
        ]),
        "modifiers released so the synthesized Cmd+V is clean"
    );
    assert_eq!(h.down(VK_V), rep(vec![]), "repeat does not paste again");
    assert_eq!(h.up(VK_V), rep(vec![]));
    assert_eq!(h.up(VK_LSHIFT), rep(vec![d(VK_LWIN), d(VK_LMENU)]));
    assert_eq!(h.up(VK_LMENU), PASS);
    assert_eq!(h.up(VK_LWIN), PASS);
    h.assert_clean();
    // plain Cmd+V untouched
    h.down(VK_LWIN);
    assert_eq!(h.tap(VK_V), (PASS, PASS));
    h.up(VK_LWIN);
}

#[test]
fn paste_disabled_or_master_switch() {
    let mut c = Config::default();
    c.paste_plain_text.enabled = false;
    let mut h = Harness::new(c);
    for k in [VK_LWIN, VK_LMENU, VK_LSHIFT, VK_LCONTROL, VK_V] {
        assert_eq!(h.down(k), PASS);
    }
    // master switch off disables remaps but not paste-plain
    let mut c = cfg(&[(VK_CAPITAL, Target::Key(VK_ESCAPE))], &[]);
    c.enabled = false;
    c.paste_plain_text.enabled = true;
    let mut h = Harness::new(c);
    assert_eq!(h.tap(VK_CAPITAL), (PASS, PASS));
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn reset_releases_injected_keys() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    h.down(VK_LCONTROL);
    h.down(VK_C);
    let acts = h.e.reset();
    assert_eq!(
        acts,
        vec![u(VK_LWIN)],
        "C is physically held so it's left alone"
    );
    assert!(!h.e.shortcut_active());
}

#[test]
fn release_missing_modifier() {
    let mut h = Harness::new(cfg(&[], &[("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None)]));
    h.down(VK_LCONTROL);
    h.down(VK_C);
    h.up(VK_C);
    // Ctrl's release was missed (e.g. secure input): the backend notices.
    let acts = h.e.release_missing(|k| k != VK_LCONTROL);
    assert_eq!(acts, vec![u(VK_LWIN)]);
    h.os.remove(&VK_LWIN);
    assert!(h.e.os_keys().is_empty());
    assert_eq!(h.tap(VK_C), (PASS, PASS));
}

/// Random key mashing over a set of overlapping remaps must never send a
/// key-up for a key the OS doesn't have down, and must leave nothing stuck.
#[test]
fn fuzz_no_stuck_keys() {
    let run = RunProgram {
        path: "x".into(),
        args: vec![],
        working_dir: None,
    };
    let c = cfg(
        &[
            (VK_CAPITAL, Target::Key(VK_LCONTROL)),
            (VK_LMENU, Target::Key(VK_LWIN)),
            (VK_LWIN, Target::Key(VK_LMENU)),
            (VK_F1, Target::Shortcut(sc("Ctrl+Shift+T"))),
            (VK_F2, Target::Disabled),
            (VK_F3, Target::Text("x".into())),
        ],
        &[
            ("Ctrl+C", Target::Shortcut(sc("Cmd+C")), None),
            ("Ctrl+Left", Target::Shortcut(sc("Option+Left")), None),
            (
                "Ctrl+Shift+Left",
                Target::Shortcut(sc("Option+Shift+Left")),
                None,
            ),
            ("Cmd+Q", Target::Disabled, None),
            ("Alt+D", Target::Key(VK_LWIN), None),
            ("Ctrl+Alt+B", Target::Key(VK_BACK), None),
            ("Ctrl+Shift+D", Target::Text("t".into()), None),
            ("Ctrl+Alt+R", Target::Run(run), Some("app")),
            ("Win+Ctrl+A", Target::Shortcut(sc("Ctrl+V")), None),
            ("Cmd+Option+Shift+V", Target::PastePlainText, None),
        ],
    );
    let keys = [
        VK_CAPITAL,
        VK_LMENU,
        VK_LWIN,
        VK_RWIN,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_F1,
        VK_F2,
        VK_F3,
        VK_C,
        VK_LEFT,
        VK_Q,
        VK_D,
        VK_B,
        VK_R,
        VK_A,
        VK_V,
        VK_T,
    ];
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for dummy in [false, true] {
        for round in 0..3000 {
            let mut h = Harness::with_opts(
                c.clone(),
                EngineOptions {
                    dummy_key: dummy,
                    windows_style: true,
                },
            );
            h.app = if round % 2 == 0 {
                vec!["app".into()]
            } else {
                vec![]
            };
            let mut held: Vec<u32> = Vec::new();
            for _ in 0..60 {
                let r = next();
                let k = keys[(r % keys.len() as u64) as usize];
                if held.contains(&k) && (r >> 8) % 3 != 0 {
                    held.retain(|&x| x != k);
                    h.up(k);
                } else {
                    if !held.contains(&k) {
                        held.push(k);
                    }
                    h.down(k); // may be an auto-repeat
                }
            }
            // release in random order
            while !held.is_empty() {
                let i = (next() % held.len() as u64) as usize;
                let k = held.remove(i);
                h.up(k);
            }
            h.assert_clean();
        }
    }
}

// ---------------------------------------------------------------------------
// Windows-style keys preset
// ---------------------------------------------------------------------------

mod windows_style {
    use super::*;
    use crate::windows_style::WindowsStylePreset;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const FINDER: &str = "com.apple.finder";

    fn preset(f: impl FnOnce(&mut WindowsStylePreset)) -> Config {
        let mut c = cfg(&[], &[]);
        c.windows_style.enabled = true;
        f(&mut c.windows_style);
        c
    }

    /// Harness with a fake focused-role provider; returns the role cell and
    /// a probe call counter.
    fn harness(c: Config) -> (Harness, Arc<Mutex<Option<String>>>, Arc<AtomicUsize>) {
        let mut h = Harness::new(c);
        let role = Arc::new(Mutex::new(Some("AXOutline".to_string())));
        let calls = Arc::new(AtomicUsize::new(0));
        let (r2, c2) = (role.clone(), calls.clone());
        h.e.set_focus_probe(Arc::new(move || {
            c2.fetch_add(1, Ordering::SeqCst);
            r2.lock().unwrap().clone()
        }));
        (h, role, calls)
    }

    #[test]
    fn off_by_default() {
        let c = Config::default();
        assert!(!c.windows_style.enabled);
        assert!(c.windows_style.home_end && !c.windows_style.ctrl_shortcuts_to_cmd);
        let mut h = Harness::new(cfg(&[], &[]));
        assert_eq!(h.tap(VK_HOME), (PASS, PASS));
        h.down(VK_LCONTROL);
        assert_eq!(h.tap(VK_LEFT), (PASS, PASS));
        h.up(VK_LCONTROL);
        h.assert_clean();
    }

    #[test]
    fn engine_option_disables_it() {
        let mut h = Harness::with_opts(
            preset(|_| {}),
            EngineOptions {
                dummy_key: false,
                windows_style: false,
            },
        );
        assert_eq!(h.tap(VK_HOME), (PASS, PASS));
        h.assert_clean();
    }

    #[test]
    fn master_switch_of_keyboard_manager_disables_it() {
        let mut c = preset(|_| {});
        c.enabled = false;
        let mut h = Harness::new(c);
        assert_eq!(h.tap(VK_HOME), (PASS, PASS));
    }

    #[test]
    fn home_end_line_and_document() {
        let (mut h, _, calls) = harness(preset(|_| {}));
        // Home -> Cmd+Left, and Cmd is released with Home (doesn't linger).
        assert_eq!(h.down(VK_HOME), rep(vec![d(VK_LWIN), d(VK_LEFT)]));
        assert_eq!(h.down(VK_HOME), rep(vec![d(VK_LEFT)]), "auto-repeat");
        assert_eq!(h.up(VK_HOME), rep(vec![u(VK_LEFT), u(VK_LWIN)]));
        assert!(!h.e.shortcut_active());
        assert_eq!(h.tap(VK_END).0, rep(vec![d(VK_LWIN), d(VK_RIGHT)]));
        h.assert_clean();
        // Shift+Home selects to line start.
        assert_eq!(h.down(VK_LSHIFT), PASS);
        assert_eq!(h.down(VK_HOME), rep(vec![d(VK_LWIN), d(VK_LEFT)]));
        assert_eq!(h.up(VK_HOME), rep(vec![u(VK_LEFT), u(VK_LWIN)]));
        assert_eq!(h.up(VK_LSHIFT), PASS);
        // Ctrl+End -> Cmd+Down (Ctrl swapped for Cmd while held).
        h.down(VK_LCONTROL);
        assert_eq!(
            h.down(VK_END),
            rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_DOWN)])
        );
        assert_eq!(h.up(VK_END), rep(vec![u(VK_DOWN)]));
        assert_eq!(h.up(VK_LCONTROL), rep(vec![u(VK_LWIN)]));
        // Ctrl+Shift+Home -> Cmd+Shift+Up
        h.down(VK_LCONTROL);
        h.down(VK_LSHIFT);
        assert_eq!(
            h.down(VK_HOME),
            rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_UP)])
        );
        h.up(VK_HOME);
        h.up(VK_LSHIFT);
        h.up(VK_LCONTROL);
        h.assert_clean();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no focus probe outside Finder items"
        );
    }

    #[test]
    fn home_then_other_key_while_held() {
        let mut h = Harness::new(preset(|_| {}));
        h.down(VK_HOME);
        // Another key ends the remap: Cmd and Left are released first.
        assert_eq!(h.down(VK_A), rep(vec![u(VK_LEFT), u(VK_LWIN), d(VK_A)]));
        h.up(VK_A);
        assert_eq!(h.up(VK_HOME), rep(vec![]));
        h.assert_clean();
    }

    #[test]
    fn page_keys_move_caret() {
        let mut h = Harness::new(preset(|_| {}));
        assert_eq!(h.down(VK_PRIOR), rep(vec![d(VK_LMENU), d(VK_PRIOR)]));
        assert_eq!(h.up(VK_PRIOR), rep(vec![u(VK_PRIOR), u(VK_LMENU)]));
        // Shift+PageDown already selects on macOS: untouched.
        h.down(VK_LSHIFT);
        assert_eq!(h.tap(VK_NEXT), (PASS, PASS));
        h.up(VK_LSHIFT);
        let mut h = Harness::new(preset(|p| p.page_up_down = false));
        assert_eq!(h.tap(VK_NEXT), (PASS, PASS));
        h.assert_clean();
    }

    #[test]
    fn ctrl_word_nav_and_delete() {
        let mut h = Harness::new(preset(|_| {}));
        h.down(VK_LCONTROL);
        assert_eq!(
            h.down(VK_LEFT),
            rep(vec![u(VK_LCONTROL), d(VK_LMENU), d(VK_LEFT)])
        );
        assert_eq!(h.up(VK_LEFT), PASS);
        // Ctrl still held: Option stays down, the next Left repeats it.
        assert_eq!(h.down(VK_LEFT), PASS);
        h.up(VK_LEFT);
        // A different key ends the remap first, then Ctrl+Backspace fires.
        assert_eq!(
            h.down(VK_BACK),
            rep(vec![
                u(VK_LMENU),
                d(VK_LCONTROL),
                u(VK_LCONTROL),
                d(VK_LMENU),
                d(VK_BACK)
            ])
        );
        h.up(VK_BACK);
        assert_eq!(h.up(VK_LCONTROL), rep(vec![u(VK_LMENU)]));
        h.assert_clean();
        // Ctrl+Shift+Right selects a word.
        h.down(VK_LCONTROL);
        h.down(VK_LSHIFT);
        assert_eq!(
            h.down(VK_RIGHT),
            rep(vec![u(VK_LCONTROL), d(VK_LMENU), d(VK_RIGHT)])
        );
        h.up(VK_RIGHT);
        h.up(VK_LCONTROL);
        h.up(VK_LSHIFT);
        h.assert_clean();
        // Toggled off.
        let mut h = Harness::new(preset(|p| {
            p.ctrl_word_nav = false;
            p.ctrl_word_delete = false
        }));
        h.down(VK_LCONTROL);
        assert_eq!(h.tap(VK_LEFT), (PASS, PASS));
        assert_eq!(h.tap(VK_BACK), (PASS, PASS));
        h.up(VK_LCONTROL);
        h.assert_clean();
    }

    #[test]
    fn excluded_apps_are_left_alone() {
        let mut h = Harness::new(preset(|_| {}));
        h.app = vec!["com.apple.Terminal".into(), "Terminal".into()];
        assert_eq!(h.tap(VK_HOME), (PASS, PASS));
        h.down(VK_LCONTROL);
        assert_eq!(h.tap(VK_LEFT), (PASS, PASS));
        h.up(VK_LCONTROL);
        h.app = vec!["com.apple.TextEdit".into()];
        assert_ne!(h.tap(VK_HOME).0, PASS);
        h.assert_clean();
    }

    #[test]
    fn ctrl_shortcuts_to_cmd_is_opt_in() {
        let mut h = Harness::new(preset(|_| {}));
        h.down(VK_LCONTROL);
        assert_eq!(h.tap(VK_C), (PASS, PASS), "off by default");
        h.up(VK_LCONTROL);
        let mut h = Harness::new(preset(|p| p.ctrl_shortcuts_to_cmd = true));
        h.down(VK_LCONTROL);
        assert_eq!(h.down(VK_C), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_C)]));
        h.up(VK_C);
        h.up(VK_LCONTROL);
        // Ctrl+Y -> Cmd+Shift+Z (redo)
        h.down(VK_LCONTROL);
        assert_eq!(
            h.down(VK_Y),
            rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_LSHIFT), d(VK_Z)])
        );
        h.up(VK_Y);
        h.up(VK_LCONTROL);
        h.assert_clean();
        // Not in terminals (Ctrl+C is SIGINT there).
        h.app = vec!["com.googlecode.iterm2".into()];
        h.down(VK_LCONTROL);
        assert_eq!(h.tap(VK_C), (PASS, PASS));
        h.up(VK_LCONTROL);
        h.assert_clean();
    }

    #[test]
    fn user_remaps_win_over_the_preset() {
        let mut c = preset(|_| {});
        c.remap_shortcuts.push(ShortcutRemap {
            from: sc("Ctrl+Left"),
            to: Target::Shortcut(sc("Cmd+Left")),
            app: None,
        });
        let mut h = Harness::new(c);
        h.down(VK_LCONTROL);
        assert_eq!(
            h.down(VK_LEFT),
            rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_LEFT)])
        );
        h.up(VK_LEFT);
        h.up(VK_LCONTROL);
        h.assert_clean();
    }

    #[test]
    fn finder_enter_opens_and_f2_renames() {
        let (mut h, role, calls) = harness(preset(|_| {}));
        // Outside Finder: untouched, no probe.
        h.app = vec!["com.apple.TextEdit".into()];
        assert_eq!(h.tap(VK_RETURN), (PASS, PASS));
        assert_eq!(h.tap(VK_F2), (PASS, PASS));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // Finder file list.
        h.app = vec![FINDER.into(), "Finder".into()];
        assert_eq!(h.down(VK_RETURN), rep(vec![d(VK_LWIN), d(VK_O)]));
        assert_eq!(h.up(VK_RETURN), rep(vec![u(VK_O), u(VK_LWIN)]));
        assert_eq!(h.down(VK_F2), rep(vec![d(VK_RETURN)]));
        assert_eq!(h.up(VK_F2), rep(vec![u(VK_RETURN)]));
        h.assert_clean();
        // Renaming (text field focused): Return confirms the rename.
        *role.lock().unwrap() = Some("AXTextField".into());
        assert_eq!(h.tap(VK_RETURN), (PASS, PASS));
        assert_eq!(h.tap(VK_F2), (PASS, PASS));
        assert_eq!(h.tap(VK_BACK), (PASS, PASS));
        assert_eq!(h.tap(VK_DELETE), (PASS, PASS));
        // Search field reported by subrole.
        *role.lock().unwrap() = Some("AXSearchField".into());
        assert_eq!(h.tap(VK_BACK), (PASS, PASS));
        // Unknown focus: treated as not editing.
        *role.lock().unwrap() = None;
        assert_eq!(h.tap(VK_BACK).0, rep(vec![d(VK_LWIN), d(VK_UP)]));
        h.assert_clean();
    }

    #[test]
    fn finder_delete_and_backspace() {
        let (mut h, _, _) = harness(preset(|_| {}));
        h.app = vec![FINDER.into()];
        assert_eq!(h.down(VK_DELETE), rep(vec![d(VK_LWIN), d(VK_BACK)]));
        assert_eq!(h.up(VK_DELETE), rep(vec![u(VK_BACK), u(VK_LWIN)]));
        assert_eq!(h.down(VK_BACK), rep(vec![d(VK_LWIN), d(VK_UP)]));
        assert_eq!(h.up(VK_BACK), rep(vec![u(VK_UP), u(VK_LWIN)]));
        // Cmd+Backspace (the Mac habit) still passes.
        h.down(VK_LWIN);
        assert_eq!(h.tap(VK_BACK), (PASS, PASS));
        h.up(VK_LWIN);
        h.assert_clean();
        let (mut h, _, _) = harness(preset(|p| {
            p.finder_delete_to_trash = false;
            p.finder_backspace_up = false
        }));
        h.app = vec![FINDER.into()];
        assert_eq!(h.tap(VK_DELETE), (PASS, PASS));
        assert_eq!(h.tap(VK_BACK), (PASS, PASS));
    }

    fn cmd_tap(h: &mut Harness, k: u32) -> Outcome {
        h.down(VK_LWIN);
        let o = h.down(k);
        h.up(k);
        h.up(VK_LWIN);
        o
    }

    #[test]
    fn finder_cut_then_paste_moves() {
        let (mut h, role, _) = harness(preset(|_| {}));
        h.app = vec![FINDER.into()];
        // Plain Cmd+V with nothing cut: a normal paste.
        assert_eq!(cmd_tap(&mut h, VK_V), PASS);
        // Cmd+X -> Cmd+C, cut pending.
        assert_eq!(cmd_tap(&mut h, VK_X), rep(vec![d(VK_C)]));
        assert!(h.e.cut_pending());
        // Cmd+V -> Cmd+Option+V (move), pending cleared.
        assert_eq!(cmd_tap(&mut h, VK_V), rep(vec![d(VK_LMENU), d(VK_V)]));
        assert!(!h.e.cut_pending());
        // A second Cmd+V is a normal paste again.
        assert_eq!(cmd_tap(&mut h, VK_V), PASS);
        h.assert_clean();

        // Esc cancels.
        cmd_tap(&mut h, VK_X);
        assert!(h.e.cut_pending());
        h.tap(VK_ESCAPE);
        assert!(!h.e.cut_pending());
        assert_eq!(cmd_tap(&mut h, VK_V), PASS);

        // Another copy cancels (in any app).
        cmd_tap(&mut h, VK_X);
        h.app = vec!["com.apple.TextEdit".into()];
        assert_eq!(cmd_tap(&mut h, VK_C), PASS);
        assert!(!h.e.cut_pending());
        h.app = vec![FINDER.into()];
        assert_eq!(cmd_tap(&mut h, VK_V), PASS);

        // Cut + paste while keeping Cmd held.
        h.down(VK_LWIN);
        assert_eq!(h.down(VK_X), rep(vec![d(VK_C)]));
        h.up(VK_X);
        assert_eq!(
            h.down(VK_V),
            rep(vec![u(VK_C)]
                .into_iter()
                .filter(|_| false)
                .chain([d(VK_LMENU), d(VK_V)])
                .collect())
        );
        h.up(VK_V);
        h.up(VK_LWIN);
        assert!(!h.e.cut_pending());
        h.assert_clean();

        // In a text field (rename / search) Cmd+X is a text cut: passes, and
        // doesn't arm the file move.
        *role.lock().unwrap() = Some("AXTextField".into());
        assert_eq!(cmd_tap(&mut h, VK_X), PASS);
        assert!(!h.e.cut_pending());
        // Armed in the list, then Cmd+V inside a text field pastes text.
        *role.lock().unwrap() = Some("AXOutline".into());
        cmd_tap(&mut h, VK_X);
        *role.lock().unwrap() = Some("AXTextField".into());
        assert_eq!(cmd_tap(&mut h, VK_V), PASS);
        assert!(h.e.cut_pending(), "still pending for the file list");
        *role.lock().unwrap() = Some("AXOutline".into());
        assert_eq!(cmd_tap(&mut h, VK_V), rep(vec![d(VK_LMENU), d(VK_V)]));
        h.assert_clean();
    }

    #[test]
    fn finder_cut_with_ctrl_when_ctrl_shortcuts_on() {
        let (mut h, _, _) = harness(preset(|p| p.ctrl_shortcuts_to_cmd = true));
        h.app = vec![FINDER.into()];
        h.down(VK_LCONTROL);
        assert_eq!(h.down(VK_X), rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_C)]));
        h.up(VK_X);
        h.up(VK_LCONTROL);
        assert!(h.e.cut_pending());
        h.down(VK_LCONTROL);
        assert_eq!(
            h.down(VK_V),
            rep(vec![u(VK_LCONTROL), d(VK_LWIN), d(VK_LMENU), d(VK_V)])
        );
        h.up(VK_V);
        h.up(VK_LCONTROL);
        assert!(!h.e.cut_pending());
        h.assert_clean();
    }

    #[test]
    fn finder_cut_toggle_off_and_config_update_clears() {
        let (mut h, _, _) = harness(preset(|p| p.finder_cut_paste = false));
        h.app = vec![FINDER.into()];
        assert_eq!(cmd_tap(&mut h, VK_X), PASS);
        assert!(!h.e.cut_pending());
        let c = preset(|_| {});
        h.e.set_config(&c);
        cmd_tap(&mut h, VK_X);
        assert!(h.e.cut_pending());
        h.e.set_config(&preset(|p| p.finder_cut_paste = false));
        assert!(!h.e.cut_pending());
    }

    #[test]
    fn config_json() {
        let c = Config::from_json(
            r#"{"windowsStyle": {"enabled": true, "finderEnterOpens": false, "excludedApps": ["Foo"]}}"#,
        )
        .unwrap();
        let w = &c.windows_style;
        assert!(w.enabled && w.home_end && !w.finder_enter_opens && !w.ctrl_shortcuts_to_cmd);
        assert_eq!(w.excluded_apps, vec!["Foo".to_string()]);
        assert_eq!(Config::from_json(&c.to_json()).unwrap(), c);
        let json = Config::default().to_json();
        assert!(
            json.contains("\"windowsStyle\"") && json.contains("\"ctrlShortcutsToCmd\": false")
        );
        // get/set by key cover every item.
        let mut p = WindowsStylePreset::default();
        for it in crate::windows_style::ITEMS {
            assert_eq!(p.get(it.key), Some(it.default), "{}", it.key);
            assert!(p.set(it.key, !it.default));
            assert_eq!(p.get(it.key), Some(!it.default));
        }
        assert!(!p.set("nope", true));
        // describe lists the enabled mappings.
        let mut p = WindowsStylePreset {
            enabled: true,
            ..Default::default()
        };
        let d = p.describe();
        assert!(
            d.iter()
                .any(|(f, t, a)| f == "Home" && t == "Cmd+Left" && a.is_none()),
            "{d:?}"
        );
        assert!(d
            .iter()
            .any(|(f, t, a)| f == "Enter" && t == "Cmd+O" && *a == Some(FINDER)));
        p.enabled = false;
        assert!(p.describe().is_empty());
    }

    #[test]
    fn fuzz_preset_never_leaves_keys_stuck() {
        let c = preset(|p| p.ctrl_shortcuts_to_cmd = true);
        let keys = [
            VK_LWIN,
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LSHIFT,
            VK_LMENU,
            VK_HOME,
            VK_END,
            VK_PRIOR,
            VK_LEFT,
            VK_BACK,
            VK_DELETE,
            VK_RETURN,
            VK_F2,
            VK_X,
            VK_V,
            VK_C,
            VK_Y,
            VK_ESCAPE,
            VK_A,
        ];
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..300 {
            let (mut h, role, _) = harness(c.clone());
            h.app = match round % 3 {
                0 => vec![FINDER.into()],
                1 => vec!["com.apple.Terminal".into()],
                _ => vec!["com.apple.TextEdit".into()],
            };
            let mut held: Vec<u32> = Vec::new();
            for _ in 0..60 {
                let r = next();
                *role.lock().unwrap() = if r % 5 == 0 {
                    Some("AXTextField".into())
                } else {
                    Some("AXOutline".into())
                };
                let k = keys[((r >> 4) % keys.len() as u64) as usize];
                if held.contains(&k) && (r >> 12) % 3 != 0 {
                    held.retain(|&x| x != k);
                    h.up(k);
                } else {
                    if !held.contains(&k) {
                        held.push(k);
                    }
                    h.down(k);
                }
            }
            while !held.is_empty() {
                let i = (next() % held.len() as u64) as usize;
                let k = held.remove(i);
                h.up(k);
            }
            h.assert_clean();
        }
    }
}
