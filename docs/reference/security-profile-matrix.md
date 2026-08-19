# 安全配置矩阵

Echo Agent 通过两个独立的 Profile 维度控制安全策略：安全级别（security.profile）和工具访问级别（tools.profile）。

## 安全 Profile（security.profile）

控制运行时的安全边界与隔离策略。

| 特性 | minimal | standard | extended |
|------|---------|----------|----------|
| 沙箱隔离 | 否 | 是 | 是（强化） |
| 文件系统限制 | 无 | 工作区内 | 白名单路径 |
| 网络访问限制 | 无 | 无 | 白名单域名 |
| Shell 命令过滤 | 无 | 黑名单 | 白名单 |
| 进程隔离 | 无 | 基本 | 完全隔离 |
| 敏感信息检测 | 无 | 警告 | 拦截 |
| 审计日志 | 基本 | 详细 | 完整（含参数） |
| Token/凭据泄露防护 | 无 | 输出扫描 | 输入+输出扫描 |
| 最大文件大小限制 | 无 | 50MB | 10MB |
| 递归深度限制 | 无 | 20 层 | 10 层 |

### minimal

适用场景：本地开发、受信任环境、快速测试。

```yaml
security:
  profile: minimal
```

- 不启用沙箱
- 不限制文件系统和网络访问
- 仅记录基本操作日志
- 最大灵活性，最低安全性

!!! danger "风险提示"
    `minimal` Profile 不提供任何安全隔离。仅建议在完全受控的本地环境中使用。

### standard（默认）

适用场景：日常使用、团队共享实例。

```yaml
security:
  profile: standard
```

- 启用沙箱隔离
- 限制文件访问在工作区目录内
- 使用命令黑名单过滤危险命令
- 扫描输出中的潜在凭据泄露
- 记录详细操作日志

### extended

适用场景：生产环境、面向外部用户、高合规要求。

```yaml
security:
  profile: extended
```

- 强化沙箱（不可逃逸）
- 文件访问仅限白名单路径
- 网络请求仅限白名单域名
- 命令执行使用白名单模式
- 输入和输出双向凭据扫描
- 完整审计日志（含所有参数）

---

## 工具 Profile（tools.profile）

控制 Agent 可以使用哪些工具类别。

| Profile | 可用工具数 | 包含类别 | 典型用途 |
|---------|-----------|----------|----------|
| `minimal` | 11 | MINIMAL_TOOLS | 只读助手、信息查询 |
| `messaging` | 17 | + MESSAGING_TOOLS | 消息机器人、通知服务 |
| `coding` | 24 | + CODING_TOOLS | 开发助手、代码生成 |
| `full` | 30 | + HIGH_RISK_TOOLS | 完全自主 Agent |

---

## 组合矩阵

安全 Profile 和工具 Profile 可独立配置。以下是推荐的组合：

| 组合 | security | tools | 适用场景 | 风险等级 |
|------|----------|-------|----------|----------|
| 安全查询 | extended | minimal | 面向外部的 Q&A Bot | 极低 |
| 安全通知 | extended | messaging | 生产告警机器人 | 低 |
| 标准开发 | standard | coding | 团队开发助手 | 中 |
| 本地全能 | minimal | full | 本地开发完全自主 | 高 |
| 生产全能 | extended | full | 生产环境完全自主 | 中高 |

!!! warning "不推荐的组合"
    `minimal` + `full` 组合提供所有工具但无安全隔离。强烈建议至少使用 `standard` 安全 Profile。

---

## 详细权限对比

### 文件系统权限

| 操作 | minimal | standard | extended |
|------|---------|----------|----------|
| 读取工作区文件 | ✓ | ✓ | ✓ |
| 读取工作区外文件 | ✓ | 否 | 白名单 |
| 写入工作区文件 | ✓ | ✓ | ✓ |
| 写入工作区外文件 | ✓ | 否 | 否 |
| 创建目录 | ✓ | 工作区内 | 白名单 |
| 删除文件 | ✓ | 工作区内 | 需审批 |
| 符号链接 | ✓ | 否 | 否 |
| 修改权限 | ✓ | 否 | 否 |

### 网络权限

| 操作 | minimal | standard | extended |
|------|---------|----------|----------|
| HTTP/HTTPS 请求 | ✓ | ✓ | 白名单域名 |
| 非标准端口 | ✓ | ✓ | 否 |
| 本地网络访问 | ✓ | ✓ | 否 |
| DNS 查询 | ✓ | ✓ | ✓ |
| WebSocket 外连 | ✓ | ✓ | 白名单 |

### 执行权限

| 操作 | minimal | standard | extended |
|------|---------|----------|----------|
| 任意 Shell 命令 | ✓ | 黑名单过滤 | 白名单模式 |
| 安装软件包 | ✓ | 需审批 | 否 |
| 启动后台进程 | ✓ | 需审批 | 否 |
| 修改环境变量 | ✓ | 工作区内 | 否 |
| 访问系统信息 | ✓ | 部分 | 最小集 |

---

## 配置示例

### 团队开发环境

```yaml
security:
  profile: standard
  allowed_paths:
    - /home/dev/projects
    - /tmp/echo-agent
  blocked_commands:
    - "rm -rf /"
    - "dd if="
    - "mkfs"
    - "> /dev/"

tools:
  profile: coding
  approval_mode: ask
  overrides:
    shell:
      approval_mode: ask
      timeout: 60
    filesystem:
      approval_mode: auto
```

### 生产环境 Bot

```yaml
security:
  profile: extended
  allowed_paths:
    - /opt/echo-agent/workspace
  allowed_domains:
    - "api.anthropic.com"
    - "api.openai.com"
    - "slack.com"
  allowed_commands:
    - "curl"
    - "jq"
    - "python3"

tools:
  profile: messaging
  approval_mode: auto
  overrides:
    notify:
      approval_mode: auto
    message:
      approval_mode: auto
```

### 本地开发（最大灵活性）

```yaml
security:
  profile: minimal

tools:
  profile: full
  approval_mode: auto
```

---

## 审批流程

当工具的 `approval_mode` 为 `ask` 时，执行前需要用户确认：

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│ Agent 请求  │────▶│  审批队列    │────▶│ 用户审批/拒绝│
│ 使用工具    │     │  (pending)   │     │             │
└─────────────┘     └──────────────┘     └─────────────┘
                                                │
                          ┌─────────────────────┤
                          ▼                     ▼
                   ┌─────────────┐     ┌─────────────┐
                   │  执行工具   │     │  返回拒绝   │
                   │  返回结果   │     │  Agent 调整 │
                   └─────────────┘     └─────────────┘
```

TUI 中使用 `/approve` 和 `/deny` 命令处理审批。

!!! question "需维护者确认"
    审批超时行为：当用户长时间未响应审批请求时，默认行为是等待还是自动拒绝？超时时长是否可配置？
