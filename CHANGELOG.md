# Changelog

All notable changes to PowerMac are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

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

- **Battery** charge limit (and its privileged helper). Old config files still load; the section is ignored. The Notch Shelf's Battery widget stays.

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
