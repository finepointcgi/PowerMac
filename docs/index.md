# PowerMac

PowerMac brings PowerToys-style utilities to **macOS** (and, compile-checked, to **Windows**) in one Rust app. It lives in the menu bar on macOS and the notification area on Windows. A settings window configures it, and every module can be switched on or off on its own.

It started as a port of Microsoft PowerToys and grew to cover the gaps people hit when they move to a Mac: window snapping, a window-based Alt+Tab, Dock previews, sensible mouse behaviour, external-monitor brightness, HiDPI on non-Retina screens, a battery charge limit and more.

!!! warning "Status: pre-release (0.1.0)"
    Most modules have been run live on macOS. The Windows backends compile but have **not** been run on Windows yet. See [Production readiness](production-readiness.md) for what is left before a 1.0.

## What it does

| Area | Modules |
|---|---|
| **Windows and desktop** | [FancyZones and edge snapping](features/fancyzones.md) · [Window Switcher](features/window-switcher.md) · [Dock Previews](features/dock-previews.md) · [Menu Bar Manager](features/menu-bar.md) · [Notch Shelf and widgets](features/notch-shelf.md) |
| **Input** | [Mouse Without Borders](features/mouse-without-borders.md) · [Mouse tuning](features/mouse-tuning.md) · [Scrolling](features/scrolling.md) · [Mouse Utilities](features/mouse-utils.md) · [Keyboard Manager](features/keyboard-manager.md) · [Text Expander](features/text-expander.md) |
| **Hardware** | [Displays](features/displays.md) · [Display Scaling](features/display-scaling.md) · [Sound](features/sound.md) · [Battery charge limit](features/battery.md) · [Awake](features/awake.md) |
| **Tools** | [Clipboard History](features/clipboard-history.md) · [Color Picker](features/color-picker.md) · [Text Extractor](features/text-extractor.md) · [File Tools](features/file-tools.md) (PowerRename, New+, Image Resizer, Finder basics) |

The [feature overview](features/index.md) lists every module with its default state, shortcut and the permissions it needs.

## Quick start

```bash
git clone https://github.com/finepointcgi/PowerMac.git
cd PowerMac
./scripts/bundle-macos.sh          # builds target/release/PowerMac.app
open target/release/PowerMac.app
```

Then grant the [permissions](getting-started/permissions.md) the modules you want need, and open **Settings…** from the menu-bar icon. [Install and first run](getting-started/install.md) has the details.

## How it is put together

PowerMac is a Cargo workspace of about 25 crates. Each feature is a library crate with a platform-independent, unit-tested core and thin OS backends. The `powermac` binary starts the modules, owns the tray icon and reloads the config file when it changes. The settings window and the other helper windows run as separate processes of the same executable and talk to the app only through JSON files. See [Architecture](architecture/overview.md).
