# Changelog

All notable changes to PowerMac are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.1.1] - 2026-10-27

### Fixed

- **Window Switcher**: setting its shortcut to Cmd+Tab now switches between every window, not apps. Before, macOS kept Cmd+Tab unless **Replace Cmd+Tab** was also on. Setting the front app's windows shortcut to Cmd+` takes that over the same way, and the settings page says when a shortcut replaces the system one.
- **Window Switcher**: PowerMac's own Settings window is listed like any other window.
- **Window Switcher**: when a password field turns on secure input (which hides the keyboard from PowerMac, so Cmd+Tab goes back to the macOS app switcher), the log now names the app that turned it on.

### Added

- `powermac switcher-windows [--app]` lists the windows the switcher would show right now.

## [1.1.0] - 2026-10-27

### Added

- **Shared input hook** (`pm-hook`): every module now subscribes to one keyboard/mouse hook per platform instead of installing its own, in a fixed priority order. About 4–5× less CPU spent in input handling and roughly half the input latency; the hook re-enables itself if macOS disables it.
- **Mouse Without Borders pairing with Windows**: the Mac now answers Windows LLMNR name queries, so PowerToys' **Connect** with the Mac's name (e.g. `MacBook-Pro`) resolves. Fallback: add `MACBOOK-PRO <mac-ip>` under PowerToys → Mouse Without Borders → Advanced → IP address mapping. Connecting with a bare IP doesn't work in PowerToys (it does a reverse lookup first). A machine that connects in is remembered and dialled back. `powermac mwb doctor` checks VPN routes, the firewall, the key and recent attempts, and prints the PowerShell steps for the PC.
- **PowerMac Audio** virtual device (Core Audio HAL driver, installed only from Settings with one admin prompt), per-app routing and a mixer with EQ, limiter and meters.
- **Network monitor**: `powermac net list|watch|audit|rules …` plus the Network page; rules are modelled and audited, not enforced (needs Apple's Network Extension entitlement).


- **Sound → App routing**: send an app to other output devices or PowerMac virtual devices (only there, or as well as its normal output, with extra gain), with a live status per route. Saved in `audio.appRoutes`.
- **Mixer** page (Displays & Power): app, microphone and loopback sources and device outputs as channel strips with faders, mute/solo and live 20 Hz meters (`audio-watch.json` / `audio-meters.json`, only while the page is open), a send matrix, per-strip parametric EQ with a draggable response curve and a look-ahead limiter, strip states with sample-rate guidance (**Open Audio MIDI Setup**) and the System Audio Recording / Microphone rows. Saved in `audio.mixer`; live edits go through `AudioRequest::SetMixer`.
- The app bundle ships **PowerMac Audio.driver** in `Contents/Resources` (`bundle-macos.sh` runs `build-audio-driver.sh`); it is installed only from Settings → Sound. `NSMicrophoneUsageDescription` for mixer inputs.
- **Battery** is back: Settings → Battery (health, the strategy this Mac allows — charge inhibit, firmware range or limit via discharge — with its trade-offs shown before enabling, limit, sailing, heat protection, discharge, charge to 100 % once, macOS's own Charge Limit with a button to Battery settings, and an explicit **Install helper…**), the tray's Battery line and the `batteryLimit` section. Nothing is installed automatically.
- **Automation** page (Mouse & Keyboard): hotkeys (`keyboardManager.hotkeys`), mouse buttons with every automation action, and **trackpad gestures** (`gestures` section; record a gesture through the app, per-app bindings, warnings for gestures macOS uses too). One action editor for all of them, including shell commands (command or script, interpreter, working folder, environment, background / wait, time limit, notifications, per-action logs with **Open log**) and a **Test** button that shows the outcome and output. Hotkeys join the global shortcut check. Shell actions log to the app's log folder and notify through the app.
- **Network** page (Security & Network; tray → **Network activity…**): live per-app connections with scope badges and a filter, the rules editor (allow / deny / ask, app, remote, ports, protocol, direction, priority, default action) with Little Snitch `.lsrules` import, and an Audit view — with a banner that nothing is blocked.
- `pm_hook::priority::GESTURES` (650) for the gestures click watcher.

- **Setup**: a first-run window (Welcome → Choose features → Permissions → Done; `powermac settings --page welcome`) that turns feature groups on or off and walks through the permissions they need: why each is needed and by which features, live status refreshed every second (from a fresh process, since macOS caches some answers per process), **Open** for exactly the right System Settings pane, **Request…** where macOS has a prompt, guidance for outdated entries ("remove the old PowerMac with −, add PowerMac.app with +") and **Restart PowerMac**, which quits cleanly and starts the bundle again with `open -n`, reopening the window. It opens on first launch and, unless turned off, when a needed permission is missing at startup; tray → **Setup & permissions…**, a **⚠ Needs …** line and a dot on the tray icon while something is missing.
- **Settings → General → Permissions & Health**: the same permission rows, the shared input hook's health (state, re-enables, slow handlers) and one line per module — running, waiting for a permission, didn't start, off — from the new `app-status.json`.
- **Updates**: a daily check of GitHub Releases (`updates.check`, `includePrereleases`), **Update available: vX.Y.Z** in the tray, and a Settings → General card with the release notes and a Download button (opens the browser; nothing installs itself). Handles no releases, rate limits and being offline. `powermac update check`, `powermac --version`.
- **Workspaces** in the app: `workspaces` section in `config.json`, tray → **Workspaces** (launch one, Capture desktop…, Edit workspaces…; a notification reports the result), an editor shortcut (in the global shortcut registry), a Settings → Workspaces page with Launch buttons, and `powermac workspace list|capture|launch|edit`.
- **Mouse Without Borders → Pairing with Windows** card: this Mac's name and IP addresses with copy buttons, the LLMNR responder's status, recent incoming connection attempts, learned addresses, the PC steps with the IP-mapping line filled in, and **Run diagnostics** (`powermac mwb doctor`) in a scrollable box.
- **Disk image**: `scripts/package-dmg.sh` builds `PowerMac-<version>-macos-<arch>.dmg` with an Applications link (optional background with `FANCY=1`), signs it with the local identity, verifies it and prints its SHA-256. The release workflow uses it (`-macos-universal.dmg`). Install docs explain Gatekeeper's **Open Anyway** for the non-notarized builds.
- `powermac permissions [--json]`.

- **Window Switcher**: a "Replace Cmd+Tab" switch at the top of its settings page. Cmd+Tab / Cmd+Shift+Tab open the switcher, holding Cmd keeps it open, releasing switches, Esc cancels and Cmd+Q/W/M/H act on the selected window; Cmd+\` can switch the front app's windows. The macOS app switcher is only off while the switcher's keyboard hook works, and is turned back on when PowerMac stops, the hook fails, Secure Input is on, or (after a crash) on the next start.
- **Window Switcher**: preview of the selected window — its live image at its real position above everything else, with the rest of the screen dimmed; minimized windows and windows on other desktops are shown centered with a badge, and without Screen Recording an outline with the app icon and title.
- **Notch widgets**: the Home view is now a widget platform — pages of widget cards in three sizes, each an instance with its own settings, and kinds that can be added several times. New kinds: Script, xbar plugins, Web / JSON, Shortcuts, Launcher, Bluetooth batteries, Sound, Reminders and World Clocks. Settings → **Notch Widgets** (tray → *Notch widgets…*, `powermac settings --page notch-widgets`) has a widget gallery, a generated settings form with a Test run, and layout export / import.
- **Documentation**: screenshots of every settings page and GIFs of FancyZones Shift+drag, the Window Switcher, Dock Previews, the notch, the settings window, the layout editor and the widget gallery, made by `scripts/docs-media/` from a throwaway demo config.

### Fixed

- **Notch Now Playing** shows the track from any player again on macOS 15.4 and later.

### Removed

- (Earlier in this cycle the battery charge limit was removed; it is back, see above. Config files from either version load.)

## [1.0.0] - 2026-10-26

The first release: PowerToys-style utilities for macOS in one menu-bar app.
Download `PowerMac-1.0.0.dmg`, drag PowerMac to Applications and open it.
Full documentation: https://finepointcgi.github.io/PowerMac/

### Windows and desktop

- **FancyZones**: Focus, Columns, Rows, Grid and Priority Grid layouts plus custom grid and canvas layouts in a layout editor; Shift+drag into zones, span zones, keyboard snapping, quick layout switching, window cycling within a zone, monitor rotation, per-Space layouts, excluded apps. Compatible with PowerToys FancyZones JSON.
- **Edge snapping**: Rectangle-style halves, quarters, thirds and maximize, with a preview.
- **Window Switcher**: Windows-style Alt+Tab over windows (Option+Tab) and over the front app's windows (Option+\`), with live thumbnails; can replace Cmd+Tab.
- **Dock Previews**: window thumbnails when hovering a Dock icon; optional taskbar-style click-to-cycle.
- **Menu Bar Manager**: hide menu bar icons behind a chevron, with an always-hidden section, show on hover, auto-rehide and a floating bar for notched MacBooks.
- **Notch Shelf**: a drop shelf in the notch, plus Home-view widgets (Now Playing, Calendar, Weather, Battery, System Stats, Timer, Quick Toggles, Note, Mirror) and live activities.

### Input

- **Mouse Without Borders**: share keyboard, mouse and clipboard text with PCs running PowerToys, wire-compatible, with automatic LAN discovery.
- **Mouse tuning**: acceleration off and pointer speed for mice and trackpads, per-device overrides, side buttons for Back/Forward and button remapping.
- **Scrolling**: reverse the mouse wheel independently of the trackpad, and smooth scrolling for wheel mice.
- **Mouse Utilities**: Find My Mouse, click highlighter and crosshairs.
- **Keyboard Manager**: key and shortcut remaps (per app), run programs, open URLs, type text; Paste as plain text; Windows-style keys; Hyper key.
- **Text Expander**: snippets with date, time, clipboard and cursor variables; espanso import.

### Hardware

- **Displays**: brightness, contrast and volume of external monitors over DDC/CI (Apple Silicon), native brightness for Apple displays, software dimming for the rest; brightness and volume keys act on the display under the pointer.
- **Display Scaling**: every display mode with a revert-unless-confirmed safety net, HiDPI on non-Retina external displays, custom resolutions.
- **Sound**: default-device priority rules, block automatic switching to chosen devices, keep the built-in mic with Bluetooth headphones, per-app volume (experimental).
- **Battery**: charge limit with sailing, heat protection and discharge-to-limit through a privileged helper.
- **Awake**: keep the Mac awake indefinitely, for a time, until a time or while a process runs.

### Tools

- **Clipboard History** with search, pinning and privacy filters.
- **Color Picker** with PowerToys color formats and an editor.
- **Text Extractor**: OCR any screen region.
- **File Tools**: PowerRename, New+, Image Resizer and Finder basics (Open in Terminal/Editor, Copy Path, hidden files, Eject all, who uses a file), as Finder Quick Actions.

### Known limitations

- macOS only. The Windows code builds but hasn't been run on Windows, so no Windows build is published.
- Mouse Without Borders: no file transfer, drag-and-drop or clipboard images.
- FancyZones zone assignments are lost when PowerMac restarts.
- DDC/CI monitor control needs an Apple Silicon Mac.
- Several modules use private macOS APIs and may need updates for future macOS versions.
