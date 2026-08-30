# CLI 命令参考

本文档对应当前 `echo-agent` 参数解析器。最可靠的即时参考始终是
`echo-agent --help` 和 `echo-agent <command> --help`。

## 全局用法

```bash
echo-agent [--version] [-c CONFIG] [-w WORKSPACE] <command> ...
```

`-c/--config` 与 `-w/--workspace` 既可放在主命令前，也可放在支持它们的子命令后。
项目没有全局 `--verbose`、`--quiet`，也没有隐式远程 CLI 连接。

## 命令总览

| 命令 | 用途 |
|---|---|
| `run` | 前台启动完整 Agent |
| `setup` | 运行配置向导或单独配置一个区段 |
| `status` | 查看配置与运行能力摘要 |
| `cost` | 查看成本归因与趋势 |
| `gateway` | 前台运行网关或管理后台服务 |
| `cli` | 连接本机网关的交互终端 |
| `dashboard build` | 构建完整 Web Dashboard |
| `cron` | 查看、授权或撤销定时任务 |
| `eval` | 运行评估数据集 |
| `plugin` | 管理插件 |
| `evolution` | 管理技能进化 |
| `skill` | 审批暂存技能 |
| `config` | 查看、解释、校验配置或生成文档 |
| `checkpoint` | 查看和恢复文件检查点 |
| `migrate` | 执行数据迁移 |
| `deps` | 管理技能依赖 |
| `service` | 已弃用的 `gateway` 兼容别名 |

## run

```bash
echo-agent run [-c CONFIG] [-w WORKSPACE] [--force]
```

`--force` 会跳过同一 workspace 的单实例保护，可能造成重复回复和并发写库，只应在明确
知道风险时使用。当前命令没有 `--dry-run`、`--no-gateway` 或 `--no-scheduler`。

## setup

```bash
echo-agent setup [SECTION] [-c CONFIG] [-w WORKSPACE]
                 [--lang en|zh|auto] [--flow quickstart|full] [--json]
```

- 省略 `SECTION` 时显示交互菜单；可用区段以 `echo-agent setup --help` 的实时列表为准。
- `--flow` 跳过菜单直接执行快速或完整流程。
- `--json` 仅用于 `doctor` 区段，输出无 ANSI 的机器可读结果。

```bash
echo-agent setup
echo-agent setup gateway --lang zh
echo-agent setup doctor --json
```

## status

```bash
echo-agent status [-c CONFIG] [-w WORKSPACE] [--json]
```

展示当前配置、网关与关键能力状态；`--json` 适合脚本和监控。当前没有 `--watch`。

## cost

```bash
echo-agent cost [-c CONFIG] [-w WORKSPACE] [--days N] [--json]
```

`--days` 默认 `7`。报告包含总成本、预算状态及模型/渠道归因；当前没有
`--period`、`--by-model` 或 `--by-tool`。

## gateway

```bash
# 前台运行
echo-agent gateway [-c CONFIG] [-w WORKSPACE] [--host HOST] [--port PORT]

# 后台服务生命周期
echo-agent gateway install|uninstall|start|stop|restart|status|logs
                   [-c CONFIG] [-w WORKSPACE] [--system] [--force] [-f|--follow]
```

- 省略 action 时只前台启动 Gateway，不会单独构造另一套 Agent。
- `--host`、`--port` 只覆盖本次前台运行。
- `--system` 在 Linux 上管理系统级而非用户级服务。
- `--force` 允许重新生成已安装的服务文件。
- `-f/--follow` 用于持续跟随日志。

## cli

```bash
echo-agent cli [--port PORT] [--token TOKEN] [--user USER]
               [-c CONFIG] [-w WORKSPACE] [--inline | --tui]
```

- 默认使用保留原生终端 scrollback 的 `--inline` 界面。
- `--tui` 使用全屏 Textual 界面，需要安装 `echo-agent[tui]`。
- 客户端固定连接 loopback；远程使用应先建立 SSH 端口转发。
- 默认会话键为 `cli:local`，`--user` 会把它改为 `cli:<USER>`。
- 配置动态端口 `gateway.port: 0` 时，客户端会读取 workspace 中的运行时端点文件。
- 断线后可用 `/reconnect`；客户端会恢复权威 turn 状态，并补显示断线期间完成的回复。

交互命令、快捷键与审批操作见 [终端交互命令](tui-commands.md)。

## dashboard

```bash
echo-agent dashboard build [--force]
```

构建完整 SPA；已有产物有效时会复用，`--force` 强制重建。当前没有 `--output` 或
`--dev` 参数。

## cron

```bash
echo-agent cron list [-c CONFIG] [-w WORKSPACE]
echo-agent cron authorize JOB_ID [-y] [-c CONFIG] [-w WORKSPACE]
echo-agent cron revoke JOB_ID [-y] [-c CONFIG] [-w WORKSPACE]
```

`authorize` / `revoke` 直接修改 scheduler 持久化文件，要求常驻实例停止，否则内存中的
旧状态可能覆盖离线修改。服务运行期间请改用 Dashboard 或对话命令。`-y/--yes` 跳过确认。

## eval

```bash
echo-agent eval [-d DATASET] [-t TAG] [-p PARALLEL] [-o OUTPUT]
                [-c CONFIG] [-w WORKSPACE]
```

数据集通过 `-d/--dataset` 指定，不是位置参数；并发默认 `3`。

## plugin

```bash
echo-agent plugin list|check [--json] [-c CONFIG] [-w WORKSPACE]
echo-agent plugin info|enable|disable NAME [--json] [-c CONFIG] [-w WORKSPACE]
```

## evolution

```bash
echo-agent evolution status|run|list-candidates|show-candidate|promote|rollback|init-dataset
                     [TARGET] [--status STATUS] [-c CONFIG] [-w WORKSPACE]
```

`TARGET` 在 `show-candidate` / `promote` 中是 candidate id，在 `rollback` 中是技能名。

## skill

```bash
echo-agent skill list-staged [-c CONFIG] [-w WORKSPACE]
echo-agent skill approve CANDIDATE_ID [-c CONFIG] [-w WORKSPACE]
echo-agent skill reject CANDIDATE_ID [--reason TEXT] [-c CONFIG] [-w WORKSPACE]
```

## config

```bash
echo-agent config dump [--format yaml|json] [-c CONFIG] [-w WORKSPACE]
echo-agent config explain DOTTED_KEY [-c CONFIG] [-w WORKSPACE]
echo-agent config validate [-c CONFIG] [-w WORKSPACE]
echo-agent config gen-docs [-c CONFIG] [-w WORKSPACE]
```

`dump` 默认 YAML。当前没有 `--show-source` 参数。

## checkpoint

```bash
echo-agent checkpoint list [--json] [-c CONFIG] [-w WORKSPACE]
echo-agent checkpoint show SHA [--json] [-c CONFIG] [-w WORKSPACE]
echo-agent checkpoint restore SHA [-y] [--json] [-c CONFIG] [-w WORKSPACE]
echo-agent checkpoint prune [--json] [-c CONFIG] [-w WORKSPACE]
```

恢复操作会改写工作区文件；省略 `-y/--yes` 时需要确认。当前没有 `--older-than` 参数。

## migrate

```bash
echo-agent migrate run|rollback|status|memory-md
                   [--dry-run] [--adopt-empty] [-y]
                   [-c CONFIG] [-w WORKSPACE]
```

`--adopt-empty` 仅影响 `run`：把空 scope 的 USER 记忆收编到 owner key。

## deps

`deps` 的剩余参数会原样交给依赖管理器：

```bash
echo-agent deps status [--json] [-w WORKSPACE]
echo-agent deps install FEATURE [-y] [--json] [-w WORKSPACE]
echo-agent deps refresh [-w WORKSPACE]
```

具体 feature 与状态以 `echo-agent deps --help` 为准。

## service（已弃用）

`service install|uninstall|start|stop|restart|status|logs` 只为旧脚本保留，并映射到旧的
Linux 系统级服务语义。新代码应使用 `echo-agent gateway <action>`；实际移除版本由命令行
警告中的 `SERVICE_ALIAS_REMOVAL_VERSION` 决定，不在文档中硬编码。

## 退出码

命令成功返回 `0`，参数解析错误通常返回 `2`，其余失败由具体子命令返回非零值。
不要依赖旧文档中未由实现统一保证的 `3/4/5/130` 固定映射；自动化脚本应同时读取
stderr 或使用支持的 `--json` 输出。
