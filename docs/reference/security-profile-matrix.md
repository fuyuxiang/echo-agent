# 安全档位矩阵

本页说明 Echo Agent 的工具准入判定：哪些工具会暴露给模型，以及哪些调用需要审批。所有取值均取自 `echo_agent/config/schema.py` 与 `echo_agent/security/tool_policy.py`。

!!! warning "两个 profile 字段作用不同"
    配置中有两个 `profile` 字段，取值互不通用，混用会导致配置不生效：

    - `tools.profile` — 取值 `minimal` / `messaging` / `coding` / `full`，默认 `full`
    - `security.profile` — 取值 `personal_cli` / `daemon` / `public_gateway`，默认 `personal_cli`

    不存在 `standard`、`extended`、`strict` 这些档位。填入未定义的值会在启动时被配置校验拒绝。

## tools.profile：工具白名单

四档为累加关系，后一档包含前一档的全部工具。

| 档位 | 工具数 | 适用场景 |
|------|--------|----------|
| `minimal` | 14 | 只读问答，不写文件、不发媒体 |
| `messaging` | 18 | 在 `minimal` 基础上增加记忆与媒体生成 |
| `coding` | 24 | 在 `messaging` 基础上增加文件写入与编排 |
| `full` | 全部 | 白名单为 `*`，放通所有工具（默认） |

各档位的具体工具清单见[内置工具参考](tools.md)的档位对照表。

`full` 档的白名单是字面量 `*`，因此新增工具会自动在该档可用；其余三档是显式集合，新工具不会自动进入。

## security.profile：运行形态基线

`security.profile` 不改变白名单，而是在白名单之上追加拒绝规则。拒绝可以按工具名，也可以按能力标签。

| 档位 | 含义 | 追加拒绝 |
|------|------|----------|
| `personal_cli` | 本机单人使用（默认） | 无 |
| `daemon` | 长期后台运行 | 4 个工具 + 4 类能力 |
| `public_gateway` | 网关对外暴露 | 11 个工具 + 8 类能力 |

### daemon

拒绝工具：`exec`、`execute_code`、`process`、`skill_install`

拒绝能力：`code.exec`、`process.exec`、`process.manage`、`skill.install`

### public_gateway

拒绝工具为 `HIGH_RISK_TOOLS` 的 6 个（`cronjob`、`exec`、`execute_code`、`process`、`skill_install`、`skill_manage`）再加 5 个写入类工具（`edit_file`、`knowledge_index`、`patch`、`workflow`、`write_file`），共 11 个。

拒绝能力：`code.exec`、`fs.write`、`process.exec`、`process.manage`、`scheduler.write`、`skill.install`、`skill.write`、`workflow.write`

!!! danger "对外暴露网关前务必确认"
    将网关暴露到公网前，除设置 `security.profile: public_gateway` 外，还需确认已启用鉴权、限制监听地址与来源。网关默认监听 `127.0.0.1`，改为对外监听是一项需要显式开启的动作。详见[安全加固](../operations/security-hardening.md)。

## 判定顺序

`is_tool_allowed()` 按固定顺序逐层判定，任一层拒绝即终止：

1. **显式拒绝** — 工具名在 `tools.deny` 中，直接拒绝。这一层优先级最高，无法被任何配置豁免。
2. **白名单** — 若配置了 `tools.allow`，则只有其中的工具通过，档位不再参与判定；否则由 `tools.profile` 的档位白名单或 `tools.also_allow` 决定。
3. **运行形态拒绝** — 按 `security.profile` 追加的工具名与能力规则拒绝。工具名同时出现在 `tools.allow` 或 `tools.also_allow` 中时可豁免本层。
4. **网络策略** — 当 `execution.network_policy` 为 `deny` 时，拒绝 `web_fetch`、`web_search` 以及任何带 `network.outbound` 能力的工具。`network_policy` 默认即为 `deny`。

被拒绝的工具不会报错，而是不出现在模型的工具列表中，同时以 INFO 级别记录一条 `Tool policy skipped N tools` 日志。排查"工具没被调用"时应先看这条日志。

### 配置项对照

| 配置项 | 类型 | 默认值 | 作用 |
|--------|------|--------|------|
| `tools.deny` | list[str] | `[]` | 无条件拒绝的工具名 |
| `tools.allow` | list[str] | `[]` | 非空时作为唯一白名单，覆盖档位 |
| `tools.also_allow` | list[str] | `[]` | 在档位之外追加放通，并可豁免运行形态拒绝 |
| `tools.profile` | 枚举 | `full` | 档位白名单 |
| `security.profile` | 枚举 | `personal_cli` | 运行形态基线 |
| `execution.network_policy` | `allow` / `deny` / `restricted` | `deny` | 出站网络策略 |

## 审批

工具通过准入后，仍可能在执行前需要审批。审批配置位于 `permissions.approval` —— 注意不在 `security` 下，`SecurityConfig` 只有 `profile` 一个字段。

| 配置项 | 类型 | 默认值 | 作用 |
|--------|------|--------|------|
| `mode` | `manual` / `smart` / `off` | `smart` | 审批模式 |
| `default_policy` | `approve` / `deny` / `ask` | `approve` | 未命中具体规则时的默认处置 |
| `require_approval` | list[str] | 见下 | 需要审批的工具名 |
| `auto_approve` | list[str] | `[]` | 自动批准的工具名 |
| `auto_deny` | list[str] | `[]` | 自动拒绝的工具名 |
| `cli_auto_approve` | bool | `true` | CLI 通道是否自动批准 |
| `trusted_channels` | list[str] | `[]` | 免审批的通道 |
| `unattended_policy` | `deny` / `allow_safe` | `deny` | 无人值守场景的处置 |
| `wait_timeout_seconds` | int | `300` | 等待人工响应的超时 |
| `smart_model` | str | `""` | `smart` 模式使用的判定模型 |

`require_approval` 的默认值为 9 项：`cronjob`、`delegate_task`、`dep_install`、`exec`、`execute_code`、`process`、`skill_install`、`skill_manage`、`spawn_task`。

!!! note "审批模式的取值"
    `mode` 的合法值是 `manual`、`smart`、`off`。`auto`、`ask`、`deny` 不是模式取值 —— 其中 `approve`/`deny`/`ask` 属于 `default_policy`。

### 提权

`permissions.elevated` 用于临时放开限制：

| 配置项 | 类型 | 默认值 | 作用 |
|--------|------|--------|------|
| `enabled` | bool | `false` | 是否启用提权 |
| `allow_from` | dict | `{}` | 允许发起提权的来源 |

## 配置示例

以下配置在 `coding` 档基础上放通 `exec`，同时保持后台运行形态的其余限制：

```yaml
tools:
  profile: coding
  also_allow:
    - exec          # 同时豁免 daemon 形态对 exec 的拒绝

security:
  profile: daemon

permissions:
  approval:
    mode: smart
    require_approval:
      - exec
```

若要彻底禁用某个工具，用 `tools.deny` 而非从白名单中移除 —— `deny` 是第一层判定，不会被 `also_allow` 或提权绕过：

```yaml
tools:
  deny:
    - skill_install
```

## 相关页面

- [内置工具参考](tools.md) — 36 个工具的参数与能力标签
- [配置参考](configuration.md) — 由 schema 自动生成的逐项说明
