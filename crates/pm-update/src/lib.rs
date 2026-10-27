//! Update check for PowerMac: asks GitHub Releases whether a newer version
//! than the running one exists. Nothing is downloaded or installed — the
//! caller shows the release (notes, a "Download" button that opens the page
//! or the DMG in the browser).
//!
//! * [`Version`] — semantic versions (`v1.2.3`, `1.2.3-beta.2`) with
//!   SemVer precedence.
//! * [`evaluate`] — turns an HTTP status + body into a [`CheckOutcome`] or a
//!   [`CheckError`] (no releases → [`CheckOutcome::NoReleases`], rate limits,
//!   other HTTP errors, bad JSON). Pure, unit-tested with fixtures.
//! * [`check`] — one request with the system `curl` (macOS has it, Windows
//!   10+ too), so the app needs no TLS stack of its own. Offline / DNS /
//!   timeouts come back as [`CheckError::Offline`].
//! * [`due`] — at most once a day.
//!
//! `/releases/latest` never returns pre-releases; with
//! `include_prereleases` the list endpoint is used and the highest
//! non-draft release wins.

use std::cmp::Ordering;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The repository releases are published in.
pub const REPO: &str = "finepointcgi/PowerMac";
/// `GET` the latest (non-pre-release) release.
pub const LATEST_URL: &str = "https://api.github.com/repos/finepointcgi/PowerMac/releases/latest";
/// `GET` recent releases (pre-releases included).
pub const LIST_URL: &str = "https://api.github.com/repos/finepointcgi/PowerMac/releases?per_page=15";
/// The releases page (for "no releases yet" and as a fallback link).
pub const RELEASES_PAGE: &str = "https://github.com/finepointcgi/PowerMac/releases";
/// Automatic checks run at most this often.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

// ---------------------------------------------------------------------------
// Versions

/// One dot-separated identifier of a pre-release (`beta`, `2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, o: &Self) -> Ordering {
        match (self, o) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            // Numeric identifiers have lower precedence than alphanumeric ones.
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// A semantic version. Build metadata (`+…`) is ignored, a leading `v` is
/// allowed, missing minor / patch numbers count as 0 (`v2` = `2.0.0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Empty for a normal release.
    pub pre: Vec<Ident>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            match p {
                None => Some(0),
                Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => t.parse().ok(),
                Some(_) => None,
            }
        };
        let major = match nums.next() {
            Some(t) if !t.is_empty() => num(Some(t))?,
            _ => return None,
        };
        let minor = num(nums.next())?;
        let patch = num(nums.next())?;
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let mut v = Vec::new();
                for id in p.split('.') {
                    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                        return None;
                    }
                    v.push(match id.parse::<u64>() {
                        Ok(n) if id.bytes().all(|b| b.is_ascii_digit()) => Ident::Num(n),
                        _ => Ident::Alpha(id.to_string()),
                    });
                }
                v
            }
        };
        Some(Version { major, minor, patch, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch)).then_with(|| {
            match (self.pre.is_empty(), o.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A pre-release comes before its release.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&o.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            let parts: Vec<String> = self
                .pre
                .iter()
                .map(|i| match i {
                    Ident::Num(n) => n.to_string(),
                    Ident::Alpha(a) => a.clone(),
                })
                .collect();
            write!(f, "-{}", parts.join("."))?;
        }
        Ok(())
    }
}

/// Whether `candidate` is newer than `current` (both version strings; an
/// unparsable one is never newer).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Releases

/// A published release, reduced to what the app shows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Release {
    /// The tag (`v1.2.0`).
    pub tag: String,
    /// The version without the `v` (`1.2.0`).
    pub version: String,
    pub name: String,
    /// Release notes (Markdown).
    pub notes: String,
    /// The release's page on GitHub.
    pub url: String,
    pub prerelease: bool,
    /// ISO 8601.
    pub published_at: String,
    /// The disk image for this Mac (or another macOS DMG), if attached.
    pub download_url: Option<String>,
    pub download_name: Option<String>,
    pub download_size: Option<u64>,
}

impl Release {
    /// The DMG when there is one, else the release page.
    pub fn best_link(&self) -> &str {
        self.download_url.as_deref().unwrap_or(&self.url)
    }
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// The CPU architecture names used in asset names (`arm64`, `x86_64`).
pub fn current_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => other,
    }
}

/// The best asset for `os` / `arch`: `PowerMac-1.2.0-macos-arm64.dmg`,
/// then a universal DMG, then any `.dmg` (Windows: `.zip` / `.msi` / `.exe`).
fn pick_asset<'a>(assets: &'a [GhAsset], os: &str, arch: &str) -> Option<&'a GhAsset> {
    let lower = |a: &GhAsset| a.name.to_ascii_lowercase();
    let exts: &[&str] = if os == "windows" { &[".msi", ".exe", ".zip"] } else { &[".dmg"] };
    let of_kind: Vec<&GhAsset> = assets.iter().filter(|a| exts.iter().any(|e| lower(a).ends_with(e))).collect();
    let os_tag = if os == "windows" { "windows" } else { "macos" };
    of_kind
        .iter()
        .find(|a| lower(a).contains(os_tag) && lower(a).contains(arch))
        .or_else(|| of_kind.iter().find(|a| lower(a).contains("universal")))
        .or_else(|| of_kind.iter().find(|a| lower(a).contains(os_tag)))
        .or_else(|| if os == "windows" { None } else { of_kind.first() })
        .copied()
}

fn to_release(r: GhRelease, os: &str, arch: &str) -> Release {
    let asset = pick_asset(&r.assets, os, arch);
    Release {
        version: Version::parse(&r.tag_name).map(|v| v.to_string()).unwrap_or_else(|| r.tag_name.clone()),
        name: r.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| r.tag_name.clone()),
        tag: r.tag_name,
        notes: r.body.unwrap_or_default().replace("\r\n", "\n"),
        url: r.html_url,
        prerelease: r.prerelease,
        published_at: r.published_at.unwrap_or_default(),
        download_url: asset.map(|a| a.browser_download_url.clone()),
        download_name: asset.map(|a| a.name.clone()),
        download_size: asset.map(|a| a.size).filter(|s| *s > 0),
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CheckOutcome {
    /// `latest` is not newer than the running version.
    UpToDate { latest: Release },
    Available { release: Release },
    /// The repository has no (matching) releases yet.
    NoReleases,
}

/// Why a check failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CheckError {
    /// No network, DNS failure, timeout, or no `curl`.
    Offline { message: String },
    /// GitHub's unauthenticated limit (60 requests an hour per IP);
    /// `reset` is when it resets (Unix seconds), if known.
    RateLimited { reset: Option<u64> },
    Http { status: u16, message: String },
    /// The response wasn't the JSON we expected.
    Parse { message: String },
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckError::Offline { message } => write!(f, "can't reach GitHub ({message})"),
            CheckError::RateLimited { reset: Some(r) } => {
                let mins = r.saturating_sub(now_secs()).div_ceil(60);
                write!(f, "GitHub's rate limit was reached; try again in {mins} min")
            }
            CheckError::RateLimited { reset: None } => write!(f, "GitHub's rate limit was reached; try again later"),
            CheckError::Http { status, message } if message.is_empty() => write!(f, "GitHub answered HTTP {status}"),
            CheckError::Http { status, message } => write!(f, "GitHub answered HTTP {status}: {message}"),
            CheckError::Parse { message } => write!(f, "unexpected answer from GitHub ({message})"),
        }
    }
}

impl std::error::Error for CheckError {}

/// Response metadata besides the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// `x-ratelimit-remaining`.
    pub remaining: Option<u64>,
    /// `x-ratelimit-reset` (Unix seconds).
    pub reset: Option<u64>,
}

fn message_of(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// Interpret a response of [`LATEST_URL`] (`list = false`) or [`LIST_URL`]
/// (`list = true`) for the running version `current`.
pub fn evaluate(current: &str, status: u16, body: &str, meta: &Meta, list: bool, include_prereleases: bool) -> Result<CheckOutcome, CheckError> {
    evaluate_for(current, status, body, meta, list, include_prereleases, std::env::consts::OS, current_arch())
}

/// [`evaluate`] for another OS / architecture (tests).
#[allow(clippy::too_many_arguments)]
pub fn evaluate_for(
    current: &str,
    status: u16,
    body: &str,
    meta: &Meta,
    list: bool,
    include_prereleases: bool,
    os: &str,
    arch: &str,
) -> Result<CheckOutcome, CheckError> {
    match status {
        200 => {}
        // `/releases/latest` of a repository without releases.
        404 => return Ok(CheckOutcome::NoReleases),
        403 | 429 => {
            let msg = message_of(body);
            if status == 429 || meta.remaining == Some(0) || msg.to_ascii_lowercase().contains("rate limit") {
                return Err(CheckError::RateLimited { reset: meta.reset });
            }
            return Err(CheckError::Http { status, message: msg });
        }
        s => return Err(CheckError::Http { status: s, message: message_of(body) }),
    }
    let release = if list {
        let all: Vec<GhRelease> = serde_json::from_str(body).map_err(|e| CheckError::Parse { message: e.to_string() })?;
        all.into_iter()
            .filter(|r| !r.draft && (include_prereleases || !r.prerelease))
            .filter_map(|r| Version::parse(&r.tag_name).map(|v| (v, r)))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, r)| r)
    } else {
        let r: GhRelease = serde_json::from_str(body).map_err(|e| CheckError::Parse { message: e.to_string() })?;
        (!r.draft).then_some(r)
    };
    let Some(r) = release else { return Ok(CheckOutcome::NoReleases) };
    if Version::parse(&r.tag_name).is_none() {
        return Err(CheckError::Parse { message: format!("tag {:?} isn't a version", r.tag_name) });
    }
    let rel = to_release(r, os, arch);
    Ok(if is_newer(&rel.tag, current) { CheckOutcome::Available { release: rel } } else { CheckOutcome::UpToDate { latest: rel } })
}

// ---------------------------------------------------------------------------
// Fetching

/// Options of one check.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    /// The running version (`env!("CARGO_PKG_VERSION")`).
    pub current: String,
    pub include_prereleases: bool,
    pub timeout: Duration,
}

impl CheckOptions {
    pub fn new(current: &str, include_prereleases: bool) -> Self {
        CheckOptions { current: current.to_string(), include_prereleases, timeout: Duration::from_secs(20) }
    }
}

/// Marker between the body and curl's `--write-out` line.
const TRAILER: &str = "\n__PM_UPDATE__ ";

/// Split curl's output (body + trailer) into (status, meta, body).
pub fn parse_curl_output(out: &str) -> Option<(u16, Meta, &str)> {
    let i = out.rfind(TRAILER)?;
    let (body, rest) = (&out[..i], &out[i + TRAILER.len()..]);
    let mut w = rest.split_whitespace();
    let status: u16 = w.next()?.parse().ok()?;
    let num = |s: Option<&str>| s.and_then(|s| s.trim().parse::<u64>().ok());
    let meta = Meta { remaining: num(w.next()), reset: num(w.next()) };
    Some((status, meta, body))
}

/// One request to GitHub with `curl`.
pub fn fetch(url: &str, user_agent: &str, timeout: Duration) -> Result<(u16, Meta, String), CheckError> {
    let mut cmd = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "/usr/bin/curl" });
    if cfg!(not(any(target_os = "macos", windows))) {
        cmd = std::process::Command::new("curl");
    }
    cmd.args(["-sS", "-L", "--max-time"])
        .arg(timeout.as_secs().max(1).to_string())
        .args(["-H", "Accept: application/vnd.github+json", "-H", "X-GitHub-Api-Version: 2022-11-28", "-H"])
        .arg(format!("User-Agent: {user_agent}"))
        .arg("-w")
        .arg(format!("{TRAILER}%{{http_code}} %header{{x-ratelimit-remaining}} %header{{x-ratelimit-reset}}"))
        .arg(url)
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| CheckError::Offline { message: format!("curl: {e}") })?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let code = out.status.code().unwrap_or(-1);
        // 6 resolve, 7 connect, 28 timeout, 35 TLS handshake, 56 recv failure.
        let msg = if err.is_empty() { format!("curl exit code {code}") } else { err };
        return Err(CheckError::Offline { message: msg });
    }
    match parse_curl_output(&text) {
        Some((status, meta, body)) => Ok((status, meta, body.to_string())),
        None => Err(CheckError::Parse { message: "no status from curl".into() }),
    }
}

/// Check GitHub now (one request).
pub fn check(opts: &CheckOptions) -> Result<CheckOutcome, CheckError> {
    let list = opts.include_prereleases;
    let url = if list { LIST_URL } else { LATEST_URL };
    let ua = format!("PowerMac/{} ({}; {})", opts.current, std::env::consts::OS, current_arch());
    log::info!("update check: GET {url}");
    let (status, meta, body) = fetch(url, &ua, opts.timeout)?;
    evaluate(&opts.current, status, &body, &meta, list, opts.include_prereleases)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether an automatic check is due (`last_check` in Unix seconds; a
/// clock that went backwards counts as due).
pub fn due(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        None => true,
        Some(t) if t > now => true,
        Some(t) => now - t >= CHECK_INTERVAL.as_secs(),
    }
}

/// The last check's result, as stored for the tray and the settings window.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateState {
    /// Unix seconds.
    pub checked_at: u64,
    /// The version that checked.
    pub current: String,
    pub outcome: Option<CheckOutcome>,
    /// Set when the check failed (the previous outcome is kept).
    pub error: Option<String>,
}

impl UpdateState {
    /// The release to offer, if one is newer than `current` (re-evaluated,
    /// so an update installed meanwhile isn't offered again).
    pub fn available(&self, current: &str) -> Option<&Release> {
        match &self.outcome {
            Some(CheckOutcome::Available { release }) if is_newer(&release.tag, current) => Some(release),
            _ => None,
        }
    }

    /// Record `r` (checked now by `current`).
    pub fn record(&mut self, current: &str, r: Result<CheckOutcome, CheckError>) {
        self.checked_at = now_secs();
        self.current = current.to_string();
        match r {
            Ok(o) => {
                self.outcome = Some(o);
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// One line for the UI ("PowerMac 1.2.0 is available", "You're up to date", …).
    pub fn summary(&self, current: &str) -> String {
        if let Some(r) = self.available(current) {
            return format!("PowerMac {} is available (you have {current}).", r.version);
        }
        if let Some(e) = &self.error {
            return format!("The last check failed: {e}.");
        }
        match &self.outcome {
            Some(CheckOutcome::NoReleases) => "No releases have been published yet.".into(),
            Some(_) => format!("You're up to date (PowerMac {current})."),
            None => "Not checked yet.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATEST: &str = include_str!("fixtures/latest.json");
    const LIST: &str = include_str!("fixtures/list.json");
    const NOT_FOUND: &str = include_str!("fixtures/not_found.json");
    const RATE: &str = include_str!("fixtures/rate_limited.json");

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn version_parsing() {
        assert_eq!(v("v1.2.3"), Version { major: 1, minor: 2, patch: 3, pre: vec![] });
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("V2"), v("2.0.0"));
        assert_eq!(v("1.0.0+build.7"), v("1.0.0"));
        assert_eq!(v("1.0.0-beta.2").pre, vec![Ident::Alpha("beta".into()), Ident::Num(2)]);
        assert_eq!(v("1.0.0-rc.1").to_string(), "1.0.0-rc.1");
        for bad in ["", "v", "1..2", "1.2.3.4", "x.1.2", "1.2.3-", "1.2.3-be$ta", "latest", "1.-2"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn version_precedence() {
        // The SemVer 2.0 example chain.
        let chain = [
            "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0",
            "1.0.1", "1.1.0", "1.10.0", "2.0.0",
        ];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
        assert!(!is_newer("v0.9.9", "1.0.0"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(is_newer("1.1.0-beta.1", "1.0.0"));
        assert!(!is_newer("nightly", "1.0.0"));
        assert!(!is_newer("2.0.0", "garbage"));
    }

    #[test]
    fn latest_release_fixture() {
        let o = evaluate_for("1.0.0", 200, LATEST, &Meta::default(), false, false, "macos", "arm64").unwrap();
        let CheckOutcome::Available { release } = o else { panic!("{o:?}") };
        assert_eq!(release.tag, "v1.2.0");
        assert_eq!(release.version, "1.2.0");
        assert_eq!(release.name, "PowerMac 1.2.0");
        assert!(release.notes.starts_with("### Added\n- **Workspaces**"), "{}", release.notes);
        assert_eq!(release.url, "https://github.com/finepointcgi/PowerMac/releases/tag/v1.2.0");
        assert_eq!(release.download_name.as_deref(), Some("PowerMac-1.2.0-macos-arm64.dmg"));
        assert_eq!(release.download_size, Some(9_437_184));
        assert_eq!(release.best_link(), "https://github.com/finepointcgi/PowerMac/releases/download/v1.2.0/PowerMac-1.2.0-macos-arm64.dmg");
        // Intel Macs get the universal image; Windows has no asset here.
        let o = evaluate_for("1.0.0", 200, LATEST, &Meta::default(), false, false, "macos", "x86_64").unwrap();
        let CheckOutcome::Available { release } = o else { panic!() };
        assert_eq!(release.download_name.as_deref(), Some("PowerMac-1.2.0-macos-universal.dmg"));
        let o = evaluate_for("1.0.0", 200, LATEST, &Meta::default(), false, false, "windows", "x86_64").unwrap();
        let CheckOutcome::Available { release } = o else { panic!() };
        assert_eq!(release.download_url, None);
        assert_eq!(release.best_link(), release.url);
        // Same or newer version running: up to date.
        for cur in ["1.2.0", "1.3.0-beta.1"] {
            let o = evaluate_for(cur, 200, LATEST, &Meta::default(), false, false, "macos", "arm64").unwrap();
            assert!(matches!(o, CheckOutcome::UpToDate { .. }), "{cur}: {o:?}");
        }
    }

    #[test]
    fn release_list_fixture() {
        // Without pre-releases: 1.2.0 (the draft 9.9.9 and the pre-release 1.3.0-beta.1 are skipped).
        let o = evaluate_for("1.0.0", 200, LIST, &Meta::default(), true, false, "macos", "arm64").unwrap();
        let CheckOutcome::Available { release } = o else { panic!("{o:?}") };
        assert_eq!(release.tag, "v1.2.0");
        let o = evaluate_for("1.0.0", 200, LIST, &Meta::default(), true, true, "macos", "arm64").unwrap();
        let CheckOutcome::Available { release } = o else { panic!() };
        assert_eq!((release.tag.as_str(), release.prerelease), ("v1.3.0-beta.1", true));
        assert_eq!(release.name, "v1.3.0-beta.1", "an empty name falls back to the tag");
        assert_eq!(release.download_name.as_deref(), Some("PowerMac-1.3.0-beta.1.dmg"));
        // An empty list: no releases.
        assert_eq!(evaluate_for("1.0.0", 200, "[]", &Meta::default(), true, true, "macos", "arm64"), Ok(CheckOutcome::NoReleases));
    }

    #[test]
    fn errors() {
        assert_eq!(evaluate("1.0.0", 404, NOT_FOUND, &Meta::default(), false, false), Ok(CheckOutcome::NoReleases));
        let meta = Meta { remaining: Some(0), reset: Some(1_800_000_000) };
        assert_eq!(evaluate("1.0.0", 403, RATE, &meta, false, false), Err(CheckError::RateLimited { reset: Some(1_800_000_000) }));
        assert_eq!(evaluate("1.0.0", 403, RATE, &Meta::default(), false, false), Err(CheckError::RateLimited { reset: None }));
        assert_eq!(evaluate("1.0.0", 429, "", &Meta::default(), false, false), Err(CheckError::RateLimited { reset: None }));
        assert_eq!(
            evaluate("1.0.0", 403, r#"{"message":"Resource not accessible"}"#, &Meta { remaining: Some(42), reset: None }, false, false),
            Err(CheckError::Http { status: 403, message: "Resource not accessible".into() })
        );
        assert!(matches!(evaluate("1.0.0", 502, "<html>", &Meta::default(), false, false), Err(CheckError::Http { status: 502, .. })));
        assert!(matches!(evaluate("1.0.0", 200, "<html>", &Meta::default(), false, false), Err(CheckError::Parse { .. })));
        assert!(matches!(
            evaluate("1.0.0", 200, r#"{"tag_name":"nightly","html_url":"x"}"#, &Meta::default(), false, false),
            Err(CheckError::Parse { .. })
        ));
        assert!(CheckError::RateLimited { reset: None }.to_string().contains("rate limit"));
        assert!(CheckError::Offline { message: "Could not resolve host".into() }.to_string().contains("can't reach GitHub"));
    }

    #[test]
    fn curl_output() {
        let out = format!("{{\"a\":1}}{TRAILER}200 59 1800000000");
        let (s, m, b) = parse_curl_output(&out).unwrap();
        assert_eq!((s, m, b), (200, Meta { remaining: Some(59), reset: Some(1_800_000_000) }, "{\"a\":1}"));
        // Headers missing (older curl prints nothing for %header{}).
        let out = format!("[]{TRAILER}404  ");
        let (s, m, b) = parse_curl_output(&out).unwrap();
        assert_eq!((s, m, b), (404, Meta::default(), "[]"));
        assert!(parse_curl_output("no trailer").is_none());
    }

    #[test]
    fn once_a_day() {
        let day = CHECK_INTERVAL.as_secs();
        assert!(due(None, 1000));
        assert!(!due(Some(1000), 1000 + day - 1));
        assert!(due(Some(1000), 1000 + day));
        assert!(due(Some(5000), 1000), "clock went backwards");
    }

    #[test]
    fn state_summary() {
        let mut st = UpdateState::default();
        assert_eq!(st.summary("1.0.0"), "Not checked yet.");
        let rel = Release { tag: "v1.2.0".into(), version: "1.2.0".into(), ..Default::default() };
        st.record("1.0.0", Ok(CheckOutcome::Available { release: rel }));
        assert_eq!(st.available("1.0.0").map(|r| r.version.as_str()), Some("1.2.0"));
        assert!(st.summary("1.0.0").contains("1.2.0 is available"));
        // Updated meanwhile: no longer offered.
        assert!(st.available("1.2.0").is_none());
        st.record("1.0.0", Err(CheckError::Offline { message: "offline".into() }));
        assert!(st.available("1.0.0").is_some(), "a failed check keeps the last outcome");
        assert!(st.error.is_some());
        st.record("1.0.0", Ok(CheckOutcome::NoReleases));
        assert_eq!(st.summary("1.0.0"), "No releases have been published yet.");
        let json = serde_json::to_string(&st).unwrap();
        assert!(json.contains("\"kind\":\"noReleases\""), "{json}");
        assert_eq!(serde_json::from_str::<UpdateState>(&json).unwrap(), st);
    }
}
