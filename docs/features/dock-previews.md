# Dock Previews

Hover an app's Dock icon for a moment and a panel with thumbnails of that app's windows appears above it (beside it for a Dock on the left or right).

Crate: `crates/dock-previews` · Settings page: **Dock Previews** · Config: `dockPreviews` · macOS only

<figure markdown="span">
  ![The Dock Previews settings page](../assets/screenshots/settings-dock-previews.png){ loading=lazy }
  <figcaption>The Dock Previews settings page.</figcaption>
</figure>

## Using it

| In the panel | Action |
|---|---|
| click a tile | focus that window (un-minimizing or switching Space as needed) |
| hover a tile | close (red) and minimize (yellow) buttons appear |
| W / M while hovering | close / (un)minimize that window |
| middle-click a tile | close that window |
| scroll over the panel or icon | cycle through the windows, raising each |
| Esc, click elsewhere, leave | close the panel |

With `clickToCycle` on, clicking the Dock icon of the app that is already in front minimizes its window (one window) or cycles through its windows (several), like the Windows taskbar.

## Settings

| Key | Default | Meaning |
|---|---|---|
| `enabled` | true | master switch |
| `hoverDelayMs` | 300 | rest time before the panel appears (0–2000) |
| `previewSize` | medium | thumbnail size |
| `showSingleWindow` | true | also for apps with one window |
| `clickToCycle` | false | taskbar-style clicks on the front app's icon |

## Permissions

Accessibility (reading the Dock, focusing windows; without it nothing is shown and it retries every 2 s). Screen Recording for thumbnails; without it tiles show the app icon and window titles.

## How it works

An active `CGEventTap` that only swallows events aimed at the panel (and Dock clicks when click-to-cycle applies); a worker thread reads the Dock's Accessibility tree, tracks the pointer at ~30 Hz near the Dock and captures thumbnails with ScreenCaptureKit; the panel is an `NSPanel` on the main thread. On Windows (which has taskbar thumbnails) the module does nothing.
