//! What the driver and the app agree on: bundle identity, the configuration
//! format and its limits. Platform-independent and dependency-light, so
//! `audio-control` can use it without pulling in the plug-in.

use serde::{Deserialize, Serialize};

/// `CFBundleIdentifier` of the driver bundle.
pub const BUNDLE_ID: &str = "com.powermac.audio.driver";
/// The bundle's directory name in `/Library/Audio/Plug-Ins/HAL`.
pub const BUNDLE_NAME: &str = "PowerMac Audio.driver";
/// `CFBundleExecutable`.
pub const EXECUTABLE_NAME: &str = "PowerMac Audio";
/// Where Core Audio looks for HAL plug-ins.
pub const HAL_DIR: &str = "/Library/Audio/Plug-Ins/HAL";
/// The CFPlugIn factory (Info.plist `CFPlugInFactories`).
pub const FACTORY_UUID: &str = "219B70CD-DB1C-43E0-90E0-5410F1BFF83F";
/// The exported factory function.
pub const FACTORY_FUNCTION: &str = "PowerMacAudio_Create";
/// `kAudioServerPlugInTypeUUID`.
pub const PLUGIN_TYPE_UUID: &str = "443ABAB8-E7B3-491A-B985-BEB9187030DB";
/// `kAudioServerPlugInDriverInterfaceUUID`.
pub const DRIVER_INTERFACE_UUID: &str = "EEA5773D-CC43-49F1-8E00-8F96E7D23B17";

pub const MANUFACTURER: &str = "PowerMac";

/// Custom property on the plug-in object (`kAudioObjectPlugInObject`): the
/// configuration as a JSON CFString. Readable and settable by clients
/// through `AudioObjectGetPropertyData` / `AudioObjectSetPropertyData` on
/// the object found with `kAudioHardwarePropertyTranslateBundleIDToPlugIn`.
pub const CONFIG_PROPERTY: u32 = fourcc(b"pmcf");
/// Host storage key the configuration is persisted under.
pub const STORAGE_KEY: &str = "config";

/// Channel counts a device may have.
pub const CHANNEL_COUNTS: [u32; 5] = [2, 8, 16, 32, 64];
/// Sample rates a device may run at.
pub const SAMPLE_RATES: [f64; 3] = [44_100.0, 48_000.0, 96_000.0];
/// Most devices one driver publishes.
pub const MAX_DEVICES: usize = 16;
/// Loopback ring size in frames; also the zero-timestamp period (the HAL
/// requires at least 10923).
pub const RING_FRAMES: u32 = 16_384;
pub const MAX_NAME_LEN: usize = 64;

pub const MAIN_UID: &str = "PowerMacAudio:main";
pub const MAIN_NAME: &str = "PowerMac Audio";

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

/// Uid of the n-th (1-based) mix device.
pub fn mix_uid(n: usize) -> String {
    format!("PowerMacAudio:mix{n}")
}

/// Default name of the n-th (1-based) mix device.
pub fn mix_name(n: usize) -> String {
    format!("PowerMac Mix {n}")
}

fn default_channels() -> u32 {
    2
}

fn default_rate() -> f64 {
    48_000.0
}

/// One virtual device: output and input with the same channel count; what
/// is played to the output can be recorded from the input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSpec {
    /// Stable identity (`kAudioDevicePropertyDeviceUID`). Apps remember
    /// devices by it, so it should not change when the name does.
    pub uid: String,
    pub name: String,
    #[serde(default = "default_channels")]
    pub channels: u32,
    #[serde(default = "default_rate")]
    pub sample_rate: f64,
}

impl DeviceSpec {
    pub fn new(uid: impl Into<String>, name: impl Into<String>, channels: u32, sample_rate: f64) -> Self {
        DeviceSpec { uid: uid.into(), name: name.into(), channels, sample_rate }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.uid.trim().is_empty() || self.uid.len() > 128 {
            return Err("a device needs a uid of 1–128 characters".into());
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
            return Err(format!("device names are 1–{MAX_NAME_LEN} characters"));
        }
        if !CHANNEL_COUNTS.contains(&self.channels) {
            return Err(format!("{}: {} channels is not supported (2, 8, 16, 32 or 64)", self.name, self.channels));
        }
        if !is_supported_rate(self.sample_rate) {
            return Err(format!("{}: {} Hz is not supported (44100, 48000 or 96000)", self.name, self.sample_rate));
        }
        Ok(())
    }

    /// Whether going from `self` to `other` changes the IO format (needs a
    /// device configuration change rather than a property notification).
    pub fn format_differs(&self, other: &DeviceSpec) -> bool {
        self.channels != other.channels || self.sample_rate != other.sample_rate
    }
}

pub fn is_supported_rate(rate: f64) -> bool {
    SAMPLE_RATES.iter().any(|r| (r - rate).abs() < 0.5)
}

/// The driver's devices, in the order they are published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverConfig {
    pub devices: Vec<DeviceSpec>,
}

impl Default for DriverConfig {
    /// "PowerMac Audio", stereo, 48 kHz.
    fn default() -> Self {
        DriverConfig { devices: vec![DeviceSpec::new(MAIN_UID, MAIN_NAME, 2, 48_000.0)] }
    }
}

impl DriverConfig {
    /// The main device plus `mixes` devices "PowerMac Mix 1"…, all with
    /// `channels` channels at `sample_rate`.
    pub fn with_mixes(channels: u32, sample_rate: f64, mixes: usize) -> Self {
        let mut devices = vec![DeviceSpec::new(MAIN_UID, MAIN_NAME, channels, sample_rate)];
        for n in 1..=mixes {
            devices.push(DeviceSpec::new(mix_uid(n), mix_name(n), channels, sample_rate));
        }
        DriverConfig { devices }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.devices.is_empty() {
            return Err("at least one device is needed".into());
        }
        if self.devices.len() > MAX_DEVICES {
            return Err(format!("at most {MAX_DEVICES} devices"));
        }
        for (i, d) in self.devices.iter().enumerate() {
            d.validate()?;
            if self.devices[..i].iter().any(|o| o.uid == d.uid) {
                return Err(format!("two devices share the uid {}", d.uid));
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse and validate.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let c: DriverConfig = serde_json::from_str(s).map_err(|e| format!("bad driver config: {e}"))?;
        c.validate()?;
        Ok(c)
    }

    pub fn device(&self, uid: &str) -> Option<&DeviceSpec> {
        self.devices.iter().find(|d| d.uid == uid)
    }
}

/// The driver bundle's Info.plist (`@VERSION@` is replaced by the version).
/// `scripts/build-audio-driver.sh` uses the same file.
pub const INFO_PLIST_TEMPLATE: &str = include_str!("../Info.plist.in");

pub fn info_plist(version: &str) -> String {
    INFO_PLIST_TEMPLATE.replace("@VERSION@", version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let c = DriverConfig::default();
        c.validate().unwrap();
        assert_eq!(c.devices[0].name, "PowerMac Audio");
        assert_eq!(c.devices[0].channels, 2);
    }

    #[test]
    fn mixes_are_named_and_unique() {
        let c = DriverConfig::with_mixes(16, 96_000.0, 3);
        c.validate().unwrap();
        let names: Vec<_> = c.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["PowerMac Audio", "PowerMac Mix 1", "PowerMac Mix 2", "PowerMac Mix 3"]);
        assert!(c.devices.iter().all(|d| d.channels == 16 && d.sample_rate == 96_000.0));
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut c = DriverConfig::default();
        c.devices[0].channels = 6;
        assert!(c.validate().is_err());
        c.devices[0].channels = 64;
        c.devices[0].sample_rate = 22_050.0;
        assert!(c.validate().is_err());
        c.devices[0].sample_rate = 44_100.0;
        c.validate().unwrap();
        c.devices.push(c.devices[0].clone());
        assert!(c.validate().unwrap_err().contains("uid"));
        assert!(DriverConfig { devices: vec![] }.validate().is_err());
        let too_many = DriverConfig::with_mixes(2, 48_000.0, MAX_DEVICES);
        assert!(too_many.validate().is_err());
        let mut blank = DriverConfig::default();
        blank.devices[0].name = "  ".into();
        assert!(blank.validate().is_err());
    }

    #[test]
    fn json_round_trip_and_defaults() {
        let c = DriverConfig::with_mixes(8, 44_100.0, 1);
        assert_eq!(DriverConfig::from_json(&c.to_json()).unwrap(), c);
        let minimal = DriverConfig::from_json(r#"{"devices":[{"uid":"a","name":"A"}]}"#).unwrap();
        assert_eq!(minimal.devices[0].channels, 2);
        assert_eq!(minimal.devices[0].sample_rate, 48_000.0);
        assert!(DriverConfig::from_json("{").is_err());
        assert!(DriverConfig::from_json(r#"{"devices":[{"uid":"a","name":"A","channels":3}]}"#).is_err());
    }

    #[test]
    fn info_plist_declares_the_factory() {
        let p = info_plist("1.2.3");
        assert!(p.contains("<string>1.2.3</string>"));
        assert!(!p.contains("@VERSION@"));
        assert!(p.contains(FACTORY_UUID));
        assert!(p.contains(FACTORY_FUNCTION));
        assert!(p.contains(PLUGIN_TYPE_UUID));
        assert!(p.contains(BUNDLE_ID));
        assert!(p.contains(&format!("<string>{EXECUTABLE_NAME}</string>")));
    }

    #[test]
    fn format_change_detection() {
        let a = DeviceSpec::new("u", "A", 2, 48_000.0);
        let mut b = a.clone();
        b.name = "B".into();
        assert!(!a.format_differs(&b));
        b.channels = 8;
        assert!(a.format_differs(&b));
    }
}
