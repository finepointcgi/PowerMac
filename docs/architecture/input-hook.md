# Global input hook

Every module that watches the keyboard or mouse subscribes to **one** global hook owned by the `pm-hook` crate, instead of installing its own:

| Platform | The hook |
|---|---|
| macOS | One active `CGEventTap` at `kCGHIDEventTap` (falling back to `kCGSessionEventTap`), head-inserted, on the `pm-hook` thread. Its event mask is the union of the subscribers' masks (keys, flagsChanged, every mouse button and drag, moves, scroll, `NSSystemDefined` media keys) and it is recreated when that union changes. With no subscribers there is no tap. |
| Windows | One `WH_KEYBOARD_LL` and one `WH_MOUSE_LL` hook (each only while someone wants that kind of event) on the `pm-hook` thread, which runs a message loop. |

Before, about ten modules each created a tap (or a pair of LL hooks) on their own thread: every event woke all of them, their order depended on which module had been (re)started last, and each could be disabled by a timeout on its own.

```rust
let guard = pm_hook::subscribe(
    pm_hook::Subscription::new("keyboard-manager", pm_hook::priority::KEYBOARD_MANAGER, mask, |ev| {
        // ev.kind(), ev.source_tag(), ev.cg_event() / ev.keyboard() / ev.mouse()
        pm_hook::Verdict::Pass
    })
    .ignore_tags(&[pm_hook::tags::MWB])
    .on_status(|st| { /* tap created / disabled / re-enabled, Secure Input */ }),
);
drop(guard); // unsubscribe
```

## Dispatch

Subscribers run in descending priority on the tap thread, each only for the event types in its mask and not for events whose injection tag it ignores. A handler returns:

| Verdict | Effect |
|---|---|
| `Pass` | next subscriber |
| `Modified` | the native event was edited in place (macOS); later subscribers and the OS see the edit. Windows can't edit LL events: same as `Pass` |
| `Swallow` | dropped; later subscribers never see it |
| `Replace(emits)` | replaced by `emits` in order; `Emit::Original` stands for the event itself. On macOS each emitted event is first dispatched to the **lower**-priority subscribers, then posted right after the tap, exactly like an event posted with `CGEventTapPostEvent` reached the taps behind the posting one. On Windows the event is swallowed and the replacements are `SendInput`ed (they come back through the hook as injected events, as with separate hooks). |

A panicking handler counts as `Pass`. Handlers may subscribe and unsubscribe from inside a dispatch (it works on a snapshot); dropping a guard from another thread waits for a call in progress, after which the handler is never called again.

## Priorities

The order is what the separate taps did at a fresh start of the app (the only deterministic case before): on macOS every `kCGHIDEventTap` tap (Keyboard Manager, Scroll Reverser, Mouse Without Borders, mouse buttons) ran before every `kCGSessionEventTap` tap, and within each group the module started last ran first. The intent behind that order:

1. **Transforms** turn physical input into what the user meant (key remaps, Hyper key, wheel direction). Every later consumer, and a remote machine, sees the result — as PowerToys' Keyboard Manager output is what FancyZones reacts to.
2. **Routing:** Mouse Without Borders swallows everything while it controls another machine, so nothing local fires.
3. **Local consumers** act on shortcuts, clicks and the pointer and swallow only what they act on.

| Priority | Subscriber | Events | Why here |
|---:|---|---|---|
| 1000 | color-picker (Windows, while picking) | mouse, keys | modal; its hooks used to be installed last, i.e. ran first |
| 900 | keyboard-manager | keys, flagsChanged, buttons, drags, scroll | remaps / Hyper / modifier-flag rewriting before anyone matches a shortcut |
| 850 | scroll-reverser | scroll | direction and smoothing before routing and before FancyZones' layout wheel |
| 800 | mwb (`pm-input` capture) | keys, buttons, moves, scroll | swallows all input while controlling a remote machine |
| 700 | mouse-tuning | other-mouse buttons | button actions are local; a remote machine gets the real button |
| 600 | text-expander | key down, clicks | sees the remapped keys; its typed replacement reaches the subscribers below |
| 550 | dock-previews | moves, buttons, scroll, key down | its panel's clicks and keys |
| 500 | window-switcher | keys, flagsChanged, moves, left button | Option+Tab, Cmd+Tab take-over |
| 450 | clipboard-history (macOS) | keys | Cmd+Shift+V |
| 400 | display-control | keys, `NSSystemDefined` | brightness / volume keys |
| 300 | fancyzones | buttons, drag, scroll, keys, flagsChanged | drag, hotkeys, layout wheel |
| 200 | mouse-utils | moves, buttons, keys, flagsChanged | observes almost everything, swallows only its hotkeys |

Ties go to the earlier subscription. The constants are in `pm_hook::priority`.

## Injection tags

All tags live in `pm_hook::tags` (the crates' old constants re-export them): `MWB` "PMMW" (`pm-input`), `KEYBOARD_MANAGER` "PMKM", `TEXT_EXPANDER` "PMTE", `SCROLL_REVERSER` "SRVS", `MOUSE_TUNING` "MTUN". `Event::source_tag()` is `kCGEventSourceUserData` on macOS and the `dwExtraInfo` of an injected event on Windows. Each subscriber skips exactly what its own tap skipped before (Keyboard Manager: its own, MWB's and the text expander's; FancyZones and display keys on macOS: nothing; most others: MWB's).

## Health

- `pm_hook::status()` / `Subscription::on_status`: `Idle`, `Starting`, `Active`, `ListenOnly`, `Disabled`, `Unavailable(reason)`, plus Secure Event Input and a re-enable counter.
- A tap disabled by timeout or user input is re-enabled in the callback; a watchdog checks it every 0.5 s (and `pm_hook::is_healthy()` on demand), re-enabling a tap disabled without notice.
- Without Accessibility the tap is retried every 2 s (Window Switcher and Dock Previews used to do this themselves). If only a listen-only tap is possible, only subscribers that opted in (`allow_listen_only`, Mouse Utilities) are called.
- Window Switcher's Cmd+Tab take-over follows this status: the system switcher comes back whenever the tap is missing, disabled or blind (Secure Input).
- Handlers slower than 10 ms are logged (rate-limited) and counted (`pm_hook::stats()`); slow work belongs on a `pm_hook::Worker`. On Windows a dispatch slower than ~0.9 s reinstalls the hooks (Windows removes a hook that times out).
- `pm_hook::win::Forwarder` runs a handler synchronously on a module's own GUI thread (Window Switcher, FancyZones, Color Picker on Windows), as their own LL hooks used to.

## Measured (macOS, M4 Pro, 10 000 synthetic mouse moves at 4 kHz)

| Arrangement | Process CPU per event | Of which hooks | Post → end of chain |
|---|---|---|---|
| No modules (posting + measuring tap only) | 27 µs | — | 1.1–1.8 ms |
| Old: one tap per module (7 modules) | 112–140 µs | ~85–113 µs | 10–12.5 ms |
| New: one shared tap (same 7 modules) | 46–47 µs | ~20 µs | 3.5–6.7 ms |
