//! Model bindings for explicitly configured, isolated native acceptance only.
use crate::native_review_scope::ordinary_private;
use serde::Serialize;
use std::path::Path;

pub(crate) const PRIVATE_MODEL_ID: &str = "echoagent-review/claude-opus-5-5";

pub(crate) fn allowed_model(id: &str) -> bool {
    matches!(
        id,
        "echoagent-ojlab/Echo Reed" | "echoagent-ojlab/Echo Grove" | PRIVATE_MODEL_ID
    )
}

// Deliberately no Debug: this DTO may contain the explicitly supplied key.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewConnection {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

pub(crate) fn connection(home: &Path, id: &str) -> Result<ReviewConnection, String> {
    if id == "echoagent-ojlab/Echo Reed" {
        return Ok(ReviewConnection {
            base_url: crate::agent_runtime::OJLAB_BASE_URL.into(),
            api_key: None,
            model: "Echo Reed".into(),
        });
    }
    if id != PRIVATE_MODEL_ID {
        return Err("Unsupported core review model binding".into());
    }
    let path = home.join("config.toml");
    ordinary_private(&path, false)?;
    if std::fs::metadata(&path)
        .map_err(|_| "Missing private model config")?
        .len()
        > 64 * 1024
    {
        return Err("Private model config exceeds review bound".into());
    }
    let config: toml::Value = std::fs::read_to_string(&path)
        .map_err(|_| "Cannot read private model config")?
        .parse()
        .map_err(|_| "Invalid private model config")?;
    let model = config
        .get("model")
        .and_then(|models| models.get(id))
        .ok_or("Missing private review model")?;
    let provider = config
        .get("model_providers")
        .and_then(|providers| providers.get("echoagent-review"))
        .ok_or("Missing private review provider")?;
    if model.get("model_provider").and_then(toml::Value::as_str) != Some("echoagent-review")
        || model.get("model").and_then(toml::Value::as_str) != Some("claude-opus-5-5")
        || provider.get("api_backend").and_then(toml::Value::as_str) != Some("chat_completions")
        || provider.get("auth_scheme").and_then(toml::Value::as_str) != Some("bearer")
    {
        return Err("Private review model/provider binding differs".into());
    }
    let base_url = provider
        .get("base_url")
        .and_then(toml::Value::as_str)
        .ok_or("Missing private model URL")?;
    let url = url::Url::parse(base_url).map_err(|_| "Invalid private model URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Private model URL must use HTTPS without embedded credentials".into());
    }
    let key = provider
        .get("api_key")
        .and_then(toml::Value::as_str)
        .filter(|key| key.len() >= 16 && !key.contains(['\r', '\n']))
        .ok_or("Missing private review credential")?;
    Ok(ReviewConnection {
        base_url: base_url.into(),
        api_key: Some(key.into()),
        model: "claude-opus-5-5".into(),
    })
}

pub(crate) fn validate_config(home: &Path, id: &str) -> Result<(), String> {
    if !allowed_model(id) {
        return Err("Unsupported review model".into());
    }
    if id == PRIVATE_MODEL_ID {
        connection(home, id)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn config(home: &Path, url: &str, key: &str) {
        let body = format!(
            r#"[model_providers.echoagent-review]
base_url = {url:?}
api_key = {key:?}
api_backend = "chat_completions"
auth_scheme = "bearer"
[model."echoagent-review/claude-opus-5-5"]
model_provider = "echoagent-review"
model = "claude-opus-5-5"
"#
        );
        std::fs::write(home.join("config.toml"), body).unwrap();
        std::fs::set_permissions(
            home.join("config.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    #[test]
    fn private_binding_requires_https_ordinary_owner_only_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temp.path()).unwrap();
        let key = "synthetic-test-credential-only";
        config(&home, "https://example.invalid/v1", key);
        let binding = connection(&home, PRIVATE_MODEL_ID).unwrap();
        assert_eq!(binding.model, "claude-opus-5-5");
        assert_eq!(binding.api_key.as_deref(), Some(key));
        for url in [
            "http://example.invalid/v1",
            "https://user:pass@example.invalid/v1",
            "https://example.invalid/v1?key=secret",
        ] {
            config(&home, url, key);
            let error = connection(&home, PRIVATE_MODEL_ID).err().unwrap();
            assert!(!error.contains(key));
        }
        config(&home, "https://example.invalid/v1", key);
        std::fs::set_permissions(
            home.join("config.toml"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(connection(&home, PRIVATE_MODEL_ID).is_err());
        std::fs::rename(home.join("config.toml"), home.join("actual.toml")).unwrap();
        std::os::unix::fs::symlink(home.join("actual.toml"), home.join("config.toml")).unwrap();
        assert!(connection(&home, PRIVATE_MODEL_ID).is_err());
        assert!(validate_config(&home, "unapproved/model").is_err());
    }
}
