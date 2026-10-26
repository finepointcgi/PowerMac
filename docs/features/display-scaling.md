# Display Scaling (HiDPI)

Fix "my 1440p external monitor looks blurry or tiny", BetterDisplay-style.

Crate: `crates/display-scaling` · Settings page: **Displays** · Config: `displayScaling` · off by default

## Features

- **Display modes:** every mode of every display, including HiDPI and the hidden duplicate low-resolution ones. Switching uses a safety net: the change is reverted after `revertTimeoutSecs` (5–120) unless you confirm it. A confirmed mode is saved as the display's `preferredMode` and comes back on the next connect.
- **HiDPI on non-Retina external displays** (macOS 11+): a virtual display (private `CGVirtualDisplay` API) offers HiDPI modes — e.g. 1920×1080 HiDPI is a 3840×2160 backing store on a 1440p panel — and the physical display mirrors it, so the panel shows the downsampled, crisp image.
- **Custom resolutions:** extra logical sizes added to the virtual display's HiDPI modes.
- **Display override plist** (alternative): generate and install an override plist with an admin password; takes effect after reconnect or restart.

## Per-display settings

Keyed by stable display id (`"<name>#<serial>"`, or `"<name>#v<vendor>m<model>"`). A key `"v<vendor>m<model>"` matches every display of that model.

| Key | Meaning |
|---|---|
| `hidpiEnabled` | HiDPI through a mirrored virtual display (macOS, non-built-in) |
| `targetLogicalResolution` | logical size while HiDPI is on (default: the panel's own size at 2×) |
| `customResolutions` | extra HiDPI sizes |
| `preferredMode` | mode applied whenever the display connects |

## Safety

The virtual display lives as long as the module. What was changed is persisted, so after a crash it is cleaned up on the next start. Stopping the module removes virtual displays and restores modes.

## Windows

Modes via `EnumDisplaySettingsEx`, switching via `ChangeDisplaySettingsEx` (tested with `CDS_TEST` first) with the same revert flow. Per-monitor DPI is shown read-only, since Windows scales natively.
