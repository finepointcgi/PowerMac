//! Read-only probe: `cargo run -p display-scaling --example display_scaling_probe` lists
//! every display and its modes; `-- --virtual-selftest [W H [TW TH [HOLD_MS]]]` (macOS) also
//! creates a virtual HiDPI display for a W x H panel, prints its modes and
//! destroys it (no mirroring, no mode changes).

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("hidpi supported: {}", display_scaling::hidpi_supported());
    let list = if args.iter().any(|a| a == "--no-list") { Vec::new() } else { display_scaling::list_displays() };
    for d in list {
        println!(
            "{} — {:?} builtin={} vendor={:04x} model={:04x} native={:?} hidpiCapable={} nativeHidpi={} scale={:?} current={:?}",
            d.id, d.name, d.is_builtin, d.vendor, d.model, d.native.map(|n| n.to_string()), d.hidpi_capable, d.native_hidpi, d.scale_percent, d.current.map(|m| m.label())
        );
        for m in &d.modes {
            println!("    {} [{}x{} px]", m.label(), m.pixel_width, m.pixel_height);
        }
        println!("    suggested HiDPI: {}", d.suggested_hidpi.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", "));
    }
    #[cfg(target_os = "macos")]
    if args.iter().any(|a| a == "--nsapp") {
        #[link(name = "AppKit", kind = "framework")]
        extern "C" {}
        let cls = objc2::runtime::AnyClass::get(c"NSApplication").unwrap();
        let _app: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![cls, sharedApplication] };
        println!("NSApplication initialized");
    }
    #[cfg(target_os = "macos")]
    if args.iter().any(|a| a == "--virtual-selftest") {
        let n: Vec<u32> = args.iter().filter_map(|a| a.parse().ok()).collect();
        let (w, h) = if n.len() >= 2 { (n[0], n[1]) } else { (2560, 1440) };
        let target = (n.len() >= 4).then(|| display_scaling::Resolution { width: n[2], height: n[3] });
        let hold = std::time::Duration::from_millis(n.get(4).copied().unwrap_or(0) as u64);
        match display_scaling::virtual_display_self_test(w, h, target, hold) {
            Ok(r) => print!("{r}"),
            Err(e) => println!("self-test failed: {e:#}"),
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--start-noop") {
        // Starts the module with the default config (changes nothing) and a
        // state file at the given path, lists displays through the handle, stops.
        let path = args.get(i + 1).map(std::path::PathBuf::from);
        let h = display_scaling::DisplayScaling::start_with_state_file(display_scaling::Config::default(), path.clone()).expect("start");
        for d in h.displays() {
            println!("handle: {} current={:?} modes={} hidpiCapable={}", d.id, d.current.map(|m| m.label()), d.modes.len(), d.hidpi_capable);
        }
        println!("pending: {:?}", h.pending());
        h.stop();
        println!("stopped; state file exists: {}", path.is_some_and(|p| p.exists()));
    }
}
