//! Load the crate's own cdylib as a `.driver` bundle through CFPlugIn (like
//! coreaudiod does) and run the fake-host checks against it.

#![cfg(target_os = "macos")]

use pm_audio_driver::harness;
use std::path::PathBuf;

fn built_dylib() -> Option<PathBuf> {
    // target/<profile>/deps/load_bundle-… → target/<profile>/libpm_audio_driver.dylib
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?;
    let p = profile_dir.join("libpm_audio_driver.dylib");
    p.exists().then_some(p)
}

#[test]
fn bundle_loads_through_cfplugin_and_passes_the_harness() {
    let Some(dylib) = built_dylib() else {
        eprintln!("libpm_audio_driver.dylib not built next to the test binary; skipping");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("driver-bundle-test");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle = harness::assemble_bundle(&dylib, &dir, "0.0.0-test").unwrap();
    let drv = harness::load_bundle(&bundle).unwrap_or_else(|e| panic!("{e}"));
    let host = harness::FakeHost::new();
    let client = unsafe { harness::DriverClient::new(drv) };
    let log = harness::exercise(&client, &host).unwrap_or_else(|e| panic!("{e}"));
    for l in &log {
        eprintln!("{l}");
    }
    assert!(log.iter().filter(|l| l.starts_with("loopback ok")).count() >= 3);
}
