# Features

Every module can be turned on or off on its own in **Settings** or in `config.json`. The table shows each module's default and its main macOS shortcut; on Windows, shortcuts that use Ctrl+Option on macOS use Win, as in PowerToys.

| Module | Default | macOS shortcut | Permissions (macOS) | Windows |
|---|---|---|---|---|
| [FancyZones](fancyzones.md) | on | Shift+drag, Ctrl+Option+Arrow, Ctrl+Option+\` | Accessibility, Input Monitoring | yes |
| [Edge snapping](fancyzones.md#edge-snapping) | on (macOS) | drag to an edge | Accessibility | off (Windows has Snap) |
| [Window Switcher](window-switcher.md) | on | Option+Tab, Option+\` | Accessibility (+ Screen Recording for thumbnails) | Alt+\` |
| [Workspaces](workspaces.md) | on | none (set one for the editor) | Accessibility | yes |
| [Dock Previews](dock-previews.md) | on | hover a Dock icon | Accessibility (+ Screen Recording) | no-op |
| [Menu Bar Manager](menu-bar.md) | off | click the chevron | Accessibility | no-op |
| [Notch Shelf](notch-shelf.md) · [widgets](notch-widgets.md) | on with a notch | drag to the notch | none (widgets: see page) | top-center shelf |
| [Mouse Without Borders](mouse-without-borders.md) | on | move past a screen edge | Accessibility, Input Monitoring | yes |
| [Mouse tuning](mouse-tuning.md) | changes nothing until set | — | Accessibility (buttons only) | yes |
| [Scrolling](scrolling.md) | on (macOS) | — | Accessibility | yes |
| [Mouse Utilities](mouse-utils.md) | per tool | double Left Ctrl, Cmd+Shift+H, Cmd+Option+P | Accessibility, Input Monitoring | yes |
| [Keyboard Manager](keyboard-manager.md) | on | Cmd+Option+Shift+V (paste plain) | Accessibility, Input Monitoring | yes |
| [Automation](automation.md): hotkeys, mouse buttons, gestures | nothing bound | your own | Accessibility | hotkeys and buttons |
| [Text Expander](text-expander.md) | off | type a trigger | Accessibility | yes |
| [Displays](displays.md) | on | brightness / volume keys | Accessibility (keys only) | yes |
| [Display Scaling](display-scaling.md) | off | — | none (admin for override plists) | modes only |
| [Sound](sound.md) | on | — | Audio capture (per-app volume) | yes |
| [App routing and Mixer](audio-routing-mixer.md) | no routes, mixer off | — | System Audio Recording (apps), Microphone (inputs) | routes only |
| [Battery](battery.md) | status; limit off | tray battery line | admin password once (helper, on request) | status only |
| [Awake](awake.md) | off | tray menu | none | yes |
| [Clipboard History](clipboard-history.md) | off | Cmd+Shift+V | Accessibility (hotkey) | Win+Shift+V |
| [Color Picker](color-picker.md) | on | Ctrl+Option+C | none | Win+Shift+C |
| [Text Extractor](text-extractor.md) | on | Ctrl+Option+T | Screen Recording | Win+Shift+T |
| [File Tools](file-tools.md) | Quick Actions installed on request | Finder right-click | none | Explorer verbs |
| [Network Monitor](network-monitor.md) | Settings → Network, `powermac net` | tray → Network activity… | none (root to see other users' processes) | yes (UDP without peers) |

## Common behaviour

- **Hot reload.** Every module reconfigures live when its config changes; there is no restart.
- **Graceful failure.** A module that can't start (missing permission, unsupported hardware) logs the reason and the rest of PowerMac keeps running.
- **Clean shutdown.** Modules that change system state put it back when they stop: pointer settings, gamma tables, virtual HiDPI displays, Caps Lock mappings, and per-app audio taps are all restored.
- **No double handling.** Events injected by PowerMac's own modules are tagged, and every event tap skips tagged events, so modules never react to each other's synthetic input.
