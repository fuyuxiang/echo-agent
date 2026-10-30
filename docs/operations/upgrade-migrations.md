# 升级与数据迁移

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

升级前阅读 [Changelog](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/CHANGELOG.md)，确认兼容性变化，并备份工作区。下列命令以默认工作区 `~/.echo-agent` 和已安装的用户级 Gateway 服务为例。使用自定义 `--workspace` 或 `--config` 时，应替换为实际路径。

## 升级已安装的包

```bash
echo-agent gateway stop
tar -czf "$HOME/echo-agent-backup-$(date +%Y%m%d).tar.gz" -C "$HOME" .echo-agent
pip install --upgrade "echo-agent[all]"
echo-agent config validate
echo-agent gateway start
echo-agent gateway status
```

如果没有安装后台服务，先停止前台运行的 `echo-agent run` 或 `echo-agent gateway` 进程，升级后重新启动。备份 SQLite 数据库前应停止写入。其他备份方式见[备份与恢复](backup-restore.md)。

SQLite 表结构迁移由 `echo_agent/storage/sqlite.py` 在数据库连接初始化时自动执行。**`echo-agent migrate` 不是数据库表结构迁移命令。**启动失败时应先查看错误日志，再决定是否从升级前备份恢复。

## `migrate` 命令的用途

当前 CLI 的 `migrate` 子命令处理 USER 记忆的归属键，以及旧 `MEMORY.*.md` 分片的导入：

| 命令 | 作用 |
|---|---|
| `echo-agent migrate status` | 统计命中 `memory.principal_bindings` 的旧 `source_session` 和可选的空 scope USER 记忆；显示最近的记忆备份 |
| `echo-agent migrate run --dry-run` | 预览将改写的 USER 记忆归属键 |
| `echo-agent migrate run` | 确认后备份 `user_memory.json`，并改写命中绑定关系的归属键 |
| `echo-agent migrate run --adopt-empty` | 同时收编空 scope、非 global 的 USER 记忆；应先预演并确认目标 owner |
| `echo-agent migrate memory-md` | 将旧 `MEMORY.*.md` 分片中的事实导入记忆存储 |
| `echo-agent migrate rollback` | 用最近的 `user_memory.json.migbak-*` 备份覆盖该文件；**不会回滚 SQLite 表结构** |

只有更新说明或运维计划要求迁移记忆归属时才运行这些命令。`status` 不报告数据库版本，也不列出待执行的 SQL 迁移。

## 回退版本

`checkpoint restore` 恢复工作区文件快照，不包含数据库、会话、记忆和日志目录。回退 Python 包也不会逆向改写数据库。需要恢复升级前的数据时，使用升级前创建的完整备份：

1. 停止 Gateway 或前台进程。
2. 保存当前数据副本，以便排查或重新升级。
3. 安装目标旧版本，并将其对应的完整备份恢复到原工作区。
4. 运行 `echo-agent config validate`，启动并检查日志与关键功能。

不要把 `echo-agent migrate rollback` 用作数据库回退。具体恢复命令见[备份与恢复](backup-restore.md)。跨多个 Beta 版本升级或降级时，逐版本查阅更新说明；不应假定每个数据变化都有可逆的迁移脚本。

## 2026-10-21 服务端修复兼容说明

升级应以实际安装包或源码提交为单位核对变更，确保相关依赖匹配。仅比较版本字符串不足以确认源码修复是否已进入某个发布包；部署前应检查对应发行内容。

1. **停新入站、等待在途执行结束，再备份。** 备份配置、完整workspace、SQLite及其一致性状态、scheduler.json、用户skills/禁用状态、记忆与知识源、artifact/spill清单、OAuth token和 `.credential_key`。配置中的绝对外部目录/显式共享checkpoint目录也需独立备份。不要只复制正在写入的SQLite主文件。
2. **模型依赖。** Gemini的`gemini`/`google`配置名、key/env和模型保持；extras和lazy安装统一为`google-genai>=2.32,<3`，不要继续只装旧google-generativeai。`cryptography`与有超时的`regex`为核心依赖；不要通过关闭加密解决缺包。PyMuPDF暂限定1.x以避开2.0发行元数据错误。重新运行`pip check`。
3. **任务与Cron数据。** Scheduler JSON新增run身份/active_run_id等兼容字段；旧文件可读。新版本用本次run精准归属历史并在整个执行周期保持重叠/容量约束。崩溃后queued/dispatching记interrupted，不盲目再次执行外部副作用。API触发不存在为404，暂停/禁用/正在运行或容量冲突为409。损坏的权威scheduler/凭据/会话数据显式报错并保留文件，应修复或恢复备份，不写空文件“修启动”。
4. **原生消息身份。** 新版为原生消息使用稳定ID并在准入成功后ACK/推进游标；同版本重启可用持久账本去重。旧版本随机event_id不能从既存历史推导成新映射；升级时停入站并排空在途消息，平台重放的旧消息仍需按实际历史核对，不能宣称跨旧版历史exactly-once。
5. **OAuth。** MCP token现在绑定完整resource URL。旧未记录绑定或同server_name更换URL的token文件被保留但不再复用；需要在同一workspace的前台有TTY进程重新授权一次，再由后台服务使用绑定token/refresh。无TTY首次或失效授权立即给出操作诊断，不打开浏览器后无声等待300秒。不要把旧bearer复制到新resource。未完成MCP2026无会话协议迁移，服务器须提供受支持的legacy版本。
6. **保存与生效。** Dashboard普通配置PATCH只保存磁盘；刷新可见运行值、待重启字段和restart_required。显式渠道生命周期可局部应用保存的渠道配置，其他Runtime设置需重启。敏感值不能用`***`占位符写回，也不能通过整段对象更新绕校验。服务停止/启用失败和重复运行/端口冲突均退出非零，应先排查日志。
7. **评测与轨迹。** baseline/candidate在技能快照、独立会话/SQLite/临时文件工作区运行；无法隔离的外部工具/必需插件是inconclusive，不是效果提升。只有门禁通过才晋升在线技能。新版默认轨迹工具args/result/error仅存摘要哈希，但旧版已保存的明文预览不会自动删除；由管理员停服务备份后按真实敏感数据处理历史 `evolution_trajectories`，不要把含旧预览的数据库公开。
8. **恢复与回退。** 会话SQLite写故障产生的较新原子fallback在恢复时回迁，成功保存后清理；凭据失败保存不改变内存授权值。文件checkpoint仍不包含数据库/会话/记忆，也不能撤销已发消息或shell外部作用。回退前先留存当前数据副本，安装旧代码及匹配依赖并恢复对应完整备份，特别是scheduler、OAuth和技能目录；不要让旧进程在新执行run仍活跃时接管。

重启后检查 `/api/v1/health`、`/api/v1/ready`（自定义API前缀相应替换），验证一个真实工具任务、历史查询与产物投递。ready只验证本地运行条件，不代替真实模型测试。可复现包/进程测试见[评审证据](../reviews/server-review-and-remediation.md)，已验证与环境阻塞明确分列。
