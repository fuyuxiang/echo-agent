//! Runtime-configurable parameters for the `web_fetch` tool.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::register_resource;

// Safety-boundary constants. Not configurable.
pub const MAX_URL_LENGTH: usize = 2_000;
pub const MAX_REDIRECTS: usize = 10;
pub const USER_AGENT_STRING: &str =
    "Mozilla/5.0 (compatible; echo-agent-agent/1.0; +https://echo-agent.invalid)";

/// Runtime-configurable parameters for the `web_fetch` tool.
///
/// Injected via `Params<WebFetchParams>` in `SharedResources`.
/// All fields are optional — `None` means "use built-in default."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebFetchParams {
    /// Cache time-to-live in seconds. Default: 900 (15 minutes).
    pub cache_ttl_secs: Option<u64>,
    /// Maximum number of cached pages. Default: 128.
    pub max_cache_entries: Option<usize>,
    /// HTTP request timeout in seconds. Default: 60.
    pub timeout_secs: Option<u64>,
    /// Maximum response body size in bytes. Default: 10 MB.
    pub max_content_length: Option<usize>,
    /// Maximum inline markdown output length in bytes. Default: 100,000.
    pub max_markdown_length: Option<usize>,
    /// Model context window size in tokens. Used to enforce 3% cap on web content.
    pub context_window_tokens: Option<u64>,
    /// Domains the tool is allowed to fetch. All other
    /// domains are rejected before any network I/O.
    /// Defaults to `DEFAULT_ALLOWED_DOMAINS` if no
    /// list given.
    #[serde(default)]
    pub allowed_domains: Option<Vec<String>>,
    /// Optional egress proxy endpoint. When set, all HTTP requests are
    /// routed through this URL.
    #[serde(default)]
    pub proxy_endpoint: Option<String>,
    /// When true, allow fetches to **explicit** loopback hosts only
    /// (`localhost`, `127.0.0.0/8`, `::1`). Private/metadata stay blocked.
    /// Default: `false` (fail closed). Set via `[toolset.web_fetch]
    /// allow_local = true` or `ECHO_AGENT_WEB_FETCH_ALLOW_LOCAL=1`.
    #[serde(default)]
    pub allow_local: Option<bool>,
}

register_resource!("echo_agent_build", "WebFetch", WebFetchParams);

// Keep defaults here so call-sites don't have to manage unwrapping.
// Vars are still public following other conventions though.
impl WebFetchParams {
    pub fn cache_ttl_secs(&self) -> Duration {
        Duration::from_secs(self.cache_ttl_secs.unwrap_or(15 * 60))
    }

    pub fn max_cache_entries(&self) -> usize {
        self.max_cache_entries.unwrap_or(128)
    }

    pub fn timeout_secs(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(60))
    }

    pub fn max_content_length(&self) -> usize {
        self.max_content_length.unwrap_or(10 * 1024 * 1024)
    }

    pub fn max_markdown_length(&self) -> usize {
        self.max_markdown_length.unwrap_or(100_000)
    }

    pub fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens.unwrap_or(128_000)
    }

    pub fn allow_local(&self) -> bool {
        self.allow_local.unwrap_or(false)
    }

    pub fn allowed_domains(&self) -> Vec<String> {
        match &self.allowed_domains {
            Some(v) => v.clone(),
            None => DEFAULT_ALLOWED_DOMAINS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

/// Default allowlist for web_fetch tool.
/// Note: GET-only preapproved domains. Path-scoped entries (e.g. vercel.com/docs) are included as-is.
pub static DEFAULT_ALLOWED_DOMAINS: &[&str] = &[
    // EchoAgent
    "echo.agent",
    "console.echo-agent.invalid",
    "docs.echo-agent.invalid",
    "api.echo-agent.invalid",
    // Programming languages
    "docs.python.org",
    "en.cppreference.com",
    "docs.oracle.com",
    "learn.microsoft.com",
    "developer.mozilla.org",
    "go.dev",
    "pkg.go.dev",
    "www.php.net",
    "docs.swift.org",
    "kotlinlang.org",
    "ruby-doc.org",
    "doc.rust-lang.org",
    "docs.rs",
    "www.typescriptlang.org",
    // Web and JS frameworks
    "react.dev",
    "angular.io",
    "vuejs.org",
    "nextjs.org",
    "expressjs.com",
    "nodejs.org",
    "bun.sh",
    "jquery.com",
    "getbootstrap.com",
    "tailwindcss.com",
    "d3js.org",
    "threejs.org",
    "redux.js.org",
    "webpack.js.org",
    "jestjs.io",
    "reactrouter.com",
    // Python frameworks
    "docs.djangoproject.com",
    "flask.palletsprojects.com",
    "fastapi.tiangolo.com",
    "pandas.pydata.org",
    "numpy.org",
    "www.tensorflow.org",
    "pytorch.org",
    "scikit-learn.org",
    "matplotlib.org",
    "requests.readthedocs.io",
    "jupyter.org",
    // PHP frameworks
    "laravel.com",
    "symfony.com",
    "wordpress.org",
    // Java frameworks
    "docs.spring.io",
    "hibernate.org",
    "tomcat.apache.org",
    "gradle.org",
    "maven.apache.org",
    // .NET
    "asp.net",
    "dotnet.microsoft.com",
    "nuget.org",
    "blazor.net",
    // Mobile
    "reactnative.dev",
    "docs.flutter.dev",
    "developer.apple.com",
    "developer.android.com",
    // Data science / ML
    "keras.io",
    "spark.apache.org",
    "huggingface.co",
    "www.kaggle.com",
    // Databases
    "redis.io",
    "www.postgresql.org",
    "dev.mysql.com",
    "www.sqlite.org",
    "graphql.org",
    "prisma.io",
    // Cloud and DevOps
    "docs.aws.amazon.com",
    "cloud.google.com",
    "kubernetes.io",
    "www.docker.com",
    "www.terraform.io",
    "www.ansible.com",
    "vercel.com/docs",
    "docs.netlify.com",
    "devcenter.heroku.com",
    // Testing and monitoring
    "cypress.io",
    "selenium.dev",
    // Game development
    "docs.unity.com",
    "docs.unrealengine.com",
    // Other tools
    "git-scm.com",
    "nginx.org",
    "httpd.apache.org",
];
