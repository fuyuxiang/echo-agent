//! End-to-end guard for `auth_provider_command`: a configured external auth
//! provider must actually mint the session credential on the host platform.
//!
//! Regression cover. The provider used to be spawned through a hardcoded
//! `sh -c`. On Windows that either fails to spawn (no `sh` in a default
//! install) or, where Git Bash is present, silently eats the backslashes in a
//! native path — `C:\Windows\System32\whoami.exe` reaches the shell as
//! `C:WindowsSystem32whoami.exe` and exits 127. Either way the auth flow fell
//! through to the built-in browser login, so a configured provider looked like
//! it had been ignored.
//!
//! The test drives the public entry point (`try_ensure_fresh_auth` →
//! `AuthManager::auth` → external refresher → platform shell) and is hermetic:
//! a throwaway `ECHO_AGENT_HOME`, no network, and a provider command that needs no
//! binary beyond what the platform shell already provides.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::Utc;
use echo_agent_runtime::auth::{
    AuthMode, EchoAgentAuth, EchoAgentComConfig, try_ensure_fresh_auth,
};

const SEED_TOKEN: &str = "stale-token-that-must-be-replaced";

/// Point the process at a throwaway echoagent home. `echo_agent_home()` memoizes into a
/// `OnceLock`, so every phase below shares this one directory — which is why
/// they live in a single test rather than racing each other as separate ones.
fn use_temp_echo_agent_home(dir: &Path) {
    // SAFETY: single-threaded test entry, before any thread that reads the
    // environment is spawned.
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", dir);
    }
}

/// Seed an expired credential so `auth()` takes the refresh path; a cold home
/// returns `NotLoggedIn` without ever consulting the provider.
fn seed_expired_credential(home: &Path, scope: &str) {
    let expired = EchoAgentAuth {
        key: SEED_TOKEN.to_owned(),
        auth_mode: AuthMode::External,
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        ..EchoAgentAuth::default()
    };
    let store: BTreeMap<String, EchoAgentAuth> =
        [(scope.to_owned(), expired)].into_iter().collect();
    std::fs::write(
        home.join("auth.json"),
        serde_json::to_string(&store).expect("serialize auth store"),
    )
    .expect("write auth.json");
}

/// Run one provider command through the real auth path and return the token.
async fn mint_with_provider(home: &Path, command: &str) -> String {
    let config = EchoAgentComConfig {
        auth_provider_command: Some(command.to_owned()),
        ..EchoAgentComConfig::default()
    };
    seed_expired_credential(home, &config.auth_scope());

    let auth = try_ensure_fresh_auth(&config).await.unwrap_or_else(|| {
        panic!("auth_provider_command `{command}` was configured but no credential was minted")
    });
    assert_eq!(
        auth.auth_mode,
        AuthMode::External,
        "credential must come from the provider, not a cached or built-in path"
    );
    assert_ne!(
        auth.key, SEED_TOKEN,
        "the expired seed must have been replaced by the provider's output"
    );
    auth.key
}

#[tokio::test]
async fn auth_provider_command_mints_the_session_credential() {
    let home = tempfile::tempdir().expect("tempdir");
    use_temp_echo_agent_home(home.path());

    // `echo <token>` is valid in both `sh -c` and `cmd /C`, so this phase needs
    // no external binary and runs identically on every platform.
    let token = mint_with_provider(home.path(), "echo echo-agent-ext-token").await;
    assert_eq!(token, "echo-agent-ext-token");

    // Windows only: an absolute native path, the form an operator actually
    // writes in config.toml, and the exact shape a POSIX shell mangles. Run
    // after the portable phase so a failure here is unambiguously about
    // backslash handling rather than the provider path in general.
    #[cfg(windows)]
    {
        let token = mint_with_provider(home.path(), r"C:\Windows\System32\whoami.exe").await;
        assert!(
            !token.trim().is_empty(),
            "a native Windows path must reach the provider intact"
        );
    }
}
