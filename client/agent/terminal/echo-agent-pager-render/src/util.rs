//! Shared utility functions.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub use echo_agent_config::echo_agent_home;
pub use echo_agent_tools::util::format_bytes;

/// A closed stdout (`echoagent du | head`) is a clean stop, not a failure.
pub fn ignore_broken_pipe(result: std::io::Result<()>) -> std::io::Result<()> {
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// Path to `$ECHO_AGENT_HOME/pager.toml`.
pub fn pager_toml_path() -> PathBuf {
    echo_agent_home().join("pager.toml")
}

/// `~/.echo-agent` or `$ECHO_AGENT_HOME`, decided by the resolved home rather than by
/// whether `ECHO_AGENT_HOME` is set in the environment.
pub fn display_echo_agent_home_prefix() -> String {
    display_echo_agent_home_prefix_for(&echo_agent_home())
}

pub fn display_echo_agent_home_prefix_for(home: &Path) -> String {
    let default = echo_agent_config::default_echo_agent_home();
    if home == default || home == dunce::canonicalize(&default).unwrap_or(default) {
        "~/.echo-agent".to_string()
    } else {
        "$ECHO_AGENT_HOME".to_string()
    }
}

/// User-facing path under [`echo_agent_home()`], e.g. ``~/.echo-agent/config.toml``.
pub fn display_user_echo_agent_path(relative: impl AsRef<Path>) -> String {
    display_user_echo_agent_path_for(&echo_agent_home(), relative)
}

fn display_user_echo_agent_path_for(home: &Path, relative: impl AsRef<Path>) -> String {
    let rel = relative.as_ref();
    let prefix = display_echo_agent_home_prefix_for(home);
    if rel.as_os_str().is_empty() {
        return prefix;
    }
    format!("{prefix}/{}", rel.display())
}

/// Abbreviate an absolute path for display: prefer [`echo_agent_home()`], then `$HOME`.
pub fn abbreviate_path(path: &str) -> Cow<'_, str> {
    let path_buf = Path::new(path);
    let echoagent = echo_agent_home();
    if let Ok(rest) = path_buf.strip_prefix(&echoagent) {
        let prefix = display_echo_agent_home_prefix();
        if rest.as_os_str().is_empty() {
            return Cow::Owned(prefix);
        }
        return Cow::Owned(format!("{prefix}/{}", rest.display()));
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
        && let Some(rest) = path.strip_prefix(&home)
    {
        if rest.is_empty() {
            return Cow::Borrowed("~");
        }
        if rest.starts_with('/') {
            return Cow::Owned(format!("~{rest}"));
        }
    }
    Cow::Borrowed(path)
}

/// True when `path` is under user [`echo_agent_home()`] (not project `{cwd}/.echo-agent`).
pub fn is_under_user_echo_agent_home(path: &Path) -> bool {
    path.starts_with(echo_agent_home())
}

/// Compact duration: `5.2s`, `32s`, `2m5s`, `1h2m`.
pub fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs < 10 {
        return format!("{:.1}s", d.as_secs_f64());
    }
    if total_secs < 60 {
        return format!("{total_secs}s");
    }
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins < 60 {
        return format!("{mins}m{secs}s");
    }
    let hours = mins / 60;
    let remaining_mins = mins % 60;
    format!("{hours}h{remaining_mins}m")
}

/// Coarse recency for age columns: `just now`, `5m`, `3h`, `2d`, `1mo`, `1y`.
/// Buckets stay wide so the column does not churn at second granularity.
pub fn format_time_ago(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        return "just now".to_string();
    }
    if secs < 3600 {
        let mins = secs / 60;
        return format!("{mins}m");
    }
    if secs < 86400 {
        let hours = secs / 3600;
        return format!("{hours}h");
    }
    let days = secs / 86400;
    if days < 30 {
        return format!("{days}d");
    }
    if days < 365 {
        let months = days / 30;
        return format!("{months}mo");
    }
    let years = days / 365;
    format!("{years}y")
}

/// Wall-clock [`SystemTime`] from unix-epoch millis. A non-positive value is
/// the `#[serde(default)]` sentinel for a missing timestamp and reads as now.
pub fn system_time_from_unix_ms(unix_ms: i64) -> SystemTime {
    if unix_ms <= 0 {
        return SystemTime::now();
    }
    UNIX_EPOCH
        .checked_add(Duration::from_millis(unix_ms as u64))
        .unwrap_or_else(SystemTime::now)
}

/// Project a monotonic [`Instant`] onto the wall clock, so live local anchors
/// and on-disk timestamps (which can predate boot, and so have no `Instant`)
/// compare in one space. The skew between the two `now()` samples is below
/// [`format_time_ago`]'s minute granularity.
pub fn system_time_from_instant(instant: Instant) -> SystemTime {
    SystemTime::now()
        .checked_sub(instant.elapsed())
        .unwrap_or_else(SystemTime::now)
}

/// Decode the HTML entities that appear in generated session summaries.
pub fn decode_html_entities(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('&') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = s.to_string();
    out = out.replace("&amp;", "&");
    out = out.replace("&lt;", "<");
    out = out.replace("&gt;", ">");
    out = out.replace("&quot;", "\"");
    out = out.replace("&#39;", "'");
    out = out.replace("&#x27;", "'");
    out = out.replace("&apos;", "'");
    std::borrow::Cow::Owned(out)
}

pub fn parse_schedule_interval_secs(human: &str) -> Option<u64> {
    let s = human.trim_start();
    if !s.starts_with("every ") {
        return None;
    }
    let rest = s[6..].trim_start();
    let (num_str, unit) = if let Some(sp) = rest.find(char::is_whitespace) {
        (&rest[..sp], &rest[sp + 1..])
    } else if rest.len() >= 2 {
        let (d, u) = rest.split_at(rest.len() - 1);
        (d, u)
    } else {
        return None;
    };
    let n: u64 = num_str.parse().ok()?;
    let unit = unit.trim();
    let secs_per = match unit {
        "s" | "second" | "seconds" => 1,
        "m" | "minute" | "minutes" => 60,
        "h" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86400,
        _ => return None,
    };
    Some(n * secs_per)
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Compact `N ago` age relative to `now`; future timestamps saturate to
/// `0s ago`. [`format_time_ago`] buckets coarser and drops the `ago`.
pub fn format_age(created_at: i64, now: i64) -> String {
    let delta = now.saturating_sub(created_at).max(0);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86400)
    }
}

/// Truncate to at most `max_width` display columns (CJK counts 2), ending
/// with `…` when cut; a zero budget yields an empty string.
pub fn truncate_to_width(s: &str, max_width: usize) -> Cow<'_, str> {
    if byte_offset_at_width(s, max_width) == s.len() {
        return Cow::Borrowed(s);
    }
    if max_width == 0 {
        return Cow::Borrowed("");
    }
    let end = byte_offset_at_width(s, max_width - 1);
    Cow::Owned(format!("{}…", &s[..end]))
}

/// Byte offset at which display width would exceed `max_width`, or `s.len()`.
pub fn byte_offset_at_width(s: &str, max_width: usize) -> usize {
    let mut width = 0;
    for (i, ch) in s.char_indices() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_width {
            return i;
        }
        width += cw;
    }
    s.len()
}

/// Left-align `s` in `width` display columns. `format!`'s `{:<width$}` pads
/// by char count, which shears columns after wide (e.g. CJK) glyphs.
pub fn pad_to_width(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(s.width());
    let mut out = String::with_capacity(s.len() + pad);
    out.push_str(s);
    out.extend(std::iter::repeat_n(' ', pad));
    out
}

/// Group a count's digits with commas for display: `1234567` → `"1,234,567"`.
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_age_buckets_relative_to_now() {
        let now = 1_000_000;
        assert_eq!(format_age(now - 30, now), "30s ago");
        assert_eq!(format_age(now - 120, now), "2m ago");
        assert_eq!(format_age(now - 7200, now), "2h ago");
        assert_eq!(format_age(now - 172_800, now), "2d ago");
        assert_eq!(format_age(now + 60, now), "0s ago");
    }

    #[test]
    fn truncate_to_width_keeps_marker_within_budget() {
        assert_eq!(truncate_to_width("hello", 10).as_ref(), "hello");
        assert_eq!(truncate_to_width("hello world", 5).as_ref(), "hell…");
        assert_eq!(truncate_to_width("héllo wörld", 5).as_ref(), "héll…");
        assert_eq!(truncate_to_width("日本語ラベル", 5).as_ref(), "日本…");
        assert_eq!(truncate_to_width("hello", 1).as_ref(), "…");
        assert_eq!(truncate_to_width("hello", 0).as_ref(), "");
    }

    #[test]
    fn pad_to_width_pads_by_display_width() {
        assert_eq!(pad_to_width("ab", 4), "ab  ");
        assert_eq!(pad_to_width("日本", 6), "日本  ");
        assert_eq!(pad_to_width("toolong", 3), "toolong");
    }

    #[test]
    fn group_thousands_inserts_separators() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn format_duration_buckets() {
        let cases = [
            (Duration::from_millis(500), "0.5s"),
            (Duration::from_secs_f64(5.23), "5.2s"),
            (Duration::from_secs(10), "10s"),
            (Duration::from_secs_f64(12.3), "12s"),
            (Duration::from_secs(125), "2m5s"),
            (Duration::from_secs(3725), "1h2m"),
        ];
        for (d, expected) in cases {
            assert_eq!(format_duration(d), expected, "{d:?}");
        }
    }

    #[test]
    fn format_time_ago_buckets() {
        let cases = [
            (0, "just now"),
            (59, "just now"),
            (60, "1m"),
            (3599, "59m"),
            (3600, "1h"),
            (86_399, "23h"),
            (86_400, "1d"),
            (2_592_000 - 1, "29d"),
            (2_592_000, "1mo"),
            // 359d is still 11mo (359/30 = 11); 360d would be 12mo.
            (359 * 86_400, "11mo"),
            (31_536_000, "1y"),
        ];
        for (secs, expected) in cases {
            assert_eq!(
                format_time_ago(Duration::from_secs(secs)),
                expected,
                "{secs}s"
            );
        }
    }

    fn now_unix_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    // Ages beyond the machine's uptime have no monotonic `Instant`.
    #[test]
    fn system_time_from_unix_ms_renders_real_age() {
        let two_hours_ago = now_unix_ms() - 2 * 3_600_000;
        let elapsed = system_time_from_unix_ms(two_hours_ago)
            .elapsed()
            .unwrap_or_default();
        assert_eq!(format_time_ago(elapsed), "2h");

        let forty_five_days_ago = now_unix_ms() - 45 * 86_400_000;
        let elapsed = system_time_from_unix_ms(forty_five_days_ago)
            .elapsed()
            .unwrap_or_default();
        assert_eq!(format_time_ago(elapsed), "1mo");
    }

    #[test]
    fn system_time_from_unix_ms_zero_falls_back_to_now() {
        let elapsed = system_time_from_unix_ms(0).elapsed().unwrap_or_default();
        assert!(elapsed.as_secs() < 5, "zero sentinel must fall back to now");
    }

    // `elapsed()` errors on a future `SystemTime`; callers default to zero.
    #[test]
    fn system_time_from_unix_ms_future_renders_just_now() {
        let future = now_unix_ms() + 10_000_000;
        let elapsed = system_time_from_unix_ms(future)
            .elapsed()
            .unwrap_or_default();
        assert_eq!(format_time_ago(elapsed), "just now");
    }

    #[test]
    fn system_time_from_instant_reflects_elapsed() {
        let ten_min_ago = Instant::now() - Duration::from_secs(600);
        let elapsed = system_time_from_instant(ten_min_ago)
            .elapsed()
            .unwrap_or_default();
        assert_eq!(format_time_ago(elapsed), "10m");
    }

    #[test]
    fn parse_schedule_interval_secs_reads_units() {
        let cases = [
            ("every 5 minutes", Some(300)),
            ("every 5m", Some(300)),
            ("every 10s", Some(10)),
            ("every 1 hour", Some(3600)),
            ("every 1 day", Some(86400)),
            ("foo bar", None),
            ("every foo", None),
            ("every 5x", None),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_schedule_interval_secs(input), expected, "{input:?}");
        }
    }

    #[test]
    fn decode_html_entities_decodes_and_borrows() {
        let untouched = decode_html_entities("hello world");
        assert!(matches!(untouched, std::borrow::Cow::Borrowed(_)));
        assert_eq!(untouched.as_ref(), "hello world");

        let cases = [
            ("foo &amp; bar", "foo & bar"),
            ("1 &lt; 2 &amp;&amp; 3 &gt; 2", "1 < 2 && 3 > 2"),
            (
                "&quot;hello&quot; &amp; &#39;world&#39;",
                "\"hello\" & 'world'",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(decode_html_entities(input).as_ref(), expected, "{input:?}");
        }
    }

    #[test]
    fn display_echo_agent_home_prefix_default_install() {
        if std::env::var("ECHO_AGENT_HOME").is_ok() {
            return;
        }
        assert_eq!(display_echo_agent_home_prefix(), "~/.echo-agent");
    }

    #[test]
    fn display_user_echo_agent_path_joins_relative() {
        let path = display_user_echo_agent_path("config.toml");
        assert!(path.ends_with("/config.toml") || path.ends_with("\\config.toml"));
        assert!(path.contains(".echo-agent") || path.contains("$ECHO_AGENT_HOME"));
    }

    #[test]
    fn display_user_echo_agent_path_for_custom_home_uses_override_label() {
        let custom = std::env::temp_dir().join("echo-agent-home-display-regression");
        assert_eq!(
            display_user_echo_agent_path_for(&custom, "config.toml"),
            "$ECHO_AGENT_HOME/config.toml"
        );
        assert_eq!(
            display_user_echo_agent_path_for(&custom, "sandbox.toml"),
            "$ECHO_AGENT_HOME/sandbox.toml"
        );
    }

    #[test]
    fn abbreviate_path_uses_home_when_under_default_echo_agent() {
        if let Ok(home) = std::env::var("HOME") {
            if home.is_empty() {
                return;
            }
            let full = format!("{home}/.echo-agent/memory/MEMORY.md");
            let abbreviated = abbreviate_path(&full);
            assert!(
                abbreviated.contains("memory/MEMORY.md"),
                "got {abbreviated}"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn abbreviate_path_empty_home_does_not_fake_tilde() {
        let prev = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", "");
        }
        assert_eq!(abbreviate_path("/foo").as_ref(), "/foo");

        match prev {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
