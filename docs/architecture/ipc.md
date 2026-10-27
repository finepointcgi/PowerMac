# Process model and IPC

## Processes

| Process | Started by | Role |
|---|---|---|
| `powermac` (run) | login item / app bundle | Runs the modules, tray icon, config watcher |
| `powermac settings` | tray → Settings… | Edits config files; never touches hardware itself |
| `powermac editor` | hotkey / tray | FancyZones layout editor |
| `powermac clipboard` | hotkey / tray | Clipboard history picker |
| `powermac color-picker --editor` | after a pick | Color editor |
| `powermac workspace edit` | tray / hotkey / settings | Workspaces editor |
| `powermac permissions --json` | settings, every second while a permission row is visible | Reports the live permission state of a fresh process |
| `powermac mwb doctor` | settings → Run diagnostics | Pairing diagnostics (stdout shown in the window) |
| `powermac rename` / `new` / `resize` / `finder` | Finder Quick Actions | File tools |

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
| `menubar-status.json` | app → settings | permissions, items and their sections | on request |
| `menubar-request.json` | settings → app | list / move / show / hide | polled |
| `notch-status.json` | app → settings | Script / xbar / Web widget results, reminder lists, widget permission states | every 2 s while the shelf runs |
| `notch-request.json` | settings → app | refresh a widget, open the notch on a page | polled every 250 ms |
| `settings-request.json` | tray → settings | switch an open settings window to a page (tray → Notch widgets…) | polled every second |
| `app-status.json` | app → settings | version, pid, executable, permissions as the app sees them, input-hook health (state, re-enables, slow handlers), one status per module | every 2 s |
| `app-request.json` | settings → app | restart (reopening a settings page), launch / capture a workspace, open the Workspaces editor, check for updates | polled every 250 ms |
| `update-status.json` | app or settings → both | the last update check (`pm_update::UpdateState`) | after each check |
| `mwb-diagnostics.json` | app → settings, `mwb doctor` | incoming connection attempts, learned addresses, LLMNR responder counters | every 5 s |

Status files carry a timestamp (`at`, ms since the epoch); the settings window treats stale status as "app not running". Requests carry one too and old ones are ignored, so a request written while the app was down never fires later.

## Restarting

**Restart PowerMac** (setup, Settings → General) writes `{"action": "restart", "reopenPage": "permissions"}`. The app shuts every module down cleanly, runs `open -n PowerMac.app --args --config <path> run --wait-pid <old pid> --open-settings <page>` (or starts its executable directly outside a bundle) and exits; the new instance waits for the old pid to be gone, starts the modules with fresh permissions and reopens the settings window. The settings window closes itself after sending the request. Without a running app it starts one the same way.

## Other channels

- **Clipboard picker ↔ app:** loopback TCP (`clipboard-history::ipc`). Without a running app the picker reads the history files directly.
- **Mouse Without Borders:** TCP 15100/15101 between machines, encrypted with AES-256-CBC.
