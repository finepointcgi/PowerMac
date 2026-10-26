# Battery charge limit

Keep a MacBook's battery below a set charge to reduce wear, AlDente-style.

Crate: `crates/battery-limit` · Settings page: **Battery** · Config: `batteryLimit` · off by default

## Features

- **Status** (no privileges): percentage, charging state, adapter and wattage, cycle count, health, time remaining and temperature.
- **Charge limit:** stop charging at `limitPercent` (20–100).
- **Sailing:** once at the limit, let the charge drift down `sailingPercent` before charging again.
- **Discharge to limit:** when plugged in above the limit, turn the adapter off so the Mac runs from the battery down to the limit.
- **Heat protection:** pause charging at or above `heatProtectionCelsius` (resumes 2 °C below).
- **Charge to 100 % now:** a one-off top-up from Settings.
- **Sleep safety:** by default the helper re-enables charging and the adapter before sleep, so the Mac is never left in a non-default state unattended (`keepLimitDuringSleep` changes that).

## Settings

| Key | Default |
|---|---|
| `enabled` | false — nothing is ever written while off |
| `limitPercent` | 80 |
| `sailingPercent` | 5 |
| `dischargeToLimit` | false |
| `heatProtectionCelsius` | 35 (0 = off) |
| `keepLimitDuringSleep` | false |
| `pollSeconds` | 30 (5–600) |

## The privileged helper

Writing the SMC needs root, so a small LaunchDaemon does the writes for the app:

| | |
|---|---|
| Label | `com.powermac.battery-helper` |
| Binary | `/Library/PrivilegedHelperTools/com.powermac.battery-helper` |
| Plist | `/Library/LaunchDaemons/com.powermac.battery-helper.plist` |
| Socket | `/var/run/com.powermac.battery-helper.sock` (only the installing user may connect) |
| Log | `/var/log/com.powermac.battery-helper.log` |

Install it from **Settings → Battery** (one admin prompt). Stopping the module hands charging back to the firmware.

## SMC keys

| Purpose | Recent Apple Silicon firmware (macOS 15+) | Older firmware |
|---|---|---|
| Inhibit charging | `CHTE` = `01000000` | `CH0B`/`CH0C` = `02` |
| Adapter off | `CHIE` = `08` | `CH0I` (or `CH0J`) = `01` |

The MagSafe LED key `ACLC` is detected but never written. Intel Macs with only `BCLM` are reported but not controlled.

## How it works

`policy::step` is a pure state machine (limit, sailing, heat, discharge, top-up, sleep) that is fully unit-tested; the controller runs it every `pollSeconds` and sends the result to the helper.

## Windows

Status only. Charge thresholds on Windows laptops are vendor tools (Lenovo Vantage, MyASUS, Dell Power Manager, Surface UEFI), so PowerMac never changes anything there.
