//! Shared number and date formatting helpers (used by the HUD, the CLI
//! subcommands, and commit-date resolution).

use anyhow::{Result, bail};

/// Insert thousands separators: 1026707 -> "1,026,707".
pub fn commafy(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Compact number formatting for LoC, one decimal (e.g. 209_000 -> "209.0k",
/// 24_502 -> "24.5k", 1_600_000 -> "1.6M", 76 -> "76").
pub fn fmt_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Unix seconds -> "YYYY-MM-DD" (UTC).
pub fn fmt_date(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-from-epoch -> (year, month, day).
pub fn civil_from_days(z0: i64) -> (i64, u32, u32) {
    let z = z0 + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// (year, month, day) -> days from the Unix epoch (inverse of `civil_from_days`).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse "YYYY-MM-DD" to the Unix timestamp of the END of that day (UTC), so a
/// date selects the last commit on or before it.
pub fn parse_date_to_epoch(d: &str) -> Result<i64> {
    let parts: Vec<&str> = d.split(['-', '/']).collect();
    if parts.len() != 3 {
        bail!("date must be YYYY-MM-DD, got '{d}'");
    }
    let y: i64 = parts[0].parse()?;
    let m: u32 = parts[1].parse()?;
    let day: u32 = parts[2].parse()?;
    Ok(days_from_civil(y, m, day) * 86400 + 86399)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commafy_cases() {
        assert_eq!(commafy(0), "0");
        assert_eq!(commafy(999), "999");
        assert_eq!(commafy(1000), "1,000");
        assert_eq!(commafy(1026707), "1,026,707");
    }

    #[test]
    fn compact_cases() {
        assert_eq!(fmt_compact(76), "76");
        assert_eq!(fmt_compact(24502), "24.5k");
        assert_eq!(fmt_compact(209000), "209.0k");
        assert_eq!(fmt_compact(1_600_000), "1.6M");
    }

    #[test]
    fn date_roundtrips() {
        for &(y, m, d) in &[
            (1970, 1, 1),
            (1999, 12, 31),
            (2000, 2, 29),
            (2020, 9, 13),
            (2023, 3, 20),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
    }

    #[test]
    fn fmt_date_known() {
        assert_eq!(fmt_date(0), "1970-01-01");
        assert_eq!(fmt_date(1_600_000_000), "2020-09-13");
    }

    #[test]
    fn parse_date_cases() {
        let e = parse_date_to_epoch("2020-01-01").unwrap();
        assert_eq!(e, days_from_civil(2020, 1, 1) * 86400 + 86399);
        assert!(parse_date_to_epoch("nope").is_err());
        assert!(parse_date_to_epoch("2020-01").is_err());
    }
}
