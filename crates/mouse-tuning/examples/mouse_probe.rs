//! Live check of mouse-tuning.
//!
//! ```text
//! cargo run -p mouse-tuning --example mouse_probe            # list devices and properties
//! cargo run -p mouse-tuning --example mouse_probe -- engine  # run linear mode for 4 s, then stop and check restore
//! ```

use std::time::Duration;

use mouse_tuning::{Config, MouseTuning, PointerSettings};

fn dump(label: &str) -> Vec<(u64, Option<i64>, Option<i64>)> {
    println!("-- {label}");
    let mut out = Vec::new();
    for (d, key, acc, res) in mouse_tuning::pointer_properties() {
        println!(
            "  [{}] {:<40} {:?} vid={:#06x} pid={:#06x} {:<10} {key}={:?} ({:?}) resolution={:?} ({:?} dpi)",
            d.id,
            d.name,
            d.kind,
            d.vendor_id,
            d.product_id,
            d.transport,
            acc,
            acc.map(|a| a as f64 / 65536.0),
            res,
            res.map(|r| r as f64 / 65536.0),
        );
        out.push((d.id, acc, res));
    }
    out
}

fn main() {
    let _ = env_logger_init();
    println!("devices: {:#?}", mouse_tuning::list_devices());
    let before = dump("current");
    if std::env::args().nth(1).as_deref() != Some("engine") {
        return;
    }
    let lin = PointerSettings { disable_acceleration: true, speed: Some(0.75) };
    let cfg = Config {
        mouse: lin,
        trackpad: lin,
        buttons: mouse_tuning::ButtonsConfig { side_buttons_navigation: false, mappings: vec![] },
        ..Config::default()
    };
    let mut h = MouseTuning::start(cfg.clone()).expect("start");
    std::thread::sleep(Duration::from_millis(500));
    let during = dump("linear, 2x (engine running)");
    // Back to "system speed" while still linear.
    h.update(Config { mouse: PointerSettings { speed: None, ..lin }, trackpad: PointerSettings { speed: None, ..lin }, ..cfg });
    std::thread::sleep(Duration::from_millis(300));
    dump("linear, native speed");
    std::thread::sleep(Duration::from_millis(1000));
    h.stop();
    let after = dump("after stop");
    // The OS does not report values set through the event system client, so
    // the log lines above (what was set / restored) are the evidence; this
    // only checks that the reported values are back to where they were.
    let _ = during;
    println!("reported values unchanged after stop: {}", before == after);
}

fn env_logger_init() -> Option<()> {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
        fn flush(&self) {}
    }
    static LOGGER: L = L;
    log::set_logger(&LOGGER).ok()?;
    log::set_max_level(log::LevelFilter::Info);
    Some(())
}
