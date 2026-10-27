# Crates

| Crate | Purpose |
|---|---|
| `powermac` | The app binary: tray, module lifecycle, config reload, helper processes, IPC glue (`app_glue`: app status, restart, update checks, Workspaces) |
| `pm-config` | The shared `config.json` model (`AppConfig`), status/request file types, the PC-side pairing help, launch at login |
| `pm-update` | Update check against GitHub Releases: SemVer comparison, response parsing (fixtures), once a day, via the system `curl` |
| `pm-hook` | The one global input hook (`CGEventTap` / LL hooks) every module subscribes to: priority-ordered dispatch, injection tags, health ([details](input-hook.md)) |
| `pm-input` | Mouse Without Borders' input capture (a `pm-hook` subscriber) and injection (`CGEventPost` / `SendInput`), VK keymaps |
| `pm-ui` | Shared egui theme and widgets (switches, cards, keycaps) |
| `settings-ui` | The egui settings window (with the first-run setup and `permissions`: which permissions the enabled modules need, live state, panes, prompts) and the FancyZones layout editor |
| `workspaces` | Capture / restore desktops of apps and windows, PowerToys import, desktop shortcuts, the Workspaces editor |
| `mwb` | Mouse Without Borders protocol (packets, PBKDF2 + AES-256-CBC, handshake), discovery, engine, C# interop harness |
| `fancyzones` | Zone math, edge snapping, config (imports FancyZones JSON), AX / Win32 backends, overlay |
| `window-switcher` | Alt+Tab-style window switcher |
| `dock-previews` | Window previews on Dock icons |
| `menubar-manager` | Hide menu bar icons behind separators |
| `notch-shelf` | The notch drop shelf and Home-view widgets |
| `awake` | IOPMAssertion / SetThreadExecutionState controller |
| `scroll-reverser` | Device classification, wheel reversal, smooth scrolling |
| `mouse-tuning` | Pointer acceleration / speed, button remapping |
| `mouse-utils` | Find My Mouse, highlighter, crosshairs |
| `keyboard-manager` | Remap state machine, paste as plain text, Windows-style keys, Hyper key, hotkeys, the shared automation actions (shell commands, …), PowerToys importer |
| `text-expander` | Snippet expansion, variables, espanso import |
| `display-control` | DDC/CI, native and gamma brightness, media-key takeover, OSD |
| `display-scaling` | Display modes, virtual HiDPI displays, override plists |
| `audio-control` | Default-device rules, device levels, per-app volume, per-app routing and the mixer matrix (gain/mute/solo, EQ, limiter, meters), installing and configuring the virtual audio driver |
| `battery-limit` | Battery status, the charge-limit policy and strategies (inhibit, firmware range, limit via discharge), the privileged SMC helper and its installer |
| `gestures` | Multi-finger trackpad gestures (MultitouchSupport reader, recognizer) that run automation actions |
| `pm-audio-driver` | PowerMac Audio: Core Audio HAL plug-in (cdylib in `PowerMac Audio.driver`) with loopback virtual devices |
| `clipboard-history` | Clipboard watcher, history store, picker |
| `color-picker` | Screen color sampling, PowerToys color formats, editor |
| `text-extractor` | Region capture + Vision / Windows.Media.Ocr |
| `net-monitor` | Read-only per-app TCP/UDP connection monitor (libproc / IP Helper tables, snapshot diffing) and the per-app firewall rules model (most-specific-wins evaluation, `net-rules.json`, Little Snitch `.lsrules` import; not enforced) |
| `file-tools` | PowerRename, New+, Image Resizer, Finder basics, Finder / Explorer integration |

## Layout of a feature crate

```text
crates/<feature>/
  Cargo.toml
  src/
    lib.rs            public API: Config, start(), Handle
    config.rs         serde config (camelCase, defaults)
    engine.rs …       platform-independent logic + tests
    platform/
      mod.rs          picks the backend
      macos/…         AppKit / CoreGraphics / IOKit
      windows.rs      Win32
      unsupported.rs  no-op backend for other targets
```

Every crate's `lib.rs` starts with a module doc comment describing how it works; `cargo doc --workspace --no-deps --open` renders them.
