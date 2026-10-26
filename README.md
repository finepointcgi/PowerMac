# PowerMac

A Rust port of three PowerToys modules that runs on **macOS and Windows**:

| Module | What it does |
|---|---|
| **Mouse Without Borders** | Share one keyboard + mouse (and clipboard text) across up to 4 machines. Wire-compatible with the real Windows PowerToys MWB, so a Mac running PowerMac can sit in the same matrix as Windows PCs running PowerToys. |
| **FancyZones** | Zone layouts (Focus, Columns, Rows, Grid, Priority Grid, custom). Shift+drag a window to show zones and drop into one; Ctrl+Option+Arrow (Win+Arrow on Windows) snaps the focused window. |
| **Awake** | Keep the computer (and optionally the display) awake: indefinitely, for a duration, until a time, or while a process lives. |

## Quick start

```bash
cargo build --release
./target/release/powermac init      # writes config.json and prints a new MWB key
./target/release/powermac doctor    # checks config, permissions, screens
./target/release/powermac run       # runs every enabled module
```

Config lives at `~/Library/Application Support/PowerMac/config.json` (`%APPDATA%\PowerMac\config.json` on Windows); FancyZones layouts are in `fancyzones.json` next to it.

### Pairing with Windows PowerToys

1. On the Windows PC, open PowerToys → Mouse Without Borders, and copy its security key and machine name. You can also go the other way: run `powermac init` and paste the key it prints into PowerToys.
2. In `config.json` set `mouseWithoutBorders.key` and list both machines in `matrix` in left-to-right order, e.g. `["MacBook-Pro", "DESKTOP-ABC"]`. If the PC's name doesn't resolve, add `"peers": {"DESKTOP-ABC": "192.168.1.20"}`.
3. Allow TCP **15101** through the firewall on both machines.
4. `swapCmdCtrl` (on by default on macOS) makes Cmd+C on the Mac arrive as Ctrl+C on Windows, and the other way round.

### macOS permissions

Mouse Without Borders and FancyZones need **Accessibility** and **Input Monitoring** in System Settings → Privacy & Security. Grant them to the app that launches `powermac`, such as your terminal. `powermac doctor --prompt` asks for them.

### Standalone Awake

```bash
powermac awake --display-on --time-limit 3600
powermac awake --expire-at 17:30
powermac awake --pid 1234
```

## Layout

```
crates/pm-input     global input capture/injection (CGEventTap / LL hooks + SendInput), VK keymaps
crates/mwb          MWB protocol (packets, PBKDF2 + AES-256-CBC, handshake), engine, C# interop harness
crates/fancyzones   zone math, config (imports FancyZones JSON), AX / Win32 window backends, overlay
crates/awake        IOPMAssertion / SetThreadExecutionState controller
crates/powermac     the app binary
```

## Testing

`cargo test --workspace` runs about 100 tests. They include `crates/mwb/tests/interop.rs`, which builds a .NET harness from PowerToys' own encryption and packet code (needs `dotnet`). That harness checks wire compatibility two ways: byte-for-byte test vectors, and live TCP sessions in both directions (C# controls Rust, Rust controls C#).

## Known gaps

- **Mouse Without Borders:** file transfer, drag-and-drop, clipboard images, and the separate clipboard socket (port 15100) are not implemented. Clipboard text over the main socket works.
- **Windows:** the backends are compile-checked (`cargo check --target x86_64-pc-windows-msvc`) but have not been run on Windows yet.
- **macOS testing:** live input capture, injection, and window snapping need the permissions above and have not been exercised on real hardware yet.
- **About 1 in 512 keys:** Windows MWB rejects every packet when the key's magic hash has bit 31 set. This is a C# quirk, reproduced here; PowerMac logs a warning. Generate a new key if you hit it.
- **No tray icon or settings UI:** PowerMac is CLI and config-file only for now.
