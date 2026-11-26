use super::render::render_runtime_contract_from_manifest;
use super::*;

pub(super) fn validate_manifest(
    root: &Path,
    manifest: &SkillCapabilityManifest,
) -> Result<(), String> {
    if manifest.schema_version != 1 {
        return Err(format!(
            "unsupported {CAPABILITY_MANIFEST_FILE} schemaVersion {}; expected 1",
            manifest.schema_version
        ));
    }
    validate_unique_ids("capability", &manifest.capabilities, false)?;
    if manifest.capabilities.is_empty() {
        return Err("capability manifest must declare at least one capability".into());
    }
    if manifest.runtime.is_none() && manifest.requirements.connectors.is_empty() {
        return Err(
            "capability manifest must declare a runtime or a connector implementation".into(),
        );
    }
    if let Some(runtime) = &manifest.runtime {
        if runtime.entrypoints.is_empty() || runtime.entrypoints.len() > MAX_ACTIONS {
            return Err(format!(
                "runtime entrypoints must contain 1–{MAX_ACTIONS} actions"
            ));
        }
        if !(1..=MAX_TIMEOUT_SECONDS).contains(&runtime.timeout_seconds) {
            return Err(format!(
                "runtime timeoutSeconds must be between 1 and {MAX_TIMEOUT_SECONDS}"
            ));
        }
        if let Some(command) = &runtime.command {
            validate_command(command)?;
        }
        let mut entries = HashSet::new();
        for (action, relative) in &runtime.entrypoints {
            validate_id("action", action)?;
            if !entries.insert(action.to_ascii_lowercase()) {
                return Err(format!("duplicate action `{action}`"));
            }
            let path = validate_relative_resource_path(relative, "entrypoint")?;
            let absolute = root.join(&path);
            let metadata = std::fs::symlink_metadata(&absolute)
                .map_err(|_| format!("entrypoint `{relative}` does not exist"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!("entrypoint `{relative}` must be a regular file"));
            }
            let canonical_root = root
                .canonicalize()
                .map_err(|error| format!("canonicalize skill root: {error}"))?;
            let canonical_entrypoint = absolute
                .canonicalize()
                .map_err(|error| format!("canonicalize entrypoint `{relative}`: {error}"))?;
            if !canonical_entrypoint.starts_with(&canonical_root) {
                return Err(format!("entrypoint `{relative}` escapes the Skill package"));
            }
        }
    }

    validate_unique_ids("command", &manifest.requirements.commands, true)?;
    for command in &manifest.requirements.commands {
        validate_command(command)?;
    }
    if manifest.requirements.connectors.len() > MAX_LIST_ITEMS {
        return Err(format!(
            "connector requirements exceed {MAX_LIST_ITEMS} items"
        ));
    }
    let mut connectors = HashSet::new();
    for connector in &manifest.requirements.connectors {
        validate_id("connector", &connector.id)?;
        if !connectors.insert(connector.id.to_ascii_lowercase()) {
            return Err(format!("duplicate connector `{}`", connector.id));
        }
        validate_optional_text("connector label", connector.label.as_deref())?;
        validate_optional_text("connector purpose", connector.purpose.as_deref())?;
    }
    validate_unique_ids(
        "OS permission",
        &manifest.requirements.os_permissions,
        false,
    )?;
    if manifest.permissions.network.len() > MAX_LIST_ITEMS {
        return Err(format!(
            "network allowlist exceeds {MAX_LIST_ITEMS} origins"
        ));
    }
    for origin in &manifest.permissions.network {
        validate_network_origin(origin)?;
    }
    validate_unique_ids(
        "external action",
        &manifest.permissions.external_actions,
        false,
    )?;
    if manifest.artifacts.len() > MAX_LIST_ITEMS {
        return Err(format!(
            "artifact declarations exceed {MAX_LIST_ITEMS} items"
        ));
    }
    let mut artifacts = HashSet::new();
    for artifact in &manifest.artifacts {
        validate_id("artifact", &artifact.id)?;
        if !artifacts.insert(artifact.id.to_ascii_lowercase()) {
            return Err(format!("duplicate artifact `{}`", artifact.id));
        }
        validate_artifact_pattern(&artifact.pattern)?;
        if let Some(mime_type) = artifact.mime_type.as_deref() {
            validate_mime_type(mime_type)?;
        }
        if artifact
            .max_bytes
            .is_some_and(|value| value == 0 || value > MAX_ARTIFACT_BYTES)
        {
            return Err(format!(
                "artifact `{}` maxBytes must be between 1 and {MAX_ARTIFACT_BYTES}",
                artifact.id
            ));
        }
    }
    // The contract is injected into the model context on every invocation.
    // Validate its worst-case (all commands missing) representation up front
    // so install, listing, and execution agree on whether a manifest is valid.
    render_runtime_contract_from_manifest(manifest, |_| false)?;
    Ok(())
}

fn validate_unique_ids(label: &str, values: &[String], command: bool) -> Result<(), String> {
    if values.len() > MAX_LIST_ITEMS {
        return Err(format!("{label} list exceeds {MAX_LIST_ITEMS} items"));
    }
    let mut seen = HashSet::new();
    for value in values {
        if command {
            validate_command(value)?;
        } else {
            validate_id(label, value)?;
        }
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(format!("duplicate {label} `{value}`"));
        }
    }
    Ok(())
}

fn validate_id(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 100
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(format!(
            "{label} `{value}` must use 1–100 lowercase letters, digits, dots, dashes, or underscores"
        ));
    }
    Ok(())
}

fn validate_command(command: &str) -> Result<(), String> {
    if command.is_empty()
        || command.len() > 128
        || command.contains(['/', '\\'])
        || command.chars().any(char::is_whitespace)
        || command.chars().any(char::is_control)
    {
        return Err(format!("invalid command name `{command}`"));
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: Option<&str>) -> Result<(), String> {
    if value.is_some_and(|value| {
        value.is_empty()
            || value.chars().count() > MAX_TEXT_CHARS
            || value.chars().any(char::is_control)
    }) {
        return Err(format!(
            "{label} must contain 1–{MAX_TEXT_CHARS} non-control characters"
        ));
    }
    Ok(())
}

fn validate_relative_resource_path(value: &str, label: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    let components = path.components().collect::<Vec<_>>();
    if value.is_empty()
        || value.len() > 512
        || value.chars().any(char::is_control)
        || components.is_empty()
        || components.len() > 12
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("{label} `{value}` must be a safe relative path"));
    }
    Ok(path.to_path_buf())
}

fn validate_artifact_pattern(pattern: &str) -> Result<(), String> {
    validate_relative_resource_path(pattern, "artifact pattern").map(|_| ())
}

fn validate_mime_type(value: &str) -> Result<(), String> {
    let Some((category, subtype)) = value.split_once('/') else {
        return Err(format!("invalid artifact MIME type `{value}`"));
    };
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 127
            && part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    if !valid_part(category) || !valid_part(subtype) || subtype.contains('/') {
        return Err(format!("invalid artifact MIME type `{value}`"));
    }
    Ok(())
}

fn validate_network_origin(value: &str) -> Result<(), String> {
    if value.len() > 2048 {
        return Err("network origin is too long".into());
    }
    let url = url::Url::parse(value).map_err(|error| format!("invalid network origin: {error}"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if url.host_str().is_none()
        || (url.scheme() != "https" && !(loopback && url.scheme() == "http"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(format!(
            "network permission `{value}` must be an HTTPS origin without credentials, path, query, or fragment"
        ));
    }
    Ok(())
}

pub(super) fn command_available(command: &str) -> bool {
    if command.contains(['/', '\\']) {
        return false;
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    #[cfg(windows)]
    let extensions = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![".exe".into(), ".cmd".into(), ".bat".into()]);
    std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(command);
        if is_executable_file(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            if Path::new(command).extension().is_none()
                && extensions
                    .iter()
                    .any(|extension| directory.join(format!("{command}{extension}")).is_file())
            {
                return true;
            }
        }
        false
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
