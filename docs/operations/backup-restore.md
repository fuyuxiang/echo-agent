# 备份与恢复

Echo Agent 的所有持久化数据存储在文件系统中，备份策略围绕数据目录展开。

---

## 数据目录结构

```
~/.echo-agent/                   # 全局 Home
├── config.yaml                  # 配置（必须备份）
├── data/
│   ├── echo_agent.db            # SQLite 主数据库（必须备份）
│   ├── memory/                  # 长期记忆存储（必须备份）
│   ├── knowledge/               # 知识库索引（建议备份）
│   ├── spill/                   # 工具输出溢写（可选备份）
│   ├── logs/                    # 运行日志（可选备份）
│   └── checkpoints/             # 状态检查点（建议备份）
└── env                          # 环境变量（如有则备份）
```

### 备份优先级

| 路径 | 优先级 | 说明 |
|------|--------|------|
| `config.yaml` | 必须 | 配置丢失需重建 |
| `data/echo_agent.db` | 必须 | 会话、任务、元数据 |
| `data/memory/` | 必须 | Agent 长期记忆，丢失不可恢复 |
| `data/knowledge/` | 建议 | 知识库索引，可从源文档重建 |
| `data/checkpoints/` | 建议 | 运行状态快照 |
| `data/spill/` | 可选 | 大输出临时存储，通常可丢弃 |
| `data/logs/` | 可选 | 历史日志，用于审计 |

---

## 备份方法

### 方法一：目录整体备份

最简单可靠的方式，停止服务后复制整个目录：

```bash
# 停止服务（避免备份期间写入）
echo-agent gateway stop

# 整体备份
tar -czf echo-agent-backup-$(date +%Y%m%d).tar.gz ~/.echo-agent/

# 重新启动
echo-agent gateway start
```

!!! warning "热备份风险"
    在 Gateway 运行时直接复制目录可能导致 SQLite 数据库文件不一致。如果无法停止服务，请使用 SQLite 在线备份方法。

### 方法二：SQLite 在线备份

无需停止服务，利用 SQLite 的 `.backup` 命令进行一致性备份：

```bash
# SQLite 在线备份（服务运行时安全）
sqlite3 ~/.echo-agent/data/echo_agent.db ".backup /backup/echo_agent.db"

# 配合文件备份
rsync -a --exclude='data/echo_agent.db' ~/.echo-agent/ /backup/echo-agent/
cp /backup/echo_agent.db /backup/echo-agent/data/
```

### 方法三：checkpoint 命令

Echo Agent 内置的检查点机制，可在运行时创建一致性快照：

```bash
# 查看已有检查点
echo-agent checkpoint list

# 查看检查点详情
echo-agent checkpoint show <checkpoint-id>

# 手动触发检查点（运行时创建）
# 通常由系统在关键操作前后自动创建
```

---

## 定时备份脚本

### Cron 定时备份

```bash
#!/bin/bash
# /usr/local/bin/echo-agent-backup.sh

BACKUP_DIR="/backup/echo-agent"
RETENTION_DAYS=30
DATE=$(date +%Y%m%d_%H%M%S)

mkdir -p "$BACKUP_DIR"

# SQLite 在线备份
sqlite3 ~/.echo-agent/data/echo_agent.db \
  ".backup $BACKUP_DIR/echo_agent_$DATE.db"

# 配置和记忆备份
tar -czf "$BACKUP_DIR/memory_$DATE.tar.gz" \
  ~/.echo-agent/config.yaml \
  ~/.echo-agent/data/memory/ \
  ~/.echo-agent/data/knowledge/ \
  ~/.echo-agent/data/checkpoints/

# 清理过期备份
find "$BACKUP_DIR" -type f -mtime +$RETENTION_DAYS -delete

echo "Backup completed: $DATE"
```

```bash
# 添加 cron 任务（每日凌晨 3 点）
crontab -e
# 0 3 * * * /usr/local/bin/echo-agent-backup.sh >> /var/log/echo-agent-backup.log 2>&1
```

---

## 恢复流程

### 从完整备份恢复

```bash
# 1. 停止服务
echo-agent gateway stop

# 2. 备份当前数据（防止误操作）
mv ~/.echo-agent ~/.echo-agent.old

# 3. 解压备份
tar -xzf echo-agent-backup-20240101.tar.gz -C ~/

# 4. 验证数据完整性
sqlite3 ~/.echo-agent/data/echo_agent.db "PRAGMA integrity_check;"
# ok

# 5. 重启服务
echo-agent gateway start

# 6. 验证恢复成功
echo-agent status
```

### 从检查点恢复

```bash
# 查看可用检查点
echo-agent checkpoint list

# 恢复到指定检查点
echo-agent checkpoint restore <checkpoint-id>

# 验证状态
echo-agent status
```

!!! warning "检查点恢复的范围"
    `checkpoint restore` 恢复 Agent 运行状态（会话、记忆快照），但不恢复配置文件变更。配置需从备份中单独恢复。

### 从 SQLite 备份恢复

```bash
# 停止服务
echo-agent gateway stop

# 替换数据库文件
cp /backup/echo_agent_20240101.db ~/.echo-agent/data/echo_agent.db

# 重启
echo-agent gateway start
```

---

## 检查点管理

```bash
# 列出所有检查点
echo-agent checkpoint list

# 查看详情（包含时间、大小、关联的操作）
echo-agent checkpoint show <id>

# 恢复
echo-agent checkpoint restore <id>

# 清理旧检查点（释放磁盘空间）
echo-agent checkpoint prune
```

!!! tip "检查点 vs 备份"
    检查点是轻量级运行状态快照，适合快速回滚最近的变更。完整备份适合灾难恢复和跨机器迁移。两者配合使用效果最佳。

---

## 灾难恢复清单

1. **停止服务** — `echo-agent gateway stop`
2. **评估损失** — 确认哪些数据受损
3. **选择恢复源** — 最近的备份或检查点
4. **执行恢复** — 按上述流程操作
5. **验证完整性** — `sqlite3 ... "PRAGMA integrity_check;"`
6. **验证功能** — `echo-agent status` 并执行测试对话
7. **重启服务** — `echo-agent gateway start`
8. **补充备份** — 恢复后立即创建新备份
