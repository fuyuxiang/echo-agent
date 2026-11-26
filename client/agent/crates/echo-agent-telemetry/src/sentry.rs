use std::borrow::Cow;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use sentry::ClientInitGuard;
use sentry::ClientOptions;
use sentry::protocol::{Event, Value};

const TRACES_SAMPLE_RATE: f32 = 0.01;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

// ─── Host integration ─────────────────────────────────────────────────────

/// Per-host config; everything that varies between binaries lives here.
pub struct Config {
    /// Sentry tag `client`, e.g. `"echo-agent-pager"`.
    pub client: &'static str,
    pub client_version: &'static str,
    pub release: &'static str,
    /// When `true`, [`init`] returns a no-op guard regardless of `SENTRY_DSN`.
    pub disabled: bool,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

// ─── Public API ────────────────────────────────────────────────────────────

/// Init Sentry + apply the process-wide scope tags. Call once at process
/// start; the returned guard must outlive the process. No-op guard when
/// `config.disabled`.
pub fn init(config: Config) -> ClientInitGuard {
    let config = CONFIG.get_or_init(|| config);

    if config.disabled {
        return sentry::init(ClientOptions::default());
    }

    let dsn = std::env::var("SENTRY_DSN")
        .ok()
        .or_else(|| option_env!("SENTRY_DSN").map(|s| s.to_string()))
        .unwrap_or_default();

    let scrubber = Scrubber::from_env();

    let guard = sentry::init((
        dsn.as_str(),
        ClientOptions {
            release: Some(config.release.into()),
            send_default_pii: false,
            server_name: Some("".into()),
            attach_stacktrace: true,
            traces_sample_rate: TRACES_SAMPLE_RATE,
            environment: Some(environment().into()),
            before_send: Some(Arc::new(move |event| before_send(event, &scrubber))),
            ..Default::default()
        },
    ));

    sentry::configure_scope(|scope| {
        scope.set_tag("client", config.client);
        scope.set_tag("client_version", config.client_version);
        scope.set_tag("os", std::env::consts::OS);
        scope.set_tag("arch", std::env::consts::ARCH);
    });

    guard
}

/// Flush in-flight events. Call before `std::process::exit` in signal handlers.
pub fn flush_on_shutdown() {
    if let Some(client) = sentry::Hub::current().client() {
        client.flush(Some(FLUSH_TIMEOUT));
    }
}

// ─── Internals ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Scrubber {
    home_dir: Option<String>,
    usernames: Vec<String>,
}

impl Scrubber {
    fn from_env() -> Self {
        Self {
            home_dir: dirs::home_dir().map(|p| p.to_string_lossy().to_string()),
            usernames: collect_usernames_from_env(),
        }
    }

    fn scrub(&self, s: &str) -> String {
        let out = echo_agent_secrets::redact_secrets(s);
        let out = match self.home_dir.as_deref() {
            Some(home) => Cow::Owned(replace_home_prefix(out.as_ref(), home)),
            None => out,
        };
        redact_username_segments(out.as_ref(), &self.usernames)
    }

    fn scrub_value(&self, val: &mut Value) {
        echo_agent_secrets::walk_json_strings(val, &mut |s| *s = self.scrub(s));
    }
}

const REDACTED_USER: &str = "<user>";

/// `$USERNAME` then `$USER`, deduped, 3-char floor to avoid over-matching.
fn collect_usernames_from_env() -> Vec<String> {
    let mut usernames: Vec<String> = Vec::new();
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var) {
            let trimmed = name.trim();
            if trimmed.len() >= 3 && !usernames.iter().any(|u| u.eq_ignore_ascii_case(trimmed)) {
                usernames.push(trimmed.to_owned());
            }
        }
    }
    usernames
}

/// Replace whole `/`- or `\`-delimited segments matching any entry in
/// `usernames` with `<user>`. Substrings inside a segment are untouched.
/// Case-insensitive on Windows (NTFS), case-sensitive elsewhere (POSIX).
fn redact_username_segments(value: &str, usernames: &[String]) -> String {
    if usernames.is_empty() {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut buf = String::new();
    for ch in value.chars() {
        if ch == '/' || ch == '\\' {
            push_segment(&mut out, &buf, usernames);
            buf.clear();
            out.push(ch);
        } else {
            buf.push(ch);
        }
    }
    push_segment(&mut out, &buf, usernames);
    out
}

fn push_segment(out: &mut String, segment: &str, usernames: &[String]) {
    let matches = if cfg!(windows) {
        usernames.iter().any(|u| u.eq_ignore_ascii_case(segment))
    } else {
        usernames.iter().any(|u| u == segment)
    };
    if matches {
        out.push_str(REDACTED_USER);
    } else {
        out.push_str(segment);
    }
}

/// Whole-segment `home` -> `~` so `/Users/bob` doesn't fold over `/Users/bobby/...`.
fn replace_home_prefix(input: &str, home: &str) -> String {
    if home.is_empty() || !input.contains(home) {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(home) {
        let (before, tail) = rest.split_at(idx);
        out.push_str(before);
        let after = &tail[home.len()..];
        let prev_ok = before.chars().last().is_none_or(is_segment_boundary_char);
        let next_ok = after
            .chars()
            .next()
            .is_none_or(|c| c == '/' || c == '\\' || is_segment_boundary_char(c));
        if prev_ok && next_ok {
            out.push('~');
        } else {
            out.push_str(home);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn is_segment_boundary_char(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '(' | '[' | '{' | ',' | ':' | ';' | '=' | '<'
        )
}

fn before_send(mut event: Event<'static>, scrubber: &Scrubber) -> Option<Event<'static>> {
    if is_broken_pipe_panic(&event) {
        return None;
    }

    if let Some(ref msg) = event.message {
        event.message = Some(scrubber.scrub(msg));
    }

    for ex in &mut event.exception.values {
        if let Some(ref val) = ex.value {
            ex.value = Some(scrubber.scrub(val));
        }
        if let Some(ref mut stacktrace) = ex.stacktrace {
            for frame in &mut stacktrace.frames {
                if let Some(ref f) = frame.filename {
                    frame.filename = Some(scrubber.scrub(f));
                }
                if let Some(ref f) = frame.abs_path {
                    frame.abs_path = Some(scrubber.scrub(f));
                }
            }
        }
    }

    for bc in &mut event.breadcrumbs.values {
        if let Some(ref msg) = bc.message {
            bc.message = Some(scrubber.scrub(msg));
        }
        for val in bc.data.values_mut() {
            scrubber.scrub_value(val);
        }
    }

    event.extra.remove("cwd");
    for val in event.extra.values_mut() {
        scrubber.scrub_value(val);
    }

    for tag in event.tags.values_mut() {
        *tag = scrubber.scrub(tag);
    }

    event.server_name = None;

    Some(event)
}

/// Drop panics caused by broken pipe or disk-full, both user-environment noise.
fn is_broken_pipe_panic(event: &Event<'_>) -> bool {
    event.exception.values.iter().any(|ex| {
        ex.value.as_deref().is_some_and(|v| {
            v.contains("Broken pipe")
                || v.contains("os error 32")
                || v.contains("No space left on device")
                || v.contains("os error 28")
        })
    })
}

fn environment() -> &'static str {
    if cfg!(debug_assertions) {
        "development"
    } else {
        "production"
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // setup reads better field-by-field
mod tests {
    use super::*;
    use sentry::protocol::{Breadcrumb, Exception, Frame, Stacktrace};

    fn make_scrubber() -> Scrubber {
        Scrubber {
            home_dir: Some("/Users/alice".to_string()),
            usernames: vec!["alice".to_owned()],
        }
    }

    /// `aliceapp.log` case guards against a refactor to naive `str::replace`.
    #[test]
    fn scrub_applies_all_layers() {
        let s = make_scrubber();
        assert_eq!(s.scrub("/Users/alice/code/foo.rs"), "~/code/foo.rs");
        assert_eq!(s.scrub("/srv/alice/data"), "/srv/<user>/data");
        assert_eq!(s.scrub("aliceapp.log"), "aliceapp.log");
        assert!(s.scrub("token=longvalue123").contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn before_send_drops_broken_pipe_keeps_others() {
        let s = make_scrubber();

        let mut event = Event::default();
        event.exception.values.push(Exception {
            value: Some("Broken pipe (os error 32)".into()),
            ..Default::default()
        });
        assert!(before_send(event, &s).is_none());

        let mut event = Event::default();
        event.exception.values.push(Exception {
            value: Some("unrelated panic".into()),
            ..Default::default()
        });
        assert!(before_send(event, &s).is_some());
    }

    #[test]
    fn before_send_drops_no_space_left_panic() {
        let s = make_scrubber();
        let mut event = Event::default();
        event.exception.values.push(Exception {
            value: Some("failed printing to stderr: No space left on device (os error 28)".into()),
            ..Default::default()
        });
        assert!(before_send(event, &s).is_none());
    }

    #[test]
    fn before_send_scrubs_message_exception_stacktrace_breadcrumbs() {
        let s = make_scrubber();
        let mut event = Event::default();
        event.message = Some("error in /Users/alice/foo".into());
        event.exception.values.push(Exception {
            value: Some("/Users/alice/x failed".into()),
            stacktrace: Some(Stacktrace {
                frames: vec![Frame {
                    filename: Some("/Users/alice/src/lib.rs".into()),
                    abs_path: Some("/Users/alice/src/lib.rs".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        });
        event.breadcrumbs.values.push(Breadcrumb {
            message: Some("opened /srv/alice/log".into()),
            ..Default::default()
        });

        let out = before_send(event, &s).unwrap();
        assert_eq!(out.message.as_deref(), Some("error in ~/foo"));
        let ex = &out.exception.values[0];
        assert_eq!(ex.value.as_deref(), Some("~/x failed"));
        let frame = &ex.stacktrace.as_ref().unwrap().frames[0];
        assert_eq!(frame.filename.as_deref(), Some("~/src/lib.rs"));
        assert_eq!(frame.abs_path.as_deref(), Some("~/src/lib.rs"));
        assert_eq!(
            out.breadcrumbs.values[0].message.as_deref(),
            Some("opened /srv/<user>/log"),
        );
    }

    #[test]
    fn before_send_handles_extras_and_server_name() {
        let s = make_scrubber();
        let mut event = Event::default();
        event.extra.insert("cwd".into(), "/Users/alice/proj".into());
        event
            .extra
            .insert("other".into(), "/Users/alice/foo".into());
        event.server_name = Some("hostname.example.com".into());

        let out = before_send(event, &s).unwrap();
        assert!(!out.extra.contains_key("cwd"));
        assert_eq!(
            out.extra.get("other").and_then(|v| v.as_str()),
            Some("~/foo")
        );
        assert!(out.server_name.is_none());
    }

    #[test]
    fn before_send_scrubs_breadcrumb_data() {
        let s = make_scrubber();
        let mut event = Event::default();
        let mut bc = Breadcrumb::default();
        bc.data.insert("path".into(), "/Users/alice/foo".into());
        event.breadcrumbs.values.push(bc);

        let out = before_send(event, &s).unwrap();
        assert_eq!(
            out.breadcrumbs.values[0]
                .data
                .get("path")
                .and_then(|v| v.as_str()),
            Some("~/foo"),
        );
    }

    #[test]
    fn before_send_scrubs_tags() {
        let s = make_scrubber();
        let mut event = Event::default();
        event.tags.insert("workspace".into(), "/srv/alice/x".into());

        let out = before_send(event, &s).unwrap();
        assert_eq!(
            out.tags.get("workspace").map(String::as_str),
            Some("/srv/<user>/x")
        );
    }

    /// `/Users/bob` must not partial-match the prefix of `/Users/bobby/...`.
    #[test]
    fn home_dir_replacement_is_segment_aware() {
        let s = Scrubber {
            home_dir: Some("/Users/bob".to_string()),
            usernames: vec![],
        };
        assert_eq!(s.scrub("/Users/bobby/code"), "/Users/bobby/code");
        assert_eq!(s.scrub("opened /Users/bob/x"), "opened ~/x");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_match_is_case_sensitive() {
        let out = redact_username_segments("/Users/Alice/proj", &["alice".to_owned()]);
        assert_eq!(out, "/Users/Alice/proj");
        let out = redact_username_segments("/Users/alice/proj", &["alice".to_owned()]);
        assert_eq!(out, "/Users/<user>/proj");
    }

    #[cfg(windows)]
    #[test]
    fn windows_match_is_case_insensitive() {
        let out = redact_username_segments(r"C:\Users\Alice\proj", &["alice".to_owned()]);
        assert_eq!(out, r"C:\Users\<user>\proj");
    }
}
