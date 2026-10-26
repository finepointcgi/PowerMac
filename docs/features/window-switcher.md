# Window Switcher

A Windows-style Alt+Tab for macOS: hold a modifier, press Tab to cycle through **windows** (not apps) in most-recently-used order, release to switch.

Crate: `crates/window-switcher` · Settings page: **Window Switcher** · Config: `windowSwitcher`

## Shortcuts

| Shortcut | Action |
|---|---|
| **Option+Tab** | all windows |
| **Option+\`** | windows of the front app (Alt+\` on Windows) |
| Cmd+Tab | optional: `replaceCmdTab` takes over the system app switcher |

While the switcher is open:

| Key | Action |
|---|---|
| Tab / Shift+Tab (modifier held) | next / previous window |
| arrows, mouse hover | move the selection |
| release the modifier, Return, Space, click | switch to the selected window |
| Esc, click outside | cancel |
| Q / W / M / H / F | quit app / close window / (un)minimize / (un)hide app / full screen |

A tap-and-release quicker than `showDelayMs` (90 ms) switches to the previous window without showing the overlay.

## Settings

| Key | Default | Meaning |
|---|---|---|
| `enabled` | true | master switch |
| `trigger` | Option+Tab | all-windows shortcut (unset on Windows) |
| `appWindowsTrigger` | Option+\` | front-app windows shortcut |
| `replaceCmdTab` | false | also take over Cmd+Tab / Alt+Tab |
| `showMinimized`, `showHidden`, `showOtherSpaces` | true | which windows are listed |
| `onlyCurrentScreen` | false | only windows on the screen with the pointer |
| `thumbnails` | true | live thumbnails (needs Screen Recording; app icons otherwise) |
| `showDelayMs` | 90 | hold time before the overlay appears |
| `excludedApps` | [] | bundle ids, app names or exe names |

## How it works

- Pure logic (MRU order, filtering, key state machine, tile grid) is unit-tested.
- **macOS:** an active `CGEventTap`; windows are enumerated through Accessibility and CGWindowList (other Spaces through the private CGS APIs AltTab uses); thumbnails via ScreenCaptureKit; the overlay is an `NSPanel` with `NSVisualEffectView`.
- **Windows:** low-level hooks, `EnumWindows` with Alt+Tab rules, DWM live thumbnails.
