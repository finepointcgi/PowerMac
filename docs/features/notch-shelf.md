# Notch Shelf and widgets

Turns the MacBook notch into a drop shelf and a small dashboard, in the spirit of NotchNook, Dropover and Yoink.

Crate: `crates/notch-shelf` · Settings page: **Notch Shelf** · Config: `notchShelf`

## The shelf

- Collapsed, the panel is a black shape exactly over the notch, so it's invisible.
- It opens when the pointer rests on the notch, when you click it, or when you drag files, links, text or images near the top of the screen.
- Dropped items become tiles with Quick Look thumbnails. Drag them out to any app, open them, reveal them in Finder, share them (AirDrop, …) or remove them.
- Files dropped from temporary locations (screenshot previews, `/var/folders`, caches) are copied into the shelf's own folder so they survive.
- The shelf is saved in the data folder and survives restarts.
- Macs without a notch can get a "virtual notch" pill at the top center (`virtualNotchOnNonNotchScreens`).

## Home view widgets

When at least one widget is on, the notch gets a Home view next to the shelf. The order in `widgets` is the display order.

| Widget | Notes |
|---|---|
| Now Playing | play/pause/next/previous; title and artwork from the system, or from Music/Spotify via AppleScript when `nowPlaying.useAppleScript` is on (macOS 15.4+ restricts the system data) |
| Calendar | upcoming events (`calendar.days`, `calendar.calendars`); needs Calendars access |
| Weather | `weather.location` as a city or `lat, lon`, looked up with Open-Meteo; `weather.units` |
| Battery | level and charging state |
| System Stats | CPU load and network rates with sparklines |
| Timer | timer and stopwatch |
| Quick Toggles | Awake on/off, Pick color, Extract text, Clipboard history, Window switcher, Lock screen, Show desktop |
| Note | a scratchpad |
| Mirror | camera preview; needs Camera access |

`liveActivities` shows music and timer activity beside the collapsed notch. `batteryIndicator` briefly widens the notch with the battery level when the charger is connected or disconnected. `showClipboard` adds the current clipboard item as the first tile (may trigger macOS's paste prompt).

## Settings

| Key | Default |
|---|---|
| `enabled` | true on Macs with a notch |
| `expandOnHover`, `hoverDelayMs`, `collapseDelayMs` | true, 150, 350 |
| `expandOnDrag`, `expandOnClick` | true |
| `screens` | notch only |
| `copyTemporaryFiles` | true |
| `removeAfterDragOut` | false |
| `haptics` | true |
| `panelWidth` | 620 |
| `defaultView` | last used |

## Platforms

- **macOS:** a borderless, non-activating `NSPanel` above the menu bar on every Space and over full-screen apps, drawn with Core Animation.
- **Windows:** a top-center layered window that slides down when files are dragged to the top center of the primary monitor (OLE drop target and drag source).
