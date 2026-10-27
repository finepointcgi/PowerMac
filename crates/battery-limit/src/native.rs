//! Read-only view of macOS's own Charge Limit (System Settings → Battery →
//! Charge Limit, macOS 26.4+, 80–100 %). PowerMac never changes it; it is
//! reported because it is firmware-enforced (also during sleep) and, when
//! set, takes precedence over adapter switching.

/// Parse `pmset -g battlimit` output: `Some(n)` when a limit is set
/// (`chargeSocLimitSoc = n;`), `None` for "No battery level limits set" or
/// anything unrecognised.
pub fn parse_battlimit(out: &str) -> Option<u8> {
    out.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        if k.trim() != "chargeSocLimitSoc" {
            return None;
        }
        let v = v.trim().trim_end_matches(';').trim().trim_matches('"');
        v.parse::<u8>().ok().filter(|n| (1..100).contains(n))
    })
}

/// The current macOS Charge Limit (runs `/usr/bin/pmset -g battlimit`,
/// unprivileged).
#[cfg(target_os = "macos")]
pub fn read_native_limit() -> Option<u8> {
    let out = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "battlimit"])
        .output()
        .ok()?;
    parse_battlimit(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(target_os = "macos"))]
pub fn read_native_limit() -> Option<u8> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pmset() {
        assert_eq!(parse_battlimit("No battery level limits set\n"), None);
        assert_eq!(
            parse_battlimit(
                "Battery level limits:\n{\n    chargeSocLimitReason = manualChargeLimit;\n    chargeSocLimitSoc = 80;\n}\n"
            ),
            Some(80)
        );
        assert_eq!(
            parse_battlimit("chargeSocLimitSoc = 100;"),
            None,
            "100 = no limit"
        );
        assert_eq!(parse_battlimit("chargeSocLimitSocX = 80;"), None);
    }
}
