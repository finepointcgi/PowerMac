# PowerMac

A Rust port of PowerToys-style utilities that runs on **macOS and Windows**, as a menu-bar (macOS) or notification-area (Windows) app with a settings window.

**Documentation:** the full guide (every feature, its settings, the architecture and the release checklist) is in [`docs/`](docs/index.md) and is published as a site by GitLab Pages (`.gitlab-ci.yml`) or GitHub Pages (`.github/workflows/docs.yml`). Preview it locally with `pip install -r requirements-docs.txt && mkdocs serve`.

| Module | What it does |
|---|---|
| **Mouse Without Borders** | Share one keyboard and mouse (plus clipboard text) across up to 4 machines. It is wire-compatible with Windows PowerToys MWB, so a Mac running PowerMac can join a matrix of PCs running PowerToys. |
| **FancyZones** | PowerToys FancyZones, with the same features on macOS:<br>• Layouts: Focus, Columns, Rows, Grid, Priority Grid, plus custom grid and canvas layouts you build in the **layout editor** (Ctrl+Option+`, or tray → FancyZones → Edit layouts…).<br>• Dragging: Shift+drag to drop a window into a zone; Ctrl or middle-click to span zones; scroll while dragging to switch layouts.<br>• Shortcuts: Ctrl+Option+Arrow snaps the focused window; Ctrl+Option+Cmd+0–9 switches layouts; Ctrl+Option+] and [ cycle the windows in a zone.<br>• Window handling: monitor rotation, excluded apps, separate layouts for each Space, windows kept in their zones when layouts or displays change, new windows placed in their app's last zone.<br>On Windows the shortcuts use Win in place of Ctrl+Option, as in PowerToys. |
| **Edge snapping** (Rectangle-style, part of FancyZones) | Drag a window to an edge to snap it: left/right edge for a half, top for maximize, bottom for the bottom half or thirds, corners for quarters. A preview shows first, and dragging the window out restores its size. On by default on macOS; off on Windows, which has its own Snap. |
| **Awake** | Keep the computer (and optionally the display) awake: indefinitely, for a set time, until a set time, or while a process runs. |
| **Window Switcher** | Windows-style Alt+Tab: Option+Tab cycles through **windows** (not apps) in most-recently-used order with live previews; Option+` cycles the front app's windows. Can also replace Cmd+Tab. Q/W/M/H/F quit, close, minimize, hide or full-screen the selected window. |
| **Notch Shelf** | Turns the notch into a drop shelf: drag files, links, text or images to the top of the screen, park them, and drag them out again later. Macs without a notch can get a "virtual notch". |
| **Mouse** (mouse tuning) | Turn pointer acceleration off and set the speed, separately for mice and trackpads, with per-device overrides (macOS). Side buttons go back/forward; the middle and side buttons can be remapped to Mission Control, App Exposé, Launchpad, copy/paste or any shortcut. |
| **Scrolling** | Reverse the mouse wheel but keep natural scrolling on the trackpad, or any other combination (vertical and horizontal separately), plus **smooth scrolling** for mouse wheels with adjustable distance, duration, acceleration and easing curve. |
| **Mouse Utilities** | Find My Mouse (double-tap Left Ctrl for a spotlight), a click highlighter (Cmd/Win+Shift+H) and crosshairs (Cmd+Option+P / Win+Alt+P). |
| **Keyboard Manager** | Remap keys (e.g. CapsLock→Esc) and shortcuts (e.g. Ctrl+Left→Option+Left, optionally only in one app). A shortcut can also run a program, open a URL or type text. **Paste as plain text** is Cmd+Option+Shift+V (Win+Ctrl+Alt+V on Windows). **Windows-style keys** (macOS, off by default): Home/End, Ctrl+arrows, Ctrl+Backspace, and in Finder Cmd+X/Cmd+V to move files, Return to open, Delete to trash. |
| **Displays** | Brightness, contrast and volume of external monitors over DDC/CI (Apple Silicon; native brightness for Apple displays, software dimming for the rest). The brightness/volume keys act on the display under the pointer, with an on-screen level. Sliders in Settings and presets in the tray. |
| **Clipboard History** | Off by default. Everything you copy (text, rich text, images, files), searchable in a picker on Cmd+Shift+V (Win+Shift+V); pin items, paste again. Password managers' concealed items and excluded apps are never recorded. The tray lists the 10 most recent items. |
| **Color Picker** | Ctrl+Option+C (Win+Shift+C) picks a color anywhere on screen and copies it as HEX, RGB, HSL, … or a custom format; the editor shows history, variations and every format. |
| **Text Extractor** | Ctrl+Option+T (Win+Shift+T): drag over any part of the screen and the text in it is copied (Vision OCR on macOS, Windows.Media.Ocr on Windows). |
| **File Tools** | **PowerRename** (bulk rename with search/replace, regex and previews) and **New+** (new files and folders from your templates), both from Finder Quick Actions (Explorer context menu on Windows) once installed from Settings → File Tools or the tray. |

## Quick start (macOS)

```bash
./scripts/bundle-macos.sh                # builds target/release/PowerMac.app
open target/release/PowerMac.app         # or move it to /Applications first
```

The first launch creates a config with a new Mouse Without Borders key. Everything else is in the tray menu: **Settings…**, Awake modes, FancyZones layouts, scroll direction, mouse utilities, Paste as plain text, Copy security key, Launch at login, and a **Tools** section: Pick color, Extract text, Clipboard history (picker, recent items, pause, clear), Window switcher, Show shelf, Displays (brightness/volume presets per external display) and the Finder Quick Actions switch.

### Command line

```bash
cargo build --release
./target/release/powermac            # same as `run`: all enabled modules + tray icon
./target/release/powermac settings   # the settings window
./target/release/powermac doctor     # config, permissions, screens
./target/release/powermac init       # write a fresh config.json and print the key
./target/release/powermac awake --display-on --time-limit 3600
./target/release/powermac clipboard              # the clipboard history picker
./target/release/powermac color-picker           # pick a color, copy + print it (--editor: the editor)
./target/release/powermac ocr shot.png           # text in an image (--clipboard: the clipboard image; neither: select on screen)
./target/release/powermac rename a.jpg b.jpg     # PowerRename (what the Finder Quick Action runs)
./target/release/powermac new --dir ~/Desktop    # New+ (what the Finder Quick Action runs)
```

Config lives at `~/Library/Application Support/PowerMac/config.json` (`%APPDATA%\PowerMac\config.json` on Windows), with FancyZones layouts in `fancyzones.json` and PowerRename/New+ settings in `file-tools.json` next to it. Changes take effect within about a second, whether they come from the settings window, the tray or a text editor. Config files from older versions keep loading; new sections get their defaults. The same folder holds the clipboard history (`clipboard/`), the notch shelf, the color history, and `display-status.json` / `display-set.json`, through which the settings window (a separate process that never talks to the monitors itself) shows and changes display values via the running app.

The settings window checks shortcuts across all modules and won't save two modules on the same keys.

### Pairing with Windows PowerToys

1. In PowerToys → Mouse Without Borders on the PC, copy its security key and machine name. You can also go the other way: use **Copy security key** in the PowerMac tray and paste it into PowerToys.
2. In Settings → Mouse Without Borders, enter the key. With **Find other computers automatically** on (the default), PowerMac probes TCP 15101 on the hosts of this Mac's private subnets (at most 1024 per network, the /24 around the Mac on larger ones) every minute until a machine is connected, then every 5 minutes, or right away with **Search now** / tray → **Find computers now**. A machine that completes the key handshake is connected, put to the right of this Mac in the layout, and its address is saved; PowerToys then adds the Mac to its own layout. A machine with a different key is left alone for 30 minutes. Otherwise put both machines in the matrix in left-to-right order, and if the PC's name doesn't resolve, add a peer address such as `192.168.1.20`.
3. Allow TCP **15101** through the firewall on both machines.
4. "Swap Cmd/Ctrl" (on by default on macOS) makes Cmd+C on the Mac arrive as Ctrl+C on Windows, and the other way round.

### macOS permissions

| Permission | Needed by |
|---|---|
| **Accessibility** | Mouse Without Borders, FancyZones, Scrolling, Mouse Utilities, Keyboard Manager, Window Switcher, Clipboard History's hotkey, Mouse button mappings, Displays' brightness/volume keys |
| **Input Monitoring** | Mouse Without Borders, FancyZones, Mouse Utilities, Keyboard Manager |
| **Screen Recording** | Text Extractor (capturing the screen; files and clipboard images work without it) and Window Switcher previews (app icons are shown without it) |
| none | Color Picker (system loupe, Carbon hotkey), Notch Shelf, Displays sliders, pointer speed/acceleration, File Tools |

Settings → General (and each module's page) shows their status and links to the right panes. A module whose permission is missing logs a message and keeps the rest of PowerMac running.

macOS ties these grants to the app's code signature. The bundle is ad-hoc signed, so after rebuilding `PowerMac.app` you may need to remove it from both lists and add it again.

## Layout

```
crates/pm-input          global input capture/injection (CGEventTap / LL hooks + SendInput), VK keymaps
crates/mwb               MWB protocol (packets, PBKDF2 + AES-256-CBC, handshake), engine, C# interop harness
crates/fancyzones        zone math, edge snapping, config (imports FancyZones JSON), AX / Win32 backends, overlay
crates/awake             IOPMAssertion / SetThreadExecutionState controller
crates/scroll-reverser   trackpad-vs-mouse classification and wheel reversal
crates/mouse-utils       Find My Mouse, highlighter, crosshairs (Core Animation / layered-window overlays)
crates/keyboard-manager  remap state machine, paste as plain text, Windows-style keys preset, PowerToys default.json importer
crates/mouse-tuning      pointer acceleration / speed (HID event system, SPI_SETMOUSE), button remapping
crates/display-control   DDC/CI, native and gamma brightness, media-key takeover, OSD
crates/clipboard-history clipboard watcher, history store, picker (egui, own process)
crates/notch-shelf       the notch drop shelf (NSPanel / layered window)
crates/color-picker      NSColorSampler / magnifier pick, PowerToys color formats, editor
crates/text-extractor    region capture + Vision / Windows.Media.Ocr
crates/file-tools        PowerRename, New+, Finder Quick Actions / Explorer verbs
crates/window-switcher   Alt+Tab-style window switcher (event tap, CGWindowList, overlay)
crates/pm-ui             shared egui theme and widgets
crates/pm-config         the shared config.json model, launch-at-login
crates/settings-ui       egui settings window (runs as `powermac settings`)
crates/powermac          the app: tray, module lifecycle, config reload, clipboard bridge
```

## Testing

`cargo test --workspace` runs about 550 tests. `PM_UI_SHOTS=<dir> cargo test -p settings-ui shots` renders every settings page (light and dark) to PNGs. `crates/mwb/tests/interop.rs` builds a .NET harness from PowerToys' own encryption and packet code (it needs `dotnet`). That harness checks byte-for-byte test vectors and runs live TCP sessions in both directions.

On a Mac with permissions granted, these were also run live:
- Input capture and injection.
- Edge snapping (halves, quarters, maximize), Shift+drag into zones and Ctrl+Option+Arrow, on a real TextEdit window.
- The real app connecting to the C# peer. Hitting the screen edge switched control and forwarded moves and the scroll wheel; moving back across the edge returned control.
- Scroll reversal of real scroll events.
- Remapping harmless F13–F20 keys.
- Awake power assertions, checked with `pmset`.

## Known gaps

- **Windows:** every crate is compile-checked (`cargo check --target x86_64-pc-windows-msvc`) but none has been run on Windows yet.
- **Mouse Without Borders:** no file transfer, drag-and-drop or clipboard images; clipboard text works. About 1 in 512 keys trips a C# quirk that makes Windows MWB reject every packet; PowerMac warns about it, and you should generate a new key if you see the warning.
- **Scroll Reverser on Windows:** it can't tell a touchpad from a mouse, so the mouse settings apply to all wheel input. There is an optional heuristic.
- **Keyboard Manager:** no two-step chords and no PowerToys "alone" remaps. CapsLock works as a tap only on macOS.
- **Other window snappers:** quit Rectangle (or turn off its edge snapping) and leave macOS's "Drag windows to screen edges to tile" off; otherwise windows get snapped twice.
- **FancyZones on macOS:** a dragged window can't be made see-through (macOS offers no API for that). Zone assignments are kept in memory, so they're lost when PowerMac restarts. 
- **Displays:** DDC/CI is implemented for Apple Silicon Macs; on Intel Macs only native and software brightness work.
- **Not ported yet:** Mouse Jump, Always On Top, Workspaces.
