//! Recovery-aware point-in-time view of local session storage.
use super::{RelocationError, RelocationJournal, RelocationStorage, Result, journal};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
type SessionCandidates = HashMap<String, Vec<PathBuf>>;
pub(crate) struct RelocationView {
    echo_agent_home: PathBuf,
    sessions_root: PathBuf,
    journals: HashMap<String, RelocationJournal>,
    all_candidates: SessionCandidates,
    persisted_candidates: SessionCandidates,
}
impl RelocationView {
    pub(crate) fn load(echo_agent_home: &Path) -> Result<Self> {
        Self::load_for_sessions_root(&echo_agent_home.join("sessions"))
    }
    pub(crate) fn journal_ids(echo_agent_home: &Path) -> Result<Vec<String>> {
        Ok(load_journals(echo_agent_home)?.into_keys().collect())
    }
    pub(crate) fn load_for_sessions_root(sessions_root: &Path) -> Result<Self> {
        let echo_agent_home = sessions_root
            .parent()
            .ok_or_else(|| RelocationError::Inconsistent("sessions root has no parent".into()))?;
        let journals = load_journals(echo_agent_home)?;
        let (all_candidates, persisted_candidates) = load_candidates(sessions_root)?;
        Ok(Self {
            echo_agent_home: echo_agent_home.into(),
            sessions_root: sessions_root.into(),
            journals,
            all_candidates,
            persisted_candidates,
        })
    }
    pub(crate) fn protects_cwd_dir(&self, cwd_dir: &Path) -> bool {
        self.journals.values().any(|journal| {
            [&journal.source_cwd, &journal.target_cwd]
                .into_iter()
                .any(|cwd| {
                    self.sessions_root
                        .join(echo_agent_config::encode_cwd_dirname(cwd))
                        == cwd_dir
                })
        })
    }
    pub(crate) fn session_dirs(&self, cwd: Option<&str>) -> Result<Vec<PathBuf>> {
        let cwd_parent = cwd.map(|cwd| {
            self.sessions_root
                .join(echo_agent_config::encode_cwd_dirname(cwd))
        });
        let mut ids = self
            .persisted_candidates
            .iter()
            .filter_map(|(id, paths)| match self.journals.get(id) {
                Some(relocation) => cwd
                    .is_none_or(|cwd| authoritative_cwd(relocation) == cwd)
                    .then_some(id),
                None => cwd_parent
                    .as_deref()
                    .is_none_or(|parent| paths.iter().any(|path| path.parent() == Some(parent)))
                    .then_some(id),
            })
            .collect::<Vec<_>>();
        ids.extend(self.journals.iter().filter_map(|(id, relocation)| {
            (!self.persisted_candidates.contains_key(id)
                && cwd.is_none_or(|cwd| authoritative_cwd(relocation) == cwd))
            .then_some(id)
        }));
        ids.into_iter()
            .filter_map(|id| {
                let paths = self
                    .persisted_candidates
                    .get(id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                self.select(id, paths, cwd_parent.as_deref()).transpose()
            })
            .collect()
    }
    pub(crate) fn find_persisted_session_dir(&self, session_id: &str) -> Result<Option<PathBuf>> {
        self.find_session_dir(session_id, &self.persisted_candidates)
    }
    pub(crate) fn find_any_session_dir(&self, session_id: &str) -> Result<Option<PathBuf>> {
        self.find_session_dir(session_id, &self.all_candidates)
    }
    fn find_session_dir(
        &self,
        session_id: &str,
        candidates: &SessionCandidates,
    ) -> Result<Option<PathBuf>> {
        journal::validate_component("session id", session_id)?;
        let paths = candidates.get(session_id).map(Vec::as_slice).unwrap_or(&[]);
        if paths.is_empty() && !self.journals.contains_key(session_id) {
            return Ok(None);
        }
        self.select(session_id, paths, None)
    }
    fn select(
        &self,
        session_id: &str,
        paths: &[PathBuf],
        cwd_parent: Option<&Path>,
    ) -> Result<Option<PathBuf>> {
        let selected = if let Some(relocation) = self.journals.get(session_id) {
            let expected = journal::session_dir_at(
                &self.echo_agent_home,
                authoritative_cwd(relocation),
                session_id,
            );
            let path = self
                .all_candidates
                .get(session_id)
                .and_then(|paths| paths.iter().find(|path| **path == expected))
                .cloned()
                .ok_or_else(|| {
                    RelocationError::Inconsistent(format!(
                        "authoritative {:?} session path is missing: {}",
                        relocation.phase,
                        expected.display()
                    ))
                })?;
            RelocationStorage::new(self.echo_agent_home.clone())
                .validate_authoritative_dir(relocation, &path)?;
            Some(path)
        } else if paths.len() == 1 {
            Some(paths[0].clone())
        } else {
            None
        };
        Ok(selected.filter(|path| cwd_parent.is_none_or(|parent| path.parent() == Some(parent))))
    }
}
fn authoritative_cwd(relocation: &RelocationJournal) -> &str {
    if super::has_target_authority(relocation.phase) {
        &relocation.target_cwd
    } else {
        &relocation.source_cwd
    }
}
fn load_candidates(sessions_root: &Path) -> Result<(SessionCandidates, SessionCandidates)> {
    let entries = match fs::read_dir(sessions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((HashMap::new(), HashMap::new()));
        }
        Err(error) => return Err(super::fs::io_error("read", sessions_root, error)),
    };
    let mut all = SessionCandidates::new();
    let mut persisted = SessionCandidates::new();
    for cwd_entry in entries {
        let cwd_entry =
            cwd_entry.map_err(|error| super::fs::io_error("read", sessions_root, error))?;
        let cwd_path = cwd_entry.path();
        let cwd_type = cwd_entry
            .file_type()
            .map_err(|error| super::fs::io_error("inspect", &cwd_path, error))?;
        if !cwd_type.is_dir() || cwd_type.is_symlink() {
            continue;
        }
        for session_entry in fs::read_dir(&cwd_path)
            .map_err(|error| super::fs::io_error("read", &cwd_path, error))?
        {
            let session_entry =
                session_entry.map_err(|error| super::fs::io_error("read", &cwd_path, error))?;
            let path = session_entry.path();
            let file_type = session_entry
                .file_type()
                .map_err(|error| super::fs::io_error("inspect", &path, error))?;
            let Some(id) = session_entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() || id.starts_with('.') {
                continue;
            }
            all.entry(id.clone()).or_default().push(path.clone());
            let summary = path.join(super::super::SUMMARY_FILE);
            match fs::symlink_metadata(&summary) {
                Ok(metadata)
                    if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
                {
                    persisted.entry(id).or_default().push(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(super::fs::io_error("inspect", &summary, error)),
                Ok(_) => {}
            }
        }
    }
    Ok((all, persisted))
}
fn load_journals(echo_agent_home: &Path) -> Result<HashMap<String, RelocationJournal>> {
    let dir = journal::relocation_dir(echo_agent_home);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(super::fs::io_error("read", &dir, error)),
    };
    let mut journals = HashMap::new();
    for entry in entries {
        let entry = entry.map_err(|error| super::fs::io_error("read", &dir, error))?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| RelocationError::Inconsistent("journal name is not UTF-8".into()))?;
        journals.insert(
            session_id.to_owned(),
            journal::read(echo_agent_home, session_id)?,
        );
    }
    Ok(journals)
}
