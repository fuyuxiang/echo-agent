//! Checks that the CLI config-reference page matches the live registries.
//!
//! The page
//! `terminal/echo-agent-pager/docs/user-guide/26-config-reference.md` is the
//! source. Edit that file; CI fails when a registered key has no row, a
//! `features.*` / MCP row names an unknown key, or a Requirements / Managed
//! cell disagrees with the resolver metadata. The pager extracts the file to
//! `~/.echo-agent/docs/user-guide/` on launch.

use std::collections::BTreeMap;
use std::path::PathBuf;

use echo_agent_config_types::{FEATURES, KNOWN_MCP_SERVER_FIELDS};

use crate::agent::config::UNMIRRORED_BOOLEAN_FEATURES;
use crate::util::config::MANAGED_WINS_OVER_USER;

pub const USER_GUIDE_FILENAME: &str = "26-config-reference.md";

/// Keys the pager / `load_from_disk()` read from user `config.toml` only.
const USER_ONLY_KEYS: &[&str] = &["features.remember_mode", "privacy.privacy_banner_acked"];

/// Nested EchoAgentComConfig / OAuth2 / OIDC leaves enterprise writes today.
/// Keep in sync with `src/auth/config.rs`.
const ECHO_AGENT_COM_CONFIG_LEAVES: &[&str] = &[
    "echo_agent_com_config.echo_agent_ws_origin",
    "echo_agent_com_config.echo_agent_ws_url",
    "echo_agent_com_config.token_header",
    "echo_agent_com_config.auth_provider_label",
    "echo_agent_com_config.auth_token_ttl",
    "echo_agent_com_config.auth_provider_command",
    "echo_agent_com_config.preferred_method",
    "echo_agent_com_config.disable_api_key_auth",
    "echo_agent_com_config.force_login_team_uuid",
    "echo_agent_com_config.oauth2.issuer",
    "echo_agent_com_config.oauth2.client_id",
    "echo_agent_com_config.oauth2.scopes",
    "echo_agent_com_config.oauth2.principal_type",
    "echo_agent_com_config.oauth2.principal_id",
    "echo_agent_com_config.oauth2.referrer",
    "echo_agent_com_config.oidc.issuer",
    "echo_agent_com_config.oidc.client_id",
    "echo_agent_com_config.oidc.scopes",
    "echo_agent_com_config.oidc.audience",
];

#[derive(Clone, Debug)]
struct Row {
    key: String,
    #[allow(dead_code)]
    type_name: String,
    requirements: String,
    managed: Option<String>,
    details: String,
}

fn committed_markdown_path() -> PathBuf {
    if let Some(path) = std::env::var_os("ECHO_AGENT_CONFIG_REFERENCE_MD") {
        return PathBuf::from(path);
    }
    let root = find_monorepo_root().unwrap_or_else(|| {
        panic!(
            "committed config-reference user-guide not found; set ECHO_AGENT_CONFIG_REFERENCE_MD or run from the monorepo (CARGO_MANIFEST_DIR={})",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    root.join(format!(
        "terminal/echo-agent-pager/docs/user-guide/{USER_GUIDE_FILENAME}"
    ))
}

fn find_monorepo_root() -> Option<PathBuf> {
    let mut starts = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Some(srcdir) = std::env::var_os("TEST_SRCDIR") {
        starts.push(PathBuf::from(srcdir).join("_main"));
    }
    for mut dir in starts {
        for _ in 0..12 {
            if dir
                .join(format!(
                    "terminal/echo-agent-pager/docs/user-guide/{USER_GUIDE_FILENAME}"
                ))
                .exists()
            {
                return Some(dir);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

fn load_markdown() -> String {
    let path = committed_markdown_path();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn agents_md_path() -> PathBuf {
    if let Some(path) = std::env::var_os("ECHO_AGENT_CONFIG_DOCS_AGENTS_MD") {
        return PathBuf::from(path);
    }
    let crate_agents = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("AGENTS.md");
    if crate_agents.exists() {
        return crate_agents;
    }
    let root = find_monorepo_root().unwrap_or_else(|| {
        panic!(
            "echo-agent-runtime AGENTS.md not found; set ECHO_AGENT_CONFIG_DOCS_AGENTS_MD or run from the monorepo (CARGO_MANIFEST_DIR={})",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    root.join("crates/echo-agent-runtime/AGENTS.md")
}

fn load_agents_markdown() -> String {
    let path = agents_md_path();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn strip_cell(s: &str) -> String {
    s.trim().trim_matches('`').trim().to_string()
}

/// Parse `| `key` | type | req | managed | details |` (config.toml) and the
/// requirements-only `| key | type | default | details |` table.
fn parse_tables(markdown: &str) -> (Vec<Row>, Vec<Row>) {
    let mut config = Vec::new();
    let mut requirements_only = Vec::new();
    let mut section = "none";
    for line in markdown.lines() {
        if line.starts_with("## ") {
            section = if line.starts_with("## config.toml") {
                "config"
            } else if line.starts_with("## managed_config.toml") {
                "managed"
            } else if line.starts_with("## requirements.toml") {
                "requirements"
            } else {
                "none"
            };
            continue;
        }
        if !line.starts_with("| `") {
            continue;
        }
        let cells: Vec<&str> = line
            .trim()
            .trim_start_matches('|')
            .trim_end_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 4 {
            continue;
        }
        let key = strip_cell(cells[0]);
        if key == "Key" || key.is_empty() {
            continue;
        }
        match section {
            "config" if cells.len() >= 5 => config.push(Row {
                key,
                type_name: strip_cell(cells[1]),
                requirements: strip_cell(cells[2]),
                managed: Some(strip_cell(cells[3])),
                details: cells[4].trim().to_string(),
            }),
            "requirements" => requirements_only.push(Row {
                key,
                type_name: strip_cell(cells[1]),
                requirements: String::new(),
                managed: None,
                details: cells.last().copied().unwrap_or("").trim().to_string(),
            }),
            _ => {}
        }
    }
    (config, requirements_only)
}

fn by_key(rows: &[Row]) -> BTreeMap<&str, &Row> {
    let mut map = BTreeMap::new();
    for row in rows {
        assert!(
            map.insert(row.key.as_str(), row).is_none(),
            "duplicate row {}",
            row.key
        );
    }
    map
}

fn unmirrored_requirements(key: &str) -> &'static str {
    match key {
        "remote_fetch" | "zdr_access_enabled" => "pin",
        "campaigns" => "yes",
        "remember_mode" => "—",
        other => {
            panic!("UNMIRRORED_BOOLEAN_FEATURES key `{other}` has no Requirements expectation")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn page() -> (Vec<Row>, Vec<Row>, String) {
        let md = load_markdown();
        let (config, req) = parse_tables(&md);
        (config, req, md)
    }

    #[test]
    fn registered_features_have_pin_rows() {
        let (config, _, _) = page();
        let map = by_key(&config);
        for spec in FEATURES {
            let row = map.get(spec.path).unwrap_or_else(|| {
                panic!(
                    "missing FEATURES path {}; add a row to {USER_GUIDE_FILENAME}",
                    spec.path
                )
            });
            assert_eq!(
                row.requirements, "pin",
                "{} Requirements must be pin (Feature::resolve treats requirements as pins)",
                spec.path
            );
        }
    }

    #[test]
    fn unmirrored_features_have_rows() {
        let (config, _, _) = page();
        let map = by_key(&config);
        for key in UNMIRRORED_BOOLEAN_FEATURES {
            let path = format!("features.{key}");
            let row = map
                .get(path.as_str())
                .unwrap_or_else(|| panic!("missing {path}; add a row to {USER_GUIDE_FILENAME}"));
            assert_eq!(
                row.requirements,
                unmirrored_requirements(key),
                "{path} Requirements cell"
            );
        }
    }

    #[test]
    fn mcp_known_fields_have_rows() {
        let (config, _, _) = page();
        let map = by_key(&config);
        for leaf in KNOWN_MCP_SERVER_FIELDS {
            if *leaf == "urlTemplate" || *leaf == "url_template" {
                continue;
            }
            let path = format!("mcp_servers.<name>.{leaf}");
            assert!(
                map.contains_key(path.as_str()),
                "missing {path}; add a row to {USER_GUIDE_FILENAME}"
            );
        }
    }

    #[test]
    fn rows_name_real_keys() {
        let (config, req_only, _) = page();
        let mcp_leaves: BTreeSet<&str> = KNOWN_MCP_SERVER_FIELDS.iter().copied().collect();
        for row in config.iter().chain(req_only.iter()) {
            assert!(
                row.key.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '.' | '_' | '<' | '>' | '-' | '[' | ']')
                }),
                "row key {} is not a TOML path",
                row.key
            );
            if let Some(leaf) = row.key.strip_prefix("mcp_servers.<name>.") {
                assert!(
                    mcp_leaves.contains(leaf),
                    "MCP row {} is not in KNOWN_MCP_SERVER_FIELDS",
                    row.key
                );
            }
        }
    }

    #[test]
    fn managed_column_matches_resolver_metadata() {
        let (config, _, md) = page();
        let fleet: BTreeSet<&str> = MANAGED_WINS_OVER_USER.iter().copied().collect();
        let user_only: BTreeSet<&str> = USER_ONLY_KEYS.iter().copied().collect();
        let mut seen_fleet = BTreeSet::new();
        for row in &config {
            let managed = row
                .managed
                .as_deref()
                .unwrap_or_else(|| panic!("{} missing Managed cell", row.key));
            if fleet.contains(row.key.as_str()) {
                assert_eq!(managed, "fleet", "{} must be Managed fleet", row.key);
                seen_fleet.insert(row.key.as_str());
            } else if user_only.contains(row.key.as_str()) {
                assert_eq!(managed, "—", "{} must be Managed —", row.key);
            } else {
                assert_eq!(
                    managed, "user",
                    "{} default merge lets the user file win",
                    row.key
                );
            }
        }
        assert_eq!(
            seen_fleet,
            fleet.iter().copied().collect(),
            "every MANAGED_WINS_OVER_USER key must have a fleet row"
        );
        assert!(
            !md.contains("User `config.toml` wins except"),
            "managed exceptions belong in the Managed column, not a sentence"
        );
    }

    #[test]
    fn echo_agent_com_config_nested_fields_and_auth_aliases() {
        let (config, _, _) = page();
        let map = by_key(&config);
        for leaf in ECHO_AGENT_COM_CONFIG_LEAVES {
            let row = map.get(*leaf).unwrap_or_else(|| panic!("missing {leaf}"));
            let alias = leaf.replacen("echo_agent_com_config.", "auth.", 1);
            let alias_row = map
                .get(alias.as_str())
                .unwrap_or_else(|| panic!("missing alias {alias}"));
            assert_eq!(row.requirements, alias_row.requirements);
            assert!(
                !alias_row.details.starts_with("Same as"),
                "{alias} must state what the key does, then note the alias"
            );
            assert!(
                alias_row.details.contains(leaf),
                "{alias} should name `{leaf}`"
            );
        }
        assert_eq!(
            map["echo_agent_com_config.disable_api_key_auth"].requirements,
            "pin"
        );
        assert_eq!(
            map["echo_agent_com_config.force_login_team_uuid"].requirements,
            "pin"
        );
    }

    #[test]
    fn requirements_only_keys_are_table_rows() {
        let (config, req, _) = page();
        let config_keys: BTreeSet<_> = config.iter().map(|r| r.key.as_str()).collect();
        for key in [
            "fail_closed",
            "features.image_edit",
            "ui.disable_bypass_permissions_mode",
        ] {
            assert!(
                req.iter().any(|r| r.key == key),
                "missing requirements-only row {key}"
            );
            assert!(
                !config_keys.contains(key),
                "{key} must stay out of the config.toml table"
            );
        }
    }

    #[test]
    fn overlay_free_gates_do_not_claim_echo_agent_config() {
        let (config, req, _) = page();
        for row in config.iter().chain(req.iter()) {
            if matches!(
                row.key.as_str(),
                "features.remote_fetch"
                    | "features.managed_config"
                    | "features.zdr_access_enabled"
                    | "features.image_edit"
            ) {
                assert!(
                    !row.details.contains("ECHO_AGENT_CONFIG"),
                    "{} must not claim ECHO_AGENT_CONFIG",
                    row.key
                );
            }
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    fn details_contain_hashed_needle(details: &str, len: usize, digest: &str) -> bool {
        let bytes = details.as_bytes();
        if bytes.len() < len {
            return false;
        }
        bytes
            .windows(len)
            .any(|window| sha256_hex(window) == digest)
    }

    #[test]
    fn public_details_do_not_name_internal_systems() {
        let (config, req, _) = page();
        // Opaque SHA-256 of substrings that must not appear in user-guide details.
        const BANNED: &[(usize, &str)] = &[
            (
                10,
                "62e777d23c464ec3ed55fac94b0018f7e849ce80b438f0b1d1f0e7d410c135e7",
            ),
            (
                14,
                "0985e9349c2f0a19080b40b8a2d0b6197448c9c32c8b974da6dedc20682bd38c",
            ),
            (
                26,
                "f27bbe7acba2f769c3371c4ca86673e266ecece5a253497d851ebf5f097cbe9b",
            ),
            (
                14,
                "9e66b4a1830c1888314312aaa49fa30661b3f92eddbc7fab8ea326ad8a03480e",
            ),
            (
                13,
                "f4c7d31de84561da9632aba34f59c1430556f2623efdf66a8d44631d488b2661",
            ),
            (
                12,
                "1f17190c08e0df45e28085dda4391783b8ba44c2ecaf30f8e6c1db912d7ff607",
            ),
            (
                13,
                "b3e9e7c4c8a35c44656181cd9def9a2c9d7b35355d6bcb3130b956511adb3bb9",
            ),
        ];
        for row in config.iter().chain(req.iter()) {
            for &(len, digest) in BANNED {
                assert!(
                    !details_contain_hashed_needle(&row.details, len, digest),
                    "{} details name a banned internal system",
                    row.key
                );
            }
            for needle in ["Some(false)", "Some(true)"] {
                assert!(
                    !row.details.contains(needle),
                    "{} details leak `{needle}`",
                    row.key
                );
            }
        }
    }

    #[test]
    fn page_is_the_user_facing_field_list() {
        let (_, _, md) = page();
        assert!(md.starts_with("# Configuration reference\n"));
        assert!(md.contains("| Key | Type / Values | Requirements | Managed | Details |"));
        assert!(md.contains("| `models.allowed_models` | `string[]` | `pin` |"));
        assert!(md.contains("### `cli`\n"));
        assert!(!md.contains("Generated from `echo-agent-runtime`"));
        for leak in [
            "FEATURES",
            "UNMIRRORED_BOOLEAN_FEATURES",
            "KNOWN_MCP_SERVER_FIELDS",
        ] {
            assert!(
                !md.contains(leak),
                "user-guide must not name contributor registry {leak}"
            );
        }
        let agents = load_agents_markdown();
        assert!(agents.contains("Edit it; do not regenerate it."));
        assert!(agents.contains("FEATURES"));
        assert!(agents.contains("UNMIRRORED_BOOLEAN_FEATURES"));
        assert!(agents.contains("KNOWN_MCP_SERVER_FIELDS"));
    }
}
