use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use super::shell_token::{CurrentToken, build_insert_token, parse_current_token};
use super::{RankedSuggestion, SuggestContext, SuggestionSource, splice_token_into_line};

const CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_RESULTS: usize = 10;

pub(crate) struct PathProvider;

impl PathProvider {
    pub(crate) async fn suggest(&self, ctx: &SuggestContext) -> Vec<RankedSuggestion> {
        // shell_token quoting is POSIX-only: cmd/pwsh would misparse the
        // escaped line, so Windows serves no deterministic completions.
        if cfg!(windows) {
            return Vec::new();
        }
        let tok = match extract_command_token(ctx.prefix()) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let token_range = (tok.start, ctx.prefix().len());

        let cache = get_or_refresh_path_cache().await;
        let mut results = filter_executables(&tok, token_range, &cache.executables);
        splice_token_into_line(&mut results, &ctx.text, token_range);
        results
    }
}

/// The command token being typed, via the canonical tokenizer: quotes hide
/// separators (`echo "a | gr` is quoted data, not a command position), the
/// cursor must sit in the segment's first word, and — mirroring the file
/// provider — flag-looking tokens never complete.
fn extract_command_token(prefix: &str) -> Option<CurrentToken> {
    let tok = parse_current_token(prefix);
    if tok.tokens_before != 0 || tok.after_redirect || tok.value.is_empty() {
        return None;
    }
    if tok.value.starts_with('-') {
        return None;
    }
    Some(tok)
}

fn filter_executables(
    tok: &CurrentToken,
    token_range: (usize, usize),
    executables: &[String],
) -> Vec<RankedSuggestion> {
    let prefix = tok.value.as_str();
    // Binary search to the first entry >= prefix, then take while starts_with.
    let start = executables.partition_point(|e| e.as_str() < prefix);

    let mut results: Vec<RankedSuggestion> = Vec::new();
    let mut truncated = false;
    for exe in executables[start..]
        .iter()
        .take_while(|exe| exe.starts_with(prefix))
    {
        if results.len() == MAX_RESULTS {
            // An uncapped match remains: the set is not exhaustive.
            truncated = true;
            break;
        }
        results.push(RankedSuggestion {
            display: exe.clone(),
            // Re-quoted like filenames: an executable named `zz;echo PWNED`
            // must insert as ONE word, never a second command.
            insert_text: build_insert_token(tok, "", exe, false),
            description: String::new(),
            source: SuggestionSource::Path,
            priority: 0,
            is_ghost_candidate: false,
            replace_range: Some(token_range),
            token_text: None,
            truncated: false,
        });
    }
    if truncated {
        results.iter_mut().for_each(|s| s.truncated = true);
    }
    results
}

// --- PATH cache ---

struct PathCacheInner {
    executables: Vec<String>,
    updated_at: Instant,
    path_env: String,
}

static PATH_CACHE: OnceLock<ArcSwap<PathCacheInner>> = OnceLock::new();
static PATH_REFRESHING: AtomicBool = AtomicBool::new(false);

async fn get_or_refresh_path_cache() -> Arc<PathCacheInner> {
    let swap = PATH_CACHE.get_or_init(|| {
        ArcSwap::from_pointee(PathCacheInner {
            executables: Vec::new(),
            updated_at: Instant::now() - CACHE_TTL - Duration::from_secs(1),
            path_env: String::new(),
        })
    });

    let current = swap.load_full();
    let current_path = std::env::var("PATH").unwrap_or_default();

    if current.updated_at.elapsed() < CACHE_TTL && current.path_env == current_path {
        return current;
    }

    if PATH_REFRESHING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return current;
    }

    let result = match tokio::task::spawn_blocking(scan_path_dirs).await {
        Ok(executables) => {
            let new = Arc::new(PathCacheInner {
                executables,
                updated_at: Instant::now(),
                path_env: current_path,
            });
            swap.store(Arc::clone(&new));
            new
        }
        Err(_) => current,
    };

    PATH_REFRESHING.store(false, Ordering::Release);
    result
}

fn scan_path_dirs() -> Vec<String> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    scan_path_from(&path_var)
}

fn scan_path_from(path_var: &str) -> Vec<String> {
    let mut executables = Vec::new();

    for dir in std::env::split_paths(path_var) {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.filter_map(Result::ok) {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if !meta.is_file() {
                continue;
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }

            if let Ok(name) = entry.file_name().into_string() {
                executables.push(name);
            }
        }
    }

    executables.sort_unstable();
    executables.dedup();
    executables
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_command_token ---

    fn cmd(prefix: &str) -> Option<(usize, String)> {
        extract_command_token(prefix).map(|t| (t.start, t.value))
    }

    #[test]
    fn prefix_at_start_of_line() {
        assert_eq!(cmd("git"), Some((0, "git".into())));
        assert_eq!(cmd("gi"), Some((0, "gi".into())));
    }

    #[test]
    fn no_prefix_at_argument_position() {
        assert_eq!(cmd("git com"), None);
        assert_eq!(cmd("ls | grep foo"), None);
    }

    #[test]
    fn prefix_after_separators() {
        assert_eq!(cmd("ls | gr"), Some((5, "gr".into())));
        assert_eq!(cmd("make && gi"), Some((8, "gi".into())));
        assert_eq!(cmd("cd /tmp; ls"), Some((9, "ls".into())));
        assert_eq!(cmd("false || tr"), Some((9, "tr".into())));
        assert_eq!(cmd("sleep 10 & ls"), Some((11, "ls".into())));
    }

    #[test]
    fn none_when_empty_after_separator() {
        assert_eq!(cmd("ls | "), None);
        assert_eq!(cmd("sleep 10 & "), None);
        assert_eq!(cmd("&&"), None);
    }

    #[test]
    fn none_for_empty_or_whitespace_input() {
        assert_eq!(cmd(""), None);
        assert_eq!(cmd("   "), None);
    }

    /// A separator inside quotes is data, not a command position — the old
    /// naive segment scan offered executables inside quoted strings.
    #[test]
    fn none_inside_quoted_data() {
        assert_eq!(cmd("echo \"x | gr"), None);
    }

    /// Flag-looking and redirect-target tokens never command-complete.
    #[test]
    fn none_for_flags_and_redirect_targets() {
        assert_eq!(cmd("-gr"), None);
        assert_eq!(cmd("> lo"), None);
    }

    // --- filter_executables ---

    fn tok(prefix: &str) -> CurrentToken {
        parse_current_token(prefix)
    }

    #[test]
    fn filter_matches_prefix() {
        let exes = vec![
            "gcc".into(),
            "git".into(),
            "grep".into(),
            "less".into(),
            "ls".into(),
        ];
        let results = filter_executables(&tok("gr"), (0, 2), &exes);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].insert_text, "grep");
    }

    #[test]
    fn filter_multiple_matches() {
        let exes = vec!["git".into(), "grep".into(), "gzip".into()];
        let results = filter_executables(&tok("g"), (0, 1), &exes);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|s| !s.truncated));
    }

    #[test]
    fn filter_no_match() {
        let exes = vec!["git".into(), "grep".into()];
        assert!(filter_executables(&tok("docker"), (0, 6), &exes).is_empty());
    }

    #[test]
    fn filter_path_suggestions_are_not_ghost() {
        let exes = vec!["git".into()];
        let results = filter_executables(&tok("g"), (0, 1), &exes);
        assert!(!results[0].is_ghost_candidate);
        assert_eq!(results[0].source, SuggestionSource::Path);
        assert_eq!(results[0].priority, 0);
    }

    /// Capped sets mark every row truncated so the pager keeps
    /// dropdown-only semantics (an unshown match could disprove an LCP).
    #[test]
    fn filter_caps_at_max_and_marks_truncated() {
        let exes: Vec<String> = (0..20).map(|i| format!("test_{i:03}")).collect();
        let results = filter_executables(&tok("test"), (0, 4), &exes);
        assert_eq!(results.len(), MAX_RESULTS);
        assert!(results.iter().all(|s| s.truncated));

        let exes: Vec<String> = (0..MAX_RESULTS).map(|i| format!("test_{i:03}")).collect();
        let results = filter_executables(&tok("test"), (0, 4), &exes);
        assert_eq!(results.len(), MAX_RESULTS);
        assert!(results.iter().all(|s| !s.truncated));
    }

    /// The segment-after-pipe token range: accepting `grep` for `ls | gr`
    /// must target only the `gr` token, never the whole line.
    #[test]
    fn filter_stamps_segment_token_range() {
        let t = extract_command_token("ls | gr").unwrap();
        let exes = vec!["grep".into()];
        let results = filter_executables(&t, (t.start, 7), &exes);
        assert_eq!(results[0].replace_range, Some((5, 7)));
        assert_eq!(results[0].insert_text, "grep");
    }

    /// Metacharacter executable names insert as ONE word — accepting
    /// `zz;echo PWNED` must never put a second command on the line.
    #[test]
    fn filter_escapes_metacharacter_executable_names() {
        let exes = vec!["zz;echo PWNED".into()];
        let results = filter_executables(&tok("zz"), (0, 2), &exes);
        assert_eq!(results[0].display, "zz;echo PWNED");
        assert_eq!(results[0].insert_text, "zz\\;echo\\ PWNED");
    }

    /// A quoted command prefix completes inside its quote style.
    #[test]
    fn filter_requotes_quoted_command_prefix() {
        let exes = vec!["grep".into()];
        let results = filter_executables(&tok("\"gr"), (0, 3), &exes);
        assert_eq!(results[0].insert_text, "\"grep\"");
    }

    // --- scan_path_from ---

    #[test]
    fn scan_nonexistent_dir() {
        assert!(scan_path_from("/nonexistent/path/that/doesnt/exist").is_empty());
    }

    #[test]
    fn scan_creates_sorted_deduped_list() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();

        for name in &["zzz_cmd", "aaa_cmd", "mmm_cmd"] {
            let path = bin.join(name);
            fs::write(&path, "").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let result = scan_path_from(bin.to_str().unwrap());
        assert_eq!(result, vec!["aaa_cmd", "mmm_cmd", "zzz_cmd"]);
    }

    #[cfg(unix)]
    #[test]
    fn scan_skips_nonexecutable_files() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();

        let exec_path = bin.join("my_exec");
        fs::write(&exec_path, "").unwrap();
        fs::set_permissions(&exec_path, fs::Permissions::from_mode(0o755)).unwrap();

        let data_path = bin.join("my_data");
        fs::write(&data_path, "").unwrap();
        fs::set_permissions(&data_path, fs::Permissions::from_mode(0o644)).unwrap();

        let result = scan_path_from(bin.to_str().unwrap());
        assert_eq!(result, vec!["my_exec"]);
    }

    #[test]
    fn scan_deduplicates_across_dirs() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let bin1 = dir.path().join("bin1");
        let bin2 = dir.path().join("bin2");
        fs::create_dir(&bin1).unwrap();
        fs::create_dir(&bin2).unwrap();

        for bin in [&bin1, &bin2] {
            let path = bin.join("shared_cmd");
            fs::write(&path, "").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let path_var = format!("{}:{}", bin1.to_str().unwrap(), bin2.to_str().unwrap());
        let result = scan_path_from(&path_var);
        assert_eq!(result, vec!["shared_cmd"]);
    }
}
