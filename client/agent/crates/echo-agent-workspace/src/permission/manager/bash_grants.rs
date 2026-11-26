//! Translation between what a permission prompt showed the user and what
//! becomes a persisted bash grant key. Grant keys are dequoted word joins;
//! `evaluate_bash` matches them per segment by word-boundary prefix, and
//! whole-script exact grants additionally require the join (or the raw text)
//! to cover the entire script.

use crate::permission::bash_command_splitting::{
    BashCommandHighlights, PlainCommand, is_setup_command, primary_command_from_script,
    try_parse_shell, try_parse_word_only_commands_sequence, unwrap_wrappers,
};
use crate::permission::state::PermissionState;

/// Whether accepting the always-allow row at scope `n` (the first `n`
/// highlighted words) persists a grant that replays — the predicate the ←/→
/// arrows use to skip scopes that would save nothing. Mirrors
/// [`persist_bash_always_allow`]: an argv-unambiguous prefix at or above the
/// dangerous-command floor, or the full command of a single unwrapped script
/// (persisted as raw text). An ambiguous intermediate scope (a quoted arg
/// with a space) joins to a key that could match a different argv, so it is
/// refused and must be skipped.
pub fn always_allow_scope_persists(h: &BashCommandHighlights, n: usize) -> bool {
    let words = &h.highlighted_words;
    if n == 0 || n > words.len() || n < super::minimum_always_allow_scope(words) {
        return false;
    }
    // The raw-text fallback needs a single unwrapped command spanning the
    // whole script (empty prefix/suffix).
    words_join_unambiguously(&words[..n])
        || (n == words.len() && h.prefix.is_empty() && h.suffix.is_empty())
}

/// Whether `join(" ")` round-trips these words to the same single command.
/// A word with whitespace collapses to the join of different adjacent words
/// (`-m "fix stuff"` vs `-m fix stuff`), and a word with shell metacharacters
/// respells a different script (`-m "fix;git"` joins into a two-segment
/// chain), so a join serves as a grant key only if re-parsing it yields one
/// whole-script command with this exact argv.
pub(super) fn words_join_unambiguously(words: &[String]) -> bool {
    let joined = words.join(" ");
    try_parse_shell(&joined)
        .and_then(|tree| try_parse_word_only_commands_sequence(&tree, &joined))
        .is_some_and(|segments| {
            matches!(segments.as_slice(),
                [only] if only.spans_whole_script(&joined) && only.words() == words)
        })
}

/// Whether persisted grants exactly cover the whole script: the raw text, or
/// — because prompt-saved grants are dequoted joins while `cmd` is raw — the
/// join of a single argv-unambiguous command spanning the entire script.
/// Anything narrower (assignment prefix, chained sibling, space-bearing word)
/// must not match: it would widen the grant past what the user approved.
pub(super) fn whole_script_grant(
    cmd: &str,
    segments: &[PlainCommand],
    state: &PermissionState,
) -> bool {
    if state.allowed_bash_commands.contains(cmd) {
        return true;
    }
    match segments {
        [only] if only.spans_whole_script(cmd) && words_join_unambiguously(only.words()) => state
            .allowed_bash_commands
            .contains(&only.words().join(" ")),
        _ => false,
    }
}

/// Per-segment grant keys for a whole-script "don't ask again" answer, in the
/// wrapper-peeled dequoted form enforcement matches. Empty when the script
/// can't be decomposed, exceeds the cap, or a segment is ambiguous. Dangerous
/// verbs and exec vehicles never mint standalone grants — a silent
/// per-segment key must not authorize anything beyond the exact chain the
/// user saw (dangerous verbs ignore prefix grants and exec vehicles match
/// exactly, but there is no reason to mint keys for them at all). The exact
/// chain still replays via the raw whole-script key the caller stores
/// alongside these.
pub(super) fn bash_grant_segments(cmd: &str) -> Vec<String> {
    use crate::permission::policy::head_is_exec_vehicle;
    const MAX_SEGMENT_GRANTS: usize = 5;
    let Some(tree) = try_parse_shell(cmd) else {
        return Vec::new();
    };
    let Some(segments) = try_parse_word_only_commands_sequence(&tree, cmd) else {
        return Vec::new();
    };
    let grants: Vec<String> = segments
        .iter()
        .filter_map(|parsed| {
            let words = unwrap_wrappers(parsed.words());
            (!words.is_empty()
                && !is_setup_command(words)
                && !super::is_dangerous_command_words(words)
                && !head_is_exec_vehicle(words)
                && words_join_unambiguously(words))
            .then(|| words.join(" "))
        })
        .collect();
    if grants.len() > MAX_SEGMENT_GRANTS {
        return Vec::new();
    }
    grants
}

/// Persist an "Always allow: <words>" selection, verified against the row it
/// was built from: the label must re-derive as an argv-unambiguous prefix of
/// the primary command's words, at or above `minimum_always_allow_scope`, so a
/// forged/stale label (e.g. a remote hub reply) naming another segment — or
/// narrowing a dangerous command to a bare `git` — persists nothing. The raw
/// script is stored only when the label is the whole unwrapped command modulo
/// quoting (empty prefix/suffix), so an unseen `env` assignment can't clear
/// the injection floor via the raw key.
pub(super) fn persist_bash_always_allow(state: &mut PermissionState, cmd: &str, prefix: &str) {
    let Some(h) = primary_command_from_script(cmd) else {
        return;
    };
    let words = &h.highlighted_words;
    let min_scope = super::minimum_always_allow_scope(words);
    let labeled_words_verified = (min_scope..=words.len())
        .any(|n| words_join_unambiguously(&words[..n]) && words[..n].join(" ") == prefix);
    if labeled_words_verified {
        state.allowed_bash_commands.insert(prefix.to_owned());
    } else if h.prefix.is_empty() && h.suffix.is_empty() && words.join(" ") == prefix {
        state.allowed_bash_commands.insert(cmd.to_owned());
    }
}

/// Whether a user-authored (or client-supplied) glob grant is scoped to
/// `cmd`: it must match at least one non-setup segment under the same matcher
/// enforcement uses (the raw text when the script is unparseable), and it must
/// not be universally broad. A pattern like `*` / `**` / `* *` "matches the
/// prompted script" only by matching everything, so a forged reply could mint
/// a blanket grant; [`bash_glob_is_catchall`] refuses those. Scoped patterns
/// the pattern editor produces (`gh api repos/owner/*`) still pass.
pub(super) fn bash_glob_covers_script(cmd: &str, pattern: &str) -> bool {
    use crate::permission::policy::{bash_glob_is_catchall, bash_pattern_matches_command};
    if bash_glob_is_catchall(pattern) {
        return false;
    }
    let segments =
        try_parse_shell(cmd).and_then(|tree| try_parse_word_only_commands_sequence(&tree, cmd));
    let Some(segments) = segments else {
        return bash_pattern_matches_command(pattern, cmd);
    };
    segments.iter().any(|seg| {
        let words = unwrap_wrappers(seg.words());
        !words.is_empty()
            && !is_setup_command(words)
            && bash_pattern_matches_command(pattern, &words.join(" "))
    })
}

/// Whether the "Always allow" row can be honored for `cmd`: accepting it at
/// the scope the cursor opens on ([`default_always_allow_scope`], what Enter
/// persists) must let this same script replay without a prompt. Decided by
/// construction, not by enumerating finding classes — the default-scope save
/// is simulated into a scratch state and the pre-classifier grant gate is
/// asked whether the result would allow. A row failing this would save a grant
/// that cannot stop the script ("always allow" that keeps asking): a wrapped
/// dangerous command whose exact grant is unproducible, or a chain whose
/// primary-scoped grant leaves a segment prompting.
pub fn always_allow_row_is_effective(cmd: &str) -> bool {
    let Some(h) = primary_command_from_script(cmd) else {
        return false;
    };
    let scope = super::default_always_allow_scope(&h.highlighted_words);
    // The default cursor scope must itself persist a grant. This is the exact
    // predicate the prompt's ←/→ arrows and the enqueue invariant use, so
    // gating row visibility on it keeps "row shown" and "default scope
    // persists" from diverging: an argv-ambiguous default that only some other
    // scope's join happens to save must not surface a row Enter can't honor.
    if !always_allow_scope_persists(&h, scope) {
        return false;
    }
    // A FRESH scratch state keeps the verdict independent of any live grant
    // the prompt doesn't show. `always_allow_scope_persists` guarantees this
    // save inserts a key; the pre-classifier gate below is the real test of
    // whether that key lets the whole script replay.
    let mut scratch = PermissionState::default();
    persist_bash_always_allow(&mut scratch, cmd, &h.highlighted_words[..scope].join(" "));
    let evaluation = super::evaluate_bash(cmd, &scratch, true);
    super::bash_grant_pre_decision(
        cmd,
        &evaluation,
        &scratch,
        None,
        super::BashGrantOpts::PRE_CLASSIFIER,
    )
    .is_some_and(|(decision, _)| matches!(decision, crate::permission::types::Decision::Allow))
}

#[cfg(test)]
mod tests {
    use super::super::evaluate_bash;
    use super::*;

    #[test]
    fn always_allow_persists_exactly_the_labeled_selection() {
        // Plain full-scope dangerous command: label == raw, exact grant works.
        let mut plain = PermissionState::default();
        persist_bash_always_allow(&mut plain, "git push origin main", "git push origin main");
        assert!(plain.allowed_bash_commands.contains("git push origin main"));
        assert!(evaluate_bash("git push origin main", &plain, true).exact_grant);

        // Wrapper-prefixed: only the labeled (peeled) words are stored; the
        // raw spelling with the hidden assignment never becomes a grant.
        let mut wrapped = PermissionState::default();
        persist_bash_always_allow(
            &mut wrapped,
            "env FOO=1 git push origin main",
            "git push origin main",
        );
        assert!(
            wrapped
                .allowed_bash_commands
                .contains("git push origin main")
        );
        assert!(!evaluate_bash("env FOO=1 git push origin main", &wrapped, true).exact_grant);

        // Chained script: only a primary-command prefix persists; a label
        // naming another segment (forgeable via remote replies) is refused.
        let mut chained = PermissionState::default();
        const CHAIN: &str = "cargo build && git push --force";
        persist_bash_always_allow(&mut chained, CHAIN, "cargo build");
        assert!(chained.allowed_bash_commands.contains("cargo build"));
        persist_bash_always_allow(&mut chained, CHAIN, "git push --force");
        assert!(!chained.allowed_bash_commands.contains("git push --force"));
        assert!(!chained.allowed_bash_commands.contains(CHAIN));

        // Quoted single command: the full-scope label is argv-ambiguous, so
        // the raw text is stored instead — it matches only its own spelling.
        let mut quoted = PermissionState::default();
        const QUOTED: &str = r#"git commit -m "fix stuff""#;
        persist_bash_always_allow(&mut quoted, QUOTED, "git commit -m fix stuff");
        assert!(quoted.allowed_bash_commands.contains(QUOTED));
        assert!(evaluate_bash(QUOTED, &quoted, true).exact_grant);
        assert!(!evaluate_bash("git commit -m fix stuff", &quoted, true).exact_grant);

        // A label the primary command cannot derive persists nothing.
        let mut forged = PermissionState::default();
        persist_bash_always_allow(&mut forged, "ls -la", "rm -rf /");
        assert!(forged.allowed_bash_commands.is_empty());

        // A narrowed dangerous label persists nothing: enforcement ignores
        // dangerous prefix grants, so the rule could never match (clients
        // clamp this; the persist path is the backstop for the rest). The
        // bare-`git` shape matters — `git` alone is not a dangerous verb, so
        // a per-prefix dangerous check would let it through and a `git`
        // prefix grant would cover `git reset --hard`.
        for label in ["git push", "git"] {
            let mut narrowed = PermissionState::default();
            persist_bash_always_allow(&mut narrowed, "git push origin main", label);
            assert!(
                narrowed.allowed_bash_commands.is_empty(),
                "narrowed dangerous label {label:?} must persist nothing"
            );
        }
    }

    #[test]
    fn metacharacter_words_never_mint_chain_colliding_grants() {
        // The dequoted join respells a dangerous two-segment chain, so only
        // the raw text may persist and the chain never gains an exact grant.
        const CMD: &str = r#"git commit -m "fix;git" push --force"#;
        const COLLIDING_CHAIN: &str = "git commit -m fix;git push --force";

        let mut state = PermissionState::default();
        persist_bash_always_allow(&mut state, CMD, COLLIDING_CHAIN);
        assert!(state.allowed_bash_commands.contains(CMD));
        assert!(!state.allowed_bash_commands.contains(COLLIDING_CHAIN));
        assert!(evaluate_bash(CMD, &state, true).exact_grant);
        assert!(!evaluate_bash(COLLIDING_CHAIN, &state, true).exact_grant);
        // The silent per-segment mint must refuse the join too.
        assert!(bash_grant_segments(CMD).is_empty());
    }

    #[test]
    fn scope_persistence_matches_what_persist_would_store() {
        // Argv-ambiguous intermediate scope: a quoted arg with a space
        // followed by another word. The full command persists (raw fallback),
        // the ambiguous middle count does not, the unambiguous prefixes do.
        // (`git show` is deliberately a non-vehicle, non-dangerous head so the
        // scope floors to one word and intermediate prefixes remain offerable.)
        const CMD: &str = r#"git show -e "A B" file"#;
        let h = primary_command_from_script(CMD).unwrap();
        assert_eq!(h.highlighted_words.len(), 5);
        assert!(always_allow_scope_persists(&h, 3)); // `git show -e`
        assert!(!always_allow_scope_persists(&h, 4)); // `git show -e A B` — ambiguous
        assert!(always_allow_scope_persists(&h, 5)); // full command → raw fallback

        // Cross-check against the real persist path for every scope.
        for n in 1..=h.highlighted_words.len() {
            let label = h.highlighted_words[..n].join(" ");
            let mut scratch = PermissionState::default();
            persist_bash_always_allow(&mut scratch, CMD, &label);
            assert_eq!(
                !scratch.allowed_bash_commands.is_empty(),
                always_allow_scope_persists(&h, n),
                "scope {n} ({label:?}) persistence disagrees with persist path"
            );
        }
    }

    #[test]
    fn glob_grants_must_cover_the_prompted_script() {
        // The editor's legitimate output: a glob widening the primary's args.
        assert!(bash_glob_covers_script(
            "gh api repos/owner/echoagent/pulls",
            "gh api repos/owner/*"
        ));
        // Chains verify against any non-setup segment.
        assert!(bash_glob_covers_script(
            "cd /tmp && gh api repos/owner/echoagent",
            "gh api *"
        ));
        // A forged scope naming commands the prompt never showed is refused.
        assert!(!bash_glob_covers_script("ls -la", "gh api *"));
        assert!(!bash_glob_covers_script("ls -la", "*push*"));
        // Catch-all patterns "match the prompted script" only by matching
        // everything, so a forged reply cannot mint a blanket grant.
        for catchall in ["*", "**", "* *", "?*"] {
            assert!(
                !bash_glob_covers_script("gh api repos/owner/echoagent", catchall),
                "catch-all {catchall:?} must not cover the script",
            );
        }
        // Unparseable scripts verify against the raw text.
        assert!(bash_glob_covers_script("deploy $(rev)", "deploy *"));
        assert!(!bash_glob_covers_script("deploy $(rev)", "rm *"));
        // …but a catch-all is refused even on the unparseable path.
        assert!(!bash_glob_covers_script("deploy $(rev)", "*"));
    }

    #[test]
    fn bash_grant_segments_match_enforcement_form() {
        assert_eq!(
            bash_grant_segments("git status && timeout 30 cargo test"),
            vec!["git status".to_owned(), "cargo test".to_owned()]
        );
        // Setup commands (cd/export/sleep) are not grants.
        assert_eq!(
            bash_grant_segments("cd /tmp && npm run build"),
            vec!["npm run build".to_owned()]
        );
        // Dangerous segments never mint standalone grants.
        assert_eq!(
            bash_grant_segments("cargo build && git push --force"),
            vec!["cargo build".to_owned()]
        );
        // Exec-vehicle segments never mint standalone (prefix-matching) grants
        // either: a `docker run nginx` key would prefix-match `docker run nginx
        // --privileged`. The chain still replays via the raw whole-script key.
        assert_eq!(
            bash_grant_segments("docker run nginx && echo done"),
            vec!["echo done".to_owned()]
        );
        assert!(
            bash_grant_segments("sudo systemctl restart x && cargo build")
                .contains(&"cargo build".to_owned())
        );
        assert!(
            !bash_grant_segments("sudo systemctl restart x && cargo build")
                .iter()
                .any(|g| g.starts_with("sudo"))
        );
        // Unparseable: no segment grants (raw exact-string only).
        assert!(bash_grant_segments("echo $(date)").is_empty());
        // Overlong chains: bounded, nothing persisted at segment level.
        assert!(bash_grant_segments("a; b; c; d; e; f; g").is_empty());
    }

    #[test]
    fn row_effective_only_when_the_simulated_save_would_allow() {
        // Ordinary commands, wrapped or not: peeled prefix grants replay them.
        assert!(always_allow_row_is_effective("cargo test --lib"));
        assert!(always_allow_row_is_effective("timeout 30 cargo test"));
        // Plain dangerous command: full-scope label equals the raw text.
        assert!(always_allow_row_is_effective("git push origin main"));
        // Wrapped or assignment-prefixed dangerous/floored commands can never
        // produce the exact grant they require — no row.
        assert!(!always_allow_row_is_effective(
            "env FOO=1 git push origin main"
        ));
        assert!(!always_allow_row_is_effective("FOO=1 git push origin main"));
        assert!(!always_allow_row_is_effective("env FOO=1 make"));
        // Chains whose primary-scoped grant leaves another segment prompting
        // get no row even with zero findings — the simulation catches what
        // finding-class enumeration missed (`ls` was already auto-safe, and
        // granting it cannot stop `npm publish` from prompting).
        assert!(!always_allow_row_is_effective("ls && npm publish"));
        assert!(!always_allow_row_is_effective(
            "git status && git push origin main"
        ));
        // Unparseable scripts have no primary command and no honorable row.
        assert!(!always_allow_row_is_effective("echo $(date)"));
        // A pipe whose primary carries a space-bearing arg is still honorable:
        // the DEFAULT scope (verb + flags, no ambiguous word) persists and the
        // safe-listed tail (`head`) replays — the row must be offered even
        // though the WIDEST join is argv-ambiguous.
        assert!(always_allow_row_is_effective(
            r#"terraform plan -var "x=1 2" | head"#
        ));
    }

    #[test]
    fn row_effective_implies_default_scope_persists() {
        // The enqueue invariant, proven crate-locally: whenever the agent
        // offers the "Always allow:" row, the cursor's default scope persists a
        // grant — so pressing Enter without touching the ←/→ arrows always
        // saves. Exec vehicles are the regression case: flooring the minimum
        // scope without flooring the default would open the row on a dead scope
        // below the floor, which no arrow could repair.
        for cmd in [
            "cargo test --lib",
            "timeout 30 cargo test",
            "git push origin main",
            "python3 -u foo.py arg",
            "sudo git status",
            "ssh host uname -a",
            "gh api repos/owner/echoagent/pulls",
            r#"terraform plan -var "x=1 2" | head"#,
        ] {
            if !always_allow_row_is_effective(cmd) {
                continue;
            }
            let h = primary_command_from_script(cmd).unwrap();
            let scope = super::super::default_always_allow_scope(&h.highlighted_words);
            assert!(
                always_allow_scope_persists(&h, scope),
                "row offered for {cmd:?} but default scope {scope} does not persist",
            );
        }
    }
}
