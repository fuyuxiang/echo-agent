//! `echoagent completions <shell>` — generate shell completion scripts.
//!
//! Used by the installers and npm postinstall; must stay side-effect free
//! (no network, auth, tracing, or tokio).

use clap::CommandFactory as _;
use clap_complete::{Shell, generate};

use crate::app::PagerArgs;

/// Generate and print the completion script for the given shell.
pub fn run(shell: Shell) {
    // Ensure the script always uses the public "echoagent" name (matches historical
    // behavior and what the installers + docs expect).
    let mut cmd = PagerArgs::command().name("echoagent");
    if shell != Shell::Zsh {
        generate(shell, &mut cmd, "echoagent", &mut std::io::stdout());
        return;
    }
    // zsh needs post-processing (see fix_zsh_root_prompt_positional).
    let mut buf = Vec::new();
    generate(shell, &mut cmd, "echoagent", &mut buf);
    match String::from_utf8(buf) {
        Ok(script) => print!("{}", fix_zsh_root_prompt_positional(&script)),
        // clap_complete output is generated from Rust strings, so this arm is
        // unreachable in practice — but the installers run this command, so
        // emit the unmodified script rather than panic.
        Err(e) => {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(e.as_bytes());
        }
    }
}

/// Work around clap_complete's broken zsh output for an optional free-form
/// positional (`[PROMPT]`) preceding the subcommand slot
/// (<https://github.com/clap-rs/clap/issues/6282>).
///
/// The generated root `_arguments` spec emits a `'::prompt …'` slot before
/// the subcommand slot but dispatches subcommands with `case $line[2]`. zsh
/// assigns the typed subcommand to the *prompt* slot (`$line[1]`), leaves
/// `$line[2]` empty, and the dispatch falls through — so `echoagent worktree <TAB>`
/// re-offers every top-level command. (`hide = true` on the positional does
/// not change the generated script.)
///
/// Completing an arbitrary prompt string is useless, so drop the prompt slot
/// and shift the root dispatch to `$line[1]`. Nested subcommand dispatch
/// blocks already use `$line[1]` and are untouched; the three rewritten
/// patterns are unique to the root block (pinned by the test below — delete
/// this whole workaround once upstream fixes the generator).
fn fix_zsh_root_prompt_positional(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    script
        .lines()
        // "prompt" is the clap arg id of the root positional.
        .filter(|line| !line.starts_with("'::prompt -- "))
        .for_each(|line| {
            out.push_str(line);
            out.push('\n');
        });
    for (from, to) in [
        (
            r#"words=($line[2] "${words[@]}")"#,
            r#"words=($line[1] "${words[@]}")"#,
        ),
        (
            r#"curcontext="${curcontext%:*:*}:echo-agent-command-$line[2]:""#,
            r#"curcontext="${curcontext%:*:*}:echo-agent-command-$line[1]:""#,
        ),
        (r#"case $line[2] in"#, r#"case $line[1] in"#),
    ] {
        out = out.replacen(from, to, 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate the zsh completion script exactly like `run` does.
    fn zsh_script() -> String {
        let mut cmd = PagerArgs::command().name("echoagent");
        let mut buf = Vec::new();
        generate(Shell::Zsh, &mut cmd, "echoagent", &mut buf);
        String::from_utf8(buf).expect("completion script is UTF-8")
    }

    // The optional `[PROMPT]` positional (app/cli.rs) makes clap_complete emit
    // a `::prompt` slot before the subcommand slot and dispatch on `$line[2]`,
    // so `echoagent worktree <TAB>` re-offered every top-level command (upstream
    // clap-rs/clap#6282).
    #[test]
    fn zsh_completions_drop_prompt_slot_and_dispatch_on_line_1() {
        let raw = zsh_script();
        // Preconditions: the workaround is still needed. If these start
        // failing, clap_complete fixed the positional handling — delete
        // `fix_zsh_root_prompt_positional` instead of updating the test.
        assert!(raw.contains("'::prompt -- "), "raw script has prompt slot");
        assert!(
            raw.contains("case $line[2] in"),
            "raw root dispatch on $line[2]"
        );

        let fixed = fix_zsh_root_prompt_positional(&raw);
        assert!(
            !fixed.contains("::prompt"),
            "prompt positional must not appear in the emitted zsh script"
        );
        assert!(
            !fixed.contains("$line[2]"),
            "root dispatch must be shifted to $line[1]"
        );
        assert!(
            fixed.contains(r#"curcontext="${curcontext%:*:*}:echo-agent-command-$line[1]:""#),
            "root dispatch context must use $line[1]"
        );
        // Subcommand dispatch blocks (already on $line[1]) must survive.
        assert!(
            fixed.contains("echo-agent-worktree-command-$line[1]"),
            "nested subcommand dispatch must be untouched"
        );
        // The subcommand list itself must still be offered at the root.
        assert!(
            fixed.contains("_echo_agent_commands"),
            "root command list intact"
        );
    }
}
