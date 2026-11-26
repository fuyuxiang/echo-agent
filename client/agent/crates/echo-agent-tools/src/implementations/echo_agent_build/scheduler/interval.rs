use super::types::SchedulerError;

const MINIMUM_INTERVAL_SECS: u64 = 60;

/// Parse an interval string like "5m", "2h", "30s", "1d" into seconds.
/// Minimum interval is 60 seconds; values below are clamped.
pub fn parse_interval(s: &str) -> Result<u64, SchedulerError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(SchedulerError::InvalidInterval(
            "interval cannot be empty".into(),
        ));
    }

    let (digits, suffix) = s.split_at(s.len() - 1);
    let value: u64 = digits.parse().map_err(|_| {
        SchedulerError::InvalidInterval(format!(
            "invalid interval format: {s:?} (expected e.g. 5m, 2h, 1d)"
        ))
    })?;

    if value == 0 {
        return Err(SchedulerError::InvalidInterval(
            "interval value must be greater than 0".into(),
        ));
    }

    let unit_secs: u64 = match suffix {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => {
            return Err(SchedulerError::InvalidInterval(format!(
                "invalid interval suffix: {suffix:?} (expected s, m, h, or d)"
            )));
        }
    };

    let secs = value
        .checked_mul(unit_secs)
        .ok_or_else(|| SchedulerError::InvalidInterval(format!("interval too large: {s:?}")))?;

    Ok(secs.max(MINIMUM_INTERVAL_SECS))
}

/// Convert seconds to a human-readable interval string.
/// e.g. 300 -> "every 5 minutes", 3600 -> "every 1 hour"
pub fn interval_to_human(secs: u64) -> String {
    if secs.is_multiple_of(86400) {
        let n = secs / 86400;
        if n == 1 {
            "every 1 day".into()
        } else {
            format!("every {n} days")
        }
    } else if secs.is_multiple_of(3600) {
        let n = secs / 3600;
        if n == 1 {
            "every 1 hour".into()
        } else {
            format!("every {n} hours")
        }
    } else if secs.is_multiple_of(60) {
        let n = secs / 60;
        if n == 1 {
            "every 1 minute".into()
        } else {
            format!("every {n} minutes")
        }
    } else if secs == 1 {
        "every 1 second".into()
    } else {
        format!("every {secs} seconds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minutes() {
        assert_eq!(parse_interval("5m").unwrap(), 300);
        assert_eq!(parse_interval("10m").unwrap(), 600);
        assert_eq!(parse_interval("1m").unwrap(), 60);
    }

    #[test]
    fn parse_hours() {
        assert_eq!(parse_interval("2h").unwrap(), 7200);
        assert_eq!(parse_interval("1h").unwrap(), 3600);
    }

    #[test]
    fn parse_days() {
        assert_eq!(parse_interval("1d").unwrap(), 86400);
        assert_eq!(parse_interval("7d").unwrap(), 604800);
    }

    #[test]
    fn parse_seconds_clamped_to_minimum() {
        assert_eq!(parse_interval("30s").unwrap(), 60);
        assert_eq!(parse_interval("1s").unwrap(), 60);
        assert_eq!(parse_interval("60s").unwrap(), 60);
        assert_eq!(parse_interval("120s").unwrap(), 120);
    }

    #[test]
    fn parse_empty_returns_error() {
        assert!(parse_interval("").is_err());
    }

    #[test]
    fn parse_invalid_format_returns_error() {
        assert!(parse_interval("abc").is_err());
        assert!(parse_interval("5x").is_err());
        assert!(parse_interval("m").is_err());
    }

    #[test]
    fn parse_zero_returns_error() {
        assert!(parse_interval("0m").is_err());
        assert!(parse_interval("0s").is_err());
    }

    #[test]
    fn parse_overflow_returns_error() {
        // Digits parse as u64 but the unit multiplication overflows — must
        // surface an error rather than panicking (debug) or wrapping (release).
        assert!(parse_interval("1000000000000000000d").is_err());
        assert!(parse_interval(&format!("{}s", u64::MAX)).is_ok());
        assert!(parse_interval(&format!("{}d", u64::MAX)).is_err());
    }

    #[test]
    fn human_readable_minutes() {
        assert_eq!(interval_to_human(300), "every 5 minutes");
        assert_eq!(interval_to_human(60), "every 1 minute");
        assert_eq!(interval_to_human(600), "every 10 minutes");
    }

    #[test]
    fn human_readable_hours() {
        assert_eq!(interval_to_human(3600), "every 1 hour");
        assert_eq!(interval_to_human(7200), "every 2 hours");
    }

    #[test]
    fn human_readable_days() {
        assert_eq!(interval_to_human(86400), "every 1 day");
        assert_eq!(interval_to_human(172800), "every 2 days");
    }

    #[test]
    fn human_readable_seconds() {
        assert_eq!(interval_to_human(45), "every 45 seconds");
        assert_eq!(interval_to_human(1), "every 1 second");
    }

    #[test]
    fn parse_with_whitespace() {
        assert_eq!(parse_interval("  5m  ").unwrap(), 300);
    }
}
