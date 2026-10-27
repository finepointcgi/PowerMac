//! The helper's line protocol.
//!
//! One ASCII command per line, one response line per command:
//!
//! | Command          | Effect                                              |
//! |------------------|-----------------------------------------------------|
//! | `PING`           | `OK PONG <version>`                                 |
//! | `STATUS`         | `OK version=2 scheme=… inhibited=0|1 discharging=0|1 sleep=restore|keep range=L-U` |
//! | `CHARGE`         | allow charging                                      |
//! | `INHIBIT`        | inhibit charging                                    |
//! | `DISCHARGE ON`   | adapter off (battery discharges while plugged in)   |
//! | `DISCHARGE OFF`  | adapter back on                                     |
//! | `SLEEP KEEP`     | keep the current state across system sleep          |
//! | `SLEEP RESTORE`  | restore charging before sleep (default)             |
//! | `RANGE <l> <u>`  | charge range used by the firmware-range scheme (`10 ≤ l < u ≤ 100`); re-applied if active |
//!
//! State-changing commands answer with the `STATUS` line. Errors are
//! `ERR <message>`. There is deliberately no command that names an SMC key
//! or a raw value; `RANGE` only takes two validated percentages.
//!
//! Version 2 added `RANGE` and the `range=` status field; a version-1
//! report parses with `range` unset.

use anyhow::{anyhow, bail, Result};

use crate::scheme::Range;

pub const PROTOCOL_VERSION: u32 = 2;
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
    Range(Range),
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
            _ => {
                if let Some(rest) = line.strip_prefix("RANGE ") {
                    let mut it = rest.split(' ');
                    let mut num = || -> Result<u8> {
                        let t = it.next().unwrap_or("");
                        if t.is_empty() || t.len() > 3 || !t.bytes().all(|b| b.is_ascii_digit()) {
                            bail!("bad RANGE");
                        }
                        Ok(t.parse()?)
                    };
                    let (l, u) = (num()?, num()?);
                    if it.next().is_some() {
                        bail!("bad RANGE");
                    }
                    Command::Range(Range::new(l, u)?)
                } else {
                    bail!("unknown command")
                }
            }
        })
    }

    /// The wire form (without the newline).
    pub fn to_line(self) -> String {
        match self {
            Command::Ping => "PING".into(),
            Command::Status => "STATUS".into(),
            Command::Charge => "CHARGE".into(),
            Command::Inhibit => "INHIBIT".into(),
            Command::DischargeOn => "DISCHARGE ON".into(),
            Command::DischargeOff => "DISCHARGE OFF".into(),
            Command::SleepKeep => "SLEEP KEEP".into(),
            Command::SleepRestore => "SLEEP RESTORE".into(),
            Command::Range(r) => format!("RANGE {} {}", r.lower, r.upper),
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
    /// Range the firmware-range scheme applies (protocol 2+).
    pub range: Option<Range>,
}

impl HelperReport {
    /// The helper found a charge-inhibit key or the firmware range
    /// (`scheme` isn't `none…`).
    pub fn can_inhibit(&self) -> bool {
        !self.scheme.is_empty() && !self.scheme.starts_with("none")
    }

    pub fn to_line(&self) -> String {
        format!(
            "OK version={} scheme={} inhibited={} discharging={} sleep={}{}",
            self.version,
            self.scheme,
            self.inhibited as u8,
            self.discharging as u8,
            if self.keep_limit_during_sleep {
                "keep"
            } else {
                "restore"
            },
            self.range
                .map(|r| format!(" range={}", r.to_wire()))
                .unwrap_or_default()
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
                "range" => r.range = Range::from_wire(v).ok(),
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
            Command::Range(Range::new(75, 80).unwrap()),
            Command::Range(Range::new(10, 100).unwrap()),
        ] {
            assert_eq!(Command::parse(&format!("{}\n", c.to_line())).unwrap(), c);
        }
        for bad in [
            "",
            "charge",
            "WRITE CHTE 00",
            "CHARGE ",
            "DISCHARGE",
            "RANGE",
            "RANGE 80",
            "RANGE 80 75",
            "RANGE 5 80",
            "RANGE 75 101",
            "RANGE 75 80 1",
            "RANGE  75 80",
            "RANGE 75 +80",
            "RANGE 0075 80",
            "RANGE 75 80 ",
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
            range: None,
        };
        assert_eq!(
            r.to_line(),
            "OK version=1 scheme=CHTE+CHIE inhibited=1 discharging=0 sleep=keep"
        );
        let r2 = HelperReport {
            version: 2,
            scheme: "bfD0/bfE0/bfF0+CHIE".into(),
            range: Some(Range::new(70, 80).unwrap()),
            ..r.clone()
        };
        assert!(r2.to_line().ends_with(" range=70-80"));
        assert_eq!(HelperReport::parse(&r2.to_line()).unwrap(), r2);
        assert_eq!(HelperReport::parse(&r.to_line()).unwrap(), r);
        assert!(r.can_inhibit());
        assert!(!HelperReport {
            scheme: "none+CHIE".into(),
            ..r.clone()
        }
        .can_inhibit());
        assert!(HelperReport::parse("ERR nope").is_err());
        assert!(check_ok("garbage").is_err());
    }
}
