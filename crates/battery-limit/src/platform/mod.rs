//! Platform battery status readers.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub mod macos_ffi;
#[cfg(target_os = "macos")]
pub use macos::{read_battery, smc_battery_temperature};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::read_battery;

#[cfg(not(any(target_os = "macos", windows)))]
pub fn read_battery() -> crate::status::BatteryReading {
    crate::status::BatteryReading::default()
}
