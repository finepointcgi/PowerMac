# Crates

| Crate | Purpose |
|---|---|
| `powermac` | The app binary: tray, module lifecycle, config reload, helper processes, IPC glue |
| `pm-config` | The shared `config.json` model (`AppConfig`), status/request file types, launch at login |
| `pm-hook` | The one global input hook (`CGEventTap` / LL hooks) every module subscribes to: priority-ordered dispatch, injection tags, health ([details](input-hook.md)) |
| `pm-input` | Mouse Without Borders' input capture (a `pm-hook` subscriber) and injection (`CGEventPost` / `SendInput`), VK keymaps |
| `pm-ui` | Shared egui theme and widgets (switches, cards, keycaps) |
| `settings-ui` | The egui settings window and the FancyZones layout editor |
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
| `keyboard-manager` | Remap state machine, paste as plain text, Windows-style keys, Hyper key, PowerToys importer |
| `text-expander` | Snippet expansion, variables, espanso import |
| `display-control` | DDC/CI, native and gamma brightness, media-key takeover, OSD |
| `display-scaling` | Display modes, virtual HiDPI displays, override plists |
| `audio-control` | Default-device rules, device levels, per-app volume |
| `clipboard-history` | Clipboard watcher, history store, picker |
| `color-picker` | Screen color sampling, PowerToys color formats, editor |
| `text-extractor` | Region capture + Vision / Windows.Media.Ocr |
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
