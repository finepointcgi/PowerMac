# Configuration files

All settings are plain JSON files that you can edit with the settings window, the tray menu or a text editor. The running app watches them and applies changes within about a second.

## Location

| OS | Folder |
|---|---|
| macOS | `~/Library/Application Support/PowerMac/` |
| Windows | `%APPDATA%\PowerMac\` |

## Files you edit

| File | Contents |
|---|---|
| `config.json` | Which modules run and their settings (one camelCase section per module). |
| `fancyzones.json` | FancyZones settings, custom layouts, applied layouts per monitor, quick-switch keys. Compatible with PowerToys FancyZones JSON. |
| `file-tools.json` | PowerRename, New+, Image Resizer and Finder basics settings and remembered state. |
| `net-rules.json` | Per-app firewall rules for the [Network Monitor](../features/network-monitor.md) (not enforced; `powermac net rules …` edits it). |
| `Templates/` | New+ templates (unless `newPlus.templatesDir` points elsewhere). |

## Files the app manages

| File / folder | Contents |
|---|---|
| `clipboard/` | Clipboard history: `index.json` and `blobs/`. |
| `color-picker-history.json` | Picked colors, newest first. |
| `workspaces.json` | Saved workspaces (PowerToys' format). |
| `update-status.json` | The last update check. |
| Notch shelf data | Items parked on the shelf. |
| `*-status.json` / `*-request.json` | Status and requests exchanged with the settings window (see [Process model and IPC](../architecture/ipc.md)). |

## `config.json` sections

| Key | Module | Default |
|---|---|---|
| `mouseWithoutBorders` | [Mouse Without Borders](../features/mouse-without-borders.md) | on |
| `fancyzones` | [FancyZones](../features/fancyzones.md) (`enabled` only; the rest is in `fancyzones.json`) | on |
| `awake` | [Awake](../features/awake.md) | off |
| `scrollReverser` | [Scrolling](../features/scrolling.md) | on (macOS) |
| `mouseUtils` | [Mouse Utilities](../features/mouse-utils.md) | per tool |
| `keyboardManager` | [Keyboard Manager](../features/keyboard-manager.md) | on |
| `mouseTuning` | [Mouse tuning](../features/mouse-tuning.md) | changes nothing until set |
| `displayControl` | [Displays](../features/displays.md) | on |
| `clipboardHistory` | [Clipboard History](../features/clipboard-history.md) | off |
| `notchShelf` | [Notch Shelf](../features/notch-shelf.md) | on with a notch |
| `colorPicker` | [Color Picker](../features/color-picker.md) | on |
| `textExtractor` | [Text Extractor](../features/text-extractor.md) | on |
| `windowSwitcher` | [Window Switcher](../features/window-switcher.md) | on |
| `displayScaling` | [Display Scaling](../features/display-scaling.md) | off |
| `audio` | [Sound](../features/sound.md) | on |
| `dockPreviews` | [Dock Previews](../features/dock-previews.md) | on |
| `textExpander` | [Text Expander](../features/text-expander.md) | off |
| `menuBar` | [Menu Bar Manager](../features/menu-bar.md) | off |
| `workspaces` | [Workspaces](../features/workspaces.md) (the workspaces are in `workspaces.json`) | on, no shortcut |
| `updates` | [Updates](updates.md): `check`, `includePrereleases`, `lastCheck` | daily check on |
| `setup` | The setup window: `completed`, `remindMissingPermissions`, `completedVersion` | not completed |

Every field is optional. Missing sections and fields take their defaults, so config files written by older versions keep loading. Unknown widget kinds written by newer versions are ignored instead of rejecting the file.

## A minimal example

```json
{
  "mouseWithoutBorders": { "enabled": true, "key": "YourSecurityKey1", "matrix": ["mac", "pc"] },
  "awake": { "enabled": true, "mode": { "type": "indefinite" }, "keepDisplayOn": true },
  "keyboardManager": { "remapKeys": [{ "from": "CapsLock", "to": "Esc" }] },
  "clipboardHistory": { "enabled": true }
}
```

## Writes are atomic

PowerMac writes every file to a `.json.tmp` next to it and renames it into place, so the watcher (and you) never read half a file. Do the same if you generate config with a script.

## Shortcut conflicts

The settings window checks shortcuts across all modules and won't save two modules on the same keys.
