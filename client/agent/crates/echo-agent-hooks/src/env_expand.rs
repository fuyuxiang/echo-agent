//! Environment variable expansion helper for hook config strings.
//!
//! Provides `${VAR}` / `$VAR` substitution that prefers a per-hook
//! `extra_env` map over the process environment. Used by:
//!
//! * the JSON hook parser ([`crate::config::parse_hook_file`]) to expand
//!   `command` and `url` fields at config-load time, and
//! * the HTTP runner ([`crate::runner::http`]) to expand `spec.url` once
//!   more right before SSRF validation, so plugin URLs that reference
//!   plugin-injected vars (e.g. `${CLAUDE_PLUGIN_ROOT}/check`) resolve.
//!
//! The expansion is **lossless on missing vars and on every parameter-
//! expansion-modifier form** -- both unset plain references (e.g.
//! `${UNSET}/x`) AND any modifier form (e.g. `${VAR:-default}`,
//! `${VAR%pat}`, see the "Parameter-expansion forms" paragraph below)
//! are preserved verbatim. This is important so that:
//!
//! * config-load-time expansion is idempotent (re-running it on an already
//!   expanded string is a no-op),
//! * vars that are intentionally deferred to runtime (set later by the
//!   shell, the dispatcher, or `extra_env`) survive the load-time pass and
//!   are caught by the runtime pre-flight check in
//!   [`crate::runner::command`] if they remain unset at execution, and
//! * shell-specific modifier semantics (especially `${VAR:-x}` for
//!   set-but-empty values) stay the responsibility of the runtime
//!   `sh -c` branch where they apply correctly.
//!
//! Parameter-expansion forms (`${VAR:-default}`, `${VAR-default}`,
//! `${VAR:=x}`, `${VAR:?msg}`, `${VAR:+x}`, `${VAR%pat}`, `${VAR#pat}`,
//! `${VAR/pat/repl}`, `${VAR:N}`, `${VAR:N:M}`) are also preserved
//! verbatim. These forms have shell-specific semantics (notably the
//! "set-but-empty" behaviour of `:-` differs between `sh` and the
//! shellexpand crate) that the runtime `sh -c` branch resolves
//! correctly. Mirroring the modifier-skip behaviour in
//! [`crate::runner::command::find_unresolved_env_vars`] keeps the two
//! layers in sync: the user wrote the modifier form because they wanted
//! the shell's interpretation, so we leave it for the shell.
//!
//! Same underlying engine (`shellexpand::env_with_context_no_errors`)
//! and same lossless-on-missing semantics as
//! `echo_agent_config::expand_env_vars_in_string`, but with an additional
//! per-hook `extra` map consulted before process env, and with the
//! parameter-expansion-modifier preservation described above.
//!
//! ## Asymmetry between `command` and `url`
//!
//! Load-time expansion in [`crate::config::parse_hook_file`] runs once
//! using a snapshot of process env at parse time. The HTTP runner does
//! a second pass at runtime so plugin-injected vars that arrive in
//! `extra_env` after parsing (e.g. `CLAUDE_PLUGIN_ROOT`) resolve, and
//! so mid-session changes to process env are picked up for URLs.
//! Command paths are not value-expanded at spawn. Unix `sh -c` expands `$VAR`
//! from the child env; Windows PowerShell rewrites known `$VAR` to `$env:VAR`.

use std::collections::HashMap;

/// Sentinel prefix for the per-call mask sentinel; see [`make_sentinel`].
///
/// Uses a Unicode Private Use Area code point (`U+F8FF`, the
/// "Apple logo" PUA char) plus a long magic ASCII prefix. The full
/// sentinel string adds 128 bits of per-call entropy as a hex suffix
/// followed by another `U+F8FF` char.
const SENTINEL_PREFIX: &str = "\u{f8ff}__ECHO_AGENT_HOOKS_MASK_";
const SENTINEL_SUFFIX: &str = "__\u{f8ff}";

/// Build a per-call sentinel string used to hide modifier-form
/// `${...}` substrings from `shellexpand::env_with_context_no_errors`.
/// The sentinel is restored to `${` after shellexpand runs, so the
/// modifier form survives expansion verbatim.
///
/// The sentinel is randomized on every call: 128 bits of entropy
/// from `fastrand` are formatted as hex between the fixed
/// [`SENTINEL_PREFIX`] / [`SENTINEL_SUFFIX`] markers. The chance of
/// a natural collision with arbitrary user-supplied input or a
/// modifier body is ~2^-128, removing the sentinel-substring
/// rewrite hazard that a fixed-string sentinel had.
///
/// Properties:
///
/// * **Unambiguous** -- per-call randomization makes accidental
///   collision with any real hook command/URL string or value
///   extracted from `extra_env` vanishingly unlikely.
/// * **UTF-8 safe** -- the leading and trailing PUA chars are 3-byte
///   UTF-8 sequences; the middle is ASCII hex.
/// * **Visually distinct in panic messages / logs** if a sentinel
///   ever escapes back to the user (it shouldn't, but if it does
///   the magic string makes triage immediate).
///
/// Replaces a previous fixed sentinel (and an even earlier 2-NUL-byte
/// sentinel `"\u{0}\u{0}"`) which could collide with a hand-crafted
/// `extra_env` value or modifier body containing the same byte
/// sequence; see the
/// `mask_helper_preserves_pre_existing_old_nul_sentinel`,
/// `expand_preserves_pre_existing_legacy_fixed_sentinel_in_extra`,
/// and related regression tests which construct legacy collision
/// inputs and assert they are preserved verbatim.
fn make_sentinel() -> String {
    let hi: u64 = fastrand::u64(..);
    let lo: u64 = fastrand::u64(..);
    format!("{SENTINEL_PREFIX}{hi:016x}{lo:016x}{SENTINEL_SUFFIX}")
}

/// Expand `${VAR}` / `$VAR` references in `input`.
///
/// Lookup order for each reference:
/// 1. `extra` (the per-hook `extra_env` map)
/// 2. The current process environment
///
/// Unresolved references are preserved verbatim so this function is safe
/// to call repeatedly (idempotent on already-expanded strings) and so
/// references that are intentionally resolved at runtime (e.g. by the
/// dispatcher's always-set `ECHO_AGENT_HOOK_*` vars) survive the load-time pass.
///
/// Parameter-expansion-modifier forms (`${VAR:-x}`, `${VAR%pat}`, etc.)
/// are ALSO preserved verbatim; see the module-level rustdoc for why.
pub(crate) fn expand_env_vars_with_extra(input: &str, extra: &HashMap<String, String>) -> String {
    expand_env_vars_with_process_skip(input, extra, &[])
}

pub(crate) fn expand_env_vars_with_process_skip(
    input: &str,
    extra: &HashMap<String, String>,
    skip_process_env: &[&str],
) -> String {
    // Generate a fresh per-call sentinel. 128 bits of entropy means a
    // natural collision with any input substring or extra-env value is
    // ~2^-128 probability. See `make_sentinel` rustdoc.
    let sentinel = make_sentinel();

    // Defence in depth: if the freshly-generated sentinel ever happens
    // to appear in the input or in any extra-env value (vanishingly
    // unlikely; would require an adversary to predict our PRNG output),
    // panic in debug builds and fall through to legacy behaviour in
    // release. Returning the input unchanged is safer than rewriting a
    // legitimate substring to `${`.
    debug_assert!(
        !input.contains(&sentinel) && !extra.values().any(|v| v.contains(&sentinel)),
        "per-call sentinel collided with input or extra-env value"
    );

    // Step 1: hide any `${VAR<modifier>...}` substring from shellexpand by
    // replacing the leading `${` with the per-call sentinel. shellexpand's
    // grammar requires `$` before a brace to recognize the form, so
    // replacing the leading `${` with a non-`$` sentinel makes the body
    // look like literal text to the expander.
    let masked = mask_modifier_forms(input, &sentinel);

    // Step 2: run shellexpand on the (possibly) masked input.
    let context = |name: &str| -> Option<String> {
        if let Some(v) = extra.get(name) {
            return Some(v.clone());
        }
        if skip_process_env.contains(&name) {
            return None;
        }
        std::env::var(name).ok()
    };
    let expanded = shellexpand::env_with_context_no_errors(&masked, context).into_owned();

    // Step 3: restore the sentinels back to `${`. Because the sentinel is
    // freshly randomized per call, the only way it appears in `expanded`
    // is if `mask_modifier_forms` put it there.
    if expanded.contains(&sentinel) {
        expanded.replace(&sentinel, "${")
    } else {
        expanded
    }
}

/// Walk `input` and, for every `${...}` substring whose contents are a
/// valid identifier followed by a parameter-expansion modifier, replace
/// the leading `${` with `sentinel`. Plain `${VAR}` and bare `$VAR`
/// references are NOT touched -- they are passed through to shellexpand
/// for normal resolution.
///
/// "Modifier" here means anything inside the braces after the
/// identifier name: `:-`, `-`, `:=`, `=`, `:?`, `?`, `:+`, `+`, `%`,
/// `#`, `/`, `:N` (digit), `:N:M`, etc. This shares its detection
/// logic with [`crate::runner::command::find_unresolved_env_vars`] via
/// [`iter_env_var_references`].
fn mask_modifier_forms(input: &str, sentinel: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor: usize = 0;
    for r in iter_env_var_references(input) {
        // Copy any literal text between the previous reference (or
        // start of string) and this one verbatim.
        if cursor < r.start {
            out.push_str(&input[cursor..r.start]);
        }
        // Modifier-form braced ref: replace leading `${` with sentinel
        // and emit the body (including closing `}`) as-is.
        if r.braced && r.has_modifier {
            out.push_str(sentinel);
            // body_start = r.start + 2 (past `${`); copy up to and
            // including the closing `}` at r.end - 1.
            out.push_str(&input[r.start + 2..r.end]);
        } else {
            // Plain `${NAME}`, bare `$NAME`, or invalid form: pass
            // through verbatim so shellexpand can resolve (or leave
            // unresolved).
            out.push_str(&input[r.start..r.end]);
        }
        cursor = r.end;
    }
    // Copy the trailing literal tail.
    if cursor < input.len() {
        out.push_str(&input[cursor..]);
    }
    out
}

/// One detected env-var reference in a string, as produced by
/// [`iter_env_var_references`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvVarRef<'a> {
    /// Byte offset where the leading `$` starts.
    pub start: usize,
    /// Byte offset one past the end of the reference. For braced
    /// forms this is one past the closing `}`; for bare forms it is
    /// one past the last identifier character.
    pub end: usize,
    /// Identifier name. For `${VAR...}` and `$VAR` this is `"VAR"`;
    /// for invalid forms (e.g. `${:-foo}`, `${}`) it is empty.
    pub name: &'a str,
    /// True for `${...}` (braced); false for `$NAME` (bare).
    pub braced: bool,
    /// True if the braced form contains a parameter-expansion
    /// modifier between the identifier and the closing `}`
    /// (`:`, `-`, `=`, `?`, `+`, `%`, `#`, `/`, digit suffix, etc.).
    /// Always false for bare references and for invalid braced forms.
    pub has_modifier: bool,
}

/// Walk `input` and yield every `$VAR` / `${...}` reference. Skips
/// shell positional / special params (`$1`, `$$`, `$?`, `$#`,
/// `$(...)`, `$@`, etc.) since none of those are env-var references.
///
/// Behaviour notes:
///
/// * Unterminated braced forms (`${VAR:-no-close`) are skipped: the
///   `$` is consumed and scanning continues at the next byte. This
///   matches `shellexpand`'s behaviour of treating unterminated
///   forms as literal text.
/// * Nested braces inside a modifier body (`${A:-${B}}`) are handled
///   by matching the FIRST `}` -- the inner `${B}` becomes part of
///   the outer modifier body. This mirrors the legacy parser
///   behaviour (and the runtime `sh -c` branch handles real nesting
///   natively when the form reaches the shell).
/// * Empty / invalid identifier (`${}`, `${:-foo}`) is yielded with
///   an empty `name`, so callers can decide whether to mask it.
pub(crate) fn iter_env_var_references(input: &str) -> EnvVarRefIter<'_> {
    EnvVarRefIter { input, pos: 0 }
}

pub(crate) struct EnvVarRefIter<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Iterator for EnvVarRefIter<'a> {
    type Item = EnvVarRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.input.as_bytes();
        while self.pos < bytes.len() {
            if bytes[self.pos] != b'$' {
                self.pos += 1;
                continue;
            }
            let dollar = self.pos;
            // Past-the-`$` index.
            let after = dollar + 1;
            if after >= bytes.len() {
                // Trailing lone `$` -- not a reference. Stop.
                self.pos = bytes.len();
                return None;
            }
            if bytes[after] == b'{' {
                // Braced form: ${...}
                let body_start = after + 1;
                // Read identifier prefix (alphanumeric / underscore).
                let mut name_end = body_start;
                while name_end < bytes.len()
                    && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
                {
                    name_end += 1;
                }
                // Find the FIRST closing `}` from the identifier end.
                let mut close = name_end;
                while close < bytes.len() && bytes[close] != b'}' {
                    close += 1;
                }
                if close >= bytes.len() {
                    // Unterminated brace -- not a real form. Skip the
                    // `$` and keep scanning.
                    self.pos = dollar + 1;
                    continue;
                }
                let name = std::str::from_utf8(&bytes[body_start..name_end]).unwrap_or("");
                let has_modifier = !name.is_empty() && name_end < close;
                let end = close + 1;
                self.pos = end;
                return Some(EnvVarRef {
                    start: dollar,
                    end,
                    name,
                    braced: true,
                    has_modifier,
                });
            }
            // Bare `$NAME`: identifier must start with letter / `_`.
            // Anything else (`$1`, `$$`, `$?`, `$#`, `$(`, etc.) is a
            // shell special and not an env-var reference.
            if bytes[after].is_ascii_alphabetic() || bytes[after] == b'_' {
                let start_id = after;
                let mut end_id = start_id;
                while end_id < bytes.len()
                    && (bytes[end_id].is_ascii_alphanumeric() || bytes[end_id] == b'_')
                {
                    end_id += 1;
                }
                let name = std::str::from_utf8(&bytes[start_id..end_id]).unwrap_or("");
                self.pos = end_id;
                return Some(EnvVarRef {
                    start: dollar,
                    end: end_id,
                    name,
                    braced: false,
                    has_modifier: false,
                });
            }
            // `$` followed by a non-identifier, non-`{` byte. Skip
            // both bytes and continue.
            self.pos = after + 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_env_var;

    #[test]
    fn expands_braced_var_from_extra() {
        let mut extra = HashMap::new();
        extra.insert("PLUGIN_HOST".to_string(), "example.com".to_string());
        let out = expand_env_vars_with_extra("https://${PLUGIN_HOST}/check", &extra);
        assert_eq!(out, "https://example.com/check");
    }

    #[test]
    fn expands_bare_var_from_extra() {
        let mut extra = HashMap::new();
        extra.insert("ROOT".to_string(), "/opt/plugin".to_string());
        let out = expand_env_vars_with_extra("$ROOT/bin/x.sh", &extra);
        assert_eq!(out, "/opt/plugin/bin/x.sh");
    }

    #[test]
    fn extra_takes_precedence_over_process_env() {
        with_env_var(
            "ECHO_AGENT_HOOKS_ENV_EXPAND_TEST_PRECEDENCE",
            Some("from-process"),
            || {
                let mut extra = HashMap::new();
                extra.insert(
                    "ECHO_AGENT_HOOKS_ENV_EXPAND_TEST_PRECEDENCE".to_string(),
                    "from-extra".to_string(),
                );
                let out = expand_env_vars_with_extra(
                    "${ECHO_AGENT_HOOKS_ENV_EXPAND_TEST_PRECEDENCE}",
                    &extra,
                );
                assert_eq!(out, "from-extra");
            },
        );
    }

    #[test]
    fn falls_back_to_process_env() {
        with_env_var(
            "ECHO_AGENT_HOOKS_ENV_EXPAND_TEST_FALLBACK",
            Some("/from/proc/env"),
            || {
                let extra = HashMap::new();
                let out = expand_env_vars_with_extra(
                    "${ECHO_AGENT_HOOKS_ENV_EXPAND_TEST_FALLBACK}/x",
                    &extra,
                );
                assert_eq!(out, "/from/proc/env/x");
            },
        );
    }

    #[test]
    fn preserves_unresolved_references() {
        // shellexpand's no-errors variant returns the original `${VAR}` text
        // when the var is unset in both `extra` and the process env. This
        // makes load-time expansion idempotent and lets runtime-only vars
        // survive the pass to be caught by `find_unresolved_env_vars`.
        with_env_var("ECHO_AGENT_HOOKS_ENV_EXPAND_NEVER_SET", None, || {
            let extra = HashMap::new();
            let input = "${ECHO_AGENT_HOOKS_ENV_EXPAND_NEVER_SET}/x.sh";
            let out = expand_env_vars_with_extra(input, &extra);
            assert_eq!(out, input);
        });
    }

    #[test]
    fn process_skip_leaves_runner_names() {
        assert_eq!(
            crate::config::expand_env_skipping_runner_vars("${CLAUDE_PROJECT_DIR:-.}"),
            "${CLAUDE_PROJECT_DIR:-.}"
        );
    }

    #[test]
    fn idempotent_on_already_expanded_string() {
        let extra = HashMap::new();
        let already = "/opt/plugins/foo/hooks/x.sh";
        let out = expand_env_vars_with_extra(already, &extra);
        assert_eq!(out, already);
    }

    #[test]
    fn empty_input_returns_empty() {
        let extra = HashMap::new();
        assert_eq!(expand_env_vars_with_extra("", &extra), "");
    }

    // ── Parameter-expansion-modifier preservation ───────────────

    /// `${VAR:-default}` must be preserved verbatim, even when `VAR` is
    /// unset at expand time. Otherwise shellexpand resolves to the
    /// literal default and the runtime branch never gets a chance to
    /// see `VAR`'s real (runtime-only) value.
    #[test]
    fn preserves_default_modifier_when_var_unset() {
        let extra = HashMap::new();
        with_env_var("ECHO_AGENT_HOOKS_ENV_EXPAND_MODIFIER_UNSET", None, || {
            let input = "${ECHO_AGENT_HOOKS_ENV_EXPAND_MODIFIER_UNSET:-/default/path.sh}";
            let out = expand_env_vars_with_extra(input, &extra);
            assert_eq!(out, input);
        });
    }

    /// Even when the var IS set, the modifier form must be preserved
    /// verbatim -- the shell's `:-` semantics differ from shellexpand's
    /// (notably for set-but-empty values), so deferring the entire form
    /// to the runtime `sh -c` branch is the only safe choice.
    #[test]
    fn preserves_default_modifier_when_var_set() {
        let mut extra = HashMap::new();
        extra.insert(
            "ECHO_AGENT_HOOKS_DEFAULT_SET".to_string(),
            "/from/extra".to_string(),
        );
        let input = "${ECHO_AGENT_HOOKS_DEFAULT_SET:-/fallback}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR-default}` (no colon) — also a modifier form.
    #[test]
    fn preserves_no_colon_default_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_NCD-/fallback}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR:=x}` — assignment modifier.
    #[test]
    fn preserves_assignment_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_ASSIGN:=/assigned/path.sh}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR:?msg}` — error modifier.
    #[test]
    fn preserves_error_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_ERR:?error message}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR:+x}` — alternate-value modifier.
    #[test]
    fn preserves_alternate_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_ALT:+/used/if/set}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR%pat}` — suffix-strip modifier.
    #[test]
    fn preserves_suffix_strip_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_SUFFIX%.sh}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR#pat}` — prefix-strip modifier.
    #[test]
    fn preserves_prefix_strip_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_PREFIX#prefix/}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR/foo/bar}` — pattern-substitution modifier.
    #[test]
    fn preserves_substitution_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_SUB/foo/bar}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// `${VAR:N:M}` — substring modifier.
    #[test]
    fn preserves_substring_modifier() {
        let extra = HashMap::new();
        let input = "${ECHO_AGENT_HOOKS_SUBSTR:0:5}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    /// Mixed: a modifier-form sits next to a plain form; only the plain
    /// one is expanded.
    #[test]
    fn mixed_plain_and_modifier_only_plain_expanded() {
        let mut extra = HashMap::new();
        extra.insert(
            "ECHO_AGENT_HOOKS_PLAIN".to_string(),
            "/usr/local".to_string(),
        );
        let input = "${ECHO_AGENT_HOOKS_PLAIN}/${ECHO_AGENT_HOOKS_DEFER:-/fallback}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, "/usr/local/${ECHO_AGENT_HOOKS_DEFER:-/fallback}");
    }

    // ── Set-but-empty regression test ────────────────────────────

    /// When the var is set in `extra` but to the empty string, the
    /// no-modifier form `${VAR}` resolves to "" (matching shellexpand's
    /// behaviour and what users typically expect).
    #[test]
    fn empty_extra_value_resolves_to_empty_for_plain_form() {
        let mut extra = HashMap::new();
        extra.insert("ECHO_AGENT_HOOKS_EMPTY".to_string(), "".to_string());
        let out = expand_env_vars_with_extra("[${ECHO_AGENT_HOOKS_EMPTY}]", &extra);
        assert_eq!(out, "[]");
    }

    /// When the var is set in `extra` but to the empty string, the
    /// modifier-form `${VAR:-default}` is preserved verbatim (so that
    /// the runtime `sh -c` branch can apply POSIX `:-` semantics, which
    /// differ from shellexpand's: bash returns the default for empty
    /// values, shellexpand returns the empty string). This documents
    /// that the load-time pass does NOT trigger the modifier branch.
    #[test]
    fn empty_extra_value_does_not_trigger_default() {
        let mut extra = HashMap::new();
        extra.insert("ECHO_AGENT_HOOKS_EMPTY_MOD".to_string(), "".to_string());
        let input = "${ECHO_AGENT_HOOKS_EMPTY_MOD:-/fallback}";
        let out = expand_env_vars_with_extra(input, &extra);
        assert_eq!(out, input);
    }

    // ── Single-pass expansion (no recursion) ────────────────────

    /// A value in `extra` that itself contains a `$VAR` reference must
    /// NOT be re-expanded. Recursion would be a DoS vector and a
    /// semantic surprise. shellexpand's
    /// `env_with_context_no_errors` is single-pass by design; this
    /// test locks the property in.
    #[test]
    fn extra_values_are_not_recursively_expanded() {
        with_env_var(
            "ECHO_AGENT_HOOKS_RECURSION_BAR",
            Some("should-not-appear"),
            || {
                let mut extra = HashMap::new();
                extra.insert(
                    "ECHO_AGENT_HOOKS_RECURSION_FOO".to_string(),
                    "$ECHO_AGENT_HOOKS_RECURSION_BAR".to_string(),
                );
                let out = expand_env_vars_with_extra("${ECHO_AGENT_HOOKS_RECURSION_FOO}", &extra);
                assert_eq!(out, "$ECHO_AGENT_HOOKS_RECURSION_BAR");
            },
        );
    }

    // ── mask_modifier_forms helper unit tests ────────────────────

    /// A fixed test-only sentinel used to make the masked-output
    /// assertions deterministic. Production code uses [`make_sentinel`]
    /// which returns a per-call randomized value (see the sentinel
    /// collision regression test below that exercises the random
    /// path end-to-end).
    const TEST_SENTINEL: &str = "<<TEST_SENTINEL>>";

    #[test]
    fn mask_helper_passes_plain_form_through() {
        assert_eq!(mask_modifier_forms("${PLAIN}", TEST_SENTINEL), "${PLAIN}");
    }

    #[test]
    fn mask_helper_masks_default_form() {
        // Lock down the exact masked output, not
        // just the sentinel-contains predicate.
        let masked = mask_modifier_forms("${VAR:-x}", TEST_SENTINEL);
        assert_eq!(masked, format!("{TEST_SENTINEL}VAR:-x}}"));
    }

    #[test]
    fn mask_helper_handles_unterminated_brace() {
        // No closing brace -- no masking, emit verbatim.
        assert_eq!(
            mask_modifier_forms("${VAR:-no-close", TEST_SENTINEL),
            "${VAR:-no-close"
        );
    }

    #[test]
    fn mask_helper_passes_bare_form_through() {
        assert_eq!(mask_modifier_forms("$BARE_VAR", TEST_SENTINEL), "$BARE_VAR");
    }

    #[test]
    fn mask_helper_handles_multibyte_chars() {
        // Full-equality assertion locks down the
        // exact bytes, including UTF-8 boundary placement.
        let input = "h\u{e9}llo${PLAIN}w\u{f6}rld${VAR:-x}";
        let masked = mask_modifier_forms(input, TEST_SENTINEL);
        let expected = format!("h\u{e9}llo${{PLAIN}}w\u{f6}rld{TEST_SENTINEL}VAR:-x}}");
        assert_eq!(masked, expected);
    }

    // ── Nested / interleaved edge cases ─────────────────────────

    /// Two consecutive modifier forms with no
    /// intervening text. Both must be masked independently.
    #[test]
    fn mask_helper_consecutive_modifier_forms() {
        let masked = mask_modifier_forms("${A:-x}${B:-y}", TEST_SENTINEL);
        assert_eq!(
            masked,
            format!("{TEST_SENTINEL}A:-x}}{TEST_SENTINEL}B:-y}}")
        );
    }

    /// Nested braces inside a modifier body. The
    /// custom byte-walker matches the FIRST closing `}`, so the
    /// inner `${B}` is NOT a separately-recognised plain form -- it
    /// becomes part of the outer modifier body and is masked along
    /// with the outer form. The literal `${B}` is preserved inside
    /// the masked body, ready for the runtime `sh -c` branch (which
    /// handles nesting natively).
    ///
    /// The trailing extra `}` is left as-is (it has no matching `${`).
    /// This documented behaviour is intentional: complex nested
    /// expansions are an explicit deferral to runtime.
    #[test]
    fn mask_helper_nested_braces_in_modifier_body() {
        let masked = mask_modifier_forms("${A:-${B}}", TEST_SENTINEL);
        // First `}` closes the outer modifier match; `${B}` is INSIDE
        // the masked body. The tail `}` is a stray brace, preserved
        // as-is.
        assert_eq!(masked, format!("{TEST_SENTINEL}A:-${{B}}}}"));
    }

    /// A closed plain form followed by an
    /// unterminated modifier form. The plain form passes through;
    /// the unterminated tail is emitted verbatim because the walker
    /// requires a closing `}` to consider a `${...}` substring a
    /// real form.
    #[test]
    fn mask_helper_closed_then_unterminated() {
        let masked = mask_modifier_forms("${A}${B:-", TEST_SENTINEL);
        assert_eq!(masked, "${A}${B:-");
    }

    // ── Sentinel collision regression ──────────────────────────

    /// The previous sentinel was `\x00\x00`. If a
    /// future change reverted to that sentinel, an `extra_env` value
    /// or input string containing the same byte sequence would be
    /// silently rewritten to `${`. The new sentinel is a long magic
    /// ASCII string sandwiched between two PUA characters --
    /// vanishingly unlikely to collide. This regression test
    /// constructs an input containing the OLD `\x00\x00` sequence
    /// AND a value containing the OLD sequence in `extra_env`, and
    /// asserts both pass through unchanged.
    #[test]
    fn mask_helper_preserves_pre_existing_old_nul_sentinel() {
        // The OLD sentinel as a literal in the input.
        let input = "prefix\u{0}\u{0}suffix";
        assert_eq!(mask_modifier_forms(input, TEST_SENTINEL), input);
    }

    /// Companion to the above: an `extra_env` value containing the
    /// OLD sentinel must not be rewritten to `${...}` after expansion.
    #[test]
    fn expand_preserves_pre_existing_old_nul_sentinel_in_extra() {
        let mut extra = HashMap::new();
        // Value contains the legacy 2-NUL sentinel followed by what
        // would have been parsed as an identifier+brace.
        extra.insert("VAL".to_string(), "\u{0}\u{0}OLD}".to_string());
        let out = expand_env_vars_with_extra("prefix${VAL}suffix", &extra);
        // Output must contain the literal NUL bytes verbatim, NOT
        // `${OLD}`.
        assert_eq!(out, "prefix\u{0}\u{0}OLD}suffix");
        assert!(
            !out.contains("${OLD}"),
            "legacy sentinel must NOT trigger an unmask-to-`${{`, got {out:?}"
        );
    }

    /// An earlier sentinel was a fixed string
    /// `"\u{f8ff}__ECHO_AGENT_HOOKS_MASK__\u{f8ff}"`. A user-supplied
    /// `extra_env` value containing that exact byte sequence would
    /// have been silently rewritten to `${` by the unmask step. The
    /// per-call randomized sentinel removes this hazard. This
    /// regression test asserts the legacy fixed sentinel passes
    /// through verbatim when it appears in an extra-env value, even
    /// though the input also references that variable through `${VAL}`.
    #[test]
    fn expand_preserves_pre_existing_legacy_fixed_sentinel_in_extra() {
        let legacy_sentinel = "\u{f8ff}__ECHO_AGENT_HOOKS_MASK__\u{f8ff}";
        let mut extra = HashMap::new();
        // Value embeds the legacy sentinel followed by what would
        // have been parsed as an identifier+brace if the unmask
        // sentinel-replace had collided.
        extra.insert(
            "VAL".to_string(),
            format!("payload-{legacy_sentinel}OLD}}-tail"),
        );
        // Reference VAL via a plain form so its value gets spliced
        // into the output.
        let out = expand_env_vars_with_extra("prefix${VAL}suffix", &extra);
        // The legacy sentinel substring must appear in the output
        // verbatim -- it must NOT be rewritten to `${`.
        assert_eq!(
            out,
            format!("prefixpayload-{legacy_sentinel}OLD}}-tailsuffix")
        );
        assert!(
            !out.contains("${OLD}"),
            "legacy fixed sentinel must NOT trigger an unmask-to-`${{`, got {out:?}"
        );
    }

    /// Companion: arbitrary high-entropy bytes in an extra-env value
    /// must also pass through verbatim. (Sanity check that the
    /// per-call sentinel doesn't collide with random binary content.)
    #[test]
    fn expand_preserves_arbitrary_bytes_in_extra() {
        let mut extra = HashMap::new();
        // A mix of printable ASCII, NULs, PUA chars, brace bytes, and
        // dollar signs -- the kinds of bytes most likely to clash
        // with any future sentinel scheme.
        let exotic = "\u{0}\u{f8ff}${weird}}\u{f8ff}\u{0}__MASK__";
        extra.insert("VAL".to_string(), exotic.to_string());
        let out = expand_env_vars_with_extra("X=${VAL}", &extra);
        assert_eq!(out, format!("X={exotic}"));
    }

    // ── iter_env_var_references unit tests ───────────────────────

    /// Lock down the iterator output for a single braced plain form.
    #[test]
    fn iter_yields_plain_braced_form() {
        let refs: Vec<_> = iter_env_var_references("foo ${BAR} baz").collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "BAR");
        assert!(refs[0].braced);
        assert!(!refs[0].has_modifier);
        assert_eq!(refs[0].start, 4);
        assert_eq!(refs[0].end, 10);
    }

    /// Lock down the iterator output for a single bare form.
    #[test]
    fn iter_yields_bare_form() {
        let refs: Vec<_> = iter_env_var_references("foo $BAR baz").collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "BAR");
        assert!(!refs[0].braced);
        assert!(!refs[0].has_modifier);
        assert_eq!(refs[0].start, 4);
        assert_eq!(refs[0].end, 8);
    }

    /// Modifier form sets has_modifier = true.
    #[test]
    fn iter_flags_modifier_form() {
        let refs: Vec<_> = iter_env_var_references("${VAR:-x}").collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "VAR");
        assert!(refs[0].braced);
        assert!(refs[0].has_modifier);
        assert_eq!(refs[0].start, 0);
        assert_eq!(refs[0].end, 9);
    }

    /// Shell positionals / specials / command substitutions are NOT
    /// yielded.
    #[test]
    fn iter_skips_shell_specials() {
        let refs: Vec<_> = iter_env_var_references("$1 $$ $? $# $(date) $@").collect();
        assert!(
            refs.is_empty(),
            "shell special params must not yield refs, got {refs:?}"
        );
    }

    /// Unterminated braced form: the `$` is consumed; nothing yielded.
    #[test]
    fn iter_skips_unterminated_brace() {
        let refs: Vec<_> = iter_env_var_references("${VAR:-no-close").collect();
        assert!(refs.is_empty(), "unterminated brace must yield no refs");
    }

    /// Empty / invalid identifier inside braces: yielded with empty
    /// name and has_modifier=false.
    #[test]
    fn iter_yields_invalid_braced_form_with_empty_name() {
        let refs: Vec<_> = iter_env_var_references("${:-foo}").collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "");
        assert!(refs[0].braced);
        assert!(
            !refs[0].has_modifier,
            "invalid form (no identifier) must not be flagged as a modifier form"
        );
    }

    /// Mixed input: plain, modifier, bare, and a positional.
    #[test]
    fn iter_yields_mixed_forms_in_order() {
        let refs: Vec<_> = iter_env_var_references("${A}${B:-x}$C $1").collect();
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].name, "A");
        assert!(refs[0].braced && !refs[0].has_modifier);
        assert_eq!(refs[1].name, "B");
        assert!(refs[1].braced && refs[1].has_modifier);
        assert_eq!(refs[2].name, "C");
        assert!(!refs[2].braced && !refs[2].has_modifier);
    }

    /// Nested braces are matched at the FIRST `}` (legacy parser
    /// behaviour, see `mask_helper_nested_braces_in_modifier_body`).
    #[test]
    fn iter_matches_first_closing_brace_for_nested() {
        // Bytes: `${A:-${B}}` (indices 0..10).
        // The outer ref begins at the leading `$` (0), reads `A` as
        // the identifier, sees `:` as the first non-identifier byte,
        // then walks forward to the FIRST `}` -- which is the closing
        // brace of the inner `${B}` at index 8. So end = 9. The
        // trailing `}` at index 9 is literal text.
        let refs: Vec<_> = iter_env_var_references("${A:-${B}}").collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "A");
        assert!(refs[0].braced);
        assert!(refs[0].has_modifier);
        assert_eq!(refs[0].start, 0);
        assert_eq!(refs[0].end, 9);
    }
}
