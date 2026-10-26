# Scrolling

Reverse the mouse wheel but keep natural scrolling on the trackpad (or any other combination), plus **smooth scrolling** for mouse wheels.

Crate: `crates/scroll-reverser` · Settings page: **Scrolling** · Config: `scrollReverser`

## Reversal

Four independent switches: `reverseMouseVertical`, `reverseMouseHorizontal`, `reverseTrackpadVertical`, `reverseTrackpadHorizontal`.

Reversal is relative to the system setting. With macOS natural scrolling on (the default), the default config gives a natural trackpad and a classic mouse wheel. With natural scrolling off, flip the flags.

The Magic Mouse sends continuous, phased events exactly like a trackpad, so it counts as a trackpad.

## Smooth scrolling

Off by default (`smoothScrolling`). Each wheel notch becomes an eased pixel glide, in the spirit of Mos. Trackpads, the Magic Mouse and mice with smooth drivers are untouched. While Shift, Control, Option or Command is held the wheel passes through unsmoothed, so zoom and horizontal-scroll modifiers keep working.

| Key | Default | Meaning |
|---|---|---|
| `smoothStepPx` | 60 | distance of one notch |
| `smoothDurationMs` | 250 | glide length |
| `smoothAcceleration` | 100 | % strength on fast spins (0 = off; 100 ≈ up to 4× per notch) |
| `smoothEasing` | `easeOutCubic` | `easeOutCubic`, `easeOutQuint`, `easeOutQuad`, … |
| `smoothHorizontal` | true | smooth tilt-wheel notches too |

## How it works

- **macOS:** an active `CGEventTap` (HID level, falling back to session level) negates scroll fields in place. Smoothing swallows the notch and replays it as continuous pixel events from a frame thread, posted at the same tap location so the system's natural flip still applies.
- **Windows:** a `WH_MOUSE_LL` hook. Hooks can't modify events, so reversed events are swallowed and re-injected with `SendInput`. Hooks carry no device info, so by default every wheel event counts as a mouse; `windowsTouchpadHeuristic` treats non-multiple-of-120 deltas as touchpad input (some high-res mice do that too).
- Events injected by Mouse Without Borders and by the module itself are always left alone.
