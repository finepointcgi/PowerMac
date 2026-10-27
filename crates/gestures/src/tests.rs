use super::*;

#[test]
fn gesture_names_round_trip() {
    for g in Gesture::all() {
        let s = g.to_string();
        assert_eq!(s.parse::<Gesture>().unwrap(), g, "{s}");
        assert!(g.is_valid());
        assert!(!g.label().is_empty());
        let j = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<Gesture>(&j).unwrap(), g);
    }
    assert_eq!(Gesture::all().len(), 12 + 3 + 3 + 8);
    assert_eq!(
        Gesture::Swipe {
            fingers: 3,
            dir: SwipeDir::Left
        }
        .to_string(),
        "3-finger-swipe-left"
    );
    assert_eq!(
        Gesture::Pinch {
            fingers: 4,
            dir: PinchDir::In
        }
        .to_string(),
        "4-finger-pinch-in"
    );
    for (s, g) in [
        (
            "swipe 3 left",
            Gesture::Swipe {
                fingers: 3,
                dir: SwipeDir::Left,
            },
        ),
        ("Three_Finger_Tap", Gesture::Tap { fingers: 3 }),
        (
            "4fingers-swipe-UP",
            Gesture::Swipe {
                fingers: 4,
                dir: SwipeDir::Up,
            },
        ),
        ("click5", Gesture::Click { fingers: 5 }),
        (
            "two finger pinch out",
            Gesture::Pinch {
                fingers: 2,
                dir: PinchDir::Out,
            },
        ),
    ] {
        assert_eq!(s.parse::<Gesture>().unwrap(), g, "{s}");
    }
    for bad in [
        "",
        "3-finger-swipe",
        "swipe-left",
        "2-finger-swipe-left",
        "2-finger-tap",
        "6-finger-click",
        "1-finger-pinch-in",
        "3-finger-tap-left",
        "3-finger-wave",
        "3-finger-swipe-in",
    ] {
        assert!(bad.parse::<Gesture>().is_err(), "{bad}");
    }
}

#[test]
fn config_defaults_bind_nothing() {
    let c = GesturesConfig::default();
    assert!(c.enabled && c.bindings.is_empty() && c.swallow_clicks);
    assert!(!c.needs_reader() && !c.needs_click_hook());
    assert_eq!(GesturesConfig::from_json("{}").unwrap(), c);
    assert_eq!(GesturesConfig::from_json(&c.to_json()).unwrap(), c);
    assert!(c.resolve(Gesture::Tap { fingers: 3 }, &[]).is_none());
}

#[test]
fn config_round_trip_and_needs() {
    let json = r#"{
      "bindings": [
        {"gesture": "3-finger-tap", "action": {"type": "shortcut", "keys": "Cmd+W"}},
        {"gesture": "4-finger-swipe-down", "action": {"type": "shell", "command": "pmset displaysleepnow", "notify": "onFailure"}},
        {"gesture": "3-finger-click", "app": "com.google.Chrome", "action": {"type": "shortcut", "keys": "Cmd+Shift+T"}},
        {"gesture": "4-finger-pinch-out", "enabled": false, "action": {"type": "openUri", "uri": "https://example.com"}}
      ],
      "thresholds": {"swipeDistance": 0.2}
    }"#;
    let c = GesturesConfig::from_json(json).unwrap();
    assert_eq!(c.bindings.len(), 4);
    assert!(!c.bindings[3].enabled);
    assert_eq!(c.thresholds.swipe_distance, 0.2);
    assert_eq!(c.thresholds.tap_max_ms, Thresholds::default().tap_max_ms);
    assert!(c.needs_reader() && c.needs_click_hook());
    assert_eq!(c.click_bindings(), (1 << 3, true));
    assert!(c.validate().is_empty(), "{:?}", c.validate());
    let back = GesturesConfig::from_json(&c.to_json()).unwrap();
    assert_eq!(back, c);
    let v: serde_json::Value = serde_json::from_str(&c.to_json()).unwrap();
    assert_eq!(v["bindings"][0]["gesture"], "3-finger-tap");
    assert!(
        v["bindings"][0].get("enabled").is_none(),
        "enabled: true is implied"
    );
    assert_eq!(v["bindings"][3]["enabled"], false);
    assert_eq!(v["swallowClicks"], true);
    // Disabled binding never resolves; the master switch turns all off.
    assert!(c
        .resolve(
            Gesture::Pinch {
                fingers: 4,
                dir: PinchDir::Out
            },
            &[]
        )
        .is_none());
    let off = GesturesConfig {
        enabled: false,
        ..c.clone()
    };
    assert!(!off.needs_reader() && off.resolve(Gesture::Tap { fingers: 3 }, &[]).is_none());
    // Only swipes bound: no click hook.
    let swipes = GesturesConfig {
        bindings: vec![c.bindings[1].clone()],
        ..Default::default()
    };
    assert!(swipes.needs_reader() && !swipes.needs_click_hook());
    // Bad entries.
    assert!(GesturesConfig::from_json(
        r#"{"bindings":[{"gesture":"2-finger-tap","action":{"type":"disabled"}}]}"#
    )
    .is_err());
    assert!(GesturesConfig::from_json(r#"{"bindings":[{"gesture":"3-finger-tap"}]}"#).is_err());
}

#[test]
fn binding_resolution_with_app_scoping() {
    let t3 = Gesture::Tap { fingers: 3 };
    let txt = |s: &str| Action::Text { text: s.into() };
    let c = GesturesConfig {
        bindings: vec![
            GestureBinding {
                app: Some("com.apple.Safari".into()),
                ..GestureBinding::new(t3, txt("safari"))
            },
            GestureBinding::new(t3, txt("global")),
            GestureBinding::new(t3, txt("second global (ignored)")),
            GestureBinding {
                app: Some("Terminal.app".into()),
                ..GestureBinding::new(t3, Action::Disabled)
            },
            GestureBinding {
                app: Some("code.exe".into()),
                ..GestureBinding::new(t3, txt("vscode"))
            },
            GestureBinding {
                app: Some("com.apple.Notes".into()),
                enabled: false,
                ..GestureBinding::new(t3, txt("notes off"))
            },
        ],
        ..Default::default()
    };
    let front = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        c.resolve(t3, &front(&["com.apple.Safari", "Safari"])),
        Some(&txt("safari"))
    );
    assert_eq!(
        c.resolve(t3, &front(&["COM.APPLE.SAFARI"])),
        Some(&txt("safari"))
    );
    assert_eq!(
        c.resolve(t3, &front(&["com.apple.finder", "Finder"])),
        Some(&txt("global"))
    );
    assert_eq!(c.resolve(t3, &front(&[])), Some(&txt("global")));
    assert_eq!(
        c.resolve(t3, &front(&["com.apple.Terminal", "Terminal"])),
        Some(&Action::Disabled)
    );
    assert_eq!(c.resolve(t3, &front(&["Code.exe"])), Some(&txt("vscode")));
    assert_eq!(
        c.resolve(t3, &front(&["com.apple.Notes", "Notes"])),
        Some(&txt("global")),
        "disabled binding skipped"
    );
    assert_eq!(
        c.resolve(Gesture::Tap { fingers: 4 }, &front(&["Safari"])),
        None
    );
    assert_eq!(c.validate().len(), 1, "{:?}", c.validate()); // the duplicate global
                                                             // App-only binding: nothing elsewhere.
    let only = GesturesConfig {
        bindings: vec![c.bindings[0].clone()],
        ..Default::default()
    };
    assert_eq!(only.resolve(t3, &front(&["Finder"])), None);
}

#[test]
fn conflict_hints() {
    assert!(Gesture::Swipe {
        fingers: 3,
        dir: SwipeDir::Up
    }
    .may_conflict_with_macos());
    assert!(!Gesture::Swipe {
        fingers: 5,
        dir: SwipeDir::Up
    }
    .may_conflict_with_macos());
    assert!(!Gesture::Click { fingers: 3 }.may_conflict_with_macos());
    assert!(Gesture::Pinch {
        fingers: 4,
        dir: PinchDir::In
    }
    .may_conflict_with_macos());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unavailable_elsewhere() {
    assert!(availability().unwrap_err().contains("gestures unavailable"));
    let mut c = GesturesConfig::default();
    c.bindings.push(GestureBinding::new(
        Gesture::Tap { fingers: 3 },
        Action::PastePlainText,
    ));
    let h = Gestures::start(c);
    assert!(matches!(h.status(), Status::Unavailable(_)));
}
