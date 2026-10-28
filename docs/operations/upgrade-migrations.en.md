# Upgrade and Data Migration

Read the [Changelog](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/CHANGELOG.md), check compatibility changes, and back up the workspace before upgrading. The commands below assume the default `~/.echo-agent` workspace and an installed user-level Gateway service. Use the actual paths for deployments with a custom `--workspace` or `--config`.

## Upgrade an installed package

```bash
echo-agent gateway stop
tar -czf "$HOME/echo-agent-backup-$(date +%Y%m%d).tar.gz" -C "$HOME" .echo-agent
pip install --upgrade "echo-agent[all]"
echo-agent config validate
echo-agent gateway start
echo-agent gateway status
```

If no background service is installed, stop the foreground `echo-agent run` or `echo-agent gateway` process before upgrading, then restart it. Stop writes before backing up the SQLite database. See [backup and restore](backup-restore.en.md) for other methods.

SQLite schema migrations run automatically when `echo_agent/storage/sqlite.py` initializes the database connection. **`echo-agent migrate` does not migrate the database schema.** If startup fails, inspect the error log before deciding whether to restore the pre-upgrade backup.

## What `migrate` does

The CLI `migrate` command handles USER memory ownership keys and imports legacy `MEMORY.*.md` shards:

| Command | Effect |
|---|---|
| `echo-agent migrate status` | Counts old `source_session` values matching `memory.principal_bindings` and optionally adoptable empty-scope USER entries; shows the latest memory backup |
| `echo-agent migrate run --dry-run` | Previews USER memory owner-key changes |
| `echo-agent migrate run` | After confirmation, backs up `user_memory.json` and changes matching owner keys |
| `echo-agent migrate run --adopt-empty` | Also adopts non-global USER entries with an empty scope; preview and verify the target owner first |
| `echo-agent migrate memory-md` | Imports facts from legacy `MEMORY.*.md` shards into memory storage |
| `echo-agent migrate rollback` | Restores the latest `user_memory.json.migbak-*` backup; **does not roll back SQLite schema** |

Run these commands only when a Changelog entry or an operations plan calls for memory ownership migration. `status` neither reports a database version nor lists pending SQL migrations.

## Roll back a release

`checkpoint restore` restores workspace file snapshots, excluding the database, sessions, memory, and logs. Downgrading the Python package does not reverse database changes. To restore pre-upgrade data:

1. Stop the Gateway or foreground process.
2. Keep a copy of the current data for diagnosis or another upgrade attempt.
3. Install the target older version and restore its matching full workspace backup.
4. Run `echo-agent config validate`, start the service, and inspect logs and critical features.

Do not use `echo-agent migrate rollback` as a database rollback. See [backup and restore](backup-restore.en.md) for restoration commands. For upgrades or downgrades spanning several Beta releases, read each intervening Changelog; do not assume every data change has a reversible migration script.

## Server remediation compatibility (2026-10-21)

These are working-tree changes, not a published release: the repository version remains 0.3.8. Upgrade matching code and dependencies together. Stop new ingress, drain in-flight work, and back up the complete workspace/config plus any configured external paths. Include consistent SQLite state, scheduler.json, memory/knowledge sources, skills/disabled state, artifacts, OAuth files, and .credential_key. A file checkpoint is not a database backup.

Gemini retains provider names/key precedence/model configuration but requires `google-genai>=2.32,<3`. cryptography and deadline-capable regex are core dependencies; keep credential encryption enabled. PyMuPDF remains below 2 pending its release metadata repair. Run pip check after installation.

Scheduler JSON fields for run identity and active admission are additive. New runs retain their own results and overlap/capacity fence through execution; interrupted queued/dispatching work is recorded rather than blindly replaying external side effects. Trigger conflicts return409, absence404. Damaged authoritative scheduler/credential/session state fails explicitly while preserving the original file. New native message IDs deduplicate across new-version restarts; historical random event IDs cannot be reverse-migrated, so drain before upgrading and review any old platform replay.

MCP OAuth bearer tokens now bind the full resource URL. Unbound legacy tokens and tokens for a changed resource remain on disk but are not reused. Reauthorize once in a foreground process with a TTY in the same workspace; background service can then use the bound token/refresh. Headless missing/expired authorization fails with instructions instead of waiting silently. Do not copy an old bearer token to another resource. MCP 2026 stateless protocol support has not been claimed.

Ordinary Dashboard saves persist configuration without hot-mutating Runtime. Saved/running differences and pending restart fields survive refresh; explicit channel lifecycle is the documented local activation path. Service failures, port conflicts, and duplicate Runtimes return nonzero. Evaluation uses temporary skill snapshots and independent per-case sessions/SQLite/files; unavailable external adapters are inconclusive. Only promotion writes online skills. New default trajectory redaction stores tool hashes, but historical plaintext previews are not automatically rewritten or deleted; administrators must back up and handle any sensitive historical evolution_trajectories themselves.

Before rollback save the current state, install the older code with matching dependencies, and restore its corresponding complete backup. Do not hand active new runs to an old scheduler or treat migrate rollback/checkpoint restore as a database or external-side-effect rollback. After restart verify health/readiness, an actual model/tool task, history, and artifact delivery. Readiness is local configuration/runtime qualification, not an upstream model probe. See the [review evidence](../reviews/server-review-and-remediation.md).
