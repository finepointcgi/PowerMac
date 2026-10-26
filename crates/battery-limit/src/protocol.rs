//! The helper's line protocol.
//!
//! One ASCII command per line, one response line per command:
//!
//! | Command          | Effect                                              |
//! |------------------|-----------------------------------------------------|
//! | `PING`           | `OK PONG <version>`                                 |
//! | `STATUS`         | `OK version=1 scheme=… inhibited=0|1 discharging=0|1 sleep=restore|keep` |
//! | `CHARGE`         | allow charging                                      |
//! | `INHIBIT`        | inhibit charging                                    |
//! | `DISCHARGE ON`   | adapter off (battery discharges while plugged in)   |
//! | `DISCHARGE OFF`  | adapter back on                                     |
//! | `SLEEP KEEP`     | keep the current state across system sleep          |
//! | `SLEEP RESTORE`  | restore charging before sleep (default)             |
//!
//! State-changing commands answer with the `STATUS` line. Errors are
//! `ERR <message>`. There is deliberately no command that names an SMC key
//! or value.

use anyhow::{anyhow, bail, Result};

pub const PROTOCOL_VERSION: u32 = 1;
/// Longest accepted command line.
pub const MAX_LINE: usize = 64;
/// Most commands accepted on one connection.
pub const MAX_COMMANDS_PER_CONNECTION: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Ping,
    Status,
    Charge,
    Inhibit,
    DischargeOn,
    DischargeOff,
    SleepKeep,
    SleepRestore,
}

impl Command {
    pub fn parse(line: &str) -> Result<Command> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.len() > MAX_LINE {
            bail!("line too long");
        }
        Ok(match line {
            "PING" => Command::Ping,
            "STATUS" => Command::Status,
            "CHARGE" => Command::Charge,
            "INHIBIT" => Command::Inhibit,
            "DISCHARGE ON" => Command::DischargeOn,
            "DISCHARGE OFF" => Command::DischargeOff,
            "SLEEP KEEP" => Command::SleepKeep,
            "SLEEP RESTORE" => Command::SleepRestore,
            _ => bail!("unknown command"),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Command::Ping => "PING",
            Command::Status => "STATUS",
            Command::Charge => "CHARGE",
            Command::Inhibit => "INHIBIT",
            Command::DischargeOn => "DISCHARGE ON",
            Command::DischargeOff => "DISCHARGE OFF",
            Command::SleepKeep => "SLEEP KEEP",
            Command::SleepRestore => "SLEEP RESTORE",
        }
    }
}

/// The helper's state as reported by `STATUS`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperReport {
    pub version: u32,
    pub scheme: String,
    pub inhibited: bool,
    pub discharging: bool,
    pub keep_limit_during_sleep: bool,
}

impl HelperReport {
    /// The helper found a charge-inhibit key (`scheme` isn't `none…`).
    pub fn can_inhibit(&self) -> bool {
        !self.scheme.is_empty() && !self.scheme.starts_with("none")
    }

    pub fn to_line(&self) -> String {
        format!(
            "OK version={} scheme={} inhibited={} discharging={} sleep={}",
            self.version,
            self.scheme,
            self.inhibited as u8,
            self.discharging as u8,
            if self.keep_limit_during_sleep {
                "keep"
            } else {
                "restore"
            }
        )
    }

    pub fn parse(line: &str) -> Result<HelperReport> {
        let rest = check_ok(line)?;
        let mut r = HelperReport::default();
        for kv in rest.split_whitespace() {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| anyhow!("bad field {kv:?}"))?;
            match k {
                "version" => r.version = v.parse()?,
                "scheme" => r.scheme = v.to_string(),
                "inhibited" => r.inhibited = v == "1",
                "discharging" => r.discharging = v == "1",
                "sleep" => r.keep_limit_during_sleep = v == "keep",
                _ => {}
            }
        }
        Ok(r)
    }
}

/// `"OK rest"` → `"rest"`, `"ERR msg"` → error.
pub fn check_ok(line: &str) -> Result<&str> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix("OK") {
        Ok(rest.trim_start())
    } else if let Some(msg) = line.strip_prefix("ERR") {
        bail!("helper: {}", msg.trim())
    } else {
        bail!("helper: unexpected response {line:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for c in [
            Command::Ping,
            Command::Status,
            Command::Charge,
            Command::Inhibit,
            Command::DischargeOn,
            Command::DischargeOff,
            Command::SleepKeep,
            Command::SleepRestore,
        ] {
            assert_eq!(Command::parse(&format!("{}\n", c.as_str())).unwrap(), c);
        }
        for bad in [
            "",
            "charge",
            "WRITE CHTE 00",
            "CHARGE ",
            "DISCHARGE",
            &"A".repeat(100),
        ] {
            assert!(Command::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn report_roundtrip() {
        let r = HelperReport {
            version: 1,
            scheme: "CHTE+CHIE".into(),
            inhibited: true,
            discharging: false,
            keep_limit_during_sleep: true,
        };
        assert_eq!(
            r.to_line(),
            "OK version=1 scheme=CHTE+CHIE inhibited=1 discharging=0 sleep=keep"
        );
        assert_eq!(HelperReport::parse(&r.to_line()).unwrap(), r);
        assert!(r.can_inhibit());
        assert!(!HelperReport {
            scheme: "none+CHIE".into(),
            ..r
        }
        .can_inhibit());
        assert!(HelperReport::parse("ERR nope").is_err());
        assert!(check_ok("garbage").is_err());
    }
}
