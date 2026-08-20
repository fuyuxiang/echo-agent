# CLI 命令参考

Echo Agent 命令行接口的完整参考。所有命令通过 `echo-agent` 主入口调用。

## 全局选项

| 选项 | 缩写 | 类型 | 说明 |
|------|------|------|------|
| `--config` | `-c` | path | 指定配置文件路径 |
| `--verbose` | `-v` | flag | 启用详细输出（可叠加 -vv） |
| `--quiet` | `-q` | flag | 静默模式，仅输出错误 |
| `--version` | — | flag | 显示版本号 |
| `--help` | `-h` | flag | 显示帮助信息 |

---

## run

启动 Echo Agent 主进程（前台运行）。

```bash
echo-agent run [OPTIONS]
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--config` / `-c` | path | `~/.echo-agent/config.yaml` | 配置文件 |
| `--workspace` / `-w` | path | `.` | 工作区目录 |
| `--profile` | string | — | 激活的配置 Profile |
| `--no-gateway` | flag | — | 不启动 Gateway 服务 |
| `--no-scheduler` | flag | — | 不启动定时任务调度 |
| `--dry-run` | flag | — | 仅验证配置，不实际启动 |

```bash
# 基本启动
echo-agent run

# 指定配置文件和工作区
echo-agent run -c ./my-config.yaml -w ~/projects/myapp

# 仅验证配置
echo-agent run --dry-run
```

---

## setup

交互式初始化向导，创建配置文件和必要目录。

```bash
echo-agent setup [OPTIONS]
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--force` | flag | — | 强制覆盖已有配置 |
| `--minimal` | flag | — | 最小化配置（跳过可选项） |
| `--non-interactive` | flag | — | 使用默认值，不交互提问 |

```bash
# 交互式向导
echo-agent setup

# 非交互式最小配置
echo-agent setup --non-interactive --minimal
```

---

## status

显示当前 Echo Agent 运行状态摘要。

```bash
echo-agent status [OPTIONS]
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--json` | flag | — | JSON 格式输出 |
| `--watch` | flag | — | 持续刷新显示 |

输出信息包含：

- 运行状态（running / stopped）
- 活跃会话数
- 通道连接状态
- 内存使用量
- 运行时长

---

## cost

查看费用统计与分析。

```bash
echo-agent cost [OPTIONS]
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--period` | string | `today` | 统计周期（today/week/month/all） |
| `--by-model` | flag | — | 按模型分组 |
| `--by-tool` | flag | — | 按工具分组 |
| `--json` | flag | — | JSON 格式输出 |

```bash
# 查看本月费用
echo-agent cost --period month

# 按模型分组统计
echo-agent cost --period week --by-model
```

---

## gateway

管理 Gateway HTTP/WebSocket 服务。

### 前台模式

```bash
echo-agent gateway [OPTIONS]
```

直接前台启动 Gateway 服务（不启动完整 Agent 运行时）。

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--host` | string | `127.0.0.1` | 监听地址 |
| `--port` | int | `8080` | 监听端口 |
| `--auth-mode` | string | `pairing` | 认证模式 |

### 子命令

| 子命令 | 说明 |
|--------|------|
| `gateway install` | 安装为系统服务（systemd/launchd/Windows Service） |
| `gateway uninstall` | 卸载系统服务 |
| `gateway start` | 启动已安装的服务 |
| `gateway stop` | 停止服务 |
| `gateway restart` | 重启服务 |
| `gateway status` | 查看服务状态 |
| `gateway logs` | 查看服务日志 |

```bash
# 安装为系统服务
echo-agent gateway install

# 查看服务日志（最近 100 行）
echo-agent gateway logs --lines 100

# 重启服务
echo-agent gateway restart
```

!!! tip "服务管理"
    `gateway install` 会自动检测当前平台并选择合适的服务管理器。Linux 使用 systemd，macOS 使用 launchd。

---

## cli

启动交互式 CLI 会话（无 TUI）。

```bash
echo-agent cli [OPTIONS]
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--session` / `-s` | string | — | 恢复指定会话 ID |
| `--one-shot` | string | — | 单次提问模式，输出后退出 |

```bash
# 进入交互式会话
echo-agent cli

# 单次提问
echo-agent cli --one-shot "帮我总结今天的任务"
```

---

## dashboard

管理 Web Dashboard。

### build

```bash
echo-agent dashboard build [OPTIONS]
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `--output` / `-o` | path | `static/dashboard/` | 构建输出目录 |
| `--dev` | flag | — | 开发模式（含 source map） |

---

## cron

管理定时任务。

| 子命令 | 说明 |
|--------|------|
| `cron list` | 列出所有定时任务 |
| `cron authorize` | 授权待审批的定时任务（需先停止服务） |
| `cron revoke` | 撤销已授权的定时任务（需先停止服务） |

```bash
# 列出所有定时任务
echo-agent cron list

# 授权任务
echo-agent cron authorize <task_id>

# 撤销授权
echo-agent cron revoke <task_id>
```

!!! warning "authorize / revoke 需要服务处于停止状态"
    这两个子命令直接改写 `<workspace>/data/scheduler.json`。运行中的 gateway 会
    在内存中持有全部任务并定期整体回写，离线改动会被覆盖，因此命令检测到实例锁被
    占用时会直接拒绝执行。服务运行期间请改用对话（「授权定时任务 `<job_id>`」）或
    Dashboard 定时任务页授权。`cron list` 只读，任何时候都可用。

### cron list 输出

| 列 | 说明 |
|----|------|
| ID | 任务 ID |
| Schedule | Cron 表达式 |
| Status | enabled / disabled / pending |
| Last Run | 上次执行时间 |
| Next Run | 下次计划时间 |
| Description | 任务描述 |

---

## eval

运行评估任务（技能与模型评测）。

```bash
echo-agent eval [OPTIONS] <dataset>
```

| 选项 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `<dataset>` | path | — | 评估数据集路径（必选） |
| `--model` | string | — | 指定评估模型 |
| `--output` / `-o` | path | `eval_results/` | 结果输出目录 |
| `--parallel` / `-p` | int | `4` | 并发数 |
| `--verbose` | flag | — | 显示每条评估的详情 |

```bash
echo-agent eval datasets/coding_bench.json --model claude-sonnet --parallel 8
```

---

## plugin

管理插件系统。

| 子命令 | 说明 |
|--------|------|
| `plugin list` | 列出所有已安装插件 |
| `plugin info <name>` | 显示插件详细信息 |
| `plugin enable <name>` | 启用插件 |
| `plugin disable <name>` | 禁用插件 |
| `plugin check` | 检查插件兼容性和健康状态 |

```bash
# 列出插件
echo-agent plugin list

# 查看插件信息
echo-agent plugin info slack-channel

# 启用插件
echo-agent plugin enable slack-channel
```

---

## evolution

管理技能进化系统。

| 子命令 | 说明 |
|--------|------|
| `evolution status` | 查看进化系统状态 |
| `evolution run` | 触发一次进化评估 |
| `evolution list-candidates` | 列出候选技能 |
| `evolution show-candidate <id>` | 显示候选详情 |
| `evolution promote <id>` | 提升候选为正式技能 |
| `evolution rollback <技能名>` | 回滚已提升的技能（参数是技能名，不是候选 id） |
| `evolution init-dataset` | 初始化评估数据集 |

```bash
# 查看状态
echo-agent evolution status

# 触发进化
echo-agent evolution run

# 提升候选技能
echo-agent evolution promote candidate_abc123
```

!!! warning "进化操作"
    `promote` 操作会立即将候选技能加入活跃技能池。建议先通过 `show-candidate` 确认评估结果。

---

## skill

管理暂存区技能审批。

| 子命令 | 说明 |
|--------|------|
| `skill list-staged` | 列出暂存区中的技能 |
| `skill approve <id>` | 批准暂存技能 |
| `skill reject <id>` | 拒绝暂存技能 |

```bash
echo-agent skill list-staged
echo-agent skill approve skill_xyz
echo-agent skill reject skill_xyz --reason "质量不达标"
```

---

## config

配置管理工具。

| 子命令 | 说明 |
|--------|------|
| `config dump` | 导出当前生效的完整配置 |
| `config explain <field>` | 解释指定配置字段 |
| `config validate` | 验证配置文件 |
| `config gen-docs` | 生成配置文档 |

```bash
# 导出配置（含来源标注）
echo-agent config dump --show-source

# 解释字段
echo-agent config explain security.profile

# 验证配置
echo-agent config validate -c ./config.yaml
```

---

## checkpoint

管理系统检查点（快照）。

| 子命令 | 说明 |
|--------|------|
| `checkpoint list` | 列出所有检查点 |
| `checkpoint show <id>` | 显示检查点详情 |
| `checkpoint restore <id>` | 恢复到指定检查点 |
| `checkpoint prune` | 清理过期检查点 |

```bash
# 列出检查点
echo-agent checkpoint list

# 恢复
echo-agent checkpoint restore chk_20240101_120000

# 清理 30 天前的检查点
echo-agent checkpoint prune --older-than 30d
```

!!! danger "恢复操作"
    `checkpoint restore` 会覆盖当前状态。操作前会自动创建一个备份检查点。

---

## migrate

数据库与数据迁移。

| 子命令 | 说明 |
|--------|------|
| `migrate run` | 执行待处理的迁移 |
| `migrate rollback` | 回滚最近一次迁移 |
| `migrate status` | 查看迁移状态 |
| `migrate memory-md` | 迁移旧版 memory.md 到新格式 |

```bash
# 执行迁移
echo-agent migrate run

# 查看状态
echo-agent migrate status

# 迁移旧版记忆文件
echo-agent migrate memory-md ./old-memory.md
```

---

## deps

管理运行时依赖。

| 子命令 | 说明 |
|--------|------|
| `deps status` | 显示依赖状态 |
| `deps install` | 安装缺失的依赖 |
| `deps refresh` | 刷新依赖锁文件 |

```bash
echo-agent deps status
echo-agent deps install
echo-agent deps refresh
```

---

## service（已废弃）

!!! warning "已废弃"
    `service` 命令已废弃，功能已迁移至 `gateway` 命令。请使用 `echo-agent gateway` 代替。

```bash
# 旧用法（已废弃）
echo-agent service start

# 新用法
echo-agent gateway start
```

---

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 成功 |
| `1` | 通用错误 |
| `2` | 配置错误 |
| `3` | 连接失败 |
| `4` | 认证失败 |
| `5` | 权限不足 |
| `130` | 用户中断（Ctrl+C） |
