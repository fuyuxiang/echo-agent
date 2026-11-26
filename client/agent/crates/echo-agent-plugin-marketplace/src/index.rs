//! Parse repo-level marketplace index.
//!
//! Catalog file lookup, in order: `.echo-agent-plugin/marketplace.json` (preferred),
//! `.echo-agent-plugin/plugin.json`, `.claude-plugin/marketplace.json`,
//! `.claude-plugin/plugin.json` (alternate layout compatibility).
//!
//! When present, an index is the preferred browse source — faster than
//! filesystem scanning and provides curated metadata (category, tags, homepage).

use std::path::Path;

use serde::Deserialize;

use crate::types::MarketplaceRelativePath;

/// Top-level marketplace index.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketplaceIndex {
    /// Marketplace display name.
    pub name: String,
    /// Marketplace description.
    #[serde(default)]
    pub description: Option<String>,
    /// Owner info.
    #[serde(default)]
    pub owner: Option<IndexOwner>,
    /// Indexed plugins.
    #[serde(default)]
    pub plugins: Vec<IndexEntry>,
}

/// Marketplace owner.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexOwner {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// A single plugin entry in the marketplace index.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexEntry {
    /// Plugin name.
    pub name: String,
    /// Version string (from index metadata).
    #[serde(default)]
    pub version: Option<String>,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Category (e.g., "development", "productivity", "design").
    #[serde(default)]
    pub category: Option<String>,
    /// Author info.
    #[serde(default)]
    pub author: Option<IndexAuthor>,
    /// Source location within the marketplace repo.
    #[serde(default)]
    pub source: Option<IndexSource>,
    /// Homepage URL.
    #[serde(default)]
    pub homepage: Option<String>,
    /// Tags/keywords.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Matcher keywords used to associate the plugin with a user request.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Matcher domains used to associate the plugin with a user request.
    #[serde(default)]
    pub domains: Vec<String>,
}

/// Author in an index entry.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexAuthor {
    pub name: String,
}

/// Source location in an index entry.
///
/// Accepts multiple formats:
/// - Object: `{ "type": "local", "path": "./plugins/foo" }`
/// - Object: `{ "source": "url", "url": "https://github.com/...", "ref": "main" }`
/// - Object: `{ "source": "url", "url": "https://...", "sha": "61f1903b..." }` (recommended for vendor pins)
/// - String: `"./plugins/foo"` (shorthand used by some marketplaces)
#[derive(Debug, Clone)]
pub struct IndexSource {
    pub r#type: Option<String>,
    pub path: Option<String>,
    /// Remote git URL (used by superpowers-style marketplaces).
    pub url: Option<String>,
    /// Git ref (branch/tag/commit) for URL sources.
    pub git_ref: Option<String>,
    pub git_sha: Option<String>,
}

impl IndexSource {
    /// Whether this source points to a remote git URL.
    pub fn is_remote(&self) -> bool {
        self.url.is_some()
    }
}

impl<'de> Deserialize<'de> for IndexSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct IndexSourceVisitor;

        impl<'de> de::Visitor<'de> for IndexSourceVisitor {
            type Value = IndexSource;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#"a string path or object with "path" or "url" field"#)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<IndexSource, E> {
                Ok(IndexSource {
                    r#type: Some("local".into()),
                    path: Some(v.to_owned()),
                    url: None,
                    git_ref: None,
                    git_sha: None,
                })
            }

            fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<IndexSource, M::Error> {
                let mut r#type = None;
                let mut source_field: Option<String> = None;
                let mut path = None;
                let mut url = None;
                let mut git_ref = None;
                let mut git_sha = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => r#type = map.next_value()?,
                        // Superpowers format: `"source": "url"` as type discriminator.
                        "source" => source_field = map.next_value()?,
                        "path" => path = map.next_value()?,
                        "url" => url = map.next_value()?,
                        "ref" => git_ref = map.next_value()?,
                        "sha" => git_sha = map.next_value()?,
                        _ => {
                            let _ = map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                // Normalize: if `source` was used instead of `type`, adopt it.
                if r#type.is_none() {
                    r#type = source_field;
                }
                Ok(IndexSource {
                    r#type,
                    path,
                    url,
                    git_ref,
                    git_sha,
                })
            }
        }

        deserializer.deserialize_any(IndexSourceVisitor)
    }
}

impl IndexEntry {
    /// Resolve the plugin path relative to the marketplace root.
    /// Returns `None` for remote URL sources (use `remote_url()` instead).
    pub fn resolved_path(&self) -> Option<String> {
        Some(self.resolved_marketplace_path().ok()?.as_str().to_string())
    }

    pub fn resolved_marketplace_path(&self) -> Result<MarketplaceRelativePath, String> {
        let Some(source) = self.source.as_ref() else {
            return Err("missing source".into());
        };
        if source.is_remote() {
            return Err("remote source has no local path".into());
        }
        let path = source
            .path
            .as_ref()
            .ok_or_else(|| "missing source path".to_string())?;
        MarketplaceRelativePath::parse(path).map_err(|e| e.to_string())
    }

    /// Get the remote git URL for URL-sourced plugins.
    pub fn remote_url(&self) -> Option<(&str, Option<&str>)> {
        let source = self.source.as_ref()?;
        let url = source.url.as_deref()?;
        Some((url, source.git_ref.as_deref()))
    }

    pub fn remote_sha(&self) -> Option<&str> {
        self.source.as_ref()?.git_sha.as_deref()
    }

    pub fn remote_subdir(&self) -> Option<&str> {
        let source = self.source.as_ref()?;
        if !source.is_remote() {
            return None;
        }
        source.path.as_deref()
    }
}

/// Attempt to load the marketplace index from the given root directory.
///
/// Checks (in order):
/// 1. `.echo-agent-plugin/marketplace.json` (preferred EchoAgent convention)
/// 2. `.echo-agent-plugin/plugin.json`
/// 3. `.claude-plugin/marketplace.json` (alternate layout compatibility)
/// 4. `.claude-plugin/plugin.json`
///
/// Returns `None` if no file exists. Returns `Err` if a file exists
/// but can't be parsed.
pub fn load_index(marketplace_root: &Path) -> Result<Option<MarketplaceIndex>, String> {
    let echo_agent_dir = marketplace_root.join(".echo-agent-plugin");
    let claude_dir = marketplace_root.join(".claude-plugin");
    let candidates = [
        echo_agent_dir.join("marketplace.json"),
        echo_agent_dir.join("plugin.json"),
        claude_dir.join("marketplace.json"),
        claude_dir.join("plugin.json"),
    ];

    for index_path in &candidates {
        if !index_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(index_path)
            .map_err(|e| format!("failed to read {}: {e}", index_path.display()))?;

        let index: MarketplaceIndex = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {e}", index_path.display()))?;

        return Ok(Some(index));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_marketplace_json() {
        let json = r#"{
            "$schema": "https://anthropic.com/claude-code/marketplace.schema.json",
            "name": "test-marketplace",
            "description": "Test marketplace",
            "owner": { "name": "Test" },
            "plugins": [
                {
                    "name": "test-plugin",
                    "description": "A test plugin",
                    "category": "development",
                    "author": { "name": "Test Author" },
                    "source": { "type": "local", "path": "./plugins/test-plugin" },
                    "homepage": "https://example.com",
                    "tags": ["test", "example"],
                    "keywords": ["kw1", "kw2"]
                }
            ]
        }"#;

        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.name, "test-marketplace");
        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].name, "test-plugin");
        assert_eq!(index.plugins[0].category.as_deref(), Some("development"));
        assert_eq!(index.plugins[0].tags, vec!["test", "example"]);
        assert_eq!(index.plugins[0].keywords, vec!["kw1", "kw2"]);
        assert_eq!(
            index.plugins[0].resolved_path().as_deref(),
            Some("plugins/test-plugin")
        );
    }

    #[test]
    fn load_index_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_index(dir.path());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn load_index_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("marketplace.json"), "not json").unwrap();
        let result = load_index(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn load_index_valid_echo_agent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_dir = dir.path().join(".echo-agent-plugin");
        std::fs::create_dir_all(&echo_agent_dir).unwrap();
        std::fs::write(
            echo_agent_dir.join("marketplace.json"),
            r#"{"name": "echoagent", "plugins": []}"#,
        )
        .unwrap();
        let result = load_index(dir.path()).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "echoagent");
    }

    #[test]
    fn load_index_echo_agent_dir_takes_precedence_over_claude_dir() {
        let dir = tempfile::tempdir().unwrap();
        for (sub, name) in [
            (".echo-agent-plugin", "echoagent"),
            (".claude-plugin", "claude"),
        ] {
            let d = dir.path().join(sub);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("marketplace.json"),
                format!(r#"{{"name": "{name}", "plugins": []}}"#),
            )
            .unwrap();
        }
        assert_eq!(load_index(dir.path()).unwrap().unwrap().name, "echoagent");
    }

    #[test]
    fn load_index_valid() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("marketplace.json"),
            r#"{"name": "test", "plugins": []}"#,
        )
        .unwrap();
        let result = load_index(dir.path()).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test");
    }

    #[test]
    fn resolved_path_strips_dot_slash() {
        let entry = IndexEntry {
            name: "test".into(),
            version: None,
            description: None,
            category: None,
            author: None,
            source: Some(IndexSource {
                r#type: Some("local".into()),
                path: Some("./plugins/my-plugin".into()),
                url: None,
                git_ref: None,
                git_sha: None,
            }),
            homepage: None,
            tags: vec![],
            keywords: vec![],
            domains: vec![],
        };
        assert_eq!(entry.resolved_path().as_deref(), Some("plugins/my-plugin"));
    }

    #[test]
    fn resolved_path_rejects_traversal() {
        let entry = IndexEntry {
            name: "test".into(),
            version: None,
            description: None,
            category: None,
            author: None,
            source: Some(IndexSource {
                r#type: Some("local".into()),
                path: Some("./plugins/../../secret".into()),
                url: None,
                git_ref: None,
                git_sha: None,
            }),
            homepage: None,
            tags: vec![],
            keywords: vec![],
            domains: vec![],
        };
        assert!(entry.resolved_path().is_none());
        assert!(entry.resolved_marketplace_path().is_err());
    }

    #[test]
    fn parse_string_source_format() {
        // enterprise-style marketplace.json uses plain strings for source.
        let json = r#"{
            "name": "acme-marketplace",
            "description": "Acme plugins",
            "plugins": [
                {
                    "name": "acme-browser",
                    "description": "Browser plugin",
                    "source": "./plugins/acme-browser"
                }
            ]
        }"#;

        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].name, "acme-browser");
        assert!(index.plugins[0].keywords.is_empty());
        assert_eq!(
            index.plugins[0].resolved_path().as_deref(),
            Some("plugins/acme-browser")
        );
    }

    #[test]
    fn parse_mixed_source_formats() {
        // Both object and string source formats in the same index.
        let json = r#"{
            "name": "mixed",
            "plugins": [
                { "name": "a", "source": { "type": "local", "path": "./plugins/a" } },
                { "name": "b", "source": "./plugins/b" }
            ]
        }"#;

        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.plugins.len(), 2);
        assert_eq!(
            index.plugins[0].resolved_path().as_deref(),
            Some("plugins/a")
        );
        assert_eq!(
            index.plugins[1].resolved_path().as_deref(),
            Some("plugins/b")
        );
    }

    #[test]
    fn parse_superpowers_url_source() {
        let json = r#"{
            "name": "superpowers-marketplace",
            "plugins": [
                {
                    "name": "superpowers",
                    "source": {
                        "source": "url",
                        "url": "https://github.com/obra/superpowers.git"
                    },
                    "description": "Core skills",
                    "version": "5.0.7"
                }
            ]
        }"#;

        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].name, "superpowers");
        // resolved_path returns None for URL sources.
        assert!(index.plugins[0].resolved_path().is_none());
        // remote_url returns the URL.
        let (url, git_ref) = index.plugins[0].remote_url().unwrap();
        assert_eq!(url, "https://github.com/obra/superpowers.git");
        assert!(git_ref.is_none());
    }

    #[test]
    fn parse_superpowers_url_source_with_ref() {
        let json = r#"{
            "name": "test",
            "plugins": [
                {
                    "name": "superpowers-dev",
                    "source": {
                        "source": "url",
                        "url": "https://github.com/obra/superpowers.git",
                        "ref": "dev"
                    }
                }
            ]
        }"#;

        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        let (url, git_ref) = index.plugins[0].remote_url().unwrap();
        assert_eq!(url, "https://github.com/obra/superpowers.git");
        assert_eq!(git_ref, Some("dev"));
    }

    #[test]
    fn parse_url_source_with_sha() {
        let json = r#"{
            "name": "test",
            "plugins": [
                {
                    "name": "vercel",
                    "source": {
                        "source": "url",
                        "url": "https://github.com/vercel/vercel-plugin.git",
                        "sha": "61f1903bed7b322c9745f6ba67095bc006de7e63"
                    }
                }
            ]
        }"#;
        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(
            index.plugins[0].remote_url().map(|(u, _)| u),
            Some("https://github.com/vercel/vercel-plugin.git")
        );
        assert_eq!(
            index.plugins[0].remote_sha(),
            Some("61f1903bed7b322c9745f6ba67095bc006de7e63")
        );
    }

    #[test]
    fn url_source_with_path_exposes_remote_subdir() {
        let json = r#"{
            "name": "test",
            "plugins": [
                {
                    "name": "acme",
                    "source": {
                        "source": "url",
                        "url": "https://github.com/acme/agent-skills.git",
                        "sha": "61f1903bed7b322c9745f6ba67095bc006de7e63",
                        "path": "plugins/acme"
                    }
                }
            ]
        }"#;
        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.plugins[0].remote_subdir(), Some("plugins/acme"));
        assert!(index.plugins[0].resolved_path().is_none());
    }

    #[test]
    fn url_source_without_sha_returns_none() {
        let json = r#"{
            "name": "test",
            "plugins": [
                {
                    "name": "p",
                    "source": { "source": "url", "url": "https://example.com/repo.git" }
                }
            ]
        }"#;
        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert!(index.plugins[0].remote_sha().is_none());
    }

    #[test]
    fn parse_full_superpowers_marketplace() {
        // Real-world superpowers-marketplace format.
        let json = r#"{
            "name": "superpowers-marketplace",
            "owner": { "name": "Jesse Vincent" },
            "metadata": { "description": "Skills", "version": "1.0.13" },
            "plugins": [
                {
                    "name": "superpowers",
                    "source": { "source": "url", "url": "https://github.com/obra/superpowers.git" },
                    "description": "Core skills",
                    "version": "5.0.7",
                    "strict": true
                },
                {
                    "name": "elements-of-style",
                    "source": { "source": "url", "url": "https://github.com/obra/the-elements-of-style.git" },
                    "description": "Writing guidance",
                    "version": "1.0.0"
                }
            ]
        }"#;

        let index: MarketplaceIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.name, "superpowers-marketplace");
        assert_eq!(index.plugins.len(), 2);
        // All are URL sources.
        for entry in &index.plugins {
            assert!(entry.resolved_path().is_none());
            assert!(entry.remote_url().is_some());
        }
    }
}
