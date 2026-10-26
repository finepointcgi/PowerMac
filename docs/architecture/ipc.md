# Process model and IPC

## Processes

| Process | Started by | Role |
|---|---|---|
| `powermac` (run) | login item / app bundle | Runs the modules, tray icon, config watcher |
| `powermac settings` | tray → Settings… | Edits config files; never touches hardware itself |
| `powermac editor` | hotkey / tray | FancyZones layout editor |
| `powermac clipboard` | hotkey / tray | Clipboard history picker |
| `powermac color-picker --editor` | after a pick | Color editor |
| `powermac rename` / `new` / `resize` / `finder` | Finder Quick Actions | File tools |
| `com.powermac.battery-helper` | launchd (root) | SMC writes for the battery limit |

Helper windows are separate processes because each egui event loop needs a main thread and the tray app's main thread runs AppKit. The app keeps at most one process per kind; asking again brings the existing window to the front.

## File-based IPC

All files live next to `config.json` and are written atomically (temp file + rename).

| File | Writer → reader | Contents | Cadence |
|---|---|---|---|
| `config.json`, `fancyzones.json`, `file-tools.json` | settings / tray / you → app | settings | watched, applied within ~1 s |
| `mwb-status.json` | app → settings | MWB machines, connection and discovery state | ≥ every 5 s |
| `mwb-discover.json` | settings → app | "search now" request | one-shot, timestamped |
| `display-status.json` | app → settings | displays and their values | every 2 s |
| `display-set.json` | settings → app | brightness / contrast / volume / mute | polled every 150 ms, ignored if > 3 s old |
| `display-scaling-status.json` | app → settings | displays, modes, pending confirmations | 2 s (0.5 s while pending) |
| `display-scaling-request.json` | settings → app | apply / confirm / revert | polled |
| `audio-status.json` | app → settings | devices, apps playing, permission | every 2 s |
| `audio-request.json` | settings → app | default device, levels | polled every 150 ms |
| `battery-status.json` | app → settings | controller status | every 5 s |
| `battery-request.json` | settings → app | charge to 100 % now, refresh | polled |
| `menubar-status.json` | app → settings | permissions, items and their sections | on request |
| `menubar-request.json` | settings → app | list / move / show / hide | polled |

Status files carry a timestamp (`at`, ms since the epoch); the settings window treats stale status as "app not running". Requests carry one too and old ones are ignored, so a request written while the app was down never fires later.

## Other channels

- **Clipboard picker ↔ app:** loopback TCP (`clipboard-history::ipc`). Without a running app the picker reads the history files directly.
- **App ↔ battery helper:** a Unix socket at `/var/run/com.powermac.battery-helper.sock`, accepted only from the user who installed it.
- **Mouse Without Borders:** TCP 15100/15101 between machines, encrypted with AES-256-CBC.
