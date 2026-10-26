# Menu Bar Manager

Hide menu bar icons behind a chevron, Ice / Bartender style. Useful when there are too many icons, and on notched MacBooks where some end up behind the camera housing.

Crate: `crates/menubar-manager` · Settings page: **Menu Bar** · Config: `menuBar` · macOS only · off by default

## How the menu bar is split

PowerMac adds three status items of its own. Left to right:

```text
[always-hidden items] ┆ [hidden items] │ ‹  [visible items] [system items]
                      │                │ └ chevron (click to show / hide)
                      │                └ hidden-section separator
                      └ always-hidden separator (optional)
```

Cmd-drag any icon across the separators to choose its section (macOS allows this for every status item); the settings page can move items for you. The arrangement survives restarts.

- Click the chevron to show or hide the hidden section; Option-click (or right-click) shows the always-hidden one too.
- Optionally show the hidden section when the pointer rests on the empty part of the menu bar.
- Hide again after N seconds without menu bar activity, and/or when another app becomes active.
- A floating bar below the menu bar can show the hidden items instead of expanding in place — always, or only when expanding would put items behind the notch.

## Settings

| Key | Default | Meaning |
|---|---|---|
| `enabled` | false | opt-in |
| `autoRehide`, `rehideDelaySecs` | true, 10 | hide again after inactivity |
| `rehideOnFocusChange` | true | hide when another app becomes active |
| `showOnHover`, `hoverDelayMs` | false, 300 | show on hover over the empty menu bar |
| `useFloatingBar` | false | always use the floating bar |
| `notchFallbackToBar` | true | use the floating bar when items would be behind the notch |
| `alwaysHiddenSection` | true | add the second separator |
| `animate`, `showSeparators` | true | visuals |

## How it works

Hiding a section makes its separator very long, which pushes every item to its left out of the menu bar; showing makes it short again. On macOS 26 and later the menu bar removes items that don't fit instead of pushing them off-screen, so the separator is made as long as still fits (found by bisection on the menu bar's layout), and other apps' items are listed through Accessibility (`AXExtrasMenuBar`) instead of the window list. On Windows the notification area already has an overflow flyout, so the module is a no-op.
