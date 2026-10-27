//! **PowerMac Audio** — a Core Audio AudioServerPlugIn (a HAL driver, like
//! BlackHole) that publishes loopback virtual devices: whatever apps play to
//! a device's output can be recorded from the same device's input.
//!
//! * Devices: "PowerMac Audio" plus optional named devices ("PowerMac Mix
//!   1"…), up to [`spec::MAX_DEVICES`]; each with 2/8/16/32/64 channels at
//!   44.1/48/96 kHz, float32 interleaved, zero latency and zero safety
//!   offset, an output volume and mute control.
//! * Configuration: a JSON [`spec::DriverConfig`] in the custom property
//!   [`spec::CONFIG_PROPERTY`] on the plug-in object, persisted in the
//!   host's storage. Format changes go through
//!   `RequestDeviceConfigurationChange`; adding, removing and renaming
//!   devices are plain notifications. The sample rate can also be set the
//!   standard way (Audio MIDI Setup).
//! * Packaging: `scripts/build-audio-driver.sh` builds this crate's cdylib
//!   into `PowerMac Audio.driver` (Info.plist from `Info.plist.in`) and
//!   signs it. `audio_control::driver` installs it.
//!
//! ## Why Rust (and not C via `cc`)
//!
//! The plug-in ABI is one flat C vtable of function pointers plus a CFPlugIn
//! factory; `#[repr(C)]` mirrors it exactly (sizes are checked against the
//! SDK headers in `sys::tests`), and a Mach-O dylib is a valid CFPlugIn
//! executable (shipping HAL drivers use both bundles and dylibs). Writing it
//! in Rust keeps the ring buffer, clock and property tables unit-testable
//! in-process with `cargo test`, shares [`spec`] with `audio-control`, and
//! needs no second toolchain. The real-time rules are kept by hand: no
//! allocation or host calls on the IO path, a panic guard on every entry
//! point, and host callbacks only after locks are released.
//!
//! Modules: [`spec`] (shared), [`ring`], [`clock`], [`volume`] (pure logic),
//! `sys` (ABI), `driver` (the plug-in), `harness` (fake host for tests).

pub mod spec;

pub mod clock;
pub mod ring;
pub mod volume;

#[cfg(all(target_os = "macos", feature = "plugin"))]
pub mod sys;

#[cfg(all(target_os = "macos", feature = "plugin"))]
pub mod driver;

#[cfg(all(target_os = "macos", feature = "plugin"))]
#[doc(hidden)]
pub mod harness;
