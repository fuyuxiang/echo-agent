//! Hermetic filesystem and child-environment owner for echoagent integration tests.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

const TEST_API_KEY: &str = "test-key-for-ci";
const REDACTED: &str = "<redacted>";

/// One test's isolated filesystem tree and canonical child environment.
///
/// Construction never mutates the process environment. Child commands start
/// from `env_clear()` and receive only platform essentials, sandbox paths,
/// echoagent network kill switches, and explicit overrides.
pub struct TestSandbox {
    root: TempDir,
    home: PathBuf,
    echo_agent_home: PathBuf,
    workspace: PathBuf,
    temp: PathBuf,
    env: BTreeMap<OsString, OsString>,
}

impl TestSandbox {
    /// Create an isolated non-git workspace with no mock endpoint configured.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Configure construction-time sandbox options.
    pub fn builder() -> TestSandboxBuilder {
        TestSandboxBuilder::default()
    }

    /// Temp root owning every sandbox path.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Isolated `HOME` / `USERPROFILE`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Explicit echoagent state root.
    pub fn echo_agent_home(&self) -> &Path {
        &self.echo_agent_home
    }

    /// Isolated working directory. When built with [`TestSandboxBuilder::git`],
    /// this contains a repository with one committed `README.md`.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Isolated `TMPDIR` / `TMP` / `TEMP`.
    pub fn temp_dir(&self) -> &Path {
        &self.temp
    }

    /// Override one child variable after the hermetic baseline. This is the
    /// supported seam for feature flags and simulated terminal brands.
    pub fn set_env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env
            .insert(key.as_ref().to_owned(), value.as_ref().to_owned());
        self
    }

    /// Apply several explicit child overrides in order; later duplicate keys win.
    pub fn extend_env<I, K, V>(&mut self, overrides: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.env.extend(
            overrides
                .into_iter()
                .map(|(key, value)| (key.as_ref().to_owned(), value.as_ref().to_owned())),
        );
        self
    }

    /// Remove one child variable from the baseline or prior overrides.
    pub fn remove_env(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.env.remove(key.as_ref());
        self
    }

    /// Wire the mock endpoint onto an already-built sandbox.
    pub fn set_mock_url(&mut self, url: impl Into<String>) -> &mut Self {
        apply_mock_url(&mut self.env, url.into());
        self
    }

    /// Return the effective child environment in stable key order.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        self.env
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    /// Apply the effective environment to a Tokio child command. Explicit
    /// command-level `.env(...)` calls made afterward have final precedence.
    pub fn apply_to_tokio_command(&self, cmd: &mut tokio::process::Command) {
        cmd.env_clear().envs(self.env());
    }

    /// Merge the effective environment into a portable PTY command builder.
    /// The caller is responsible for calling `env_clear()` first.
    pub fn apply_to_command_builder(&self, cmd: &mut portable_pty::CommandBuilder) {
        for (key, value) in &self.env {
            cmd.env(key.as_os_str(), value.as_os_str());
        }
    }

    /// Apply the effective environment to a standard child command. Explicit
    /// command-level `.env(...)` calls made afterward have final precedence.
    pub fn apply_to_std_command(&self, cmd: &mut Command) {
        cmd.env_clear().envs(self.env());
    }

    /// Build a detached, non-interactive Git command using this sandbox's
    /// selected binary and cleared child environment.
    pub fn git_command(&self) -> Command {
        let git = self
            .env
            .get(OsStr::new("GIT_BIN_PATH"))
            .map_or_else(|| OsString::from("git"), OsString::to_owned);
        let mut cmd = Command::new(git);
        self.apply_to_std_command(&mut cmd);
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        cmd.stdin(Stdio::null())
            .envs(echo_agent_tty_utils::pager_env());
        for &(key, value) in &echo_agent_tty_utils::GIT_AUTH_SUPPRESSION_ENVS {
            cmd.env(key, value);
        }
        cmd.arg("--no-optional-locks");
        cmd
    }

    /// Values that must be removed from captured child-output diagnostics.
    ///
    /// This intentionally returns values only, never keys. Endpoint URLs,
    /// credentials, and sandbox-owned private paths can be echoed by a failing
    /// child even though process diagnostics never print its environment.
    pub(crate) fn diagnostic_redactions(&self) -> Vec<String> {
        self.env
            .iter()
            .filter(|(key, _)| diagnostic_value_is_sensitive(key))
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .filter(|value| !value.is_empty())
            .collect()
    }

    /// Sanitized, deterministic summary for assertion and spawn diagnostics.
    /// Secret-bearing values are never included.
    pub fn diagnostic_summary(&self) -> String {
        let mut summary = format!(
            "root={} home={} echo_agent_home={} workspace={} temp={}",
            self.root().display(),
            self.home.display(),
            self.echo_agent_home.display(),
            self.workspace.display(),
            self.temp.display(),
        );
        for (key, value) in &self.env {
            let key = key.to_string_lossy();
            let display = if is_secret_key(&key) {
                REDACTED.to_owned()
            } else if is_endpoint_key(&key) {
                sanitize_endpoint(value)
            } else {
                value.to_string_lossy().into_owned()
            };
            let _ = write!(summary, " {key}={display}");
        }
        summary
    }
}

impl Default for TestSandbox {
    fn default() -> Self {
        Self::new()
    }
}

/// Minimal construction-time choices for [`TestSandbox`]. Runtime feature
/// variables belong on [`TestSandbox::set_env`] instead of a growing config.
#[derive(Default)]
pub struct TestSandboxBuilder {
    mock_url: Option<String>,
    git: bool,
}

impl TestSandboxBuilder {
    /// Wire echoagent API, models, feedback, trace, conversation, and web traffic to
    /// a loopback mock endpoint and install the fake CI API key.
    pub fn mock_url(mut self, url: impl Into<String>) -> Self {
        self.mock_url = Some(url.into());
        self
    }

    /// Initialize the workspace as a git repository with one committed file.
    pub fn git(mut self) -> Self {
        self.git = true;
        self
    }

    /// Materialize the filesystem tree and canonical child environment.
    pub fn build(self) -> TestSandbox {
        let root = TempDir::new().expect("create test sandbox root");
        let home = root.path().join("home");
        let echo_agent_home = home.join(".echo-agent");
        let workspace = root.path().join("workspace");
        let temp = root.path().join("tmp");
        for path in [&home, &echo_agent_home, &workspace, &temp] {
            std::fs::create_dir_all(path)
                .unwrap_or_else(|e| panic!("create sandbox path {}: {e}", path.display()));
        }

        let parent_cwd = std::env::current_dir().expect("read parent cwd for test sandbox");
        let mut env = baseline_env(&home, &echo_agent_home, &temp, &parent_cwd);
        if let Some(url) = self.mock_url {
            apply_mock_url(&mut env, url);
        }

        let sandbox = TestSandbox {
            root,
            home,
            echo_agent_home,
            workspace,
            temp,
            env,
        };
        if self.git {
            sandbox.init_git_workspace();
        }
        sandbox
    }
}

impl TestSandbox {
    fn init_git_workspace(&self) {
        run_git(self, &["init"]);
        run_git(self, &["config", "user.email", "test@test.invalid"]);
        run_git(self, &["config", "user.name", "EchoAgent Test"]);
        std::fs::write(self.workspace.join("README.md"), "test file\n")
            .expect("write sandbox git fixture");
        run_git(self, &["add", "-A"]);
        run_git(self, &["commit", "-m", "init", "--no-gpg-sign"]);
    }
}

fn run_git(sandbox: &TestSandbox, args: &[&str]) {
    let mut cmd = sandbox.git_command();
    let git = cmd.get_program().to_owned();
    cmd.args(args).current_dir(sandbox.workspace());
    let output = cmd.output().unwrap_or_else(|e| {
        panic!(
            "failed to spawn git at {} for `git {}`: {e}\n{}",
            Path::new(&git).display(),
            args.join(" "),
            sandbox.diagnostic_summary(),
        )
    });
    assert!(
        output.status.success(),
        "git {} failed (exit {:?}):\n{}\n{}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        sandbox.diagnostic_summary(),
    );
}

fn apply_mock_url(env: &mut BTreeMap<OsString, OsString>, url: String) {
    for key in [
        "ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL",
        "ECHO_AGENT_API_BASE_URL",
        "ECHO_AGENT_MODELS_BASE_URL",
        "ECHO_AGENT_FEEDBACK_BASE_URL",
        "ECHO_AGENT_TRACE_UPLOAD_URL",
        "ECHO_AGENT_MANAGED_CONFIG_URL",
        "ECHO_AGENT_CODE_WEB_URL",
        "ECHO_AGENT_CONVERSATIONS_BASE_URL",
    ] {
        env.insert(key.into(), url.clone().into());
    }
    env.insert("ECHO_AGENT_API_KEY".into(), TEST_API_KEY.into());
}

fn baseline_env(
    home: &Path,
    echo_agent_home: &Path,
    temp: &Path,
    parent_cwd: &Path,
) -> BTreeMap<OsString, OsString> {
    let parent_env = std::env::vars_os().collect();
    baseline_env_from_parent(home, echo_agent_home, temp, parent_cwd, &parent_env)
}

fn baseline_env_from_parent(
    home: &Path,
    echo_agent_home: &Path,
    temp: &Path,
    parent_cwd: &Path,
    parent_env: &BTreeMap<OsString, OsString>,
) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    for key in platform_allowlist() {
        if let Some(value) = parent_env.get(OsStr::new(key)) {
            env.insert((*key).into(), value.to_owned());
        }
    }
    apply_hermetic_git_env(&mut env, parent_cwd, parent_env);
    #[cfg(unix)]
    env.entry("SHELL".into())
        .or_insert_with(|| OsString::from("/bin/sh"));

    for (key, value) in [
        ("HOME", home),
        ("USERPROFILE", home),
        ("ECHO_AGENT_HOME", echo_agent_home),
        ("TMPDIR", temp),
        ("TMP", temp),
        ("TEMP", temp),
    ] {
        env.insert(key.into(), value.as_os_str().to_owned());
    }
    for (key, value) in [
        ("ECHO_AGENT_TELEMETRY_ENABLED", "false"),
        ("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD", "false"),
        ("ECHO_AGENT_FEEDBACK_ENABLED", "false"),
        ("ECHO_AGENT_TRACE_UPLOAD", "false"),
        ("ECHO_AGENT_INSTRUMENTATION", "disabled"),
        ("OTEL_SDK_DISABLED", "true"),
        ("DISABLE_TELEMETRY", "1"),
        ("DISABLE_FEEDBACK_COMMAND", "1"),
        ("ECHO_AGENT_DISABLE_AUTOUPDATER", "1"),
        ("ECHO_AGENT_PROMPT_SUGGESTIONS", "false"),
        // The sandbox's empty home is exactly the "no mode configured" state
        // that soft-defaults interactive launches into auto. Pin the gate off
        // for deterministic ask-mode behavior; auto-mode tests re-enable it
        // via `set_env` (later overrides win).
        ("ECHO_AGENT_AUTO_PERMISSION_MODE", "0"),
        // Post-turn summary side-calls would add unscripted requests to the
        // mock server and break exact wire-traffic assertions.
        ("ECHO_AGENT_TURN_SUMMARY", "0"),
        ("NO_PROXY", "127.0.0.1,localhost,::1"),
        ("no_proxy", "127.0.0.1,localhost,::1"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", ""),
        ("GIT_LFS_SKIP_SMUDGE", "1"),
        ("PAGER", platform_pager()),
        ("GIT_PAGER", platform_pager()),
    ] {
        env.insert(key.into(), value.into());
    }
    env.insert(
        "GIT_CONFIG_GLOBAL".into(),
        echo_agent_home.join("gitconfig").into_os_string(),
    );
    env
}

fn apply_hermetic_git_env(
    env: &mut BTreeMap<OsString, OsString>,
    parent_cwd: &Path,
    parent_env: &BTreeMap<OsString, OsString>,
) {
    let Some(git_bin) = parent_env.get(OsStr::new("GIT_BIN_PATH")) else {
        return;
    };
    let git_bin = PathBuf::from(git_bin);
    let git_bin = if git_bin.is_absolute() {
        git_bin
    } else {
        parent_cwd.join(git_bin)
    };
    let Some(parent) = git_bin.parent().map(Path::to_owned) else {
        return;
    };

    let mut paths = vec![parent.to_owned()];
    if let Some(path) = parent_env.get(OsStr::new("PATH")) {
        paths.extend(std::env::split_paths(path));
    }
    let path = std::env::join_paths(paths).unwrap_or_else(|_| parent.as_os_str().to_owned());
    env.insert("GIT_BIN_PATH".into(), git_bin.into_os_string());
    env.insert("GIT_EXEC_PATH".into(), parent.into_os_string());
    env.insert("PATH".into(), path);
}

fn platform_allowlist() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "WINDIR",
            "ComSpec",
            "NUMBER_OF_PROCESSORS",
            "GIT_BIN_PATH",
        ]
    }
    #[cfg(not(windows))]
    {
        &[
            "PATH",
            "LANG",
            "LC_ALL",
            "DYLD_LIBRARY_PATH",
            "LD_LIBRARY_PATH",
            "GIT_BIN_PATH",
            "SHELL",
        ]
    }
}

fn platform_pager() -> &'static str {
    #[cfg(unix)]
    {
        "cat"
    }
    #[cfg(not(unix))]
    {
        ""
    }
}

fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    let segments: Vec<_> = upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.iter().any(|segment| {
        matches!(
            *segment,
            "TOKEN"
                | "SECRET"
                | "PASSWORD"
                | "PASSWD"
                | "PASS"
                | "KEY"
                | "AUTH"
                | "AUTHORIZATION"
                | "CREDENTIAL"
                | "CREDENTIALS"
                | "COOKIE"
                | "SESSION"
        ) || segment.ends_with("TOKEN")
            || segment.ends_with("SECRET")
            || segment.ends_with("PASSWORD")
            || segment.ends_with("CREDENTIAL")
            || segment.ends_with("CREDENTIALS")
            || segment.ends_with("APIKEY")
    })
}

fn is_endpoint_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("URL") || upper.contains("ENDPOINT") || upper.contains("PROXY")
}

fn diagnostic_value_is_sensitive(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    is_secret_key(&key)
        || is_endpoint_key(&key)
        || matches!(
            key.to_ascii_uppercase().as_str(),
            "HOME"
                | "USERPROFILE"
                | "ECHO_AGENT_HOME"
                | "TMPDIR"
                | "TMP"
                | "TEMP"
                | "GIT_CONFIG_GLOBAL"
        )
}

fn sanitize_endpoint(value: &OsStr) -> String {
    let Ok(mut url) = url::Url::parse(value.to_string_lossy().as_ref()) else {
        return REDACTED.to_owned();
    };
    let Some(host) = url.host() else {
        return REDACTED.to_owned();
    };
    let loopback = match host {
        url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    };
    if !loopback || !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return REDACTED.to_owned();
    }

    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_value(sandbox: &TestSandbox, key: &str) -> Option<OsString> {
        sandbox
            .env()
            .into_iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn owns_distinct_isolated_paths() {
        let sandbox = TestSandbox::new();
        for path in [
            sandbox.home(),
            sandbox.echo_agent_home(),
            sandbox.workspace(),
            sandbox.temp_dir(),
        ] {
            assert!(path.starts_with(sandbox.root()), "{}", path.display());
            assert!(path.is_dir(), "{}", path.display());
        }
        assert_ne!(sandbox.home(), sandbox.workspace());
        assert_ne!(sandbox.home(), sandbox.temp_dir());
        assert_eq!(
            sandbox.echo_agent_home(),
            sandbox.home().join(".echo-agent")
        );
    }

    #[test]
    fn separate_instances_do_not_share_paths() {
        let first = TestSandbox::new();
        let second = TestSandbox::new();
        assert_ne!(first.root(), second.root());
        assert_ne!(first.home(), second.home());
        assert_ne!(first.workspace(), second.workspace());
        assert_ne!(first.temp_dir(), second.temp_dir());
    }

    #[test]
    fn git_workspace_smoke_uses_committed_fixture() {
        let sandbox = TestSandbox::builder().git().build();
        assert!(sandbox.workspace().join(".git").is_dir());
        assert_eq!(
            std::fs::read_to_string(sandbox.workspace().join("README.md")).unwrap(),
            "test file\n"
        );
        let mut cmd = sandbox.git_command();
        cmd.args(["status", "--porcelain"])
            .current_dir(sandbox.workspace());
        let output = cmd.output().expect("run git status in sandbox");
        assert!(
            output.status.success(),
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "workspace must start clean");
    }

    fn resolved_baseline_env(
        parent_cwd: &Path,
        parent_env: BTreeMap<OsString, OsString>,
    ) -> BTreeMap<OsString, OsString> {
        let root = tempfile::tempdir().expect("create baseline fixture");
        baseline_env_from_parent(
            &root.path().join("home"),
            &root.path().join("home/.echo-agent"),
            &root.path().join("tmp"),
            parent_cwd,
            &parent_env,
        )
    }

    #[test]
    fn relative_git_bin_path_resolves_against_parent_cwd() {
        let parent = tempfile::tempdir().expect("create parent cwd fixture");
        let parent_cwd = parent.path();
        let relative_git = Path::new("external/git_hermetic/bin/git");
        let env = resolved_baseline_env(
            parent_cwd,
            BTreeMap::from([
                (OsString::from("GIT_BIN_PATH"), relative_git.into()),
                (OsString::from("PATH"), OsString::from("/usr/bin")),
            ]),
        );
        let git_bin = parent_cwd.join(relative_git);
        let parent = git_bin.parent().expect("git binary parent");
        assert_eq!(
            env.get(OsStr::new("GIT_BIN_PATH")).map(OsString::as_os_str),
            Some(git_bin.as_os_str())
        );
        assert_eq!(
            env.get(OsStr::new("GIT_EXEC_PATH"))
                .map(OsString::as_os_str),
            Some(parent.as_os_str())
        );
        assert_eq!(
            std::env::split_paths(env.get(OsStr::new("PATH")).expect("git PATH"))
                .next()
                .as_deref(),
            Some(parent)
        );
    }

    #[test]
    fn absent_git_bin_path_preserves_baseline_path_without_git_vars() {
        let path = OsString::from("/ordinary/bin");
        let env = resolved_baseline_env(
            Path::new("/bazel/execroot/workspace"),
            BTreeMap::from([(OsString::from("PATH"), path.to_owned())]),
        );
        assert_eq!(env.get(OsStr::new("PATH")), Some(&path));
        assert!(!env.contains_key(OsStr::new("GIT_BIN_PATH")));
        assert!(!env.contains_key(OsStr::new("GIT_EXEC_PATH")));
    }

    #[test]
    fn git_command_uses_sandbox_state_without_process_global_mutation() {
        let root = TempDir::new().expect("create git command fixture");
        let git = root.path().join("git-dist/bin/git");
        let git_parent = git.parent().expect("git binary parent");
        let env = resolved_baseline_env(
            root.path(),
            BTreeMap::from([
                (OsString::from("GIT_BIN_PATH"), git.as_os_str().to_owned()),
                (OsString::from("PATH"), OsString::from("/ordinary/bin")),
            ]),
        );
        let sandbox = TestSandbox {
            home: root.path().join("home"),
            echo_agent_home: root.path().join("home/.echo-agent"),
            workspace: root.path().join("workspace"),
            temp: root.path().join("tmp"),
            root,
            env,
        };

        let process_git_env =
            ["GIT_BIN_PATH", "GIT_EXEC_PATH", "PATH"].map(|key| (key, std::env::var_os(key)));
        let cmd = sandbox.git_command();
        assert_eq!(
            ["GIT_BIN_PATH", "GIT_EXEC_PATH", "PATH"].map(|key| (key, std::env::var_os(key))),
            process_git_env
        );
        let command_env: BTreeMap<_, _> = cmd
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect();
        assert_eq!(cmd.get_program(), git);
        assert_eq!(
            command_env
                .get(OsStr::new("GIT_BIN_PATH"))
                .and_then(Option::as_deref),
            Some(git.as_os_str())
        );
        assert_eq!(
            command_env
                .get(OsStr::new("GIT_EXEC_PATH"))
                .and_then(Option::as_deref),
            Some(git_parent.as_os_str())
        );
        assert_eq!(
            std::env::split_paths(
                command_env
                    .get(OsStr::new("PATH"))
                    .and_then(Option::as_deref)
                    .expect("git PATH"),
            )
            .next()
            .as_deref(),
            Some(git_parent)
        );
        assert_eq!(
            command_env
                .get(OsStr::new("GIT_TERMINAL_PROMPT"))
                .and_then(Option::as_deref),
            Some(OsStr::new("0"))
        );
        assert_eq!(
            command_env
                .get(OsStr::new("GIT_SSH_COMMAND"))
                .and_then(Option::as_deref),
            Some(OsStr::new("ssh -o BatchMode=yes"))
        );
        assert_eq!(
            cmd.get_args().next(),
            Some(OsStr::new("--no-optional-locks"))
        );
    }

    #[test]
    fn baseline_is_hermetic_and_network_quiet() {
        let sandbox = TestSandbox::builder()
            .mock_url("http://127.0.0.1:43123/v1")
            .build();
        assert_eq!(env_value(&sandbox, "HOME"), Some(sandbox.home().into()));
        assert_eq!(
            env_value(&sandbox, "ECHO_AGENT_HOME"),
            Some(sandbox.echo_agent_home().into())
        );
        assert_eq!(
            env_value(&sandbox, "TMPDIR"),
            Some(sandbox.temp_dir().into())
        );
        assert_eq!(
            env_value(&sandbox, "ECHO_AGENT_API_KEY").as_deref(),
            Some(OsStr::new(TEST_API_KEY))
        );
        assert_eq!(
            env_value(&sandbox, "ECHO_AGENT_DISABLE_AUTOUPDATER").as_deref(),
            Some(OsStr::new("1"))
        );
        assert_eq!(
            env_value(&sandbox, "ECHO_AGENT_TELEMETRY_TRACE_UPLOAD").as_deref(),
            Some(OsStr::new("false"))
        );
        assert_eq!(
            env_value(&sandbox, "NO_PROXY").as_deref(),
            Some(OsStr::new("127.0.0.1,localhost,::1"))
        );
        for proxy in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            assert_eq!(env_value(&sandbox, proxy), None, "{proxy} must not leak");
        }
        assert_eq!(env_value(&sandbox, "ECHO_AGENT_LEADER_SOCKET"), None);
        assert_eq!(env_value(&sandbox, "ECHO_AGENT_DISABLE_WEB_FETCH"), None);
        assert_eq!(env_value(&sandbox, "ECHO_AGENT_WEB_FETCH"), None);
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_policy_preserves_host_or_falls_back_and_can_be_overridden() {
        let mut sandbox = TestSandbox::new();
        let expected = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
        assert_eq!(env_value(&sandbox, "SHELL"), Some(expected));

        sandbox.set_env("SHELL", "/bin/bash");
        assert_eq!(
            env_value(&sandbox, "SHELL").as_deref(),
            Some(OsStr::new("/bin/bash"))
        );

        let mut cmd = Command::new("unused");
        sandbox.apply_to_std_command(&mut cmd);
        assert_eq!(
            cmd.get_envs()
                .find(|(key, _)| *key == OsStr::new("SHELL"))
                .and_then(|(_, value)| value),
            Some(OsStr::new("/bin/bash"))
        );
    }

    #[test]
    fn command_application_clears_ambient_env_and_command_override_wins() {
        let sandbox = TestSandbox::new();
        let mut cmd = Command::new("unused");
        cmd.env("AMBIENT_SECRET", "must-disappear")
            .env("ECHO_AGENT_PROMPT_SUGGESTIONS", "ambient");
        sandbox.apply_to_std_command(&mut cmd);
        cmd.env("ECHO_AGENT_PROMPT_SUGGESTIONS", "command");
        let env: BTreeMap<_, _> = cmd
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();
        assert!(!env.contains_key(OsStr::new("AMBIENT_SECRET")));
        assert_eq!(
            env.get(OsStr::new("ECHO_AGENT_PROMPT_SUGGESTIONS"))
                .map(OsString::as_os_str),
            Some(OsStr::new("command"))
        );
    }

    #[test]
    fn explicit_overrides_win_and_can_remove_baseline_entries() {
        let mut sandbox = TestSandbox::new();
        sandbox
            .set_env("TERM_PROGRAM", "vscode")
            .set_env("ECHO_AGENT_PROMPT_SUGGESTIONS", "true")
            .set_env("NO_PROXY", "override.invalid")
            .remove_env("ECHO_AGENT_DISABLE_AUTOUPDATER");
        assert_eq!(
            env_value(&sandbox, "TERM_PROGRAM").as_deref(),
            Some(OsStr::new("vscode"))
        );
        assert_eq!(
            env_value(&sandbox, "ECHO_AGENT_PROMPT_SUGGESTIONS").as_deref(),
            Some(OsStr::new("true"))
        );
        assert_eq!(
            env_value(&sandbox, "NO_PROXY").as_deref(),
            Some(OsStr::new("override.invalid"))
        );
        assert_eq!(env_value(&sandbox, "ECHO_AGENT_DISABLE_AUTOUPDATER"), None);
    }

    #[test]
    fn cross_platform_home_and_temp_names_are_present() {
        let sandbox = TestSandbox::new();
        assert_eq!(
            env_value(&sandbox, "USERPROFILE"),
            Some(sandbox.home().into())
        );
        assert_eq!(env_value(&sandbox, "TEMP"), Some(sandbox.temp_dir().into()));
        assert_eq!(env_value(&sandbox, "TMP"), Some(sandbox.temp_dir().into()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_platform_essentials_are_allowlisted() {
        let sandbox = TestSandbox::new();
        for essential in ["PATH", "PATHEXT", "SystemRoot", "ComSpec"] {
            if std::env::var_os(essential).is_some() {
                assert!(env_value(&sandbox, essential).is_some(), "{essential}");
            }
        }
    }

    #[test]
    fn diagnostics_fail_closed_for_credential_keys() {
        let mut sandbox = TestSandbox::new();
        for (key, value) in [
            ("CUSTOM_TOKEN", "token-do-not-print"),
            ("SERVICE_API_KEY", "api-key-do-not-print"),
            ("clientSecret", "secret-do-not-print"),
            ("DB_PASSWORD_FILE", "/secret/password-file"),
            ("AWS_CREDENTIALS", "credentials-do-not-print"),
            ("SESSION_COOKIE", "cookie-do-not-print"),
            ("ECHO_AGENT_DEPLOYMENT_KEY", "deployment-key-do-not-print"),
            ("ECHO_AGENT_EXTRA_AUTH_KEY", "alpha-test-key-do-not-print"),
            ("AWS_ACCESS_KEY_ID", "aws-access-key-do-not-print"),
            ("PRIVATE_KEY", "private-key-do-not-print"),
        ] {
            sandbox.set_env(key, value);
        }
        sandbox.set_env("SAFE_FEATURE", "enabled");
        let summary = sandbox.diagnostic_summary();
        for key in [
            "CUSTOM_TOKEN",
            "SERVICE_API_KEY",
            "clientSecret",
            "DB_PASSWORD_FILE",
            "AWS_CREDENTIALS",
            "SESSION_COOKIE",
            "ECHO_AGENT_DEPLOYMENT_KEY",
            "ECHO_AGENT_EXTRA_AUTH_KEY",
            "AWS_ACCESS_KEY_ID",
            "PRIVATE_KEY",
        ] {
            assert!(summary.contains(&format!("{key}=<redacted>")), "{summary}");
        }
        assert!(summary.contains("SAFE_FEATURE=enabled"), "{summary}");
        for secret in [
            "token-do-not-print",
            "api-key-do-not-print",
            "secret-do-not-print",
            "/secret/password-file",
            "credentials-do-not-print",
            "cookie-do-not-print",
            "deployment-key-do-not-print",
            "alpha-test-key-do-not-print",
            "aws-access-key-do-not-print",
            "private-key-do-not-print",
        ] {
            assert!(!summary.contains(secret), "{summary}");
        }
    }

    #[test]
    fn diagnostics_show_only_sanitized_loopback_urls() {
        let cases = [
            (
                "HTTP_URL",
                "http://user:password@127.0.0.1:43123/v1?token=secret#fragment",
                "http://127.0.0.1:43123/v1",
            ),
            (
                "HTTPS_URL",
                "https://localhost:43124/path?api_key=secret",
                "https://localhost:43124/path",
            ),
            (
                "IPV6_URL",
                "http://user:password@[::1]:43125/v1#secret",
                "http://[::1]:43125/v1",
            ),
            (
                "IPV4_OTHER_LOOPBACK_URL",
                "http://127.0.0.2:43126/v1?secret=yes",
                "http://127.0.0.2:43126/v1",
            ),
        ];
        let mut sandbox = TestSandbox::new();
        for (key, raw, _) in cases {
            sandbox.set_env(key, raw);
        }
        sandbox
            .set_env(
                "REMOTE_URL",
                "https://user:password@example.test/v1?token=secret",
            )
            .set_env("MALFORMED_URL", "not a url password=secret")
            .set_env("HTTPS_PROXY", "https://user:pass@proxy.example.test");

        let summary = sandbox.diagnostic_summary();
        for (key, _, expected) in cases {
            assert!(summary.contains(&format!("{key}={expected}")), "{summary}");
        }
        for key in ["REMOTE_URL", "MALFORMED_URL", "HTTPS_PROXY"] {
            assert!(summary.contains(&format!("{key}=<redacted>")), "{summary}");
        }
        for secret in ["user", "password", "token=secret", "fragment", "pass@"] {
            assert!(!summary.contains(secret), "{summary}");
        }
        assert!(!summary.contains(TEST_API_KEY), "{summary}");
    }
}
