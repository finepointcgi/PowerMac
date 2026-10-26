# Mouse Utilities

Ports of PowerToys' Find My Mouse, Mouse Highlighter and Mouse Pointer Crosshairs.

Crate: `crates/mouse-utils` · Settings page: **Mouse Utilities** · Config: `mouseUtils`

| Tool | Default trigger (macOS) | Windows |
|---|---|---|
| **Find My Mouse** — dims the screen except for a spotlight around the pointer | double-tap **Left Ctrl** (or shake, or a shortcut, Cmd+Shift+F) | same / Win+Shift+F |
| **Mouse Highlighter** — colored circles at clicks | Cmd+Shift+H | Win+Shift+H |
| **Crosshairs** — lines through the pointer across the screen | Cmd+Option+P | Win+Alt+P |

## Settings

- `findMyMouse`: `activationMethod`, `hotkey`, `spotlightRadius`, `backdropColor`, `overlayOpacity`, `animationDurationMs`, `initialZoom`, `idleTimeoutMs`, shake detection (`shakeMinimumDistance`, `shakeIntervalMs`, `shakeFactor`).
- `highlighter`: `hotkey`, `autoActivate`, `leftButtonColor`, `rightButtonColor`, `highlightCursor`, `alwaysColor`, `radius`, `fadeDelayMs`, `fadeDurationMs`.
- `crosshairs`: `hotkey`, `autoActivate`, `color`, `borderColor`, `opacity`, `centerRadius`, `thickness`, `borderSize`, `orientation`, `fixedLength`.

Each tool has its own `enabled`.

## How it works

The engine (double-Ctrl and shake detection, hotkeys, animation timing) is platform-independent and produces a scene each frame. On macOS a `CGEventTap` feeds it and one click-through `NSWindow` per screen draws with Core Animation, at 60 Hz only while something is visible. On Windows, layered topmost windows are drawn by a software rasterizer.
