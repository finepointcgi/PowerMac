//! Sound widget: the host provides audio control through [`SoundProvider`]
//! (PowerMac implements it with its audio-control crate); this crate only
//! draws and forwards clicks.

use std::sync::{Arc, Mutex};

/// An output device as the host reports it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioOutput {
    /// Stable id (Core Audio device UID).
    pub id: String,
    pub name: String,
    /// Optional SF Symbol; guessed from the name when empty.
    pub symbol: String,
}

impl AudioOutput {
    pub fn new(id: &str, name: &str) -> Self {
        AudioOutput { id: id.into(), name: name.into(), symbol: String::new() }
    }

    pub fn symbol(&self) -> &str {
        if self.symbol.is_empty() {
            guess_symbol(&self.name)
        } else {
            &self.symbol
        }
    }
}

/// SF Symbol for an output device name.
pub fn guess_symbol(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.contains("airpods max") {
        "airpodsmax"
    } else if n.contains("airpods pro") {
        "airpodspro"
    } else if n.contains("airpods") {
        "airpods"
    } else if n.contains("homepod") {
        "homepod.fill"
    } else if n.contains("airplay") || n.contains("apple tv") {
        "airplayaudio"
    } else if n.contains("headphone") || n.contains("headset") || n.contains("beats") || n.contains("buds") {
        "headphones"
    } else if n.contains("macbook") || n.contains("built-in") || n.contains("internal") {
        "laptopcomputer"
    } else if n.contains("display") || n.contains("hdmi") || n.contains("lg ") || n.contains("monitor") || n.contains("tv") {
        "tv"
    } else if n.contains("blackhole") || n.contains("loopback") || n.contains("soundflower") || n.contains("virtual") || n.contains("zoom") || n.contains("teams") {
        "waveform"
    } else {
        "hifispeaker.fill"
    }
}

/// Audio control provided by the host. Calls come from a background
/// thread of the shelf (never the main thread while it waits); they
/// should be quick.
pub trait SoundProvider: Send + Sync {
    /// Output devices, in display order.
    fn outputs(&self) -> Vec<AudioOutput>;
    /// Id of the default output device.
    fn default_output(&self) -> Option<String>;
    fn set_default_output(&self, id: &str) -> Result<(), String>;
    /// Volume of the default output, 0..1 (`None`: not adjustable).
    fn volume(&self) -> Option<f32>;
    fn set_volume(&self, v: f32) -> Result<(), String>;
    fn muted(&self) -> Option<bool>;
    fn set_muted(&self, muted: bool) -> Result<(), String>;
}

/// A boxed host callback.
pub type Getter<T> = Box<dyn Fn() -> T + Send + Sync>;
/// A boxed host setter taking a device id.
pub type IdSetter = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
/// A boxed host setter.
pub type Setter<A> = Box<dyn Fn(A) -> Result<(), String> + Send + Sync>;

/// A [`SoundProvider`] built from closures.
pub struct SoundFns {
    pub outputs: Getter<Vec<AudioOutput>>,
    pub default_output: Getter<Option<String>>,
    pub set_default_output: IdSetter,
    pub volume: Getter<Option<f32>>,
    pub set_volume: Setter<f32>,
    pub muted: Getter<Option<bool>>,
    pub set_muted: Setter<bool>,
}

impl SoundProvider for SoundFns {
    fn outputs(&self) -> Vec<AudioOutput> {
        (self.outputs)()
    }
    fn default_output(&self) -> Option<String> {
        (self.default_output)()
    }
    fn set_default_output(&self, id: &str) -> Result<(), String> {
        (self.set_default_output)(id)
    }
    fn volume(&self) -> Option<f32> {
        (self.volume)()
    }
    fn set_volume(&self, v: f32) -> Result<(), String> {
        (self.set_volume)(v)
    }
    fn muted(&self) -> Option<bool> {
        (self.muted)()
    }
    fn set_muted(&self, m: bool) -> Result<(), String> {
        (self.set_muted)(m)
    }
}

/// In-memory provider for previews and tests.
#[derive(Default)]
pub struct FakeSound {
    pub state: Mutex<SoundState>,
}

impl FakeSound {
    pub fn new(devices: Vec<AudioOutput>, current: &str, volume: f32) -> Arc<FakeSound> {
        Arc::new(FakeSound { state: Mutex::new(SoundState { devices, current: Some(current.into()), volume: Some(volume), muted: false, available: true }) })
    }
    fn st(&self) -> std::sync::MutexGuard<'_, SoundState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SoundProvider for FakeSound {
    fn outputs(&self) -> Vec<AudioOutput> {
        self.st().devices.clone()
    }
    fn default_output(&self) -> Option<String> {
        self.st().current.clone()
    }
    fn set_default_output(&self, id: &str) -> Result<(), String> {
        let mut s = self.st();
        if !s.devices.iter().any(|d| d.id == id) {
            return Err("no such device".into());
        }
        s.current = Some(id.into());
        Ok(())
    }
    fn volume(&self) -> Option<f32> {
        self.st().volume
    }
    fn set_volume(&self, v: f32) -> Result<(), String> {
        self.st().volume = Some(v.clamp(0.0, 1.0));
        Ok(())
    }
    fn muted(&self) -> Option<bool> {
        Some(self.st().muted)
    }
    fn set_muted(&self, m: bool) -> Result<(), String> {
        self.st().muted = m;
        Ok(())
    }
}

/// A snapshot for drawing.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SoundState {
    pub devices: Vec<AudioOutput>,
    pub current: Option<String>,
    pub volume: Option<f32>,
    pub muted: bool,
    /// The host gave a provider.
    pub available: bool,
}

impl SoundState {
    pub fn read(p: &dyn SoundProvider) -> SoundState {
        SoundState { devices: p.outputs(), current: p.default_output(), volume: p.volume(), muted: p.muted().unwrap_or(false), available: true }
    }

    pub fn current_device(&self) -> Option<&AudioOutput> {
        let c = self.current.as_ref()?;
        self.devices.iter().find(|d| &d.id == c)
    }

    /// Speaker symbol for the level ("speaker.slash.fill" when muted).
    pub fn level_symbol(&self) -> &'static str {
        let v = self.volume.unwrap_or(0.0);
        if self.muted || v <= 0.001 {
            "speaker.slash.fill"
        } else if v < 0.34 {
            "speaker.wave.1.fill"
        } else if v < 0.67 {
            "speaker.wave.2.fill"
        } else {
            "speaker.wave.3.fill"
        }
    }
}

/// Volume for a click / drag at `x` on a slider track spanning [x0, x1].
pub fn slider_value(x: f64, x0: f64, x1: f64) -> f32 {
    if x1 <= x0 {
        return 0.0;
    }
    (((x - x0) / (x1 - x0)).clamp(0.0, 1.0)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_provider_and_state() {
        let p = FakeSound::new(vec![AudioOutput::new("bi", "MacBook Pro Speakers"), AudioOutput::new("ap", "Mitch's AirPods Pro")], "bi", 0.5);
        let s = SoundState::read(p.as_ref());
        assert_eq!(s.current_device().unwrap().symbol(), "laptopcomputer");
        assert_eq!(s.level_symbol(), "speaker.wave.2.fill");
        p.set_default_output("ap").unwrap();
        assert!(p.set_default_output("zz").is_err());
        p.set_volume(2.0).unwrap();
        p.set_muted(true).unwrap();
        let s = SoundState::read(p.as_ref());
        assert_eq!(s.current_device().unwrap().symbol(), "airpodspro");
        assert_eq!(s.volume, Some(1.0));
        assert_eq!(s.level_symbol(), "speaker.slash.fill");
        assert!(s.available);
        assert!(!SoundState::default().available);
    }

    #[test]
    fn closures_provider() {
        let v = Arc::new(Mutex::new(0.2f32));
        let (a, b) = (v.clone(), v.clone());
        let p = SoundFns {
            outputs: Box::new(|| vec![AudioOutput::new("x", "LG UltraFine Display")]),
            default_output: Box::new(|| Some("x".into())),
            set_default_output: Box::new(|_| Ok(())),
            volume: Box::new(move || Some(*a.lock().unwrap())),
            set_volume: Box::new(move |x| {
                *b.lock().unwrap() = x;
                Ok(())
            }),
            muted: Box::new(|| None),
            set_muted: Box::new(|_| Err("not supported".into())),
        };
        p.set_volume(0.9).unwrap();
        let s = SoundState::read(&p);
        assert_eq!((s.volume, s.muted), (Some(0.9), false));
        assert_eq!(s.devices[0].symbol(), "tv");
    }

    #[test]
    fn guesses_and_slider() {
        assert_eq!(guess_symbol("HomePod mini"), "homepod.fill");
        assert_eq!(guess_symbol("BlackHole 2ch"), "waveform");
        assert_eq!(guess_symbol("Studio Speakers"), "hifispeaker.fill");
        assert_eq!(slider_value(50.0, 0.0, 100.0), 0.5);
        assert_eq!(slider_value(-5.0, 0.0, 100.0), 0.0);
        assert_eq!(slider_value(500.0, 0.0, 100.0), 1.0);
        assert_eq!(slider_value(1.0, 5.0, 5.0), 0.0);
    }
}
