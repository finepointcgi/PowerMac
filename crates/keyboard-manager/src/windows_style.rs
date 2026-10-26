//! The "Windows-style keys" preset: one switch (plus one toggle per item) that
//! makes a Mac keyboard behave the way someone coming from Windows expects.
//!
//! The preset is compiled into ordinary shortcut entries of the remap
//! [`Engine`](crate::engine::Engine) (so it shares its modifier bookkeeping,
//! app scoping and injected-event tagging), with three extras the user-facing
//! config can't express:
//!
//! - **no-modifier sources** (Home, End, PageUp, Return, ...), which end as
//!   soon as the key is released so the injected Command doesn't linger;
//! - **a text-field exemption** for the Finder items: they pass through while
//!   the focused UI element is a text field (rename, search, Go to Folder),
//!   detected with the Accessibility API (`AXFocusedUIElement` → `AXRole`);
//! - **"cut pending"** state for Finder's Cmd+X / Cmd+V file move.
//!
//! User remaps always win over the preset for the same shortcut.
//!
//! | item | default | mapping |
//! |---|---|---|
//! | `homeEnd` | on | Home/End → Cmd+Left/Right (line start/end), Shift+Home/End selects, Ctrl+Home/End → Cmd+Up/Down (document start/end), Ctrl+Shift+Home/End selects |
//! | `pageUpDown` | on | PageUp/PageDown → Option+PageUp/PageDown (moves the caret in Cocoa text views; plain PageUp only scrolls on macOS) |
//! | `ctrlWordNav` | on | Ctrl+Left/Right (+Shift) → Option+Left/Right (+Shift). Takes over macOS' "switch Space" shortcuts |
//! | `ctrlWordDelete` | on | Ctrl+Backspace → Option+Backspace, Ctrl+Delete → Option+Delete |
//! | `ctrlShortcutsToCmd` | **off** | Ctrl+C/V/X/Z/A/S/F/T/W/N → Cmd+…, Ctrl+Y → Cmd+Shift+Z. Off by default: Mouse Without Borders' "Swap Cmd/Ctrl" and the user's own remaps usually cover this, and Ctrl+C etc. mean something else in terminals |
//! | `finderCutPaste` | on | Finder: Cmd+X copies and remembers "cut pending"; the next Cmd+V moves (Cmd+Option+V). Esc or any other copy cancels |
//! | `finderEnterOpens` | on | Finder: Return opens the selection (Cmd+O); F2 renames (sends Return) |
//! | `finderDeleteToTrash` | on | Finder: Delete (forward delete) → Move to Trash (Cmd+Backspace) |
//! | `finderBackspaceUp` | on | Finder: Backspace → enclosing folder (Cmd+Up) |
//!
//! The non-Finder items skip the apps in `excludedApps` (terminals, remote
//! desktop and VM clients by default), where Windows-style keys either already
//! work or must reach the remote machine unchanged.
//!
//! **Windows:** the preset is a no-op there (those keys already behave this
//! way); [`EngineOptions::windows_style`](crate::engine::EngineOptions) is
//! false by default on Windows and the engine ignores the section.

use pm_input::vk::*;
use serde::{Deserialize, Serialize};

use crate::config::Target;
use crate::keys::{Mods, Shortcut, VK_WIN_BOTH};

/// Finder's bundle id (the scope of the Finder items).
pub const FINDER: &str = "com.apple.finder";

/// Accessibility roles treated as "a text field is being edited".
pub const TEXT_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];

/// Whether an `AXRole` (or subrole) means the user is typing into a field.
pub fn is_text_role(role: &str) -> bool {
    TEXT_ROLES.iter().any(|r| r.eq_ignore_ascii_case(role))
}

/// Apps the non-Finder items leave alone by default.
pub fn default_excluded_apps() -> Vec<String> {
    [
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "dev.warp.Warp-Stable",
        "net.kovidgoyal.kitty",
        "org.alacritty",
        "com.github.wez.wezterm",
        "com.mitchellh.ghostty",
        "co.zeit.hyper",
        "com.microsoft.rdc.macos",
        "com.microsoft.rdc.osx.beta",
        "com.parallels.desktop.console",
        "com.vmware.fusion",
        "org.virtualbox.app.VirtualBoxVM",
        "com.utmapp.UTM",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `"windowsStyle"` section of the Keyboard Manager config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowsStylePreset {
    /// Master switch for the preset (off by default).
    pub enabled: bool,
    /// Home/End → line start/end, Ctrl+Home/End → document start/end.
    pub home_end: bool,
    /// PageUp/PageDown move the caret (Option+PageUp/PageDown).
    pub page_up_down: bool,
    /// Ctrl+Left/Right → word jump (Option+Left/Right).
    pub ctrl_word_nav: bool,
    /// Ctrl+Backspace/Delete → delete word.
    pub ctrl_word_delete: bool,
    /// Ctrl+C/V/X/Z/Y/A/S/F/T/W/N → Cmd equivalents. Off by default.
    pub ctrl_shortcuts_to_cmd: bool,
    /// Finder: Cmd+X then Cmd+V moves files.
    pub finder_cut_paste: bool,
    /// Finder: Return opens, F2 renames.
    pub finder_enter_opens: bool,
    /// Finder: Delete moves to the Trash.
    pub finder_delete_to_trash: bool,
    /// Finder: Backspace goes to the enclosing folder.
    pub finder_backspace_up: bool,
    /// Apps where the non-Finder items are off (bundle id / name).
    pub excluded_apps: Vec<String>,
}

impl Default for WindowsStylePreset {
    fn default() -> Self {
        Self {
            enabled: false,
            home_end: true,
            page_up_down: true,
            ctrl_word_nav: true,
            ctrl_word_delete: true,
            ctrl_shortcuts_to_cmd: false,
            finder_cut_paste: true,
            finder_enter_opens: true,
            finder_delete_to_trash: true,
            finder_backspace_up: true,
            excluded_apps: default_excluded_apps(),
        }
    }
}

/// One toggle of the preset, for building a settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetItem {
    /// JSON field name (camelCase), e.g. `"homeEnd"`.
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub default: bool,
    /// Only active in Finder.
    pub finder: bool,
}

/// The toggles in display order.
pub const ITEMS: &[PresetItem] = &[
    PresetItem {
        key: "homeEnd",
        label: "Home / End",
        description: "Home and End go to the start and end of the line (Shift selects); Ctrl+Home/End go to the start and end of the document.",
        default: true,
        finder: false,
    },
    PresetItem {
        key: "pageUpDown",
        label: "Page Up / Page Down move the caret",
        description: "Page Up and Page Down move the text cursor, not just the view, in apps that support it.",
        default: true,
        finder: false,
    },
    PresetItem {
        key: "ctrlWordNav",
        label: "Ctrl+Left / Right jump by word",
        description: "Sends Option+Left/Right (Shift selects). Replaces macOS' Ctrl+Arrow \"switch Space\" shortcuts.",
        default: true,
        finder: false,
    },
    PresetItem {
        key: "ctrlWordDelete",
        label: "Ctrl+Backspace / Delete delete a word",
        description: "Sends Option+Backspace / Option+Delete.",
        default: true,
        finder: false,
    },
    PresetItem {
        key: "ctrlShortcutsToCmd",
        label: "Ctrl+C, V, X, Z, Y, A, S, F, T, W, N act like Cmd",
        description: "Off by default: \"Swap Cmd/Ctrl\" (Mouse Without Borders) or your own remaps may already do this, and these keys mean something else in terminals.",
        default: false,
        finder: false,
    },
    PresetItem {
        key: "finderCutPaste",
        label: "Finder: Cmd+X, Cmd+V moves files",
        description: "Cmd+X copies the selection and marks it as cut; the next Cmd+V moves it. Esc or another copy cancels.",
        default: true,
        finder: true,
    },
    PresetItem {
        key: "finderEnterOpens",
        label: "Finder: Enter opens, F2 renames",
        description: "Return opens the selected items. Press F2 to rename instead.",
        default: true,
        finder: true,
    },
    PresetItem {
        key: "finderDeleteToTrash",
        label: "Finder: Delete moves to the Trash",
        description: "The forward Delete key moves the selection to the Trash.",
        default: true,
        finder: true,
    },
    PresetItem {
        key: "finderBackspaceUp",
        label: "Finder: Backspace goes up a folder",
        description: "Backspace opens the enclosing folder.",
        default: true,
        finder: true,
    },
];

impl WindowsStylePreset {
    /// Read a toggle by its JSON key (see [`ITEMS`]).
    pub fn get(&self, key: &str) -> Option<bool> {
        Some(match key {
            "enabled" => self.enabled,
            "homeEnd" => self.home_end,
            "pageUpDown" => self.page_up_down,
            "ctrlWordNav" => self.ctrl_word_nav,
            "ctrlWordDelete" => self.ctrl_word_delete,
            "ctrlShortcutsToCmd" => self.ctrl_shortcuts_to_cmd,
            "finderCutPaste" => self.finder_cut_paste,
            "finderEnterOpens" => self.finder_enter_opens,
            "finderDeleteToTrash" => self.finder_delete_to_trash,
            "finderBackspaceUp" => self.finder_backspace_up,
            _ => return None,
        })
    }

    /// Set a toggle by its JSON key. Returns false for an unknown key.
    pub fn set(&mut self, key: &str, on: bool) -> bool {
        let f = match key {
            "enabled" => &mut self.enabled,
            "homeEnd" => &mut self.home_end,
            "pageUpDown" => &mut self.page_up_down,
            "ctrlWordNav" => &mut self.ctrl_word_nav,
            "ctrlWordDelete" => &mut self.ctrl_word_delete,
            "ctrlShortcutsToCmd" => &mut self.ctrl_shortcuts_to_cmd,
            "finderCutPaste" => &mut self.finder_cut_paste,
            "finderEnterOpens" => &mut self.finder_enter_opens,
            "finderDeleteToTrash" => &mut self.finder_delete_to_trash,
            "finderBackspaceUp" => &mut self.finder_backspace_up,
            _ => return false,
        };
        *f = on;
        true
    }

    /// Human-readable list of what the enabled items do (`from`, `to`,
    /// scope), for a settings page preview. Empty when the preset is off.
    pub fn describe(&self) -> Vec<(String, String, Option<&'static str>)> {
        entries(self)
            .into_iter()
            .map(|e| {
                let to = match &e.to {
                    Target::Shortcut(s) => s.to_string(),
                    Target::Key(k) => crate::keys::key_name(*k),
                    _ => String::new(),
                };
                let mut to = to;
                if e.needs_cut {
                    to.push_str(" (after Cmd+X)");
                }
                (shortcut_label(&e.from), to, e.app)
            })
            .collect()
    }
}

fn shortcut_label(s: &Shortcut) -> String {
    if s.mods.is_empty() {
        crate::keys::key_name(s.key)
    } else {
        s.to_string()
    }
}

/// What activating a preset entry does to the cut-pending state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Effect {
    #[default]
    None,
    SetCut,
    ClearCut,
}

/// A compiled preset mapping (see [`entries`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresetEntry {
    pub from: Shortcut,
    pub to: Target,
    /// Only in this app (normalized bundle id).
    pub app: Option<&'static str>,
    /// Skip in the preset's excluded apps.
    pub honor_exclusions: bool,
    /// Pass through while a text field has focus.
    pub not_in_text: bool,
    /// Only while a Finder cut is pending.
    pub needs_cut: bool,
    pub effect: Effect,
}

fn sc(mods: &[u32], key: u32) -> Shortcut {
    let mut m = Mods::default();
    for &k in mods {
        m.add(k);
    }
    Shortcut::new(m, key)
}

const CMD: u32 = VK_WIN_BOTH;
const CTRL: u32 = VK_CONTROL;
const ALT: u32 = VK_MENU;
const SHIFT: u32 = VK_SHIFT;

/// The engine entries for a preset config (empty when disabled).
pub(crate) fn entries(p: &WindowsStylePreset) -> Vec<PresetEntry> {
    let mut v = Vec::new();
    if !p.enabled {
        return v;
    }
    let mut global = |from: Shortcut, to: Shortcut| {
        v.push(PresetEntry {
            from,
            to: Target::Shortcut(to),
            app: None,
            honor_exclusions: true,
            not_in_text: false,
            needs_cut: false,
            effect: Effect::None,
        })
    };
    if p.home_end {
        for (k, arrow) in [(VK_HOME, VK_LEFT), (VK_END, VK_RIGHT)] {
            global(sc(&[], k), sc(&[CMD], arrow));
            global(sc(&[SHIFT], k), sc(&[CMD, SHIFT], arrow));
        }
        for (k, arrow) in [(VK_HOME, VK_UP), (VK_END, VK_DOWN)] {
            global(sc(&[CTRL], k), sc(&[CMD], arrow));
            global(sc(&[CTRL, SHIFT], k), sc(&[CMD, SHIFT], arrow));
        }
    }
    if p.page_up_down {
        for k in [VK_PRIOR, VK_NEXT] {
            global(sc(&[], k), sc(&[ALT], k));
        }
    }
    if p.ctrl_word_nav {
        for k in [VK_LEFT, VK_RIGHT] {
            global(sc(&[CTRL], k), sc(&[ALT], k));
            global(sc(&[CTRL, SHIFT], k), sc(&[ALT, SHIFT], k));
        }
    }
    if p.ctrl_word_delete {
        for k in [VK_BACK, VK_DELETE] {
            global(sc(&[CTRL], k), sc(&[ALT], k));
        }
    }
    // Finder entries come before the Ctrl→Cmd ones so that Ctrl+X / Ctrl+V
    // in Finder take the cut/move route.
    let mut finder = |from: Shortcut, to: Target, needs_cut: bool, effect: Effect| {
        v.push(PresetEntry {
            from,
            to,
            app: Some(FINDER),
            honor_exclusions: false,
            not_in_text: true,
            needs_cut,
            effect,
        })
    };
    if p.finder_cut_paste {
        let mut cut_keys = vec![CMD];
        if p.ctrl_shortcuts_to_cmd {
            cut_keys.push(CTRL);
        }
        for m in cut_keys {
            finder(
                sc(&[m], VK_X),
                Target::Shortcut(sc(&[CMD], VK_C)),
                false,
                Effect::SetCut,
            );
            finder(
                sc(&[m], VK_V),
                Target::Shortcut(sc(&[CMD, ALT], VK_V)),
                true,
                Effect::ClearCut,
            );
        }
    }
    if p.finder_enter_opens {
        finder(
            sc(&[], VK_RETURN),
            Target::Shortcut(sc(&[CMD], VK_O)),
            false,
            Effect::None,
        );
        finder(sc(&[], VK_F2), Target::Key(VK_RETURN), false, Effect::None);
    }
    if p.finder_delete_to_trash {
        finder(
            sc(&[], VK_DELETE),
            Target::Shortcut(sc(&[CMD], VK_BACK)),
            false,
            Effect::None,
        );
    }
    if p.finder_backspace_up {
        finder(
            sc(&[], VK_BACK),
            Target::Shortcut(sc(&[CMD], VK_UP)),
            false,
            Effect::None,
        );
    }
    if p.ctrl_shortcuts_to_cmd {
        for k in [VK_C, VK_V, VK_X, VK_Z, VK_A, VK_S, VK_F, VK_T, VK_W, VK_N] {
            v.push(PresetEntry {
                from: sc(&[CTRL], k),
                to: Target::Shortcut(sc(&[CMD], k)),
                app: None,
                honor_exclusions: true,
                not_in_text: false,
                needs_cut: false,
                effect: Effect::None,
            });
        }
        for (from, to) in [
            (sc(&[CTRL], VK_Y), sc(&[CMD, SHIFT], VK_Z)),
            (sc(&[CTRL, SHIFT], VK_Z), sc(&[CMD, SHIFT], VK_Z)),
        ] {
            v.push(PresetEntry {
                from,
                to: Target::Shortcut(to),
                app: None,
                honor_exclusions: true,
                not_in_text: false,
                needs_cut: false,
                effect: Effect::None,
            });
        }
    }
    v
}
