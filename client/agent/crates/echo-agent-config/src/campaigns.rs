//! `[[campaigns]]` overlays. Priority (first id wins): requirements > remote >
//! user > managed > system_managed. Applied after layer merge.

use serde::{Deserialize, Serialize};

use crate::config_override::{
    CAMPAIGN_STRIP_KEYS, ConfigOverrideEntry, PatchPath, apply_patches, patch_touches_any,
    take_patch_array,
};

pub const CAMPAIGNS_KEY: &str = "campaigns";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CampaignMeta {
    #[serde(default, alias = "campaign_id")]
    pub id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CampaignEntry {
    pub id: String,
    pub patch: toml::Table,
}

/// Disk campaigns grouped by source layer. Merged with the remote layer (by
/// priority, first id wins) in [`crate::config_layers::ConfigLayers::resolve_campaigns`].
#[derive(Debug, Clone, Default)]
pub struct CampaignOverrides {
    pub requirements: Vec<CampaignEntry>,
    pub user: Vec<CampaignEntry>,
    pub managed: Vec<CampaignEntry>,
    pub system_managed: Vec<CampaignEntry>,
}

pub fn take_campaigns(config: &mut toml::Value) -> Vec<ConfigOverrideEntry<CampaignMeta>> {
    match take_patch_array::<CampaignMeta>(config, CAMPAIGNS_KEY) {
        Ok(entries) => entries,
        Err(_) => {
            // Category-only, never the raw error: a `toml::de::Error` Display
            // echoes the offending value, so `campaigns = "sk-secret"` would
            // leak into logs. Mirrors `VersionOverrideError::redacted`.
            tracing::warn!("campaigns: failed to deserialize (details omitted); ignoring entries");
            Vec::new()
        }
    }
}

pub fn build_campaign_entries(
    taken: Vec<ConfigOverrideEntry<CampaignMeta>>,
    layer: &'static str,
) -> Vec<CampaignEntry> {
    let mut out = Vec::with_capacity(taken.len());
    for entry in taken {
        let Some(id) = entry
            .meta
            .id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
        else {
            tracing::warn!(layer, "campaigns: entry missing id; skipped");
            continue;
        };
        // Skip no-op entries (id only, no fields to overlay).
        if entry.patch.is_empty() {
            continue;
        }
        out.push(CampaignEntry {
            id,
            patch: entry.patch,
        });
    }
    out
}

/// `sources` in priority order; first `id` wins.
pub fn merge_campaign_entries(sources: &[&[CampaignEntry]]) -> Vec<CampaignEntry> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut out = Vec::new();
    for source in sources {
        for entry in *source {
            if !seen.insert(entry.id.clone()) {
                continue;
            }
            out.push(entry.clone());
        }
    }
    out
}

/// Drop dismissed ids from a priority-merged list, preserving order.
pub fn filter_active_campaigns(
    merged: Vec<CampaignEntry>,
    dismissed_ids: &std::collections::HashSet<String>,
) -> Vec<CampaignEntry> {
    merged
        .into_iter()
        .filter(|e| !dismissed_ids.contains(&e.id))
        .collect()
}

/// Ids of `active` campaigns whose patch touches any of `paths` — used to dismiss
/// campaigns when the user persists a value at one of those paths.
pub fn ids_touching_paths(active: &[CampaignEntry], paths: &[PatchPath]) -> Vec<String> {
    active
        .iter()
        .filter(|e| patch_touches_any(&e.patch, paths))
        .map(|e| e.id.clone())
        .collect()
}

/// `active` is highest-priority-first; patches apply lowest-first (`.rev()`) so the
/// highest-priority source wins a leaf conflict.
pub fn apply_active_campaign_patches(effective: &mut toml::Value, active: &[CampaignEntry]) {
    apply_patches(
        effective,
        active.iter().rev().map(|e| e.patch.clone()),
        CAMPAIGN_STRIP_KEYS,
    );
}

pub fn take_campaign_entries(config: &mut toml::Value, layer: &'static str) -> Vec<CampaignEntry> {
    build_campaign_entries(take_campaigns(config), layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    fn models_default_patch(default: &str) -> toml::Table {
        let mut models = toml::map::Map::new();
        models.insert("default".into(), toml::Value::String(default.into()));
        let mut t = toml::map::Map::new();
        t.insert("models".into(), toml::Value::Table(models));
        t
    }

    #[test]
    fn campaign_overlays_any_field_over_user_config() {
        let mut layer = parse(
            r#"
            [[campaigns]]
            id = "c1"
            [campaigns.models]
            default = "new-model"
            [campaigns.features]
            web_fetch = true
            "#,
        );
        let entries = take_campaign_entries(&mut layer, "managed");
        assert!(layer.get(CAMPAIGNS_KEY).is_none());
        assert_eq!(entries.len(), 1);

        let mut effective =
            parse("[models]\ndefault = \"old-model\"\n[features]\nweb_fetch = false\n");
        apply_active_campaign_patches(&mut effective, &entries);
        assert_eq!(effective["models"]["default"].as_str(), Some("new-model"));
        assert_eq!(effective["features"]["web_fetch"].as_bool(), Some(true));
    }

    #[test]
    fn campaign_parse_error_is_redacted() {
        use std::sync::{Arc, Mutex};

        struct CaptureSubscriber {
            messages: Arc<Mutex<Vec<String>>>,
        }
        struct MessageVisitor<'a>(&'a mut Vec<String>);
        impl tracing::field::Visit for MessageVisitor<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0.push(format!("{value:?}"));
                }
            }
        }
        impl tracing::Subscriber for CaptureSubscriber {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                if let Ok(mut msgs) = self.messages.lock() {
                    event.record(&mut MessageVisitor(&mut msgs));
                }
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        // A scalar where an array is expected: the raw toml error echoes the
        // value, so `take_campaigns` must never log it verbatim.
        let secret = "sk-secret-token";
        let mut cfg = parse(&format!("campaigns = \"{secret}\"\n"));

        // Guard: logging the raw error verbatim would leak the secret.
        assert!(
            take_patch_array::<CampaignMeta>(&mut cfg.clone(), CAMPAIGNS_KEY)
                .unwrap_err()
                .to_string()
                .contains(secret),
            "guard: the raw toml error is expected to carry the value"
        );

        // Drive the real `take_campaigns` failure path and capture what it logs.
        let messages = Arc::new(Mutex::new(Vec::new()));
        let entries = tracing::subscriber::with_default(
            CaptureSubscriber {
                messages: messages.clone(),
            },
            || take_campaign_entries(&mut cfg, "env_overlay"),
        );
        assert!(entries.is_empty());

        let logged = messages.lock().unwrap().join("\n");
        assert!(
            logged.contains("campaigns"),
            "the parse-failure branch must emit its category-only warning: {logged}"
        );
        assert!(
            !logged.contains(secret),
            "take_campaigns leaked the value into its warning: {logged}"
        );
    }

    #[test]
    fn merge_first_source_wins_duplicate_id() {
        let req = [CampaignEntry {
            id: "same".into(),
            patch: models_default_patch("from-req"),
        }];
        let remote = [CampaignEntry {
            id: "same".into(),
            patch: models_default_patch("from-remote"),
        }];
        let merged = merge_campaign_entries(&[&req, &remote]);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].patch["models"]["default"].as_str(),
            Some("from-req")
        );
    }

    #[test]
    fn apply_highest_priority_wins_on_leaf_conflict() {
        // Two *distinct* ids both set models.default; the higher-priority source
        // (earlier in the merged list) must win the leaf.
        let req = [CampaignEntry {
            id: "req".into(),
            patch: models_default_patch("from-req"),
        }];
        let managed = [CampaignEntry {
            id: "managed".into(),
            patch: models_default_patch("from-managed"),
        }];
        let merged = merge_campaign_entries(&[&req, &managed]);
        assert_eq!(merged.len(), 2);

        let mut effective = parse("[models]\ndefault = \"user-old\"\n");
        apply_active_campaign_patches(&mut effective, &merged);
        assert_eq!(effective["models"]["default"].as_str(), Some("from-req"));
    }

    #[test]
    fn build_campaign_entries_skips_missing_id() {
        // A `None` id and a whitespace-only id are both dropped (with a warn);
        // only the entry carrying a real id survives.
        let taken = vec![
            ConfigOverrideEntry {
                meta: CampaignMeta { id: None },
                patch: models_default_patch("dropped-none"),
            },
            ConfigOverrideEntry {
                meta: CampaignMeta {
                    id: Some("   ".into()),
                },
                patch: models_default_patch("dropped-blank"),
            },
            ConfigOverrideEntry {
                meta: CampaignMeta {
                    id: Some("valid".into()),
                },
                patch: models_default_patch("kept"),
            },
        ];
        let out = build_campaign_entries(taken, "managed");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "valid");
        assert_eq!(out[0].patch["models"]["default"].as_str(), Some("kept"));
    }

    #[test]
    fn campaign_id_alias_is_accepted_in_toml_and_does_not_leak_into_patch() {
        for src in [
            "[[campaigns]]\ncampaign_id = \"c1\"\n[campaigns.models]\ndefault = \"m\"\n",
            "[[campaigns]]\nid = \"c1\"\n[campaigns.models]\ndefault = \"m\"\n",
        ] {
            let mut layer = parse(src);
            let entries = take_campaign_entries(&mut layer, "user");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, "c1");
            // The id key (either spelling) must be consumed by the meta, never
            // land in the patch — a leaked key would deep-merge a junk top-level
            // `id` into every effective config.
            assert!(
                entries[0].patch.get("id").is_none()
                    && entries[0].patch.get("campaign_id").is_none(),
                "id keys must not leak into the patch: {src}"
            );
        }
    }

    #[test]
    fn requirements_win_over_campaign() {
        use crate::config_layers::ConfigLayers;
        // A campaign (even from a lower layer) can't override a field the admin
        // set in requirements: `apply_campaign_overrides` re-merges requirements on top.
        let mut layers = ConfigLayers {
            user: parse("[models]\ndefault = \"user-old\"\n"),
            user_requirements: Some(parse("[models]\ndefault = \"pinned\"\n")),
            ..Default::default()
        };
        layers.campaigns.user = vec![CampaignEntry {
            id: "c1".into(),
            patch: models_default_patch("campaign"),
        }];
        let effective =
            layers.effective_config_with_campaigns(&[], &std::collections::HashSet::new());
        assert_eq!(
            effective["models"]["default"].as_str(),
            Some("pinned"),
            "requirements must beat a campaign for the same field"
        );
    }

    #[test]
    fn effective_config_honors_dismiss() {
        use crate::config_layers::ConfigLayers;
        // A dismissed campaign id stops overriding; the user's stored value returns.
        let mut layers = ConfigLayers {
            user: parse("[models]\ndefault = \"user-old\"\n"),
            ..Default::default()
        };
        layers.campaigns.managed = vec![CampaignEntry {
            id: "c1".into(),
            patch: models_default_patch("new"),
        }];

        let none = std::collections::HashSet::new();
        let active = layers.effective_config_with_campaigns(&[], &none);
        assert_eq!(active["models"]["default"].as_str(), Some("new"));

        let dismissed: std::collections::HashSet<_> = ["c1".into()].into_iter().collect();
        let off = layers.effective_config_with_campaigns(&[], &dismissed);
        assert_eq!(off["models"]["default"].as_str(), Some("user-old"));
    }
}
