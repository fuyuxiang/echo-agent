use super::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Store {
    pub path: PathBuf,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReceipt {
    pub group_id: String,
    pub session_ids: Vec<String>,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("拒绝打开符号链接专家团数据库".into());
        }
        let store = Self { path: path.into() };
        let c = store.connect()?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS group_deletions(group_id TEXT NOT NULL, session_id TEXT NOT NULL, cwd TEXT NOT NULL, PRIMARY KEY(group_id,session_id));").map_err(|e| e.to_string())?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS team_templates(id TEXT PRIMARY KEY,name TEXT NOT NULL,data TEXT NOT NULL);").map_err(|e|e.to_string())?;
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS groups(id TEXT PRIMARY KEY, data TEXT NOT NULL, updated INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS members(session_id TEXT PRIMARY KEY, group_id TEXT NOT NULL, member_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY, group_id TEXT NOT NULL, seq INTEGER NOT NULL, data TEXT NOT NULL, UNIQUE(group_id,seq)); CREATE INDEX IF NOT EXISTS message_group ON messages(group_id,seq); CREATE TABLE IF NOT EXISTS work_items(id TEXT PRIMARY KEY, group_id TEXT NOT NULL, data TEXT NOT NULL); CREATE INDEX IF NOT EXISTS work_group ON work_items(group_id); PRAGMA user_version=1;").map_err(|e| e.to_string())?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS group_cleanup(group_id TEXT PRIMARY KEY, session_ids TEXT NOT NULL); CREATE TABLE IF NOT EXISTS group_previews(id TEXT PRIMARY KEY, data TEXT NOT NULL, updated INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS group_preview_updated ON group_previews(updated DESC,id DESC);").map_err(|e| e.to_string())?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS group_migrations(name TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS group_migration_backups(migration TEXT NOT NULL, group_id TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(migration,group_id));")
            .map_err(|e| e.to_string())?;
        // One transaction repairs public history, results and derived prompts.
        // Never replay historical model calls while migrating existing data.
        c.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        let migrate = (|| -> Result<(), String> {
            migrate_builtin_models(&c)?;
            let done: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM group_migrations WHERE name='public-text-v2')",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !done {
                let ids = c
                    .prepare("SELECT id FROM groups")
                    .map_err(|e| e.to_string())?
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                for id in ids {
                    let mut group = Self::read(&c, &id)?;
                    // Keep a private, recoverable pre-migration snapshot. It is
                    // never used for history, search, prompts or public reads.
                    c.execute("INSERT OR IGNORE INTO group_migration_backups VALUES('public-text-v2',?1,?2)",params![id,serde_json::to_string(&group).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
                    let user_literals = group
                        .messages
                        .iter()
                        .filter(|m| m.kind == "user")
                        .map(|m| {
                            (
                                m.request_id.clone().unwrap_or_else(|| m.id.clone()),
                                m.text.clone(),
                            )
                        })
                        .fold(
                            std::collections::HashMap::<String, Vec<String>>::new(),
                            |mut map, (root, text)| {
                                map.entry(root).or_default().push(text);
                                map
                            },
                        );
                    let mut filters = std::collections::HashMap::<
                        String,
                        (echo_agent_memory::text_utils::PublicText, usize),
                    >::new();
                    for (index, message) in group.messages.iter_mut().enumerate() {
                        if matches!(message.kind.as_str(), "assistant" | "activity" | "handoff") {
                            let (filter, last) = filters
                                .entry(
                                    message
                                        .work_id
                                        .clone()
                                        .unwrap_or_else(|| message.id.clone()),
                                )
                                .or_default();
                            message.text = filter.push(&message.text);
                            *last = index;
                        }
                    }
                    for (_, (mut filter, last)) in filters {
                        group.messages[last].text.push_str(&filter.finish());
                    }
                    for work in &mut group.works {
                        work.output =
                            echo_agent_memory::text_utils::strip_reasoning_blocks(&work.output);
                        if let Some(literals) = user_literals.get(&work.root_id) {
                            let mut masked = String::new();
                            let mut replacements = vec![];
                            let mut cursor = 0;
                            let mut literals = literals
                                .iter()
                                .filter(|text| !text.is_empty())
                                .collect::<Vec<_>>();
                            literals.sort_by_key(|text| std::cmp::Reverse(text.len()));
                            // Search the original string, never another marker:
                            // very short user literals must not rewrite markers.
                            while let Some((offset, literal)) = literals
                                .iter()
                                .filter_map(|text| {
                                    work.prompt[cursor..]
                                        .find(text.as_str())
                                        .map(|offset| (offset, *text))
                                })
                                .min_by_key(|(offset, text)| {
                                    (*offset, std::cmp::Reverse(text.len()))
                                })
                            {
                                let start = cursor + offset;
                                masked.push_str(&work.prompt[cursor..start]);
                                let marker = format!("\u{e000}{}\u{e001}", super::id());
                                masked.push_str(&marker);
                                replacements.push((marker, literal));
                                cursor = start + literal.len();
                            }
                            masked.push_str(&work.prompt[cursor..]);
                            masked = echo_agent_memory::text_utils::strip_reasoning_blocks(&masked);
                            for (marker, literal) in replacements {
                                masked = masked.replace(&marker, literal);
                            }
                            work.prompt = masked;
                        } else {
                            work.prompt =
                                echo_agent_memory::text_utils::strip_reasoning_blocks(&work.prompt);
                        }
                    }
                    for decision in &mut group.decisions {
                        // Human-confirmed content and its audit trail are user
                        // records, including literal XML and reasoning tags.
                        if !decision.confirmed {
                            decision.text = echo_agent_memory::text_utils::strip_reasoning_blocks(
                                &decision.text,
                            );
                        }
                    }
                    Self::write(&c, &group, None)?;
                }
                c.execute("INSERT INTO group_migrations VALUES('public-text-v2')", [])
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(error) = migrate {
            let _ = c.execute_batch("ROLLBACK");
            return Err(error);
        }
        c.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        let missing = {
            let mut query = c
                .prepare("SELECT data FROM groups WHERE id NOT IN (SELECT id FROM group_previews)")
                .map_err(|e| e.to_string())?;
            let rows = query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        for data in missing {
            let group: Group = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            Self::write_preview(&c, &group)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        Ok(store)
    }
    fn connect(&self) -> Result<Connection, String> {
        let c = Connection::open(&self.path).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        Ok(c)
    }
    pub fn templates(&self) -> Result<Vec<super::management::TeamTemplate>, String> {
        let c = self.connect()?;
        let mut statement = c
            .prepare("SELECT data FROM team_templates ORDER BY name")
            .map_err(|e| e.to_string())?;
        let result = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| {
                serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
            })
            .collect();
        result
    }
    pub fn save_template(&self, template: &super::management::TeamTemplate) -> Result<(), String> {
        self.connect()?.execute("INSERT INTO team_templates VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,data=excluded.data",params![template.id,template.name,serde_json::to_string(template).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
        Ok(())
    }
    pub fn delete_template(&self, id: &str) -> Result<(), String> {
        self.connect()?
            .execute("DELETE FROM team_templates WHERE id=?", [id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<Group>, String> {
        let c = self.connect()?;
        let mut q = c
            .prepare("SELECT data FROM group_previews ORDER BY updated DESC, id DESC")
            .map_err(|e| e.to_string())?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|s| {
            serde_json::from_str(&s.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
    }
    fn read(c: &Connection, id: &str) -> Result<Group, String> {
        let data: String = c
            .query_row("SELECT data FROM groups WHERE id=?", [id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or("专家团不存在")?;
        let mut g: Group =
            serde_json::from_str(&data).map_err(|e| format!("专家团配置损坏：{e}"))?;
        let mut q = c
            .prepare("SELECT data FROM messages WHERE group_id=? ORDER BY seq")
            .map_err(|e| e.to_string())?;
        g.messages = q
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| {
                serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        let mut q = c
            .prepare("SELECT data FROM work_items WHERE group_id=? ORDER BY rowid")
            .map_err(|e| e.to_string())?;
        g.works = q
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| {
                serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(g)
    }
    pub fn get(&self, id: &str) -> Result<Group, String> {
        Self::read(&self.connect()?, id)
    }
    fn write_preview(c: &Connection, group: &Group) -> Result<(), String> {
        let mut preview = group.clone();
        preview.decisions.clear();
        for member in &mut preview.members {
            member.profile = serde_json::json!({});
        }
        preview.messages = preview
            .messages
            .into_iter()
            .rev()
            .take(1)
            .map(|mut message| {
                message.text = message.text.chars().take(160).collect();
                message.tool_calls.clear();
                message.attachments.clear();
                message.knowledge = Knowledge::default();
                message
            })
            .collect();
        // Stored headers keep recent work in reverse order; migrations and
        // ordinary writes must produce the same ordering and retain newest work.
        preview
            .works
            .sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        preview.works = preview
            .works
            .into_iter()
            .rev()
            .enumerate()
            .filter(|(index, work)| *index < 100 || !work.terminal())
            .map(|(_, work)| work)
            .map(|mut work| {
                work.prompt.clear();
                work.output.clear();
                work.seen_events.clear();
                work.error = None;
                work
            })
            .collect();
        c.execute("INSERT INTO group_previews VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated=excluded.updated", params![preview.id,serde_json::to_string(&preview).map_err(|e| e.to_string())?,preview.updated_at]).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn write(c: &Connection, g: &Group, previous: Option<&Group>) -> Result<(), String> {
        let mut header = g.clone();
        header.messages = g
            .messages
            .iter()
            .rev()
            .take(1)
            .cloned()
            .map(|mut m| {
                m.text = m.text.chars().take(160).collect();
                m.tool_calls.clear();
                m
            })
            .collect();
        header.works = g
            .works
            .iter()
            .rev()
            .enumerate()
            .filter(|(index, work)| *index < 100 || !work.terminal())
            .map(|(_, work)| work)
            .cloned()
            .map(|mut w| {
                w.prompt.clear();
                w.output.clear();
                w.seen_events.clear();
                w
            })
            .collect();
        c.execute("INSERT INTO groups VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated=excluded.updated", params![g.id, serde_json::to_string(&header).map_err(|e| e.to_string())?,g.updated_at]).map_err(|e| e.to_string())?;
        Self::write_preview(c, &header)?;
        for m in &g.members {
            for sid in [&m.session_id, &m.discussion_session_id]
                .into_iter()
                .flatten()
            {
                c.execute(
                    "INSERT OR REPLACE INTO members VALUES(?1,?2,?3)",
                    params![sid, g.id, m.id],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        let old_messages = previous
            .map(|p| {
                p.messages
                    .iter()
                    .map(|m| (m.id.as_str(), m))
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();
        let old_works = previous
            .map(|p| {
                p.works
                    .iter()
                    .map(|w| (w.id.as_str(), w))
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();
        for m in &g.messages {
            if old_messages.get(m.id.as_str()) == Some(&m) {
                continue;
            }
            c.execute("INSERT INTO messages VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data WHERE messages.group_id=excluded.group_id AND messages.seq=excluded.seq",params![m.id,g.id,m.seq,serde_json::to_string(m).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
            if c.changes() == 0 {
                return Err("消息标识已被其他专家团使用，请重新发送".into());
            }
        }
        for w in &g.works {
            if old_works.get(w.id.as_str()) == Some(&w) {
                continue;
            }
            c.execute("INSERT INTO work_items VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![w.id,g.id,serde_json::to_string(w).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn find_creation(
        &self,
        request_id: Option<&str>,
        source_session: Option<&str>,
        source_team: Option<&str>,
    ) -> Result<Option<Group>, String> {
        let c = self.connect()?;
        let existing: Option<String> = c.query_row(
            "SELECT id FROM groups WHERE id=?1 OR (json_extract(data,'$.sourceSessionId')=?2 AND json_extract(data,'$.sourceTeamId')=?3) ORDER BY CASE WHEN id=?1 THEN 0 ELSE 1 END,json_extract(data,'$.createdAt'),id LIMIT 1",
            params![request_id, source_session, source_team], |row| row.get(0),
        ).optional().map_err(|e| e.to_string())?;
        existing.map(|id| Self::read(&c, &id)).transpose()
    }

    pub fn create(&self, g: &Group) -> Result<Group, String> {
        let mut c = self.connect()?;
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        if tx
            .query_row("SELECT 1 FROM groups WHERE id=?", [&g.id], |_| Ok(()))
            .optional()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Self::read(&tx, &g.id);
        }
        if let (Some(session), Some(team)) = (&g.source_session_id, &g.source_team_id) {
            let existing: Option<String> = tx.query_row(
                "SELECT id FROM groups WHERE json_extract(data,'$.sourceSessionId')=?1 AND json_extract(data,'$.sourceTeamId')=?2 ORDER BY json_extract(data,'$.createdAt'),id LIMIT 1",
                params![session, team], |row| row.get(0),
            ).optional().map_err(|e| e.to_string())?;
            if let Some(id) = existing {
                // Include archived groups: restoring or viewing that group is
                // preferable to silently making a second source association.
                return Self::read(&tx, &id);
            }
        }
        Self::write(&tx, g, None)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(g.clone())
    }
    pub fn update<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Group) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut c = self.connect()?;
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut g = Self::read(&tx, id)?;
        let previous = g.clone();
        let out = f(&mut g)?;
        if g == previous {
            return Ok(out);
        }
        g.revision += 1;
        g.updated_at = now();
        Self::write(&tx, &g, Some(&previous))?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out)
    }
    /// Include retired model/workspace contexts, not only current member sessions.
    pub fn sessions(&self, group: &str) -> Result<Vec<String>, String> {
        let c = self.connect()?;
        let mut q = c
            .prepare("SELECT session_id FROM members WHERE group_id=? ORDER BY session_id")
            .map_err(|e| e.to_string())?;
        let rows = q
            .query_map([group], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
    pub fn stage_session_delete(
        &self,
        group: &str,
        session: &str,
        cwd: &str,
    ) -> Result<(), String> {
        self.connect()?
            .execute(
                "INSERT OR IGNORE INTO group_deletions VALUES(?1,?2,?3)",
                params![group, session, cwd],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn deletion_sessions(&self, group: &str) -> Result<Vec<(String, String)>, String> {
        let c = self.connect()?;
        let mut q = c
            .prepare(
                "SELECT session_id,cwd FROM group_deletions WHERE group_id=? ORDER BY session_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = q
            .query_map([group], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut c = self.connect()?;
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let g = Self::read(&tx, id)?;
        if g.works.iter().any(|w| !w.terminal()) {
            return Err("专家团仍有工作尚未确认停止，记录已保留，请稍后重试".into());
        }
        let sessions = {
            let mut query = tx.prepare("SELECT session_id FROM members WHERE group_id=? UNION SELECT session_id FROM group_deletions WHERE group_id=?").map_err(|e| e.to_string())?;
            let rows = query
                .query_map([id, id], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        tx.execute(
            "INSERT OR REPLACE INTO group_cleanup VALUES(?1,?2)",
            params![
                id,
                serde_json::to_string(&sessions).map_err(|e| e.to_string())?
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM group_previews WHERE id=?", [id])
            .map_err(|e| e.to_string())?;
        for table in [
            "messages",
            "work_items",
            "members",
            "group_deletions",
            "group_migration_backups",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE group_id=?"), [id])
                .map_err(|e| e.to_string())?;
        }
        tx.execute("DELETE FROM groups WHERE id=?", [id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn pending_cleanup(&self) -> Result<Vec<CleanupReceipt>, String> {
        let c = self.connect()?;
        let mut query = c
            .prepare("SELECT group_id,session_ids FROM group_cleanup ORDER BY group_id")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            let (group_id, data) = row.map_err(|e| e.to_string())?;
            Ok(CleanupReceipt {
                group_id,
                session_ids: serde_json::from_str(&data).map_err(|e| e.to_string())?,
            })
        })
        .collect()
    }
    pub fn ack_cleanup(&self, group: &str) -> Result<(), String> {
        self.connect()?
            .execute("DELETE FROM group_cleanup WHERE group_id=?", [group])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn owner(&self, session: &str) -> Result<Option<(String, String)>, String> {
        self.connect()?
            .query_row(
                "SELECT group_id,member_id FROM members WHERE session_id=?",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())
    }
    pub fn bind_owner(&self, session: &str, group: &str, member: &str) -> Result<(), String> {
        self.connect()?
            .execute(
                "INSERT OR REPLACE INTO members SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM groups WHERE id=?2)",
                params![session, group, member],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn recover(&self) -> Result<(), String> {
        for g in self.list()? {
            self.update(&g.id, |g| {
                let mut interrupted = false;
                for w in &mut g.works {
                    if w.active() {
                        w.status = "uncertain".into();
                        w.error = Some(
                            "应用重启前执行尚未确认结束，请查看原生执行记录后决定是否重试".into(),
                        );
                        interrupted = true;
                    }
                }
                g.repair_dependencies();
                if interrupted
                    || g.works
                        .iter()
                        .any(|w| w.status == "queued" || w.status == "waiting_children")
                {
                    g.paused = true;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

/// Keep persisted groups, their list projections, and reusable templates in
/// the same transaction. Original migration backups remain untouched.
fn migrate_builtin_models(c: &Connection) -> Result<(), String> {
    for table in ["groups", "group_previews", "team_templates"] {
        let mut query = c
            .prepare(&format!("SELECT id,data FROM {table}"))
            .map_err(|error| error.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        drop(query);
        for (id, data) in rows {
            let mut value: serde_json::Value =
                serde_json::from_str(&data).map_err(|error| error.to_string())?;
            let mut changed = value
                .get_mut("defaultModelId")
                .is_some_and(crate::providers::migrate_builtin_model_reference);
            if let Some(members) = value
                .get_mut("members")
                .and_then(serde_json::Value::as_array_mut)
            {
                for member in members {
                    changed |= member
                        .get_mut("modelId")
                        .is_some_and(crate::providers::migrate_builtin_model_reference);
                }
            }
            if changed {
                c.execute(
                    &format!("UPDATE {table} SET data=?1 WHERE id=?2"),
                    params![value.to_string(), id],
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}
