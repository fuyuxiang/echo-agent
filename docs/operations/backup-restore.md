# 备份与恢复

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

默认工作区是 `~/.echo-agent`，但 `workspace`、`-w/--workspace` 和各 `storage.*` 路径可以改变实际存储位置。先运行 `echo-agent config dump` 确认部署路径。完整备份至少覆盖配置、SQLite 数据库、`storage.sessions_dir`、`storage.memory_dir` 和需要保留的知识文档与技能。`checkpoint.store_path` 默认 `~/.echo-agent/checkpoints/store`，也可按需一并备份。

## 停机完整备份

先停止写入。若安装了用户级后台服务：

```bash
echo-agent gateway stop
tar -czf "$HOME/echo-agent-backup-$(date +%Y%m%d).tar.gz" -C "$HOME" .echo-agent
echo-agent gateway start
```

如果运行的是前台 `echo-agent run` 或 `echo-agent gateway`，先停止该进程；若使用自定义工作区，应将 `-C` 和目录名改为实际位置。不要直接复制正在写入的 SQLite 数据库文件。备份文件本身可能包含令牌、对话和记忆，应限制访问权限。

## 运行中备份 SQLite

SQLite 命令行的 `.backup` 可生成一致的数据库副本：

```bash
sqlite3 "$HOME/.echo-agent/data/echo_agent.db"   ".backup '$HOME/echo-agent-sqlite-backup.db'"
```

这只备份数据库；记忆、会话、配置及其他文件仍需另行备份。它们可能与数据库副本处于不同时间点，因此跨文件一致的完整备份仍以停机备份为准。

## 从完整备份恢复

停止服务或前台进程，并保留当前工作区副本，再将与目标程序版本匹配的归档恢复到原位置。例如：

```bash
echo-agent gateway stop
mv "$HOME/.echo-agent" "$HOME/.echo-agent.before-restore"
tar -xzf "$HOME/echo-agent-backup-20261001.tar.gz" -C "$HOME"
sqlite3 "$HOME/.echo-agent/data/echo_agent.db" "PRAGMA integrity_check;"
echo-agent config validate
echo-agent gateway start
echo-agent gateway status
```

路径和文件名均应替换为实际备份。运行前确认归档来源及内容；如果没有安装后台服务，就停止并重新启动对应的前台进程。

## 从 SQLite 备份恢复 { #restore-sqlite-backup }

只恢复数据库时，应确保它与保留的其他数据兼容：

```bash
echo-agent gateway stop
cp "$HOME/echo-agent-sqlite-backup.db" "$HOME/.echo-agent/data/echo_agent.db"
sqlite3 "$HOME/.echo-agent/data/echo_agent.db" "PRAGMA integrity_check;"
echo-agent gateway start
```

覆盖数据库前应保留现有文件副本。会话、记忆和配置不在这个数据库副本中时，需要从相同时间点的其他备份恢复。

## 检查点范围

`echo-agent checkpoint list`、`show`、`restore` 和 `prune` 操作影子 Git 工作区文件快照。快照排除数据库、会话、记忆和日志，不能代替完整备份，也不能回退 SQLite 表结构。详见[文件系统布局](../reference/filesystem-layout.md)与[升级与数据迁移](upgrade-migrations.md)。
