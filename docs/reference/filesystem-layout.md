# 文件系统布局

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

`Config.workspace` 默认 `~/.echo-agent`，`-w/--workspace` 可在支持的 CLI 子命令中覆盖工作区。下列相对路径均以实际工作区为根；配置项也可改写部分位置。请通过 `echo-agent config dump` 确认当前环境的最终路径。

## 工作区中的主要路径

| 路径或配置项 | 默认值 | 内容 |
|---|---|---|
| `storage.database_path` | `data/echo_agent.db` | SQLite 数据库；SQLite 可能创建 `-wal`、`-shm` 附属文件 |
| `storage.sessions_dir` | `data/sessions` | 会话数据 |
| `storage.memory_dir` | `data/memory` | `user_memory.json`、`env_memory.json` 等记忆文件；记忆分片可能以 `MEMORY.*.md` 命名 |
| `knowledge.docs_dir` | `data/knowledge` | 知识库原始文档 |
| `storage.spill_dir` | `data/spill` | 过大的工具输出；必须是工作区内的专用相对目录 |
| `storage.logs_dir` | `data/logs` | 执行轨迹、工具和记忆审计、看门狗转储等；普通 Loguru 日志输出到 stderr |
| `artifacts.root_dir` | `data/artifacts` | 用户可交付产物 |
| `skills.skills_dir` | `skills` | 工作区技能 |

并非每个目录都会在首次启动时创建。技能本身由 `SKILL.md` 描述；不要假定每个技能都含有 `handler.py`、`manifest.yaml` 或测试目录。

## 配置文件

加载器会在搜索目录中按顺序查找 `echo-agent.yaml`、`echo-agent.yml`、`config.yaml`、`config.yml`。未显式指定文件时会搜索当前目录及用户目录；`-c/--config` 可指定文件。配置加载顺序和环境变量规则见[环境变量参考](environment-variables.md)。`.echo-agent/config.yaml` 不是一个固定会自动覆盖用户配置的独立层级。

如要把配置纳入版本控制，应检查其中没有明文密钥；运行时数据库、会话、记忆与审计数据通常不应提交到代码仓库。

## 检查点

`checkpoint.store_path` 默认 `~/.echo-agent/checkpoints/store`。检查点由影子 Git 仓库存储，而不是 `chk_<timestamp>/manifest.json` 文件树。其工作区快照排除数据库、会话、记忆、日志以及检查点存储自身；不能用 `checkpoint restore` 恢复这些数据。`echo-agent checkpoint list`、`show`、`restore` 和 `prune` 是当前 CLI 操作，`prune` 没有 `--older-than` 或 `--keep` 参数。

## 备份与清理

完整备份应包含实际工作区中的数据库、会话、记忆、配置与其他重要数据，并在停止写入后进行；运行中备份 SQLite 时应使用一致性备份方法。`VACUUM` 等数据库维护需在合适的停机或低负载窗口运行。具体步骤见[备份与恢复](../operations/backup-restore.md)。
