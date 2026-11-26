//! EchoAgent Speech-to-Text language codes.
//!
//! Source of truth for the `language` query/form parameter on
//! `https://api.echo-agent.invalid/v1/stt` and `wss://api.echo-agent.invalid/v1/stt`.
//!
//! Official catalog (25 languages):
//! <https://docs.echo-agent.invalid/developers/model-capabilities/audio/speech-to-text#supported-languages>
//!
//! Per the docs, the model can transcribe these languages regardless of the
//! parameter; setting `language` enables Inverse Text Normalization (numbers,
//! currencies, units → written form) for that language. The STT API does **not**
//! accept `auto` (unlike TTS) — clients must send a concrete code. Use
//! [`language_for_api`] to resolve a stored preference (including the client-only
//! `auto` sentinel) before connecting.

/// One supported STT language from the public API catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SttLanguage {
    /// ISO / BCP-47 primary code sent as the `language` parameter (e.g. `en`).
    pub code: &'static str,
    /// English display name for UIs.
    pub name: &'static str,
}

/// Client-only sentinel meaning “resolve from the process locale at connect time”.
/// Never send this value to the STT API — use [`language_for_api`].
pub const STT_LANGUAGE_AUTO: &str = "auto";

/// Default STT language when unset or unrecognized.
pub const STT_LANGUAGE_DEFAULT: &str = "en";

/// Official EchoAgent STT languages (docs.echo-agent.invalid), sorted by English name.
///
/// Keep this list in lockstep with the public docs. Adding a code that the API
/// does not list will not break transcription, but ITN formatting may not apply.
pub const STT_LANGUAGES: &[SttLanguage] = &[
    SttLanguage {
        code: "ar",
        name: "Arabic",
    },
    SttLanguage {
        code: "cs",
        name: "Czech",
    },
    SttLanguage {
        code: "da",
        name: "Danish",
    },
    SttLanguage {
        code: "nl",
        name: "Dutch",
    },
    SttLanguage {
        code: "en",
        name: "English",
    },
    SttLanguage {
        code: "fil",
        name: "Filipino",
    },
    SttLanguage {
        code: "fr",
        name: "French",
    },
    SttLanguage {
        code: "de",
        name: "German",
    },
    SttLanguage {
        code: "hi",
        name: "Hindi",
    },
    SttLanguage {
        code: "id",
        name: "Indonesian",
    },
    SttLanguage {
        code: "it",
        name: "Italian",
    },
    SttLanguage {
        code: "ja",
        name: "Japanese",
    },
    SttLanguage {
        code: "ko",
        name: "Korean",
    },
    SttLanguage {
        code: "mk",
        name: "Macedonian",
    },
    SttLanguage {
        code: "ms",
        name: "Malay",
    },
    SttLanguage {
        code: "fa",
        name: "Persian",
    },
    SttLanguage {
        code: "pl",
        name: "Polish",
    },
    SttLanguage {
        code: "pt",
        name: "Portuguese",
    },
    SttLanguage {
        code: "ro",
        name: "Romanian",
    },
    SttLanguage {
        code: "ru",
        name: "Russian",
    },
    SttLanguage {
        code: "es",
        name: "Spanish",
    },
    SttLanguage {
        code: "sv",
        name: "Swedish",
    },
    SttLanguage {
        code: "th",
        name: "Thai",
    },
    SttLanguage {
        code: "tr",
        name: "Turkish",
    },
    SttLanguage {
        code: "vi",
        name: "Vietnamese",
    },
];

/// Look up a catalog entry by exact (case-sensitive) code.
pub fn stt_language_by_code(code: &str) -> Option<&'static SttLanguage> {
    STT_LANGUAGES.iter().find(|l| l.code == code)
}

/// Map a user/config string to a catalog code or [`STT_LANGUAGE_AUTO`].
///
/// - `None` / blank / unknown → [`STT_LANGUAGE_DEFAULT`] (`en`)
/// - `auto` (any case) → [`STT_LANGUAGE_AUTO`]
/// - Exact catalog code (any case) → that code
/// - BCP-47 / locale forms (`en-US`, `pt_BR.UTF-8`) → primary subtag when supported
/// - Common aliases: `tl` → `fil` (Tagalog → Filipino)
pub fn canonicalize_stt_language(value: Option<&str>) -> &'static str {
    let raw = value.unwrap_or_default().trim();
    if raw.is_empty() {
        return STT_LANGUAGE_DEFAULT;
    }
    if raw.eq_ignore_ascii_case(STT_LANGUAGE_AUTO) {
        return STT_LANGUAGE_AUTO;
    }

    if let Some(code) = match_supported_code(raw) {
        return code;
    }

    // Primary subtag of BCP-47 / POSIX locales.
    let primary = primary_language_subtag(raw);
    if let Some(code) = match_supported_code(primary) {
        return code;
    }
    if let Some(aliased) = alias_to_supported(primary) {
        return aliased;
    }

    STT_LANGUAGE_DEFAULT
}

/// Concrete language code to send on the STT wire.
///
/// Resolves [`STT_LANGUAGE_AUTO`] from the process locale; never returns `auto`.
pub fn language_for_api(stored: &str) -> &'static str {
    let canonical = canonicalize_stt_language(Some(stored));
    if canonical == STT_LANGUAGE_AUTO {
        system_stt_language().unwrap_or(STT_LANGUAGE_DEFAULT)
    } else {
        canonical
    }
}

/// Best-effort system locale → supported STT code (`None` if unset/unsupported).
///
/// POSIX precedence, treating set-but-empty vars as unset (an empty `LC_ALL`
/// must not mask a usable `LANG`).
fn system_stt_language() -> Option<&'static str> {
    let loc = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))?;
    if loc.eq_ignore_ascii_case("C") || loc.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let primary = primary_language_subtag(&loc);
    match_supported_code(primary).or_else(|| alias_to_supported(primary))
}

fn primary_language_subtag(raw: &str) -> &str {
    raw.split(['_', '-', '.']).next().unwrap_or("").trim()
}

fn match_supported_code(raw: &str) -> Option<&'static str> {
    STT_LANGUAGES
        .iter()
        .map(|l| l.code)
        .find(|&code| raw.eq_ignore_ascii_case(code))
}

/// Map common non-catalog primaries onto a supported code.
fn alias_to_supported(primary: &str) -> Option<&'static str> {
    // Tagalog (`tl`) is the usual system locale; API uses Filipino (`fil`).
    if primary.eq_ignore_ascii_case("tl") {
        return Some("fil");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Pins the public docs catalog (25 languages as of docs last-updated May 2026).
    const DOCS_CODES: &[&str] = &[
        "ar", "cs", "da", "nl", "en", "fil", "fr", "de", "hi", "id", "it", "ja", "ko", "mk", "ms",
        "fa", "pl", "pt", "ro", "ru", "es", "sv", "th", "tr", "vi",
    ];

    #[test]
    fn catalog_matches_public_docs_exactly() {
        let ours: HashSet<&str> = STT_LANGUAGES.iter().map(|l| l.code).collect();
        let docs: HashSet<&str> = DOCS_CODES.iter().copied().collect();
        assert_eq!(
            ours, docs,
            "STT_LANGUAGES drifted from docs.echo-agent.invalid supported languages"
        );
    }

    #[test]
    fn catalog_codes_are_unique_and_names_nonempty() {
        let mut seen = HashSet::new();
        for lang in STT_LANGUAGES {
            assert!(
                seen.insert(lang.code),
                "duplicate STT language code {}",
                lang.code
            );
            assert!(!lang.name.is_empty());
            assert!(!lang.code.is_empty());
            assert!(
                !lang.code.contains('-'),
                "use primary codes only: {}",
                lang.code
            );
        }
    }

    #[test]
    fn catalog_sorted_by_english_name() {
        let names: Vec<&str> = STT_LANGUAGES.iter().map(|l| l.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "STT_LANGUAGES must stay sorted by English name"
        );
    }

    #[test]
    fn canonicalize_known_and_unknown() {
        assert_eq!(canonicalize_stt_language(None), "en");
        assert_eq!(canonicalize_stt_language(Some("")), "en");
        assert_eq!(canonicalize_stt_language(Some("  ")), "en");
        assert_eq!(canonicalize_stt_language(Some("en")), "en");
        assert_eq!(canonicalize_stt_language(Some("ES")), "es");
        assert_eq!(canonicalize_stt_language(Some("  fr ")), "fr");
        assert_eq!(canonicalize_stt_language(Some("auto")), "auto");
        assert_eq!(canonicalize_stt_language(Some("AUTO")), "auto");
        assert_eq!(canonicalize_stt_language(Some("en-US")), "en");
        assert_eq!(canonicalize_stt_language(Some("pt_BR.UTF-8")), "pt");
        assert_eq!(canonicalize_stt_language(Some("fil")), "fil");
        assert_eq!(canonicalize_stt_language(Some("tl")), "fil");
        assert_eq!(canonicalize_stt_language(Some("tl-PH")), "fil");
        // Chinese is not in the STT formatting catalog.
        assert_eq!(canonicalize_stt_language(Some("zh")), "en");
        assert_eq!(canonicalize_stt_language(Some("zh-Hans")), "en");
        assert_eq!(canonicalize_stt_language(Some("nope")), "en");
    }

    #[test]
    fn language_for_api_never_returns_auto() {
        assert_ne!(language_for_api("auto"), "auto");
        assert_eq!(language_for_api("ja"), "ja");
        assert_eq!(language_for_api("EN"), "en");
        assert_eq!(language_for_api(""), "en");
        assert_eq!(language_for_api("xx"), "en");
    }

    #[test]
    fn lookup_is_exact_code() {
        assert!(stt_language_by_code("en").is_some());
        assert!(stt_language_by_code("EN").is_none());
        assert!(stt_language_by_code("auto").is_none());
        assert!(stt_language_by_code("zh").is_none());
    }
}
