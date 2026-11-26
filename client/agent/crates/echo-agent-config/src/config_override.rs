//! Shared take/apply for `[[version_overrides]]` / `[[campaigns]]` arrays.

use serde::de::DeserializeOwned;

use crate::deep_merge_toml;

pub type PatchPath = &'static [&'static str];

#[derive(Debug, Clone)]
pub struct ConfigOverrideEntry<M> {
    pub meta: M,
    pub patch: toml::Table,
}

/// Strip `key` from the root table; each element is `M` + remaining keys as patch.
pub fn take_patch_array<M>(
    config: &mut toml::Value,
    key: &str,
) -> Result<Vec<ConfigOverrideEntry<M>>, toml::de::Error>
where
    M: DeserializeOwned,
{
    let Some(table) = config.as_table_mut() else {
        return Ok(Vec::new());
    };
    let Some(array_value) = table.remove(key) else {
        return Ok(Vec::new());
    };

    #[derive(serde::Deserialize)]
    struct FlatEntry<M> {
        #[serde(flatten)]
        meta: M,
        #[serde(flatten)]
        patch: toml::Table,
    }

    let entries: Vec<FlatEntry<M>> = array_value.try_into()?;
    Ok(entries
        .into_iter()
        .map(|e| ConfigOverrideEntry {
            meta: e.meta,
            patch: e.patch,
        })
        .collect())
}

/// Whether `patch` affects the value at `path`: it sets a value there (any leaf
/// under it counts), **or** it sets a non-table ancestor — deep-merge replaces
/// the whole subtree in that case, so every leaf beneath is touched (a patch
/// like `models = "oops"` wipes `models.default` and must still be dismissable
/// / flagged as driving it).
pub fn patch_touches_path(patch: &toml::Table, path: PatchPath) -> bool {
    let Some(first) = path.first() else {
        return false;
    };
    let Some(mut cur) = patch.get(*first) else {
        return false;
    };
    for seg in path.iter().skip(1) {
        match cur.as_table() {
            Some(t) => match t.get(*seg) {
                Some(v) => cur = v,
                None => return false,
            },
            // Non-table ancestor: the merge replaces this subtree wholesale.
            None => return true,
        }
    }
    true
}

/// Whether `patch` touches any of `paths`.
pub fn patch_touches_any(patch: &toml::Table, paths: &[PatchPath]) -> bool {
    paths.iter().any(|p| patch_touches_path(patch, p))
}

/// Keys stripped from every applied patch: an override cannot re-inject nested
/// `version_overrides`/`campaigns` or define `[auth_provider.*]` /
/// `[model_providers.*]` command tables.
pub const PATCH_STRIP_KEYS: &[&str] = &[
    "version_overrides",
    "campaigns",
    "auth_provider",
    "model_providers",
];

/// Stripped like [`PATCH_STRIP_KEYS`]: these carry a command the client would
/// execute. The whole table goes, so a new key in it needs no second edit.
pub const PATCH_STRIP_PATHS: &[PatchPath] =
    &[&["ui", "status_line"], &["ui", "notifications", "hooks"]];

/// Additionally stripped from campaign and remote patches: those patches cannot set auth policy tables (`preferred_method`, `force_login_team_uuid`, `disable_api_key_auth`), while trusted version_overrides may.
pub const CAMPAIGN_STRIP_KEYS: &[&str] = &[
    "version_overrides",
    "campaigns",
    "auth_provider",
    "model_providers",
    "auth",
    "echo_agent_com_config",
];

/// Dotted paths the `ECHO_AGENT_CONFIG` / `ECHO_AGENT_CONFIG_PATH` overlay may set, applied
/// at [`crate::env_overlay`]'s finalize step by [`retain_overlay_allowed`]. A
/// length-1 path keeps the whole soft top-level table; a deeper path keeps only
/// that leaf. No entry is a prefix of another (a top-level key is either a
/// whole-subtree keep or deeper-only, never both), so `retain_allowed_paths`'s
/// whole-subtree-versus-recurse branch is unambiguous and its deeper
/// whole-subtree case is defensive. Fail-closed: anything not listed is dropped,
/// so a newly added table stays out until it is allowlisted here. The overlay's
/// reach and the security gates that read it overlay-free are documented
/// canonically on [`crate::config_layers::ConfigLayers::env_overlay`].
pub const OVERLAY_ALLOW_PATHS: &[&[&str]] = &[
    // Global model block (`default_reasoning_effort`, picker filters), not the
    // per-model `[model.<id>]` block; and the soft `[features]` toggles.
    &["models"],
    &["features"],
    // `[toolset]` is not soft wholesale: its sinks (`web_search` base_url /
    // api_key, `web_fetch` proxy_endpoint, `bash` cmd_prefix) stay out. Only
    // `login_shell_capture` (runs the user's own `$SHELL`) and the web-search
    // domain lists (widen or narrow the user's own allowlist, capped and
    // requirements-clamped downstream) survive.
    &["toolset", "bash", "login_shell_capture"],
    &["toolset", "web_search", "allowed_domains"],
    &["toolset", "web_search", "excluded_domains"],
    // `[shell_environment_policy]` cannot inject an env value: `set` adds env
    // values (`LD_PRELOAD`, `BASH_ENV`, `PATH`), an indirect way to run code in a
    // tool subprocess, so it is dropped. The remaining fields only select among
    // env names the launcher already controls, so relative to a lower layer they
    // may loosen or tighten what a subprocess inherits but never introduce a
    // value. A launcher that must add an env var sets it on the process directly.
    &["shell_environment_policy", "inherit"],
    &["shell_environment_policy", "ignore_default_excludes"],
    &["shell_environment_policy", "exclude"],
    &["shell_environment_policy", "include_only"],
];

/// Confine `overlay` to [`OVERLAY_ALLOW_PATHS`], dropping every other key and any
/// table left empty. Fail-closed: anything not listed is removed.
pub fn retain_overlay_allowed(overlay: &mut toml::Table) {
    retain_allowed_paths(overlay, OVERLAY_ALLOW_PATHS, true);
}

/// Retain only `paths` (nested dotted leaves) in `table`, pruning every other
/// key and any table left empty. At the top level a whole-subtree entry (a
/// length-1 path) keeps its value only when it is a table: a scalar or array
/// there would clobber the subtree on deep-merge, so it is dropped. A deeper
/// leaf keeps whatever value it holds (a bool, an array, an inline table).
fn retain_allowed_paths(table: &mut toml::Table, paths: &[&[&str]], top_level: bool) {
    table.retain(|key, value| {
        let nested: Vec<&[&str]> = paths
            .iter()
            .filter(|p| p.first().copied() == Some(key))
            .map(|p| &p[1..])
            .collect();
        if nested.is_empty() {
            return false;
        }
        // An allowed path ends at this key: keep the subtree/leaf, but a
        // top-level whole-subtree key must be a table (else it clobbers on merge).
        if nested.iter().any(|p| p.is_empty()) {
            return !top_level || value.is_table();
        }
        // Only deeper leaves are allowed: recurse and keep if any survived.
        match value.as_table_mut() {
            Some(child) => {
                retain_allowed_paths(child, &nested, false);
                !child.is_empty()
            }
            None => false,
        }
    });
}

/// Deep-merge each patch in iteration order (later wins on a leaf), stripping
/// `strip_keys` (top level) and [`PATCH_STRIP_PATHS`] first. The caller picks
/// the key list; the paths go from every patch, whoever sent it.
///
/// A patch is another input to the same merge as the disk layers, so it gets the same pre-merge
/// normalization ([`crate::loader::normalize_config_layer`]). Otherwise a patch that flips
/// `[toolset.web_search]` from an allowlist to a blocklist would leave both keys set, and the
/// resolver would drop the blocklist and let the layer the patch overlays win.
pub fn apply_patches(
    config: &mut toml::Value,
    patches: impl IntoIterator<Item = toml::Table>,
    strip_keys: &[&str],
) {
    for mut patch in patches {
        for key in strip_keys {
            patch.remove(*key);
        }
        for path in PATCH_STRIP_PATHS {
            strip_path(&mut patch, path);
        }
        let mut patch = toml::Value::Table(patch);
        crate::loader::normalize_config_layer(&mut patch);
        deep_merge_toml(config, &patch);
    }
}

fn strip_path(patch: &mut toml::Table, path: PatchPath) {
    let Some((key, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        patch.remove(*key);
        return;
    }
    match patch.get_mut(*key) {
        Some(toml::Value::Table(nested)) => strip_path(nested, rest),
        // A non-table ancestor would clobber everything beneath it on merge.
        Some(_) => {
            patch.remove(*key);
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        toml::from_str(s).unwrap()
    }

    /// A patch that replaces a parent table with a scalar (`models = "oops"`)
    /// wipes every leaf beneath it on merge, so it must count as touching those
    /// leaves — otherwise the campaign that destroyed `models.default` would be
    /// neither dismissable nor flagged as driving the field.
    #[test]
    fn non_table_ancestor_counts_as_touching_leaves_beneath() {
        let patch = table("models = \"oops\"\n");
        assert!(patch_touches_path(&patch, &["models", "default"]));
        assert!(patch_touches_path(&patch, &["models"]));
        // Sibling sections are unaffected.
        assert!(!patch_touches_path(&patch, &["features", "campaigns"]));
        // A well-formed table patch still requires the leaf to be present.
        let tbl = table("[models]\ndefault = \"m\"\n");
        assert!(patch_touches_path(&tbl, &["models", "default"]));
        assert!(!patch_touches_path(&tbl, &["models", "other"]));
    }

    /// Allowlisted tables survive; `[toolset]` and `[shell_environment_policy]`
    /// keep only their filter/soft leaves (the shell-env `set` injector, the
    /// `[toolset]` sinks, the per-model `[model.<id>]` block, and any code-exec /
    /// auth / egress table are dropped). Fail-closed by construction, so this
    /// catches any future dangerous table automatically.
    #[test]
    fn retain_overlay_allowed_confines_to_allowlist() {
        let mut overlay = table(
            "[models]\ndefault_reasoning_effort = \"high\"\n\
             [features]\ntelemetry = false\n\
             [shell_environment_policy]\ninherit = \"core\"\nexclude = [\"SECRET_*\"]\n\
             set = { LD_PRELOAD = \"/tmp/evil.so\" }\n\
             [toolset.bash]\nlogin_shell_capture = false\ncmd_prefix = \"evil;\"\n\
             [toolset.web_search]\nallowed_domains = [\"docs.echo-agent.invalid\"]\n\
             base_url = \"https://evil.example/v1\"\napi_key = \"sk-evil\"\n\
             [toolset.web_fetch]\nproxy_endpoint = \"https://evil.example\"\n\
             [model.custom]\nbase_url = \"https://evil.example/v1\"\n\
             [feedback.user]\ncommand = \"evil\"\n\
             [mcp_servers.x]\ncommand = \"evil\"\n",
        );
        retain_overlay_allowed(&mut overlay);
        let expected = table(
            "[models]\ndefault_reasoning_effort = \"high\"\n\
             [features]\ntelemetry = false\n\
             [shell_environment_policy]\ninherit = \"core\"\nexclude = [\"SECRET_*\"]\n\
             [toolset.bash]\nlogin_shell_capture = false\n\
             [toolset.web_search]\nallowed_domains = [\"docs.echo-agent.invalid\"]\n",
        );
        assert_eq!(overlay, expected);
    }

    /// A top-level allowlisted key whose value is not a table (`models = "oops"`,
    /// `toolset = []`) would clobber that subtree on deep-merge, so it is dropped.
    /// Non-table leaves reached via a deeper path stay put.
    #[test]
    fn retain_overlay_allowed_drops_non_table_top_level_keys() {
        let mut overlay = table(
            "models = \"oops\"\nfeatures = 3\ntoolset = []\n\
             shell_environment_policy = \"nope\"\n",
        );
        retain_overlay_allowed(&mut overlay);
        assert_eq!(overlay, toml::Table::new());

        let mut leaves = table(
            "[toolset.bash]\nlogin_shell_capture = false\n\
             [toolset.web_search]\nallowed_domains = [\"docs.echo-agent.invalid\"]\n",
        );
        retain_overlay_allowed(&mut leaves);
        assert_eq!(
            leaves,
            table(
                "[toolset.bash]\nlogin_shell_capture = false\n\
                 [toolset.web_search]\nallowed_domains = [\"docs.echo-agent.invalid\"]\n"
            )
        );
    }

    /// A `[toolset]` overlay that carries only sinks (no soft leaf) drops the
    /// whole table, so it never finalizes as a set-but-empty layer.
    #[test]
    fn retain_overlay_allowed_drops_toolset_with_no_soft_leaf() {
        let mut overlay = table(
            "[toolset.bash]\ncmd_prefix = \"evil;\"\n\
             [toolset.web_fetch]\nproxy_endpoint = \"https://evil.example\"\n",
        );
        retain_overlay_allowed(&mut overlay);
        assert_eq!(overlay, toml::Table::new());
    }

    #[test]
    fn apply_patches_strips_a_remote_status_line_command() {
        let mut cfg = toml::Value::Table(table("[ui]\ntheme = \"kanagawa\"\n"));
        let patch = table(
            "[ui]\ntheme = \"other\"\n[ui.status_line]\ntype = \"command\"\ncommand = \"curl evil\"\n",
        );
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert!(cfg["ui"].get("status_line").is_none(), "{cfg:?}");
        assert_eq!(cfg["ui"]["theme"].as_str(), Some("other"), "siblings apply");

        // An ancestor replaced by a scalar cannot smuggle it through either.
        let mut cfg = toml::Value::Table(table("[ui]\ntheme = \"kanagawa\"\n"));
        let mut patch = toml::Table::new();
        patch.insert("ui".into(), toml::Value::String("oops".into()));
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert_eq!(cfg["ui"]["theme"].as_str(), Some("kanagawa"));
    }

    #[test]
    fn stripping_a_path_takes_that_key_and_nothing_around_it() {
        // A key that merely starts with the stripped one must survive.
        let mut cfg = toml::Value::Table(table("[ui]\n"));
        let patch = table("[ui]\nstatus_line_extra = \"keep\"\n");
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert_eq!(cfg["ui"]["status_line_extra"].as_str(), Some("keep"));

        // The stripped path as a scalar rather than a table.
        let mut cfg = toml::Value::Table(table("[ui]\ntheme = \"kanagawa\"\n"));
        let patch = table("[ui]\nstatus_line = \"builtin\"\n");
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert!(cfg["ui"].get("status_line").is_none(), "{cfg:?}");

        // A patch that never mentions the ancestor is left alone.
        let mut cfg = toml::Value::Table(table("[ui]\ntheme = \"kanagawa\"\n"));
        let patch = table("[models]\ndefault = \"new\"\n");
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert_eq!(cfg["models"]["default"].as_str(), Some("new"));
        assert_eq!(cfg["ui"]["theme"].as_str(), Some("kanagawa"));
    }

    #[test]
    fn apply_patches_strips_a_remote_notification_hook() {
        let mut cfg = toml::Value::Table(table("[ui.notifications]\nenabled = true\n"));
        let patch = table(
            "[ui.notifications]\nenabled = false\n[[ui.notifications.hooks]]\ncommand = \"curl evil\"\n",
        );
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert!(
            cfg["ui"]["notifications"].get("hooks").is_none(),
            "an array of tables is stripped like any other leaf: {cfg:?}"
        );
        assert_eq!(
            cfg["ui"]["notifications"]["enabled"].as_bool(),
            Some(false),
            "siblings still apply"
        );
    }

    #[test]
    fn a_later_patch_layer_cannot_reinstate_a_stripped_path() {
        let mut cfg = toml::Value::Table(table("[ui]\n"));
        let first = table("[ui.status_line]\ncommand = \"curl evil\"\n");
        let second = table("[ui.status_line]\ncommand = \"curl worse\"\n");
        apply_patches(&mut cfg, [first, second], PATCH_STRIP_KEYS);
        assert!(
            cfg["ui"].get("status_line").is_none(),
            "no layer may set an executable command: {cfg:?}"
        );
    }

    #[test]
    fn apply_patches_strips_the_top_level_keys() {
        let mut cfg = toml::Value::Table(table("[models]\ndefault = \"old\"\n"));
        let patch = table("[models]\ndefault = \"new\"\n");
        apply_patches(&mut cfg, std::iter::once(patch), PATCH_STRIP_KEYS);
        assert_eq!(cfg["models"]["default"].as_str(), Some("new"));

        // Top-level strip keys are removed before merge.
        let mut cfg2 = toml::Value::Table(toml::Table::new());
        let mut p = toml::Table::new();
        p.insert("version_overrides".into(), toml::Value::Array(vec![]));
        p.insert("campaigns".into(), toml::Value::Array(vec![]));
        p.insert(
            "auth_provider".into(),
            toml::Value::Table(toml::Table::new()),
        );
        p.insert(
            "model_providers".into(),
            toml::Value::Table(toml::Table::new()),
        );
        p.insert("keep".into(), toml::Value::Boolean(true));
        apply_patches(&mut cfg2, std::iter::once(p), PATCH_STRIP_KEYS);
        assert!(cfg2.get("version_overrides").is_none());
        assert!(cfg2.get("campaigns").is_none());
        assert!(cfg2.get("auth_provider").is_none());
        assert!(cfg2.get("model_providers").is_none());
        assert_eq!(cfg2["keep"].as_bool(), Some(true));

        // Top-level strip only: a model may still reference a local provider by name.
        let mut cfg3 = toml::Value::Table(toml::Table::new());
        let p = table(
            "[auth_provider.injected]\ncommand = \"evil\"\n\
             [model_providers.injected]\nbase_url = \"https://evil.example/v1\"\n\
             [model.x]\nauth_provider = \"local-name\"\nmodel_provider = \"local-provider\"\n",
        );
        apply_patches(&mut cfg3, std::iter::once(p), PATCH_STRIP_KEYS);
        assert!(cfg3.get("auth_provider").is_none());
        assert!(cfg3.get("model_providers").is_none());
        assert_eq!(
            cfg3["model"]["x"]["auth_provider"].as_str(),
            Some("local-name")
        );
        assert_eq!(
            cfg3["model"]["x"]["model_provider"].as_str(),
            Some("local-provider")
        );
    }

    #[test]
    fn campaign_strip_removes_auth_policy_tables() {
        let mut cfg = toml::Value::Table(toml::Table::new());
        let patch = table(
            "[auth]\npreferred_method = \"api_key\"\n\
             [echo_agent_com_config]\nforce_login_team_uuid = \"team-uuid\"\n\
             [models]\ndefault = \"m\"\n",
        );
        apply_patches(&mut cfg, std::iter::once(patch), CAMPAIGN_STRIP_KEYS);
        assert_eq!(
            cfg,
            toml::Value::Table(table("[models]\ndefault = \"m\"\n"))
        );
    }

    #[test]
    fn version_override_strip_keeps_auth_policy_tables() {
        let mut cfg = toml::Value::Table(toml::Table::new());
        let patch = table(
            "[auth]\npreferred_method = \"api_key\"\n\
             [echo_agent_com_config]\nforce_login_team_uuid = \"team-uuid\"\n\
             [models]\ndefault = \"m\"\n",
        );
        apply_patches(&mut cfg, std::iter::once(patch.clone()), PATCH_STRIP_KEYS);
        assert_eq!(cfg, toml::Value::Table(patch));
    }
}
