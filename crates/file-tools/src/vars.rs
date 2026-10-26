//! Date/time variables shared by PowerRename and New+, with the PowerToys
//! semantics (`GetDatedFileName`): `$YYYY $YY $Y $MMMM $MMM $MM $M $DDDD $DDD
//! $DD $D $HH $H $TT $tt $hh $h $mm $m $ss $s $fff $ff $f`. Longer tokens win
//! over their prefixes, and `$$` escapes a dollar (it is left as `$$` here; the
//! regex replacement turns it into `$`). Month and day names are English.

use chrono::{DateTime, Datelike, Local, Timelike};

/// One date/time token (without the `$`) and how to format it.
const TOKENS: &[&str] = &[
    "YYYY", "YY", "Y", "MMMM", "MMM", "MM", "M", "DDDD", "DDD", "DD", "D", "HH", "H", "TT", "tt",
    "hh", "h", "mm", "m", "ss", "s", "fff", "ff", "f",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The value of date token `tok` (one of [`TOKENS`]) at `t`.
pub fn format_token(tok: &str, t: &DateTime<Local>) -> String {
    let h12 = match t.hour() % 12 {
        0 => 12,
        h => h,
    };
    let ms = t.timestamp_subsec_millis().min(999);
    let month = MONTHS[t.month0() as usize];
    let day = DAYS[t.weekday().num_days_from_monday() as usize];
    match tok {
        "YYYY" => format!("{:04}", t.year()),
        "YY" => format!("{:02}", t.year().rem_euclid(100)),
        "Y" => format!("{}", t.year().rem_euclid(10)),
        "MMMM" => month.to_string(),
        "MMM" => month[..3].to_string(),
        "MM" => format!("{:02}", t.month()),
        "M" => format!("{}", t.month()),
        "DDDD" => day.to_string(),
        "DDD" => day[..3].to_string(),
        "DD" => format!("{:02}", t.day()),
        "D" => format!("{}", t.day()),
        "HH" => format!("{h12:02}"),
        "H" => format!("{h12}"),
        "TT" => if t.hour() < 12 { "AM" } else { "PM" }.into(),
        "tt" => if t.hour() < 12 { "am" } else { "pm" }.into(),
        "hh" => format!("{:02}", t.hour()),
        "h" => format!("{}", t.hour()),
        "mm" => format!("{:02}", t.minute()),
        "m" => format!("{}", t.minute()),
        "ss" => format!("{:02}", t.second()),
        "s" => format!("{}", t.second()),
        "fff" => format!("{ms:03}"),
        "ff" => format!("{:02}", ms / 10),
        "f" => format!("{}", ms / 100),
        _ => String::new(),
    }
}

/// Whether `s` uses any date/time variable (PowerToys `isFileTimeUsed`).
pub fn uses_date_vars(s: &str) -> bool {
    expand_date_vars(s, &Local::now()) != s
}

/// Replace the date/time variables in `s` (PowerToys name semantics: a
/// token is matched as a prefix, so `$YYYY$MM$DD` and `$YYYYa` both work).
/// `$$` pairs are kept as they are.
pub fn expand_date_vars(s: &str, t: &DateTime<Local>) -> String {
    expand(s, t, false)
}

/// [`expand_date_vars`] for file *contents*: only multi-letter tokens, and
/// only when not followed by a letter, digit or `_`, so `$HOME` or `$s` in a
/// script are left alone.
pub fn expand_date_vars_in_text(s: &str, t: &DateTime<Local>) -> String {
    expand(s, t, true)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn expand(s: &str, t: &DateTime<Local>, text_mode: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let b = s.as_bytes();
    while i < s.len() {
        if b[i] != b'$' {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        if text_mode && i > 0 && b[i - 1] == b'\\' {
            out.push('$');
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'$') {
            out.push_str("$$");
            i += 2;
            continue;
        }
        let rest = &s[i + 1..];
        let tok = TOKENS.iter().find(|tok| {
            rest.starts_with(**tok)
                && (!text_mode || (tok.len() > 1 && !rest[tok.len()..].starts_with(is_word)))
        });
        match tok {
            Some(tok) => {
                out.push_str(&format_token(tok, t));
                i += 1 + tok.len();
            }
            None => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2024, 3, 7, 15, 4, 9)
            .unwrap()
            .checked_add_signed(chrono::Duration::milliseconds(123))
            .unwrap()
    }

    #[test]
    fn tokens() {
        let t = t();
        assert_eq!(
            expand_date_vars("$YYYY-$MM-$DD $hh.$mm.$ss.$fff", &t),
            "2024-03-07 15.04.09.123"
        );
        assert_eq!(
            expand_date_vars("$YY$Y $M $D $h:$m:$s", &t),
            "244 3 7 15:4:9"
        );
        assert_eq!(
            expand_date_vars("$MMMM $MMM $DDDD $DDD", &t),
            "March Mar Thursday Thu"
        );
        assert_eq!(
            expand_date_vars("$HH $H $TT $tt $ff $f", &t),
            "03 3 PM pm 12 1"
        );
        assert_eq!(expand_date_vars("$YYYYMMDD", &t), "2024MMDD");
    }

    #[test]
    fn escapes_and_unknowns() {
        let t = t();
        assert_eq!(expand_date_vars("$$YYYY", &t), "$$YYYY");
        assert_eq!(expand_date_vars("$$$YYYY", &t), "$$2024");
        assert_eq!(expand_date_vars("$1 $& $x", &t), "$1 $& $x");
        assert_eq!(expand_date_vars("€$YY€", &t), "€24€");
        assert!(uses_date_vars("a $YYYY"));
        assert!(!uses_date_vars("a $$YYYY $1"));
    }

    #[test]
    fn text_mode_is_conservative() {
        let t = t();
        assert_eq!(
            expand_date_vars_in_text("echo $HOME $s $YYYY-$MM-$DD $hh$mm \\$YYYY", &t),
            "echo $HOME $s 2024-03-07 1504 \\$YYYY"
        );
    }
}
