//! Item-addressable edits for marked blocks in line-comment config files.
//!
//! Structured formats such as TOML keep their native editors.

use std::path::{Path, PathBuf};

mod format;
mod source;
mod transaction;
mod validator;

pub use format::CommentSyntax;
pub use validator::SyntaxValidator;

use source::{ParentPlan, SourceState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedItem {
    pub name: String,
    pub body: String,
}

impl ManagedItem {
    pub fn new(name: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            body: body.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedConfigRequest {
    pub path: PathBuf,
    pub namespace: String,
    /// Prefix identifying every item marker owned by this writer namespace.
    pub owned_item_prefix: String,
    pub items: Vec<ManagedItem>,
    pub comments: CommentSyntax,
    pub validator: Option<SyntaxValidator>,
}

/// Validated source from the snapshot used to build the plan.
#[derive(Clone, Debug)]
pub struct ManagedTextInspection {
    original_text: Option<String>,
    unmanaged_text: String,
    requested_items: Vec<ManagedItemState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedItemState {
    Absent,
    Exact,
    NeedsUpdate,
}

impl ManagedTextInspection {
    pub fn original_text(&self) -> Option<&str> {
        self.original_text.as_deref()
    }

    /// Source outside the writer-owned outer block.
    pub fn unmanaged_text(&self) -> &str {
        &self.unmanaged_text
    }

    pub fn requested_item_state(&self, index: usize) -> Option<ManagedItemState> {
        self.requested_items.get(index).copied()
    }
}

/// Immutable source and output state presented before application.
#[derive(Clone, Debug)]
pub struct ManagedConfigPlan {
    request: ManagedConfigRequest,
    requested_path: PathBuf,
    target_path: PathBuf,
    parent_plan: ParentPlan,
    original: SourceState,
    inspection: ManagedTextInspection,
    updated: Vec<u8>,
    backup_path_hint: Option<PathBuf>,
    temp_path_hint: Option<PathBuf>,
    lock_path: PathBuf,
}

impl ManagedConfigPlan {
    pub fn requested_path(&self) -> &Path {
        &self.requested_path
    }

    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    pub fn inspection(&self) -> &ManagedTextInspection {
        &self.inspection
    }

    pub fn updated_bytes(&self) -> &[u8] {
        &self.updated
    }

    /// Exact complete managed outer block as it will appear after apply.
    pub fn managed_block(&self) -> Option<String> {
        format::outer_block(
            std::str::from_utf8(&self.updated).ok()?,
            &self.request.namespace,
            &self.request.owned_item_prefix,
            &self.request.comments,
            &self.target_path,
        )
        .ok()
        .flatten()
    }

    /// Proposed backup path shown during confirmation. Apply first tries this
    /// exact path, then atomically retries nearby names if it was claimed in
    /// the meantime. [`ManagedConfigOutcome::backup_path`] is authoritative.
    pub fn backup_path_hint(&self) -> Option<&Path> {
        self.backup_path_hint.as_deref()
    }

    pub fn changes_file(&self) -> bool {
        self.original.bytes.as_deref() != Some(self.updated.as_slice())
            || self.original.bytes.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedConfigStatus {
    Applied,
    NoChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedConfigOutcome {
    pub status: ManagedConfigStatus,
    pub requested_path: PathBuf,
    pub target_path: PathBuf,
    /// Actual collision-free backup path retained for an applied change.
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedConfigError {
    #[error("invalid managed-config request: {0}")]
    InvalidRequest(String),
    #[error("refusing unsafe config path {path}: {reason}")]
    UnsafePath { path: PathBuf, reason: String },
    #[error("could not read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid managed markers in {path}: {reason}")]
    InvalidMarkers { path: PathBuf, reason: String },
    #[error("config changed after confirmation; run the fix again: {0}")]
    StalePlan(PathBuf),
    #[error("config parent changed after confirmation; run the fix again: {0}")]
    ParentChanged(PathBuf),
    #[error("could not lock config transaction {path}: {source}")]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write config artifact {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("syntax validation failed for {path}: {reason}")]
    Validation { path: PathBuf, reason: String },
    #[error("could not atomically publish {path}: {source}")]
    Publish {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not sync config directory {path}: {source}")]
    Sync {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("post-write verification failed for {path}: {reason}")]
    Verification { path: PathBuf, reason: String },
    #[error("transaction failed: {primary}; recovery also failed: {recovery}")]
    Recovery {
        primary: Box<ManagedConfigError>,
        recovery: Box<ManagedConfigError>,
    },
    #[error("transaction phase {phase} failed: {source}")]
    Phase {
        phase: &'static str,
        #[source]
        source: std::io::Error,
    },
}

pub struct ManagedConfig;

impl ManagedConfig {
    pub fn plan(request: ManagedConfigRequest) -> Result<ManagedConfigPlan, ManagedConfigError> {
        format::validate_request(&request)?;
        let requested_path = source::absolute_lexical(&request.path)?;
        let target_path = source::resolve_final_symlink(&requested_path)?;
        let parent = target_path
            .parent()
            .ok_or_else(|| ManagedConfigError::UnsafePath {
                path: target_path.clone(),
                reason: "target has no parent directory".to_owned(),
            })?;
        let parent_plan = ParentPlan::capture(parent)?;
        let original = source::read_source(&target_path)?;
        let text = original.text(&target_path)?;
        let requested_items = request
            .items
            .iter()
            .map(|item| {
                format::item_state(
                    text,
                    &request.namespace,
                    &request.owned_item_prefix,
                    item,
                    &request.comments,
                    &target_path,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let rendered = format::render_update(
            text,
            &request.namespace,
            &request.owned_item_prefix,
            &request.items,
            &request.comments,
            &target_path,
        )?;
        let inspection = ManagedTextInspection {
            original_text: original.bytes.as_ref().map(|_| text.to_owned()),
            unmanaged_text: rendered.unmanaged_text,
            requested_items,
        };
        let updated = rendered.updated.into_bytes();
        let changes =
            original.bytes.as_deref() != Some(updated.as_slice()) || original.bytes.is_none();
        let backup_path_hint = (changes && original.bytes.is_some())
            .then(|| transaction::artifact_hint(&target_path, "echo-agent-backup"));
        let temp_path_hint =
            changes.then(|| transaction::artifact_hint(&target_path, "echo-agent-tmp"));
        let lock_path = transaction::sibling_artifact(&target_path, "echoagent.lock");

        Ok(ManagedConfigPlan {
            request,
            requested_path,
            target_path,
            parent_plan,
            original,
            inspection,
            updated,
            backup_path_hint,
            temp_path_hint,
            lock_path,
        })
    }

    pub fn apply(plan: ManagedConfigPlan) -> Result<ManagedConfigOutcome, ManagedConfigError> {
        transaction::apply(plan, &transaction::NoopObserver)
    }

    /// Verify that the exact source path, parent identities, symlink target,
    /// bytes, mode, and file identity captured by `plan` are unchanged without
    /// publishing its proposed update.
    pub fn verify_unchanged(plan: &ManagedConfigPlan) -> Result<(), ManagedConfigError> {
        source::revalidate(plan)
    }

    #[cfg(test)]
    fn apply_with_observer(
        plan: ManagedConfigPlan,
        observer: &dyn transaction::TransactionObserver,
    ) -> Result<ManagedConfigOutcome, ManagedConfigError> {
        transaction::apply(plan, observer)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
