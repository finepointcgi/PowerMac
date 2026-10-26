# Displays (brightness and volume)

Brightness, contrast and volume of external monitors over **DDC/CI**, native brightness of built-in and Apple displays, and software dimming for everything else — like MonitorControl or PowerToys PowerDisplay.

Crate: `crates/display-control` · Settings page: **Displays** · Config: `displayControl`

## Using it

- **Settings → Displays** has sliders for every connected display; the tray has presets per external display.
- The **brightness keys** act on the display under the pointer, with an on-screen level (`takeOverBrightnessKeys`, on by default on macOS). Keys pressed on the built-in display, or on a display that can't be controlled, are left to macOS.
- The **volume / mute keys** can do the same for monitors with DDC volume (`takeOverVolumeKeys`, off by default).
- Option+Shift with a key uses a quarter step, like macOS.

## Settings

| Key | Default | Meaning |
|---|---|---|
| `enabled` | true | master switch |
| `takeOverBrightnessKeys` | true (macOS) | brightness keys act on external displays |
| `takeOverVolumeKeys` | false | volume keys act on DDC monitors |
| `step` | 6 | percent per key press (1–50) |
| `showOsd` | true | on-screen level indicator |
| `softwareDimFallback` | true | gamma dimming for displays with no hardware control |
| `hotkeys` | | optional shortcuts |
| `displays` | {} | per-display overrides, keyed by stable display id |

## How it works

- **macOS (Apple Silicon):** DDC/CI through the private `IOAVService` I²C API, each service matched to its CoreGraphics display via `IODisplayLocation` / EDID. Built-in and Apple-made displays (Studio Display, Pro Display XDR, LG UltraFine) use the private DisplayServices framework. Gamma tables are restored when the module stops.
- **Windows:** Dxva2 (`Get/SetVCPFeature`) for external monitors, the `\\.\LCD` IOCTLs for laptop panels, `SetDeviceGammaRamp` for dimming.
- A worker thread owns the hardware; writes are coalesced and rate-limited, so sliders don't flood the monitor.
- The settings window never talks to the monitors itself; it reads `display-status.json` and writes `display-set.json` (see [IPC](../architecture/ipc.md)).
- [Display Scaling](display-scaling.md)'s virtual displays are skipped.

## Limitations

DDC/CI is implemented for Apple Silicon only; on Intel Macs native and software brightness work. Some monitors don't implement DDC/CI or only over certain inputs.
