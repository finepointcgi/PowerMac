# FancyZones and edge snapping

A port of PowerToys FancyZones: define a layout of zones per monitor and drop windows into them. It also adds Rectangle-style edge snapping on macOS.

Crate: `crates/fancyzones` · Settings page: **FancyZones** · Config: `fancyzones.json`

<figure markdown="span">
  ![The FancyZones settings page, with a live preview of the zone colors](../assets/screenshots/settings-fancyzones.png){ loading=lazy }
  <figcaption>The FancyZones settings page, with a live preview of the zone colors.</figcaption>
</figure>

## Layouts

- Templates: **Focus, Columns, Rows, Grid, Priority Grid**, each with adjustable zone count, spacing and sensitivity.
- **Custom grid** and **canvas** layouts built in the layout editor (Ctrl+Option+\`, or tray → FancyZones → Edit layouts…, or `powermac editor`).
- One layout per monitor, optionally one per Space (`layoutsPerVirtualDesktop`).
- Quick switch: bind layouts to 0–9 and press Ctrl+Option+Cmd+0–9.
- Layout files are compatible with PowerToys FancyZones' JSON, so layouts can be imported.

<figure markdown="span">
  ![Picking layouts in the FancyZones layout editor](../assets/gifs/fancyzones-editor.gif){ loading=lazy }
  <figcaption>The layout editor: click a template to apply it to the monitor, and use the pencil to change its zone count and spacing.</figcaption>
</figure>

## Snapping windows

<figure markdown="span">
  ![Shift+dragging windows into zones](../assets/gifs/fancyzones-drag.gif){ loading=lazy }
  <figcaption>Hold Shift while dragging: the zones appear, the one under the pointer lights up, and the window takes its size when you let go.</figcaption>
</figure>

| Action | macOS | Windows |
|---|---|---|
| Show zones while dragging | hold **Shift** | hold Shift |
| Span several zones | **Ctrl** or middle-click while dragging | same |
| Switch layout while dragging | scroll wheel | same |
| Snap focused window to a zone | Ctrl+Option+Arrow | Win+Arrow |
| Extend into the next zone | Ctrl+Option+Cmd+Arrow | Win+Ctrl+Alt+Arrow |
| Next / previous window in zone | Ctrl+Option+] / [ | Win+PgDn / PgUp |
| Apply layout 0–9 | Ctrl+Option+Cmd+0–9 | Win+Ctrl+Alt+0–9 |
| Rotate windows across monitors | hold Ctrl+Option+X, press Left/Right | Alt+X, Left/Right |
| Open the layout editor | Ctrl+Option+\` | Win+Shift+\` |

(The macOS keys avoid Cmd+Shift+\`, Cmd+PgUp/PgDn and Option+X, which macOS already uses.)

## Window handling

- Windows dragged out of a zone get their original size back (`restoreSize`).
- New windows open in their app's last zone (`appLastZoneMoveWindows`).
- When displays or work areas change, or a layout changes, zoned windows are moved back into their zones.
- Excluded apps are never snapped (bundle ids, app names or `.app` names on macOS; exe names on Windows).
- Dialogs and floating panels can be included (`allowSnapChildWindows`, `allowSnapPopupWindows`).
- Zone colors can follow the system accent color (`useSystemAccent`).

## Edge snapping

Drag a window to a screen edge (with zones not showing) to snap it:

| Edge | Result |
|---|---|
| left / right | half |
| top | maximize (configurable) |
| bottom | bottom half or thirds (configurable) |
| corners | quarter |

A preview shows before you release, and dragging the window out restores its size. Settings: `edgeSnapEnabled`, `edgeSnapMargin`, `edgeSnapCornerSize`, `edgeSnapTop`, `edgeSnapBottom`, `edgeSnapGap`. On by default on macOS; off on Windows, which has its own Snap.

!!! warning "Other snappers"
    Quit Rectangle (or turn off its edge snapping) and leave macOS's **Drag windows to screen edges to tile** off; otherwise windows get snapped twice.

## How it works

- `layout`, `navigation`, `engine` and `config` are platform-independent: zone math matches FancyZones' `LayoutConfigurator`, hit-testing `Layout::ZonesFromPoint`, keyboard navigation `WindowKeyboardSnap`.
- **macOS:** a `CGEventTap` thread watches drags and hotkeys; a worker thread moves windows through the Accessibility API; the zone overlay is an AppKit window updated on the main thread. Display, work-area and Space changes are polled once a second (Spaces via the private `CGSManagedDisplayGetCurrentSpace`). New windows are seen through an `AXObserver` per app.
- **Windows:** one thread owns a WinEvent hook, a `WH_KEYBOARD_LL` hook and layered GDI overlay windows.

## Limitations

- macOS has no API to make another app's window transparent, so `makeDraggedWindowTransparent` works on Windows only.
- Zone assignments are kept in memory, so they're lost when PowerMac restarts.
