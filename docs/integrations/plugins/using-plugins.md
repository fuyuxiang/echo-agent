# 插件系统使用指南

插件（Plugin）是 Echo Agent 的能力扩展机制，通过插件可以注册新工具并监听生命周期事件。

## 插件能做什么

| 能力 | 说明 |
|------|------|
| 注册工具 | 在 ToolRegistry 中注册新工具，Agent 可直接调用 |
| 监听钩子 | 响应系统生命周期事件（启动、消息到达、回复前等） |
| 命令清单 | `provides.commands` 为保留字段；当前运行时不注册插件命令 |

## 插件发现机制

Echo Agent 从多个来源发现插件，按优先级排列：

1. **pip 入口点** — 通过 `echo_agent.plugins` entry_points 注册的 Python 包
2. **用户目录** — `~/.echo-agent/plugins/` 下的插件目录
3. **项目目录** — 项目根目录下的 `plugins/` 文件夹
4. **额外目录** — 配置中 `extra_dirs` 指定的路径

## 插件生命周期

```
发现 → 过滤 → 拓扑排序 → 加载 → 激活
 │        │        │         │       │
 │        │        │         │       └─ 调用 activate(ctx)
 │        │        │         └─ 导入 Python 模块
 │        │        └─ 按 depends_on 排序
 │        └─ 应用 enable/disable 列表
 └─ 扫描所有来源
```

!!! tip "拓扑排序确保依赖顺序"
    如果插件 A 依赖插件 B（通过 `depends_on` 声明），系统保证 B 在 A 之前加载和激活。循环依赖会导致加载失败并报错。

## 配置

在 `config.yaml` 中管理插件：

```yaml
plugins:
  enabled: true
  extra_dirs: ["./my-plugins"]
  allow: ["my-plugin"]           # 白名单，留空表示放行全部
  deny: ["unwanted-plugin"]      # 黑名单
  trusted_plugins: []            # 跳过 manifest 注册权限预检
  permission_mode: compat        # strict | compat
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `enabled` | bool | 全局开关，设为 false 禁用所有插件 |
| `extra_dirs` | list | 额外的插件搜索目录 |
| `allow` | list | 白名单；非空时只加载列表内的插件，留空表示放行全部 |
| `deny` | list | 黑名单，命中即禁用 |
| `trusted_plugins` | list | 跳过 manifest 注册权限预检的插件；不提供进程隔离 |
| `permission_mode` | str | 注册权限模式：`strict` / `compat` |

!!! warning "deny 优先于 allow"
    两个列表同时命中同一个插件时，**`deny` 生效** —— 过滤先查黑名单，命中即跳过，不再看白名单。

!!! danger "Python 插件是受信任的进程内代码"
    插件与 Echo Agent 在同一 Python 进程中运行。当前权限机制只在注册时强制检查 `tool.register` 和 `hook.register`；`network`、`subprocess` 与 `filesystem.*` 是 manifest 声明元数据，不是 OS 级沙箱。只安装受信任插件；不受信任的代码应放到独立进程或容器中，再通过 MCP 接入。`trusted_plugins` 只跳过注册权限预检，不会增加或取消代码隔离。

## 环境变量检查

插件可在 `plugin.yaml` 中声明所需环境变量：

```yaml
requires_env:
  - OPENAI_API_KEY
  - MY_SERVICE_TOKEN
```

激活前系统会检查这些变量是否存在。若缺失，插件不会被激活，并在日志中输出警告。

## 查看已加载插件

通过 Agent 查询当前插件状态：

```
用户: 列出已加载的插件
Agent: 当前已加载 3 个插件：
  1. my-plugin (v1.2.0) - 提供自定义搜索工具
  2. translation (v0.5.0) - 多语言翻译
  3. db-query (v2.0.1) - 数据库查询工具
```

## 插件提供的工具

插件注册的工具与内置工具使用方式完全一致，Agent 会自动发现并在合适的时候调用它们：

```
用户: 帮我查一下数据库里最近的订单
Agent: [调用 db-query 插件注册的 sql_query 工具]
      最近 5 条订单如下：...
```

## 常见问题

!!! question "`allow` 为空数组时会加载哪些插件？"
    加载全部。`plugins.allow` 为空表示不做白名单限制，所有发现到的插件都会加载；只有列表非空时才按白名单过滤，未列入的插件被标记为 `disabled`。

    `plugins.deny` 的优先级高于 `allow`：同时出现在两个列表中的插件不会加载。要彻底关闭插件系统用 `plugins.enabled: false`，而不是给 `allow` 填一个空列表。

### 插件加载失败怎么办？

检查日志中的错误信息，常见原因：

- 环境变量未设置
- `depends_on` 中声明的依赖插件不存在
- Python 模块导入错误
- `plugin.yaml` 格式不正确

### 多个插件注册同名工具？

注册表会拒绝重名工具和内置工具的保留别名：原工具保持不变，冲突插件激活失败。建议为工具名称添加插件前缀以避免冲突。

## 下一步

- [插件开发指南](developing-plugins.md) — 创建自己的插件
- [MCP 集成](../mcp.md) — 通过 MCP 协议连接外部工具
