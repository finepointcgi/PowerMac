# Sound

Default-device rules, device volume and mute, and per-app volume.

Crate: `crates/audio-control` · Settings page: **Sound** · Config: `audio`

## Features

- **Devices:** list outputs and inputs with name, transport, volume and mute; set the default output, input and alert device from Settings or the tray.
- **Priority rules:** a ranked list of preferred outputs and inputs. When devices appear or disappear, the highest-ranked available one becomes the default.
- **Never auto-switch to:** devices (AirPods, …) the OS may not switch to on its own. If it does, PowerMac reverts it within ~50 ms. Choosing the device yourself still works.
- **Keep the built-in mic with Bluetooth headphones**, so they stay in high-quality (A2DP) mode.
- **Per-app volume:** macOS 14.4+ through Core Audio process taps (experimental, off by default, needs the audio capture permission). Windows uses audio sessions natively. Levels are saved in `appVolumes`, keyed by bundle id or exe name.

## Settings

| Key | Default |
|---|---|
| `enabled` | true |
| `rules` | empty |
| `blockAutoSwitchTo` | [] |
| `keepBuiltinMicWithBluetooth` | false |
| `perAppVolume` | false |
| `appVolumes` | {} |

## How it works

macOS: hand-bound CoreAudio. Windows: the MMDevice API, `IPolicyConfig` and audio sessions. The settings window reads `audio-status.json` (every 2 s) and writes `audio-request.json` (polled every 150 ms). Per-app taps are removed when the module stops.
