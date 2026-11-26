use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use super::{RankedSuggestion, SuggestContext, SuggestionSource, stamp_whole_line_range};
use crate::session::prompt_history;

const CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_CROSS_CWD_ENTRIES: usize = 200;
const MAX_CROSS_CWD_DIRS: usize = 20;
const MAX_SHELL_HISTORY_ENTRIES: usize = 200;
const MAX_RESULTS: usize = 10;

pub(crate) struct HistoryProvider;

impl HistoryProvider {
    pub(crate) async fn suggest(&self, ctx: &SuggestContext) -> Vec<RankedSuggestion> {
        let prefix = ctx.prefix();
        if prefix.is_empty() {
            return Vec::new();
        }

        let local = prompt_history::load_bash_prompts_async(ctx.cwd.clone())
            .await
            .unwrap_or_default();

        let shell_history = get_or_refresh_shell_history_cache().await;
        let cross_cwd = get_or_refresh_cross_cwd_cache().await;

        let mut results =
            rank_history_matches(prefix, &local, &shell_history.commands, &cross_cwd.prompts);
        // History carries the full command: it replaces the whole line.
        stamp_whole_line_range(&mut results, ctx.text.len());
        results
    }
}

/// Rank history matches from three tiers of history sources.
///
/// Priority order: local echoagent bash history > shell history > cross-CWD history.
fn rank_history_matches(
    prefix: &str,
    local: &[String],
    shell_history: &[String],
    cross_cwd: &[String],
) -> Vec<RankedSuggestion> {
    if prefix.is_empty() {
        return Vec::new();
    }

    let mut seen: HashSet<&str> = HashSet::new();
    let mut results = Vec::new();

    for prompt in local.iter().chain(shell_history).chain(cross_cwd) {
        if !prompt.starts_with(prefix) || !seen.insert(prompt.as_str()) {
            continue;
        }

        let base_priority = (10i32).saturating_sub(results.len() as i32).max(0);
        let priority = if *prompt == *prefix {
            base_priority + 30
        } else {
            base_priority
        };

        let text = prompt.clone();
        results.push(RankedSuggestion {
            display: text.clone(),
            insert_text: text,
            description: String::new(),
            source: SuggestionSource::History,
            priority,
            is_ghost_candidate: results.is_empty(),
            replace_range: None,
            token_text: None,
            truncated: false,
        });

        if results.len() >= MAX_RESULTS {
            break;
        }
    }

    results
}

// --- Cross-CWD cache ---

struct CrossCwdCache {
    prompts: Vec<String>,
    updated_at: Instant,
}

static CROSS_CWD_CACHE: OnceLock<ArcSwap<CrossCwdCache>> = OnceLock::new();
static CROSS_CWD_REFRESHING: AtomicBool = AtomicBool::new(false);

async fn get_or_refresh_cross_cwd_cache() -> Arc<CrossCwdCache> {
    let swap = CROSS_CWD_CACHE.get_or_init(|| {
        ArcSwap::from_pointee(CrossCwdCache {
            prompts: Vec::new(),
            updated_at: Instant::now() - CACHE_TTL - Duration::from_secs(1),
        })
    });

    let current = swap.load_full();
    if current.updated_at.elapsed() < CACHE_TTL {
        return current;
    }

    if CROSS_CWD_REFRESHING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return current;
    }

    let result = match tokio::task::spawn_blocking(scan_cross_cwd_prompts).await {
        Ok(prompts) => {
            let new = Arc::new(CrossCwdCache {
                prompts,
                updated_at: Instant::now(),
            });
            swap.store(Arc::clone(&new));
            new
        }
        Err(_) => current,
    };

    CROSS_CWD_REFRESHING.store(false, Ordering::Release);
    result
}

fn scan_cross_cwd_prompts() -> Vec<String> {
    let sessions_dir = crate::util::echo_agent_home::echo_agent_home().join("sessions");
    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut dirs: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), mtime))
        })
        .collect();

    dirs.sort_by(|a, b| b.1.cmp(&a.1));

    let mut prompts = Vec::new();
    for (dir, _) in dirs.iter().take(MAX_CROSS_CWD_DIRS) {
        if prompts.len() >= MAX_CROSS_CWD_ENTRIES {
            break;
        }

        let cwd = match crate::util::echo_agent_home::decode_cwd_from_dirname(dir) {
            Some(decoded) => decoded,
            None => continue,
        };

        if let Ok(dir_prompts) = prompt_history::load_bash_prompts(&cwd) {
            let remaining = MAX_CROSS_CWD_ENTRIES - prompts.len();
            prompts.extend(dir_prompts.into_iter().take(remaining));
        }
    }

    prompts
}

// --- Shell history cache ---

struct ShellHistoryCache {
    commands: Vec<String>,
    updated_at: Instant,
}

/// Longer TTL for shell history — the file rarely changes during a session.
const SHELL_HISTORY_CACHE_TTL: Duration = Duration::from_secs(300);

static SHELL_HISTORY_CACHE: OnceLock<ArcSwap<ShellHistoryCache>> = OnceLock::new();
static SHELL_HISTORY_REFRESHING: AtomicBool = AtomicBool::new(false);

async fn get_or_refresh_shell_history_cache() -> Arc<ShellHistoryCache> {
    let swap = SHELL_HISTORY_CACHE.get_or_init(|| {
        ArcSwap::from_pointee(ShellHistoryCache {
            commands: Vec::new(),
            updated_at: Instant::now() - SHELL_HISTORY_CACHE_TTL - Duration::from_secs(1),
        })
    });

    let current = swap.load_full();
    if current.updated_at.elapsed() < SHELL_HISTORY_CACHE_TTL {
        return current;
    }

    if SHELL_HISTORY_REFRESHING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return current;
    }

    let result = match tokio::task::spawn_blocking(load_shell_history).await {
        Ok(commands) => {
            let new = Arc::new(ShellHistoryCache {
                commands,
                updated_at: Instant::now(),
            });
            swap.store(Arc::clone(&new));
            new
        }
        Err(_) => current,
    };

    SHELL_HISTORY_REFRESHING.store(false, Ordering::Release);
    result
}

/// Detect the user's shell and load history from the appropriate file.
///
/// Returns the most recent commands in reverse chronological order, capped
/// at [`MAX_SHELL_HISTORY_ENTRIES`].
fn load_shell_history() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let shell_name = std::path::Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    let home = match home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    // Respect HISTFILE if set, otherwise use shell-specific defaults.
    let histfile = echo_agent_config::controlled_discovery_home().is_none()
        .then(|| std::env::var("HISTFILE").ok()).flatten().map(PathBuf::from);

    match shell_name {
        "zsh" => {
            let path = histfile.unwrap_or_else(|| home.join(".zsh_history"));
            load_zsh_history(&path)
        }
        "fish" => load_fish_history(&home.join(".local/share/fish/fish_history")),
        // Default to bash (covers "bash" and unknown shells)
        _ => {
            let path = histfile.unwrap_or_else(|| home.join(".bash_history"));
            load_bash_history(&path)
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    echo_agent_config::discovery_home_dir()
}

/// Keep only the most recent `max` entries, reverse to most-recent-first, and
/// deduplicate consecutive identical entries.
fn trim_to_recent(commands: &mut Vec<String>, max: usize) {
    let start = commands.len().saturating_sub(max);
    commands.drain(..start);
    commands.reverse();
    commands.dedup();
}

/// Load bash history (one command per line).
///
/// Skips `#<timestamp>` lines emitted by `HISTTIMEFORMAT`.
fn load_bash_history(path: &std::path::Path) -> Vec<String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut commands = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        // Skip empty lines and HISTTIMEFORMAT timestamp markers (`#1700000000`)
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        commands.push(trimmed.to_owned());
    }

    trim_to_recent(&mut commands, MAX_SHELL_HISTORY_ENTRIES);
    commands
}

/// Load zsh history. Lines may be in extended format: `: timestamp:0;command`
/// or plain format (one command per line).
///
/// TODO: zsh represents multiline commands with backslash-newline continuations
/// in the history file. Currently each continuation line is treated as a
/// separate command, yielding broken fragments for multiline entries.
fn load_zsh_history(path: &std::path::Path) -> Vec<String> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    // zsh history may contain invalid UTF-8 (from metafied bytes); use lossy conversion
    let content = String::from_utf8_lossy(&data);
    let mut commands = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Extended history format: `: 1234567890:0;actual command`
        let cmd = if let Some(rest) = trimmed.strip_prefix(": ") {
            // Find the `;` separator after the timestamp:duration part
            rest.find(';').map(|pos| &rest[pos + 1..]).unwrap_or(rest)
        } else {
            trimmed
        };

        if !cmd.is_empty() {
            commands.push(cmd.to_owned());
        }
    }

    trim_to_recent(&mut commands, MAX_SHELL_HISTORY_ENTRIES);
    commands
}

/// Load fish history. The file uses a YAML-like format:
/// ```text
/// - cmd: some command
///   when: 1234567890
/// - cmd: another command
///   when: 1234567891
/// ```
fn load_fish_history(path: &std::path::Path) -> Vec<String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut commands = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        // Fish history entries start with "- cmd: "
        if let Some(cmd) = line.strip_prefix("- cmd: ") {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                commands.push(cmd.to_owned());
            }
        }
    }

    trim_to_recent(&mut commands, MAX_SHELL_HISTORY_ENTRIES);
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn exact_prefix_match_gets_bonus() {
        let local = vec!["git commit".into(), "git checkout".into()];
        let results = rank_history_matches("git commit", &local, &[], &[]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].insert_text, "git commit");
        assert!(
            results[0].priority >= 30,
            "exact match priority {} should be >= 30",
            results[0].priority
        );
    }

    #[test]
    fn partial_matches_decay_by_position() {
        let local = vec![
            "git commit -m fix".into(),
            "git checkout main".into(),
            "git cherry-pick abc".into(),
        ];
        let results = rank_history_matches("git c", &local, &[], &[]);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].priority, 10);
        assert_eq!(results[1].priority, 9);
        assert_eq!(results[2].priority, 8);
    }

    #[test]
    fn priorities_decrease_per_position() {
        let local: Vec<String> = (0..5).map(|i| format!("test_{i}")).collect();
        let results = rank_history_matches("test", &local, &[], &[]);
        assert_eq!(results.len(), 5);
        let priorities: Vec<i32> = results.iter().map(|r| r.priority).collect();
        assert_eq!(priorities, &[10, 9, 8, 7, 6]);
    }

    #[test]
    fn deduplicates_across_local_and_cross_cwd() {
        let local = vec!["git push".into(), "git pull".into()];
        let cross = vec!["git pull".into(), "git fetch".into()];
        let results = rank_history_matches("git p", &local, &[], &cross);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].insert_text, "git push");
        assert_eq!(results[1].insert_text, "git pull");
    }

    #[test]
    fn local_takes_priority_over_cross_cwd() {
        let local = vec!["git push origin main".into()];
        let cross = vec!["git push origin dev".into()];
        let results = rank_history_matches("git push", &local, &[], &cross);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].insert_text, "git push origin main");
        assert!(results[0].priority > results[1].priority);
    }

    #[test]
    fn first_match_is_ghost_candidate() {
        let local = vec!["ls -la".into(), "ls -lh".into()];
        let results = rank_history_matches("ls", &local, &[], &[]);
        assert!(results[0].is_ghost_candidate);
        assert!(!results[1].is_ghost_candidate);
    }

    #[test]
    fn empty_prefix_returns_nothing() {
        let local = vec!["git commit".into()];
        assert!(rank_history_matches("", &local, &[], &[]).is_empty());
    }

    #[test]
    fn no_matching_prefix_returns_empty() {
        let local = vec!["git commit".into()];
        assert!(rank_history_matches("docker", &local, &[], &[]).is_empty());
    }

    #[test]
    fn caps_at_max_results() {
        let local: Vec<String> = (0..20).map(|i| format!("test_cmd_{i}")).collect();
        let results = rank_history_matches("test", &local, &[], &[]);
        assert_eq!(results.len(), MAX_RESULTS);
    }

    #[test]
    fn cross_cwd_duplicates_filtered() {
        let local = vec!["make build".into()];
        let cross = vec!["make build".into(), "make test".into()];
        let results = rank_history_matches("make", &local, &[], &cross);
        let texts: Vec<&str> = results.iter().map(|r| r.insert_text.as_str()).collect();
        assert_eq!(texts, &["make build", "make test"]);
    }

    #[test]
    fn exact_match_among_partial_matches() {
        let local = vec![
            "cargo build --release".into(),
            "cargo build".into(),
            "cargo bench".into(),
        ];
        let results = rank_history_matches("cargo build", &local, &[], &[]);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].priority, 10);
        assert_eq!(results[1].priority, 9 + 30);
        assert!(results[1].priority > results[0].priority);
    }

    #[test]
    fn empty_local_and_cross_cwd() {
        assert!(rank_history_matches("git", &[], &[], &[]).is_empty());
    }

    #[test]
    fn single_char_prefix() {
        let local = vec!["git commit".into(), "grep foo".into(), "ls".into()];
        let results = rank_history_matches("g", &local, &[], &[]);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].insert_text, "git commit");
        assert_eq!(results[1].insert_text, "grep foo");
    }

    // --- Shell history priority ordering ---

    #[test]
    fn shell_history_ranked_between_local_and_cross_cwd() {
        let local = vec!["git push origin main".into()];
        let shell = vec!["git push origin staging".into()];
        let cross = vec!["git push origin dev".into()];
        let results = rank_history_matches("git push", &local, &shell, &cross);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].insert_text, "git push origin main");
        assert_eq!(results[1].insert_text, "git push origin staging");
        assert_eq!(results[2].insert_text, "git push origin dev");
        // Priority decreases: local > shell > cross
        assert!(results[0].priority > results[1].priority);
        assert!(results[1].priority > results[2].priority);
    }

    #[test]
    fn shell_history_deduplicates_with_local() {
        let local = vec!["ls -la".into()];
        let shell = vec!["ls -la".into(), "ls -lh".into()];
        let results = rank_history_matches("ls", &local, &shell, &[]);
        let texts: Vec<&str> = results.iter().map(|r| r.insert_text.as_str()).collect();
        assert_eq!(texts, &["ls -la", "ls -lh"]);
    }

    // --- Bash history parsing ---

    #[test]
    fn parse_bash_history_basic() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "ls -la").unwrap();
        writeln!(f, "cd /tmp").unwrap();
        writeln!(f, "echo hello").unwrap();
        let commands = load_bash_history(f.path());
        // Reverse chrono order
        assert_eq!(commands, &["echo hello", "cd /tmp", "ls -la"]);
    }

    #[test]
    fn parse_bash_history_skips_empty_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "ls").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "   ").unwrap();
        writeln!(f, "pwd").unwrap();
        let commands = load_bash_history(f.path());
        assert_eq!(commands, &["pwd", "ls"]);
    }

    #[test]
    fn parse_bash_history_caps_at_limit() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..300 {
            writeln!(f, "cmd_{i}").unwrap();
        }
        let commands = load_bash_history(f.path());
        assert_eq!(commands.len(), MAX_SHELL_HISTORY_ENTRIES);
        // Most recent first
        assert_eq!(commands[0], "cmd_299");
    }

    #[test]
    fn parse_bash_history_deduplicates() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "ls").unwrap();
        writeln!(f, "pwd").unwrap();
        writeln!(f, "ls").unwrap();
        let commands = load_bash_history(f.path());
        // After reverse + dedup: ["ls", "pwd", "ls"] -> reversed = ["ls", "pwd", "ls"]
        // dedup removes consecutive dupes only. "ls", "pwd", "ls" has no consecutive dupes.
        assert_eq!(commands, &["ls", "pwd", "ls"]);
    }

    #[test]
    fn parse_bash_history_skips_timestamp_markers() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "#1700000000").unwrap();
        writeln!(f, "ls -la").unwrap();
        writeln!(f, "#1700000001").unwrap();
        writeln!(f, "cd /tmp").unwrap();
        let commands = load_bash_history(f.path());
        assert_eq!(commands, &["cd /tmp", "ls -la"]);
    }

    #[test]
    fn parse_bash_history_comment_only_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "#1700000000").unwrap();
        writeln!(f, "# this is also a comment").unwrap();
        writeln!(f, "#not-a-timestamp-but-still-skipped").unwrap();
        writeln!(f, "echo hello").unwrap();
        let commands = load_bash_history(f.path());
        assert_eq!(commands, &["echo hello"]);
    }

    #[test]
    fn parse_bash_history_missing_file() {
        let commands = load_bash_history(std::path::Path::new("/nonexistent/.bash_history"));
        assert!(commands.is_empty());
    }

    // --- Zsh history parsing ---

    #[test]
    fn parse_zsh_history_extended_format() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, ": 1700000000:0;ls -la").unwrap();
        writeln!(f, ": 1700000001:0;cd /tmp").unwrap();
        writeln!(f, ": 1700000002:0;echo hello world").unwrap();
        let commands = load_zsh_history(f.path());
        assert_eq!(commands, &["echo hello world", "cd /tmp", "ls -la"]);
    }

    #[test]
    fn parse_zsh_history_plain_format() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "ls -la").unwrap();
        writeln!(f, "cd /tmp").unwrap();
        let commands = load_zsh_history(f.path());
        assert_eq!(commands, &["cd /tmp", "ls -la"]);
    }

    #[test]
    fn parse_zsh_history_mixed_format() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "plain command").unwrap();
        writeln!(f, ": 1700000000:0;extended command").unwrap();
        let commands = load_zsh_history(f.path());
        assert_eq!(commands, &["extended command", "plain command"]);
    }

    #[test]
    fn parse_zsh_history_skips_empty_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, ": 1700000000:0;ls").unwrap();
        writeln!(f).unwrap();
        writeln!(f, ": 1700000001:0;pwd").unwrap();
        let commands = load_zsh_history(f.path());
        assert_eq!(commands, &["pwd", "ls"]);
    }

    #[test]
    fn parse_zsh_history_caps_at_limit() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..300 {
            writeln!(f, ": {i}:0;cmd_{i}").unwrap();
        }
        let commands = load_zsh_history(f.path());
        assert_eq!(commands.len(), MAX_SHELL_HISTORY_ENTRIES);
        assert_eq!(commands[0], "cmd_299");
    }

    #[test]
    fn parse_zsh_history_missing_file() {
        let commands = load_zsh_history(std::path::Path::new("/nonexistent/.zsh_history"));
        assert!(commands.is_empty());
    }

    #[test]
    fn parse_zsh_history_extended_with_semicolon_in_command() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, ": 1700000000:0;echo foo; echo bar").unwrap();
        let commands = load_zsh_history(f.path());
        // The command includes everything after the first `;`
        assert_eq!(commands, &["echo foo; echo bar"]);
    }

    // --- Fish history parsing ---

    #[test]
    fn parse_fish_history_basic() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "- cmd: ls -la").unwrap();
        writeln!(f, "  when: 1700000000").unwrap();
        writeln!(f, "- cmd: cd /tmp").unwrap();
        writeln!(f, "  when: 1700000001").unwrap();
        let commands = load_fish_history(f.path());
        assert_eq!(commands, &["cd /tmp", "ls -la"]);
    }

    #[test]
    fn parse_fish_history_no_when() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "- cmd: ls").unwrap();
        writeln!(f, "- cmd: pwd").unwrap();
        let commands = load_fish_history(f.path());
        assert_eq!(commands, &["pwd", "ls"]);
    }

    #[test]
    fn parse_fish_history_skips_non_cmd_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "- cmd: ls").unwrap();
        writeln!(f, "  when: 12345").unwrap();
        writeln!(f, "  paths:").unwrap();
        writeln!(f, "    - /some/path").unwrap();
        writeln!(f, "- cmd: pwd").unwrap();
        let commands = load_fish_history(f.path());
        assert_eq!(commands, &["pwd", "ls"]);
    }

    #[test]
    fn parse_fish_history_caps_at_limit() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..300 {
            writeln!(f, "- cmd: cmd_{i}").unwrap();
            writeln!(f, "  when: {i}").unwrap();
        }
        let commands = load_fish_history(f.path());
        assert_eq!(commands.len(), MAX_SHELL_HISTORY_ENTRIES);
        assert_eq!(commands[0], "cmd_299");
    }

    #[test]
    fn parse_fish_history_missing_file() {
        let commands = load_fish_history(std::path::Path::new("/nonexistent/fish/fish_history"));
        assert!(commands.is_empty());
    }

    #[test]
    fn parse_fish_history_empty_cmd_skipped() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "- cmd: ").unwrap();
        writeln!(f, "- cmd: ls").unwrap();
        let commands = load_fish_history(f.path());
        assert_eq!(commands, &["ls"]);
    }
}
