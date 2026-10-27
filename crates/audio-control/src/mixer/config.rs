//! Persisted configuration: per-app routes (`Config::app_routes`) and the
//! mixer matrix (`Config::mixer`). Everything is `camelCase`,
//! `#[serde(default)]` and empty / disabled by default.

use super::dsp::biquad::{BandKind, MAX_BANDS};
use crate::DeviceMatch;
use serde::{Deserialize, Serialize};

/// Most sources / outputs one mixer runs (extra entries are reported and
/// ignored).
pub const MAX_SOURCES: usize = 64;
pub const MAX_OUTPUTS: usize = 32;

/// Send one app's audio to other output device(s), keyed by bundle id in
/// `Config::app_routes` (exe name on Windows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppRoute {
    pub enabled: bool,
    /// Display name (the app's name when the route was made).
    pub name: String,
    /// Target devices: real outputs or PowerMac virtual devices. Matched by
    /// uid, then by name (see [`DeviceMatch`]).
    pub outputs: Vec<DeviceMatch>,
    /// Also keep playing on the app's normal output (default: **off**, the
    /// app is heard only on `outputs`). macOS only.
    pub keep_default: bool,
    /// Extra gain on the routed copy (dB). macOS only.
    pub gain_db: f32,
}

impl Default for AppRoute {
    fn default() -> Self {
        AppRoute { enabled: true, name: String::new(), outputs: Vec::new(), keep_default: false, gain_db: 0.0 }
    }
}

impl AppRoute {
    pub fn to(output: DeviceMatch) -> Self {
        AppRoute { outputs: vec![output], ..Default::default() }
    }
}

/// The mixer matrix: sources → outputs through sends (crosspoints).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Mixer {
    pub enabled: bool,
    pub sources: Vec<MixSource>,
    pub outputs: Vec<MixOutput>,
    /// Crosspoints. A source/output pair without a send is not connected.
    pub sends: Vec<MixSend>,
}

impl Mixer {
    pub fn source(&self, id: &str) -> Option<&MixSource> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn output(&self, id: &str) -> Option<&MixOutput> {
        self.outputs.iter().find(|o| o.id == id)
    }

    pub fn send(&self, source: &str, output: &str) -> Option<&MixSend> {
        self.sends.iter().find(|s| s.source == source && s.output == output)
    }

    /// Set (or add) a crosspoint.
    pub fn set_send(&mut self, source: &str, output: &str, gain_db: f32, muted: bool) {
        match self.sends.iter_mut().find(|s| s.source == source && s.output == output) {
            Some(s) => {
                s.gain_db = gain_db;
                s.muted = muted;
            }
            None => self.sends.push(MixSend { source: source.into(), output: output.into(), gain_db, muted }),
        }
    }

    pub fn remove_send(&mut self, source: &str, output: &str) {
        self.sends.retain(|s| !(s.source == source && s.output == output));
    }
}

/// Where a source's audio comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum SourceKind {
    /// An app (macOS 14.4+ process tap; needs System Audio Recording).
    App {
        #[serde(rename = "bundleId", default)]
        bundle_id: String,
    },
    /// An input device (microphone, interface; needs the Microphone
    /// permission).
    Input {
        #[serde(default)]
        device: DeviceMatch,
    },
    /// What is being played to a virtual loopback device (PowerMac Audio,
    /// PowerMac Mix N, BlackHole…), read from its input side.
    Loopback {
        #[serde(default)]
        device: DeviceMatch,
    },
}

impl Default for SourceKind {
    fn default() -> Self {
        SourceKind::App { bundle_id: String::new() }
    }
}

/// One mixer input strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct MixSource {
    /// Stable id the sends refer to.
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    /// Fader (dB; ≤ −96 is silence).
    pub gain_db: f32,
    pub muted: bool,
    /// While any source is soloed, only soloed sources are heard.
    pub solo: bool,
    /// Apps: keep the app audible on its normal output as well (default:
    /// **off**, the mixer takes the app over).
    pub keep_original: bool,
    /// Devices: first channel (0-based) of the stereo pair read.
    pub first_channel: u32,
    /// Devices: read one channel only and feed it to both sides.
    pub mono: bool,
    pub effects: Effects,
}

/// One mixer output strip (a bus written to a device).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct MixOutput {
    pub id: String,
    pub name: String,
    pub device: DeviceMatch,
    /// Master gain (dB).
    pub gain_db: f32,
    pub muted: bool,
    /// First device channel (0-based) of the stereo pair written.
    pub first_channel: u32,
    pub effects: Effects,
}

/// A crosspoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct MixSend {
    pub source: String,
    pub output: String,
    pub gain_db: f32,
    pub muted: bool,
}

/// Insert effects of a strip, in order: gain → EQ → limiter (outputs:
/// gain → EQ → master → limiter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Effects {
    /// Input trim (dB).
    pub gain_db: f32,
    pub eq: EqConfig,
    pub limiter: LimiterConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct EqConfig {
    pub enabled: bool,
    /// Up to 10 bands (more are ignored).
    pub bands: Vec<EqBand>,
}

impl EqConfig {
    /// `n` (3–10) flat bands spread from 60 Hz to 12 kHz: a low shelf, bells
    /// and a high shelf.
    pub fn flat(n: usize) -> Self {
        let n = n.clamp(3, MAX_BANDS);
        let (lo, hi) = (60f32, 12_000f32);
        let bands = (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                let kind = if i == 0 {
                    BandKind::LowShelf
                } else if i == n - 1 {
                    BandKind::HighShelf
                } else {
                    BandKind::Peak
                };
                EqBand { enabled: true, kind, freq_hz: lo * (hi / lo).powf(t), gain_db: 0.0, q: if kind == BandKind::Peak { 1.0 } else { 0.707 } }
            })
            .collect();
        EqConfig { enabled: true, bands }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EqBand {
    pub enabled: bool,
    pub kind: BandKind,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl Default for EqBand {
    fn default() -> Self {
        EqBand { enabled: true, kind: BandKind::Peak, freq_hz: 1000.0, gain_db: 0.0, q: 0.707 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LimiterConfig {
    pub enabled: bool,
    /// dBFS, −60 … 0.
    pub ceiling_db: f32,
    pub release_ms: f32,
    /// 0 … 20 ms (adds this much latency to the strip).
    pub lookahead_ms: f32,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        LimiterConfig { enabled: false, ceiling_db: -1.0, release_ms: 100.0, lookahead_ms: 5.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn defaults_are_empty_and_disabled() {
        let c = Config::default();
        assert!(c.app_routes.is_empty());
        assert!(!c.mixer.enabled);
        assert!(c.mixer.sources.is_empty() && c.mixer.outputs.is_empty() && c.mixer.sends.is_empty());
        // Old configs without the new keys still load.
        let old: Config = serde_json::from_str(r#"{"enabled":true,"perAppVolume":true}"#).unwrap();
        assert_eq!(old.mixer, Mixer::default());
        assert!(old.app_routes.is_empty());
        assert!(AppRoute::default().enabled);
    }

    #[test]
    fn config_round_trip() {
        let mut c = Config::default();
        c.app_routes.insert(
            "com.spotify.client".into(),
            AppRoute { name: "Spotify".into(), outputs: vec![DeviceMatch::uid("PowerMacAudio:mix1", "PowerMac Mix 1")], keep_default: true, gain_db: -3.0, enabled: true },
        );
        let mut eq = EqConfig::flat(5);
        eq.bands[2].gain_db = 4.0;
        c.mixer = Mixer {
            enabled: true,
            sources: vec![
                MixSource { id: "music".into(), name: "Music".into(), kind: SourceKind::App { bundle_id: "com.apple.Music".into() }, gain_db: -6.0, solo: true, ..Default::default() },
                MixSource { id: "mic".into(), kind: SourceKind::Input { device: DeviceMatch::name("MacBook") }, mono: true, effects: Effects { gain_db: 3.0, eq, limiter: LimiterConfig { enabled: true, ..Default::default() } }, ..Default::default() },
                MixSource { id: "loop".into(), kind: SourceKind::Loopback { device: DeviceMatch::uid("PowerMacAudio:main", "PowerMac Audio") }, ..Default::default() },
            ],
            outputs: vec![MixOutput { id: "stream".into(), device: DeviceMatch::uid("PowerMacAudio:mix1", ""), gain_db: -1.5, first_channel: 2, ..Default::default() }],
            sends: vec![MixSend { source: "music".into(), output: "stream".into(), gain_db: -10.0, muted: false }],
        };
        let json = serde_json::to_string_pretty(&c).unwrap();
        assert!(json.contains("\"appRoutes\""), "{json}");
        assert!(json.contains("\"keepDefault\": true"));
        assert!(json.contains("\"type\": \"app\""));
        assert!(json.contains("\"bundleId\": \"com.apple.Music\""));
        assert!(json.contains("\"type\": \"loopback\""));
        assert!(json.contains("\"lookaheadMs\""));
        assert!(json.contains("\"kind\": \"lowShelf\""));
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let m: Mixer = serde_json::from_str(
            r#"{"enabled":true,"sources":[{"id":"a","kind":{"type":"input","device":"USB"}}],"outputs":[{"id":"o"}],"sends":[{"source":"a","output":"o"}]}"#,
        )
        .unwrap();
        assert_eq!(m.sources[0].kind, SourceKind::Input { device: DeviceMatch::name("USB") });
        assert_eq!(m.sources[0].gain_db, 0.0);
        assert!(!m.sources[0].effects.limiter.enabled);
        assert_eq!(m.sources[0].effects.limiter.ceiling_db, -1.0);
        assert_eq!(m.sends[0].gain_db, 0.0);
        let r: AppRoute = serde_json::from_str(r#"{"outputs":["AirPods"]}"#).unwrap();
        assert!(r.enabled && !r.keep_default);
        assert_eq!(r.outputs, vec![DeviceMatch::name("AirPods")]);
    }

    #[test]
    fn flat_eq_layout_and_send_editing() {
        let e = EqConfig::flat(1);
        assert_eq!(e.bands.len(), 3);
        let e = EqConfig::flat(99);
        assert_eq!(e.bands.len(), 10);
        assert_eq!(e.bands[0].kind, BandKind::LowShelf);
        assert_eq!(e.bands[9].kind, BandKind::HighShelf);
        assert!((e.bands[0].freq_hz - 60.0).abs() < 0.01 && (e.bands[9].freq_hz - 12_000.0).abs() < 1.0);
        let mut m = Mixer::default();
        m.set_send("a", "b", -3.0, false);
        m.set_send("a", "b", -6.0, true);
        assert_eq!(m.sends.len(), 1);
        assert_eq!(m.send("a", "b").unwrap().gain_db, -6.0);
        m.remove_send("a", "b");
        assert!(m.sends.is_empty());
    }
}
