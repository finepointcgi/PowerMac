//! Now Playing helpers: the AppleScript snippets for Music and Spotify,
//! parsing their output, and progress extrapolation.

use super::{MediaSource, NowPlaying};

pub const MUSIC_ID: &str = "com.apple.Music";
pub const SPOTIFY_ID: &str = "com.spotify.client";

/// Player state + track as one tab-separated line. `player position` and
/// `duration` are seconds (Music) — Spotify reports duration in ms.
pub const MUSIC_SCRIPT: &str = r#"tell application id "com.apple.Music"
	set pstate to player state as string
	if pstate is "stopped" then return "stopped"
	set t to current track
	return pstate & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & ((duration of t) as string) & tab & ((player position) as string)
end tell"#;

pub const SPOTIFY_SCRIPT: &str = r#"tell application id "com.spotify.client"
	set pstate to player state as string
	if pstate is "stopped" then return "stopped"
	set t to current track
	return pstate & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (((duration of t) / 1000) as string) & tab & ((player position) as string) & tab & (artwork url of t)
end tell"#;

/// Writes the current Music track's artwork to the POSIX path in `{path}`;
/// prints "ok" or "".
pub const MUSIC_ARTWORK_SCRIPT: &str = r#"tell application id "com.apple.Music"
	try
		set d to raw data of artwork 1 of current track
	on error
		return ""
	end try
end tell
set f to open for access (POSIX file "{path}") with write permission
try
	set eof f to 0
	write d to f
end try
close access f
return "ok""#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
}

impl Command {
    /// AppleScript verb (same for Music and Spotify).
    pub fn verb(self) -> &'static str {
        match self {
            Command::PlayPause => "playpause",
            Command::Next => "next track",
            Command::Previous => "previous track",
        }
    }

    /// NX_KEYTYPE_* for the media-key fallback.
    pub fn nx_key(self) -> i32 {
        match self {
            Command::PlayPause => 16,
            Command::Next => 17,
            Command::Previous => 18,
        }
    }

    /// MediaRemote MRCommand.
    pub fn mr_command(self) -> u32 {
        match self {
            Command::PlayPause => 2,
            Command::Next => 4,
            Command::Previous => 5,
        }
    }
}

/// AppleScript number output may use the locale's decimal comma.
fn num(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// Parse [`MUSIC_SCRIPT`] / [`SPOTIFY_SCRIPT`] output. `None` when stopped
/// or unparseable. The artwork URL (Spotify) is returned separately.
pub fn parse_script_output(out: &str, source: MediaSource, now: f64) -> Option<(NowPlaying, Option<String>)> {
    let line = out.trim_end_matches(['\n', '\r']);
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() < 6 {
        return None;
    }
    let state = f[0].trim();
    let playing = state == "playing" || state == "kPSP";
    if !(playing || state == "paused" || state == "kPSp") {
        return None;
    }
    let np = NowPlaying {
        title: f[1].trim().to_string(),
        artist: f[2].trim().to_string(),
        album: f[3].trim().to_string(),
        app: match source {
            MediaSource::Spotify => "Spotify".into(),
            MediaSource::Music => "Music".into(),
            _ => String::new(),
        },
        playing,
        duration: num(f[4]).filter(|d| *d > 0.0),
        elapsed: num(f[5]),
        sampled_at: now,
        artwork: None,
        source,
    };
    let art = f.get(6).map(|s| s.trim().to_string()).filter(|s| s.starts_with("https://") || s.starts_with("http://"));
    np.has_track().then_some((np, art))
}

/// Seconds into the track at `now`, extrapolated while playing.
pub fn elapsed_at(np: &NowPlaying, now: f64) -> Option<f64> {
    let e = np.elapsed?;
    let e = if np.playing { e + (now - np.sampled_at).max(0.0) } else { e };
    Some(match np.duration {
        Some(d) => e.min(d),
        None => e,
    })
}

/// Progress 0..1, if the duration is known.
pub fn progress(np: &NowPlaying, now: f64) -> Option<f64> {
    let d = np.duration.filter(|d| *d > 0.0)?;
    Some((elapsed_at(np, now)? / d).clamp(0.0, 1.0))
}

/// Same track (ignoring position / play state).
pub fn same_track(a: &NowPlaying, b: &NowPlaying) -> bool {
    a.title == b.title && a.artist == b.artist && a.album == b.album && a.source == b.source
}

/// Pick between players: a playing one first, then the first paused.
pub fn choose(cands: Vec<NowPlaying>) -> Option<NowPlaying> {
    let mut cands = cands;
    if let Some(i) = cands.iter().position(|c| c.playing) {
        return Some(cands.swap_remove(i));
    }
    cands.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_music_and_spotify_output() {
        let out = "playing\tBohemian Rhapsody\tQueen\tA Night at the Opera\t354,947\t12,5\n";
        let (np, art) = parse_script_output(out, MediaSource::Music, 100.0).unwrap();
        assert!(np.playing);
        assert_eq!((np.title.as_str(), np.artist.as_str(), np.album.as_str(), np.app.as_str()), ("Bohemian Rhapsody", "Queen", "A Night at the Opera", "Music"));
        assert_eq!(np.duration, Some(354.947));
        assert_eq!(np.elapsed, Some(12.5));
        assert!(art.is_none());
        let sp = "paused\tSong\tArtist\tAlbum\t200.5\t3.0\thttps://i.scdn.co/image/ab67\n";
        let (np, art) = parse_script_output(sp, MediaSource::Spotify, 0.0).unwrap();
        assert!(!np.playing);
        assert_eq!(art.as_deref(), Some("https://i.scdn.co/image/ab67"));
        assert!(parse_script_output("stopped\n", MediaSource::Music, 0.0).is_none());
        assert!(parse_script_output("playing\t\t\t\t1\t1", MediaSource::Music, 0.0).is_none());
        assert!(parse_script_output("weird\ta\tb\tc\t1\t1", MediaSource::Music, 0.0).is_none());
        // Missing values parse as unknown.
        let (np, _) = parse_script_output("playing\tA\tB\tC\tmissing value\t-1", MediaSource::Music, 0.0).unwrap();
        assert_eq!((np.duration, np.elapsed), (None, None));
    }

    #[test]
    fn progress_extrapolates_while_playing() {
        let np = NowPlaying { title: "x".into(), playing: true, duration: Some(100.0), elapsed: Some(10.0), sampled_at: 50.0, ..Default::default() };
        assert_eq!(elapsed_at(&np, 55.0), Some(15.0));
        assert_eq!(progress(&np, 60.0), Some(0.2));
        assert_eq!(elapsed_at(&np, 500.0), Some(100.0));
        let paused = NowPlaying { playing: false, ..np.clone() };
        assert_eq!(elapsed_at(&paused, 500.0), Some(10.0));
        let unknown = NowPlaying { duration: None, ..np };
        assert_eq!(progress(&unknown, 60.0), None);
    }

    #[test]
    fn chooses_playing_player() {
        let a = NowPlaying { title: "a".into(), playing: false, ..Default::default() };
        let b = NowPlaying { title: "b".into(), playing: true, ..Default::default() };
        assert_eq!(choose(vec![a.clone(), b.clone()]).unwrap().title, "b");
        assert_eq!(choose(vec![a.clone()]).unwrap().title, "a");
        assert!(choose(vec![]).is_none());
        assert!(same_track(&a, &a.clone()));
        assert!(!same_track(&a, &b));
    }

    #[test]
    fn commands() {
        assert_eq!(Command::PlayPause.nx_key(), 16);
        assert_eq!(Command::Next.verb(), "next track");
        assert_eq!(Command::Previous.mr_command(), 5);
    }
}
