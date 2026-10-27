//! Battery charge limit (AlDente-style) for PowerMac.
//!
//! * **Status** (no privileges): the `AppleSmartBattery` IORegistry entry
//!   (percentage, charging, adapter + wattage, cycles, health, time
//!   remaining) and the SMC battery temperature sensor on macOS;
//!   `GetSystemPowerStatus` on Windows. See [`read_battery`].
//! * **Control** (macOS): charging is inhibited (`CHTE`, `CH0B`/`CH0C`), a
//!   firmware charge range set (`bfD0`/`bfE0`/`bfF0`), or the adapter turned
//!   off (`CHIE`/`CH0I`/`CH0J`) by writing SMC keys (see [`scheme`]). On
//!   firmware where no inhibit key is usable (Apple Silicon on macOS 27) the
//!   limit is enforced by switching the adapter off at the limit and on
//!   below the sailing threshold ([`Strategy::LimitViaDischarge`]). macOS's
//!   own Charge Limit is only read ([`native`]). Writing needs root, so a small
//!   LaunchDaemon (`powermac-battery-helper`, [`helper`]) does the writes on
//!   behalf of the app over a Unix socket. [`install_helper`] installs it
//!   with one admin prompt.
//! * **Policy**: [`policy::step`] is the pure state machine (limit, sailing,
//!   heat protection, discharge-to-limit, top-up, sleep safety);
//!   [`BatteryLimit::start`] runs it every `poll_seconds`.
//! * **Windows**: status only. There is no generic charge-limit API; charge
//!   thresholds are vendor tools (Lenovo Vantage, MyASUS, Dell Power
//!   Manager, Surface UEFI), so the controller never changes anything there.

pub mod config;
pub mod controller;
#[cfg(unix)]
pub mod helper;
pub mod installer;
pub mod native;
pub mod platform;
pub mod policy;
pub mod protocol;
pub mod scheme;
pub mod smc;
pub mod status;

pub use config::Config;
pub use controller::{BatteryLimit, Environment, Handle, SystemEnvironment};
pub use installer::{
    helper_installed, install_helper, uninstall_helper, HELPER_INSTALL_PATH, PLIST_PATH,
};
pub use policy::Action;
pub use scheme::{Range, Scheme, Strategy};
pub use status::{BatteryReading, BatteryStatus};

/// File name of the helper binary (to bundle next to the app executable,
/// e.g. `PowerMac.app/Contents/MacOS/powermac-battery-helper`).
pub const HELPER_BINARY_NAME: &str = "powermac-battery-helper";

/// Read the battery (no privileges).
pub fn read_battery() -> BatteryReading {
    platform::read_battery()
}

/// Detect the SMC key scheme by reading key info (no privileges).
#[cfg(target_os = "macos")]
pub fn detect_scheme() -> anyhow::Result<Scheme> {
    let mut smc = smc::AppleSmc::open()?;
    Scheme::detect(&mut smc)
}

#[cfg(not(target_os = "macos"))]
pub fn detect_scheme() -> anyhow::Result<Scheme> {
    anyhow::bail!("SMC is macOS-only")
}

/// Where to find the helper binary to install: next to the running
/// executable (the app bundle's `Contents/MacOS`, or `target/<profile>`).
pub fn bundled_helper_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(HELPER_BINARY_NAME);
    p.is_file().then_some(p)
}
