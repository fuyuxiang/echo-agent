use regex::{Regex, RegexSet};
use std::borrow::Cow;
use std::sync::LazyLock;

const REDACTED: &str = "[REDACTED_SECRET]";
const REDACTED_URL_VALUE: &str = "redacted";

/// Vendor API keys with `sk-`/`sk_` prefixes and EchoAgent (`echo-agent-`) keys. `\b`-anchored so
/// `task-`/`disk-`/`risk-` don't fold a stray `sk-`.
static API_KEY_PREFIX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:sk[-_]|echo-agent-)[A-Za-z0-9_-]{20,}"));
/// AWS long-term (`AKIA`) and temporary (`ASIA`) access-key IDs.
static AWS_ACCESS_KEY_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"));
/// GitHub PATs: classic (`ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`) + fine-grained
/// (`github_pat_`).
static GITHUB_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:gh[opusr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})"));
/// GitLab (`glpat-`) and Slack (`xoxa-`/`xoxb-`/`xoxp-`/`xapp-`) tokens.
static VENDOR_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\b(?:glpat-|xox[abp]-|xapp-)[A-Za-z0-9-]{10,}"));
/// Google API keys (`AIza` + 35 chars).
static GOOGLE_API_KEY_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\bAIza[0-9A-Za-z_-]{35}"));
/// PEM private-key block (any key type), base64 body included. `(?s)` so `.`
/// spans the newline-delimited body.
static PEM_PRIVATE_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
});
static BEARER_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}\b"));
/// Bare JWT (`eyJ...header.payload.signature`) with no `Bearer`/`sk-` prefix —
/// the shape used by deployment keys and OIDC tokens.
static JWT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"));
/// 8-char value floor to avoid false positives on short values.
static SECRET_ASSIGNMENT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r#"(?ix)
        \b(
            api[_-]?key
          | (?:access|refresh|id)[_-]token
          | token
          | secret
          | client[_-]secret
          | password
        )\b
        (\s*[:=]\s*)
        (["']?)
        [^\s"',&]{8,}
        "#,
    )
});

static SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "access_token",
    "api_key",
    "assertion",
    "auth",
    "client_secret",
    "code",
    "code_verifier",
    "id_token",
    "key",
    "password",
    "refresh_token",
    "requested_token",
    "session_id",
    "state",
    "subject_token",
    "token",
];

/// Excludes trailing punctuation so backticks/brackets in surrounding text
/// don't get folded into the URL match.
static URL_REGEX: LazyLock<Regex> = LazyLock::new(|| compile(r#"https?://[^\s"'<>(){}\[\],;`]+"#));

static MATCH_ANY: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        API_KEY_PREFIX_REGEX.as_str(),
        AWS_ACCESS_KEY_REGEX.as_str(),
        GITHUB_TOKEN_REGEX.as_str(),
        VENDOR_TOKEN_REGEX.as_str(),
        GOOGLE_API_KEY_REGEX.as_str(),
        PEM_PRIVATE_KEY_REGEX.as_str(),
        BEARER_TOKEN_REGEX.as_str(),
        JWT_REGEX.as_str(),
        URL_REGEX.as_str(),
        SECRET_ASSIGNMENT_REGEX.as_str(),
    ])
    .expect("redact_secrets RegexSet")
});

pub fn redact_secrets(input: &str) -> Cow<'_, str> {
    if !MATCH_ANY.is_match(input) {
        return Cow::Borrowed(input);
    }
    let s = PEM_PRIVATE_KEY_REGEX.replace_all(input, REDACTED);
    let s = API_KEY_PREFIX_REGEX.replace_all(&s, REDACTED);
    let s = AWS_ACCESS_KEY_REGEX.replace_all(&s, REDACTED);
    let s = GITHUB_TOKEN_REGEX.replace_all(&s, REDACTED);
    let s = VENDOR_TOKEN_REGEX.replace_all(&s, REDACTED);
    let s = GOOGLE_API_KEY_REGEX.replace_all(&s, REDACTED);
    let s = BEARER_TOKEN_REGEX.replace_all(&s, format!("Bearer {REDACTED}"));
    let s = JWT_REGEX.replace_all(&s, REDACTED);
    let s = redact_urls_in(&s);
    let s = SECRET_ASSIGNMENT_REGEX
        .replace_all(&s, format!("$1$2$3{REDACTED}"))
        .into_owned();
    Cow::Owned(s)
}

/// Use [`redact_json_string_values`] for the standard scrub; use this
/// directly only when composing a custom one.
pub fn walk_json_strings(value: &mut serde_json::Value, f: &mut impl FnMut(&mut String)) {
    match value {
        serde_json::Value::String(s) => f(s),
        serde_json::Value::Array(arr) => arr.iter_mut().for_each(|v| walk_json_strings(v, f)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| walk_json_strings(v, f)),
        _ => {}
    }
}

pub fn redact_json_string_values(value: &mut serde_json::Value) {
    walk_json_strings(value, &mut |s| {
        if let Cow::Owned(replaced) = redact_secrets(s) {
            *s = replaced;
        }
    });
}

fn redact_urls_in(text: &str) -> String {
    URL_REGEX
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let raw = &caps[0];
            url::Url::parse(raw).map_or_else(
                |_| raw.to_owned(),
                |mut url| {
                    redact_url(&mut url);
                    url.to_string()
                },
            )
        })
        .into_owned()
}

const REDACTED_USER_SEGMENT: &str = "<user>";

/// Env home dir (`HOME`/`USERPROFILE`), cached for the export hot path.
static HOME_DIR: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
});

/// Env usernames (`USERNAME`/`USER`), deduped; 3-char floor avoids folding
/// short generic segments.
static USERNAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut names: Vec<String> = Vec::new();
    for var in ["USERNAME", "USER"] {
        if let Ok(name) = std::env::var(var) {
            let trimmed = name.trim();
            if trimmed.len() >= 3 && !names.iter().any(|u| u.eq_ignore_ascii_case(trimmed)) {
                names.push(trimmed.to_owned());
            }
        }
    }
    names
});

/// True for any char that can't continue a path/username segment: alphanumerics
/// and `_`/`-`/`.` continue one (`/Users/bob` won't fold into `/Users/bobby`),
/// everything else ends it (so `/Users/bob: denied` still collapses).
fn is_segment_boundary(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Backstop for headless contexts where `$HOME`/`$USER` are unset. Case-sensitive
/// (`/Users`, `/home`, `\Users`) so it won't mangle REST `/users/` paths.
static HOME_ROOT_USER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"([/\\](?:Users|home)[/\\])([^/\\]+)"));

/// Collapse `$HOME` to `~` and whole path segments equal to the OS username
/// to `<user>`.
pub fn redact_user_paths(input: &str) -> Cow<'_, str> {
    redact_user_paths_with_backstop(input, HOME_DIR.as_deref(), USERNAMES.as_slice())
}

fn redact_user_paths_with_backstop<'a>(
    input: &'a str,
    home: Option<&str>,
    usernames: &[String],
) -> Cow<'a, str> {
    let env_scrubbed = redact_user_paths_env(input, home, usernames);
    // The regex backstop runs ONLY when env is unavailable; otherwise the pass
    // above is authoritative and the regex would over-redact (`/Users/Shared`,
    // REST `/users/<id>`, etc.).
    if home.is_some() || !usernames.is_empty() {
        return env_scrubbed;
    }
    match HOME_ROOT_USER_REGEX.replace_all(env_scrubbed.as_ref(), "${1}<user>") {
        Cow::Owned(o) => Cow::Owned(o),
        Cow::Borrowed(_) => env_scrubbed,
    }
}

fn redact_user_paths_env<'a>(
    input: &'a str,
    home: Option<&str>,
    usernames: &[String],
) -> Cow<'a, str> {
    let stage1 = match home {
        Some(home) if !home.is_empty() && input.contains(home) => {
            Cow::Owned(replace_home_prefix(input, home))
        }
        _ => Cow::Borrowed(input),
    };
    if !usernames.is_empty() {
        let stage2 = redact_username_segments(stage1.as_ref(), usernames);
        if stage2 != stage1.as_ref() {
            return Cow::Owned(stage2);
        }
    }
    match stage1 {
        Cow::Owned(s) if s != input => Cow::Owned(s),
        _ => Cow::Borrowed(input),
    }
}

/// Whole-segment `home` -> `~` so `/Users/bob` doesn't fold over `/Users/bobby/...`.
fn replace_home_prefix(input: &str, home: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(home) {
        let (before, tail) = rest.split_at(idx);
        let after = &tail[home.len()..];
        let prev_ok = before.chars().last().is_none_or(is_segment_boundary);
        let next_ok = after.chars().next().is_none_or(is_segment_boundary);
        out.push_str(before);
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

/// Replace whole `/`- or `\`-delimited segments equal to a username with
/// `<user>`. Case-insensitive on Windows (NTFS), case-sensitive elsewhere.
fn redact_username_segments(value: &str, usernames: &[String]) -> String {
    let mut out = String::with_capacity(value.len());
    let mut buf = String::new();
    for ch in value.chars() {
        if is_segment_boundary(ch) {
            push_username_segment(&mut out, &buf, usernames);
            buf.clear();
            out.push(ch);
        } else {
            buf.push(ch);
        }
    }
    push_username_segment(&mut out, &buf, usernames);
    out
}

fn push_username_segment(out: &mut String, segment: &str, usernames: &[String]) {
    let matches = if cfg!(windows) {
        usernames.iter().any(|u| u.eq_ignore_ascii_case(segment))
    } else {
        usernames.iter().any(|u| u == segment)
    };
    out.push_str(if matches {
        REDACTED_USER_SEGMENT
    } else {
        segment
    });
}

/// Uses `form_urlencoded::Serializer` so the placeholder isn't percent-encoded.
pub fn redact_url(url: &mut url::Url) {
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);

    let Some(query) = url.query().map(str::to_owned) else {
        return;
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| {
            let key = k.into_owned();
            let value = if SENSITIVE_QUERY_PARAMS
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&key))
            {
                REDACTED_URL_VALUE.to_owned()
            } else {
                v.into_owned()
            };
            (key, value)
        })
        .collect();
    if pairs.is_empty() {
        url.set_query(None);
        return;
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in &pairs {
        serializer.append_pair(k, v);
    }
    url.set_query(Some(&serializer.finish()));
}

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("invalid regex `{pattern}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tripwire: if you add a regex to `MATCH_ANY`, also add a redaction
    /// pass in `redact_secrets` (and update this count).
    #[test]
    fn match_any_count_matches_redact_secrets_passes() {
        assert_eq!(MATCH_ANY.patterns().len(), 10);
    }

    #[test]
    fn no_match_returns_borrowed() {
        assert!(matches!(
            redact_secrets("just a normal log line"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            redact_secrets("model=echo-agent-3"),
            Cow::Borrowed(_)
        ));
    }

    /// Joins fixture fragments at runtime so realistic-looking fake tokens
    /// never appear contiguously in the source text. Keeps secret scanners
    /// (e.g. GitHub push protection) from flagging the redaction tests'
    /// synthetic credentials while the assembled strings still exercise the
    /// real patterns.
    fn fixture(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn redacts_known_secret_shapes() {
        let cases = [
            (
                fixture(&["key: echo-agent-", "abc123XYZdef456GHIjkl789"]),
                "echoagent api key",
            ),
            (
                fixture(&["aws AKIA", "ABCDEFGHIJKLMNOP key"]),
                "aws access key",
            ),
            (
                fixture(&["Authorization: Bearer eyJhbGciOiJIUzI1NiJ9", ".foo.bar.baz"]),
                "bearer token",
            ),
            (fixture(&["api_key=", "ABCDEFGHIJ"]), "key=value"),
            (
                fixture(&["refresh_token: \"rt_", "abc1234567\""]),
                "compound token name",
            ),
            (
                fixture(&[
                    "deployment key eyJhbGciOiJIUzI1NiJ9",
                    ".eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4f",
                ]),
                "bare jwt without prefix",
            ),
        ];
        for (input, label) in cases {
            let out = redact_secrets(&input);
            assert!(out.contains("[REDACTED_SECRET]"), "{label}: {out:?}");
        }
    }

    #[test]
    fn redacts_additional_provider_prefixes() {
        let cases = [
            (
                fixture(&["token ghp_", "0123456789abcdefghijABCDEFGHIJ012345"]),
                "github classic pat",
            ),
            (
                fixture(&[
                    "github_pat_",
                    "11ABCDE0123456789_abcdefghijklmnopqrstuvwxyz0123456789",
                ]),
                "github fine-grained pat",
            ),
            (
                fixture(&["glpat-", "0123456789abcdefABCD here"]),
                "gitlab pat",
            ),
            (
                fixture(&["xoxb-", "2420837490-2420837490-AbCdEfGhIjKlMnOpQr"]),
                "slack bot token",
            ),
            (
                fixture(&["xapp-1-", "A0123BCDEF-0123456789-abcdef0123"]),
                "slack app token",
            ),
            (
                fixture(&["AIza", "SyD0123456789abcdefghijklmnopqrstuvw"]),
                "google api key",
            ),
            (
                fixture(&["aws ASIA", "ABCDEFGHIJKLMNOP creds"]),
                "aws temporary access key",
            ),
            (
                fixture(&["stripe sk_live_", "0123456789abcdefghijABCD"]),
                "vendor sk_ key",
            ),
        ];
        for (input, label) in cases {
            let out = redact_secrets(&input);
            assert!(out.contains(REDACTED), "{label} not redacted: {out:?}");
        }
    }

    #[test]
    fn does_not_over_redact_sk_lookalikes() {
        // `\b` anchor: a stray `sk-`/`sk_` mid-word must not fold the suffix.
        for input in [
            "task-deadbeefdeadbeefdeadbeef0123",
            "disk-0123456789abcdefghijklmno",
            "risk-0123456789abcdefghijklmno",
        ] {
            assert_eq!(redact_secrets(input), input, "over-redacted: {input}");
        }
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let input = "key:\n-----BEGIN PRIVATE KEY-----\nMIIabc123def456\nMIIxyz789\n-----END PRIVATE KEY-----\ndone";
        let out = redact_secrets(input);
        assert!(out.contains(REDACTED), "PEM not redacted: {out}");
        assert!(!out.contains("MIIabc123"), "PEM body leaked: {out}");
        assert!(
            !out.contains("BEGIN PRIVATE KEY"),
            "PEM header leaked: {out}"
        );
    }

    #[test]
    fn redacts_bare_jwt_leaving_no_token() {
        let out = redact_secrets(
            "deployment key eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4f",
        );
        assert!(!out.contains("eyJ"), "bare JWT survived redaction: {out}");
    }

    #[test]
    fn redacts_sensitive_url_query_params() {
        let out = redact_secrets(
            "callback https://echo-agent.invalid/cb?code=ABC123XYZ&state=xyz789 failed",
        );
        assert!(!out.contains("ABC123XYZ"), "OAuth code leaked: {out}");
        assert!(!out.contains("xyz789"), "state leaked: {out}");
    }

    #[test]
    fn url_regex_excludes_trailing_punctuation() {
        let out = redact_secrets("see `https://echo-agent.invalid/cb?code=ABCD12345`");
        assert!(out.ends_with('`'), "trailing backtick lost: {out}");
    }

    #[test]
    fn leaves_unrelated_strings_alone() {
        let input = "https://api.example.com/v1/health?region=us-east-1";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redact_user_paths_collapses_home_and_username_segments() {
        let usernames = vec!["alice".to_owned()];
        let out = redact_user_paths_env(
            "/Users/alice/work/alice/file",
            Some("/Users/alice"),
            &usernames,
        );
        assert_eq!(out, "~/work/<user>/file");
    }

    #[test]
    fn redact_user_paths_home_prefix_matches_whole_segment_only() {
        // "/Users/bob" must not collapse inside "/Users/bobby".
        let out = redact_user_paths_env("/Users/bobby/x", Some("/Users/bob"), &[]);
        assert_eq!(out, "/Users/bobby/x");
    }

    #[test]
    fn redact_user_paths_collapses_home_and_username_before_punctuation() {
        let home = Some("/Users/alice");
        let cases = [
            ("open '/Users/alice'", "open '~'"),
            ("/Users/alice: permission denied", "~: permission denied"),
            ("/Users/alice, retrying", "~, retrying"),
            ("path /Users/alice ok", "path ~ ok"),
        ];
        for (input, want) in cases {
            assert_eq!(
                redact_user_paths_env(input, home, &[]),
                want,
                "input: {input}"
            );
        }
        assert_eq!(
            redact_user_paths_env("/data/alice: denied", None, &["alice".to_owned()]),
            "/data/<user>: denied"
        );
        // A longer segment must not fold to the shorter name.
        assert_eq!(
            redact_user_paths_env("/Users/alicia/x", home, &[]),
            "/Users/alicia/x"
        );
    }

    #[test]
    fn redact_user_paths_backstop_anonymizes_when_env_unset() {
        let out = redact_user_paths_with_backstop("/Users/realname/secret/file", None, &[]);
        assert_eq!(out, "/Users/<user>/secret/file");
    }

    #[test]
    fn redact_user_paths_backstop_skipped_when_env_known() {
        // Regression guard: when env is known the regex backstop must NOT run.
        // The backstop *would* collapse `/Users/Shared`, so it must survive here.
        let out = redact_user_paths_with_backstop(
            "/Users/Shared/cfg",
            Some("/Users/alice"),
            &["alice".to_owned()],
        );
        assert_eq!(out, "/Users/Shared/cfg");
    }

    #[test]
    fn redact_url_strips_credentials_and_fragment() {
        let mut url = url::Url::parse(
            "https://user:pw@idp.example.com/cb?code=ABC123XYZ&page=2#access_token=DEF456",
        )
        .unwrap();
        redact_url(&mut url);
        let out = url.to_string();
        assert!(!out.contains("user"), "userinfo leaked: {out}");
        assert!(!out.contains("pw"), "password leaked: {out}");
        assert!(!out.contains("ABC123XYZ"), "OAuth code leaked: {out}");
        assert!(!out.contains("DEF456"), "fragment token leaked: {out}");
        assert!(out.contains("idp.example.com/cb"), "lost host/path: {out}");
        assert!(out.contains("page=2"), "lost benign param: {out}");
        assert!(
            !out.contains("%5B") && !out.contains("%5D"),
            "placeholder bracket-encoded: {out}"
        );
    }
}
