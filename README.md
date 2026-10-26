# PowerMac

A Rust port of PowerToys-style utilities that runs on **macOS and Windows**, as a menu-bar (macOS) or notification-area (Windows) app with a settings window.

| Module | What it does |
|---|---|
| **Mouse Without Borders** | Share one keyboard and mouse (plus clipboard text) across up to 4 machines. It is wire-compatible with Windows PowerToys MWB, so a Mac running PowerMac can join a matrix of PCs running PowerToys. |
| **FancyZones** | PowerToys FancyZones, with the same features on macOS:<br>• Layouts: Focus, Columns, Rows, Grid, Priority Grid, plus custom grid and canvas layouts you build in the **layout editor** (Ctrl+Option+`, or tray → FancyZones → Edit layouts…).<br>• Dragging: Shift+drag to drop a window into a zone; Ctrl or middle-click to span zones; scroll while dragging to switch layouts.<br>• Shortcuts: Ctrl+Option+Arrow snaps the focused window; Ctrl+Option+Cmd+0–9 switches layouts; Ctrl+Option+] and [ cycle the windows in a zone.<br>• Window handling: monitor rotation, excluded apps, separate layouts for each Space, windows kept in their zones when layouts or displays change, new windows placed in their app's last zone.<br>On Windows the shortcuts use Win in place of Ctrl+Option, as in PowerToys. |
| **Edge snapping** (Rectangle-style, part of FancyZones) | Drag a window to an edge to snap it: left/right edge for a half, top for maximize, bottom for the bottom half or thirds, corners for quarters. A preview shows first, and dragging the window out restores its size. On by default on macOS; off on Windows, which has its own Snap. |
| **Awake** | Keep the computer (and optionally the display) awake: indefinitely, for a set time, until a set time, or while a process runs. |
| **Scroll Reverser** | Reverse the mouse wheel but keep natural scrolling on the trackpad, or any other combination. Vertical and horizontal are set separately. |
| **Mouse Utilities** | Find My Mouse (double-tap Left Ctrl for a spotlight), a click highlighter (Cmd/Win+Shift+H) and crosshairs (Cmd+Option+P / Win+Alt+P). |
| **Keyboard Manager** | Remap keys (e.g. CapsLock→Esc) and shortcuts (e.g. Ctrl+Left→Option+Left, optionally only in one app). A shortcut can also run a program, open a URL or type text. **Paste as plain text** is Cmd+Option+Shift+V (Win+Ctrl+Alt+V on Windows). |

## Quick start (macOS)

```bash
./scripts/bundle-macos.sh                # builds target/release/PowerMac.app
open target/release/PowerMac.app         # or move it to /Applications first
```

The first launch creates a config with a new Mouse Without Borders key. Everything else is in the tray menu: **Settings…**, Awake modes, FancyZones layouts, scroll direction, mouse utilities, Paste as plain text, Copy security key, and Launch at login.

### Command line

```bash
cargo build --release
./target/release/powermac            # same as `run`: all enabled modules + tray icon
./target/release/powermac settings   # the settings window
./target/release/powermac doctor     # config, permissions, screens
./target/release/powermac init       # write a fresh config.json and print the key
./target/release/powermac awake --display-on --time-limit 3600
```

Config lives at `~/Library/Application Support/PowerMac/config.json` (`%APPDATA%\PowerMac\config.json` on Windows), with FancyZones layouts in `fancyzones.json` next to it. Changes to either file take effect within about a second, whether they come from the settings window, the tray or a text editor.

### Pairing with Windows PowerToys

1. In PowerToys → Mouse Without Borders on the PC, copy its security key and machine name. You can also go the other way: use **Copy security key** in the PowerMac tray and paste it into PowerToys.
2. In Settings → Mouse Without Borders, enter the key. With **Find other computers automatically** on (the default), PowerMac probes TCP 15101 on the hosts of this Mac's private subnets (at most 1024 per network, the /24 around the Mac on larger ones) every minute until a machine is connected, then every 5 minutes, or right away with **Search now** / tray → **Find computers now**. A machine that completes the key handshake is connected, put to the right of this Mac in the layout, and its address is saved; PowerToys then adds the Mac to its own layout. A machine with a different key is left alone for 30 minutes. Otherwise put both machines in the matrix in left-to-right order, and if the PC's name doesn't resolve, add a peer address such as `192.168.1.20`.
3. Allow TCP **15101** through the firewall on both machines.
4. "Swap Cmd/Ctrl" (on by default on macOS) makes Cmd+C on the Mac arrive as Ctrl+C on Windows, and the other way round.

### macOS permissions

Mouse Without Borders, FancyZones, the scroll reverser, mouse utilities and the keyboard manager need **Accessibility** and **Input Monitoring** (System Settings → Privacy & Security). Settings → General shows their status and links to the right panes.

macOS ties these grants to the app's code signature. The bundle is ad-hoc signed, so after rebuilding `PowerMac.app` you may need to remove it from both lists and add it again.

## Layout

```
crates/pm-input          global input capture/injection (CGEventTap / LL hooks + SendInput), VK keymaps
crates/mwb               MWB protocol (packets, PBKDF2 + AES-256-CBC, handshake), engine, C# interop harness
crates/fancyzones        zone math, edge snapping, config (imports FancyZones JSON), AX / Win32 backends, overlay
crates/awake             IOPMAssertion / SetThreadExecutionState controller
crates/scroll-reverser   trackpad-vs-mouse classification and wheel reversal
crates/mouse-utils       Find My Mouse, highlighter, crosshairs (Core Animation / layered-window overlays)
crates/keyboard-manager  remap state machine, paste as plain text, PowerToys default.json importer
crates/pm-config         the shared config.json model, launch-at-login
crates/settings-ui       egui settings window (runs as `powermac settings`)
crates/powermac          the app: tray, module lifecycle, config reload, clipboard bridge
```

## Testing

`cargo test --workspace` runs about 275 tests. `crates/mwb/tests/interop.rs` builds a .NET harness from PowerToys' own encryption and packet code (it needs `dotnet`). That harness checks byte-for-byte test vectors and runs live TCP sessions in both directions.

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
- **Not ported yet:** Mouse Jump, Color Picker, Always On Top, Workspaces.
