//! Load a built `PowerMac Audio.driver` in this process through CFPlugIn
//! and drive it with a fake host (nothing is installed, coreaudiod is not
//! involved):
//!
//! ```sh
//! ./scripts/build-audio-driver.sh
//! cargo run -p pm-audio-driver --example host_harness -- "target/release/PowerMac Audio.driver"
//! ```
//!
//! macOS only (the driver is a CoreAudio plug-in).

#[cfg(not(all(target_os = "macos", feature = "plugin")))]
fn main() {
    eprintln!("host_harness needs macOS and the `plugin` feature");
}

#[cfg(all(target_os = "macos", feature = "plugin"))]
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "target/release/PowerMac Audio.driver".into());
    let path = std::path::Path::new(&path);
    let run = || -> Result<Vec<String>, String> {
        let drv = pm_audio_driver::harness::load_bundle(path)?;
        let host = pm_audio_driver::harness::FakeHost::new();
        let client = unsafe { pm_audio_driver::harness::DriverClient::new(drv) };
        pm_audio_driver::harness::exercise(&client, &host)
    };
    match run() {
        Ok(log) => {
            println!("loaded {}", path.display());
            for l in log {
                println!("  ok  {l}");
            }
            println!("PASS");
        }
        Err(e) => {
            eprintln!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}
