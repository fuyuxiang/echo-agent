//! Recent project directories drawn from session history, plus path display shared by the dashboard.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use echo_agent_file_utils::workspace_classifier::is_project_dir;
use echo_agent_runtime::session::persistence::list_recent_summaries;

pub async fn collect_recent_dirs(limit: usize) -> Vec<(PathBuf, DateTime<Utc>)> {
    let summaries = match list_recent_summaries(500).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "recent dirs: failed to list recent sessions");
            return vec![];
        }
    };
    let mut latest: std::collections::HashMap<String, DateTime<Utc>> = Default::default();
    for s in &summaries {
        if s.is_hidden() {
            continue;
        }
        let entry = latest.entry(s.info.cwd.clone()).or_insert(s.updated_at);
        if s.updated_at > *entry {
            *entry = s.updated_at;
        }
    }
    let mut projects: Vec<(PathBuf, DateTime<Utc>)> = latest
        .into_iter()
        .filter_map(|(cwd, ts)| {
            let p = PathBuf::from(&cwd);
            if p.is_dir() && is_project_dir(&p) {
                Some((p, ts))
            } else {
                None
            }
        })
        .collect();
    projects.sort_by(|a, b| b.1.cmp(&a.1));
    projects.truncate(limit);
    projects
}

pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}
