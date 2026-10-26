# Mouse tuning

"My mouse feels awful on a Mac" fixes, in the spirit of LinearMouse and SensibleSideButtons.

Crate: `crates/mouse-tuning` · Settings page: **Mouse** · Config: `mouseTuning`

## Pointer

- Turn pointer **acceleration off** (linear movement) and set the **speed**, separately for mice and trackpads.
- Per-device overrides by vendor/product id (macOS).
- The original values are read first and restored when the module stops.

| Key | Meaning |
|---|---|
| `mouse.disableAcceleration`, `mouse.speed` | mice (on Windows: every pointing device) |
| `trackpad.disableAcceleration`, `trackpad.speed` | trackpads (macOS) |
| `devices` | per-device overrides |

`speed` is 0.0–1.0 and unset leaves the system speed alone. With acceleration off on macOS it is a linear multiplier (0.25×–4×, 0.5 = the device's native resolution); with acceleration on it is the System Settings tracking speed.

## Buttons

- Side buttons go **Back / Forward** on macOS (`buttons.sideButtonsNavigation`, on by default), sent as Cmd+[ and Cmd+].
- The middle and side buttons can be mapped (`buttons.mappings`) to keystrokes, Mission Control, App Exposé, Launchpad, Show Desktop, copy/paste and more. Left and right buttons are never remapped.

!!! tip "Keyboard layouts"
    Cmd+[ assumes `[` is on the US key position. On layouts that move it (German, …) map the side buttons to an explicit keystroke instead.

## How it works

- **macOS:** pointer settings go through the HID event system client per HID service, re-applied periodically so new devices are covered and values the system pushes back (wake, System Settings) are overridden. Changes live in memory: if PowerMac dies without stopping, they last until the device reconnects. Buttons use an active `CGEventTap` on other-mouse events (Accessibility). Mission Control and friends go through the Dock.
- **Windows:** `SPI_SETMOUSE` (acceleration) and `SPI_SETMOUSESPEED` (1–20), session-only and restored on stop. Only the `mouse` settings apply. Button mappings use a `WH_MOUSE_LL` hook.

Smooth scrolling lives in [Scrolling](scrolling.md).
