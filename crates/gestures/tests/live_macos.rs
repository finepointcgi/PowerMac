//! Live (read-only) checks against the real MultitouchSupport framework:
//! `cargo test -p gestures --test live_macos -- --ignored`. They only read
//! touches; nothing is posted and no input hook is installed (only swipes
//! are bound).

#![cfg(target_os = "macos")]

use gestures::{Action, Gesture, GestureBinding, Gestures, GesturesConfig, Status, SwipeDir};

#[test]
#[ignore = "needs a Mac with a trackpad"]
fn live_reader_starts_and_stops() {
    let devices = gestures::availability().expect("framework loads");
    assert!(!devices.is_empty(), "no multitouch device");
    let mut cfg = GesturesConfig::default();
    cfg.bindings.push(GestureBinding::new(
        Gesture::Swipe {
            fingers: 5,
            dir: SwipeDir::Up,
        },
        Action::Disabled,
    ));
    let h = Gestures::start(cfg.clone());
    assert_eq!(
        h.status(),
        Status::Off,
        "only a disabled binding: nothing read"
    );
    cfg.bindings[0].action = Action::Text {
        text: "never typed: nobody swipes in a test".into(),
    };
    h.update(cfg.clone());
    match h.status() {
        Status::Running { devices, clicks } => {
            assert!(!devices.is_empty());
            assert_eq!(clicks, None, "no tap/click bound: no input hook");
        }
        s => panic!("{s:?}"),
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    // Off again, then on again (reader restart), then drop.
    cfg.enabled = false;
    h.update(cfg.clone());
    assert_eq!(h.status(), Status::Off);
    cfg.enabled = true;
    h.update(cfg);
    assert!(matches!(h.status(), Status::Running { .. }));
    drop(h);
}
