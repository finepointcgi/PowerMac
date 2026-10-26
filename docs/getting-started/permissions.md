# Permissions (macOS)

Many modules watch or change global input, move other apps' windows or capture the screen, which macOS only allows after you grant a permission in **System Settings → Privacy & Security**. PowerMac asks for nothing it doesn't use: a permission matters only if a module that needs it is on.

| Permission | Needed by |
|---|---|
| **Accessibility** | Mouse Without Borders, FancyZones, Scrolling, Mouse Utilities, Keyboard Manager, Text Expander, Window Switcher, Dock Previews, Menu Bar Manager, Clipboard History's hotkey, mouse button mappings, Displays' brightness/volume keys |
| **Input Monitoring** | Mouse Without Borders, FancyZones, Mouse Utilities, Keyboard Manager |
| **Screen Recording** | Text Extractor (screen capture), Window Switcher and Dock Previews thumbnails, Menu Bar Manager's floating bar on macOS 15 and older. Without it these fall back to app icons; OCR of files and clipboard images still works. |
| **Automation (Finder)** | Finder basics acting on the front Finder window without a selection (opt-in). |
| **Automation (Music, Spotify)** | Notch Shelf's Now Playing widget when `useAppleScript` is on (opt-in). |
| **Audio capture** | Sound's per-app volume (experimental, macOS 14.4+). |
| **Calendars** | Notch Shelf's Calendar widget. |
| **Reminders** | Notch Shelf's Reminders widget. |
| **Camera** | Notch Shelf's Mirror widget. |
| **Administrator password (once)** | Installing a display override plist. |
| none | Color Picker, Notch Shelf's shelf, Displays sliders, pointer speed/acceleration, File Tools, Awake |

## Where to see the status

- **Settings → General** lists each permission with its state and a button that opens the matching System Settings pane. Each module's page shows the permissions it needs as well.
- `powermac doctor` prints the same; `powermac doctor --prompt` asks macOS to show the prompts for missing ones.

## After rebuilding

Grants are tied to the code signature. With an ad-hoc signed build you may need to remove PowerMac from the lists and add it again after each rebuild. Run `scripts/create-signing-cert.sh` once to avoid that (see [Install](install.md#keeping-permissions-across-rebuilds)).

## Windows

Windows needs no grants for low-level hooks. Note that hooks don't receive input aimed at elevated (administrator) windows unless PowerMac itself runs elevated.
