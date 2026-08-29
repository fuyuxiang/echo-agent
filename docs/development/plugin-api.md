# Plugin API

Echo Agent 的插件系统允许第三方扩展 Agent 的能力，包括添加工具和注册生命周期钩子。`provides.commands` 是为未来扩展保留的清单字段，当前运行时不注册插件命令。

## 插件结构

```
my-echo-plugin/
├── plugin.yaml          # 插件清单（必需）
├── __init__.py          # 入口模块
├── tools.py             # 工具实现（可选）
├── hooks.py             # 钩子实现（可选）
└── pyproject.toml       # Python 包配置
```

## plugin.yaml 清单

```yaml
name: my-awesome-plugin
version: 1.0.0
description: "A plugin that adds weather lookup and notification features"
author: "Your Name"
license: "MIT"

# Echo Agent 版本要求
requires_echo_agent: ">=0.3.0"

# 必需的环境变量（启动时检查）
requires_env:
  - WEATHER_API_KEY

# 插件提供的能力
provides:
  tools:
    - weather_lookup
    - weather_forecast
  hooks:
    - on_agent_start
    - post_tool_call
  commands: []

# 插件类型：integration / extension / theme
kind: integration

# 配置键（在 echo-agent 配置中的命名空间）
config_key: weather

# 依赖的其他插件
depends_on: []

# 需要的权限
permissions:
  - tool.register # 允许注册工具
  - hook.register # 允许注册钩子
  - network       # 声明性网络访问元数据
```

## PluginManifest 字段

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `name` | str | 是 | 插件唯一标识 |
| `version` | str | 否 | 语义化版本（默认 0.0.1） |
| `description` | str | 否 | 插件描述 |
| `author` | str | 否 | 作者 |
| `license` | str | 否 | 许可证 |
| `requires_echo_agent` | str | 否 | 兼容的 Echo Agent 版本范围 |
| `requires_env` | list[str] | 否 | 必需环境变量 |
| `provides.tools` | list[str] | 否 | 提供的工具列表 |
| `provides.hooks` | list[str] | 否 | 注册的钩子列表 |
| `provides.commands` | list[str] | 否 | 保留字段；当前运行时不注册命令 |
| `kind` | str | 否 | 类型：integration / extension / theme |
| `config_key` | str | 否 | 配置命名空间 |
| `depends_on` | list[str] | 否 | 依赖的其他插件 |
| `permissions` | list[str] | 否 | 需要的权限 |

## 生命周期钩子

插件可以注册以下生命周期钩子：

```python
VALID_HOOKS = {
    "on_agent_start",    # Agent 启动
    "on_agent_stop",     # Agent 停止
    "on_session_start",  # 会话开始
    "on_session_end",    # 会话结束
    "pre_tool_call",     # 工具调用前（可取消）
    "post_tool_call",    # 工具调用后（可修改结果）
    "pre_llm_call",      # LLM 调用前
    "post_llm_call",     # LLM 调用后
    "pre_approval",      # 审批前
    "post_approval",     # 审批后
    "on_error",          # 错误发生时
}
```

### 钩子实现示例

```python
"""hooks.py — Plugin lifecycle hooks."""

from echo_agent.plugins.hooks import HookResult


async def on_agent_start(**kwargs) -> HookResult | None:
    """Agent 启动时初始化插件资源。"""
    # 初始化连接池、加载缓存等
    return None


async def pre_tool_call(tool_name: str, params: dict, **kwargs) -> HookResult | None:
    """工具调用前拦截。"""
    # 可以修改参数或取消调用
    if tool_name == "exec" and "rm -rf" in params.get("command", ""):
        return HookResult(cancel=True, cancel_reason="Dangerous command blocked by plugin")
    return None


async def post_tool_call(tool_name: str, result: dict, **kwargs) -> HookResult | None:
    """工具调用后处理。"""
    # 可以修改结果或触发附加操作
    if tool_name == "weather_lookup":
        # 记录天气查询历史
        pass
    return None


async def on_error(error: Exception, **kwargs) -> HookResult | None:
    """错误发生时通知。"""
    # 发送告警、记录日志等
    return None
```

### HookResult

```python
@dataclass
class HookResult:
    modified: Any = None           # 修改后的数据（传递给下一个钩子/调用方）
    stop_propagation: bool = False # 阻止后续钩子执行
    cancel: bool = False           # 取消当前操作
    cancel_reason: str = ""        # 取消原因
```

## 注册方式

### 方式一：Entry Points（推荐，用于发布到 PyPI）

在插件的 `pyproject.toml` 中声明：

```toml
[project.entry-points."echo_agent.plugins"]
my-awesome-plugin = "my_plugin"
```

Echo Agent 启动时自动发现所有注册了 `echo_agent.plugins` entry point 的包。

### 方式二：用户目录安装

将插件目录放到用户配置路径：

```
~/.echo-agent/plugins/my-awesome-plugin/
├── plugin.yaml
└── __init__.py
```

### 方式三：项目目录安装

在项目工作目录中：

```
.echo-agent/plugins/my-awesome-plugin/
├── plugin.yaml
└── __init__.py
```

## 插件入口模块

`__init__.py` 是插件的 Python 入口。运行时只解析 `activate(context)` 和可选的 `deactivate(context)`；工具与钩子必须在 `activate` 中显式注册：

```python
"""my_plugin — Weather integration for Echo Agent."""

from my_plugin.tools import WeatherLookupTool, WeatherForecastTool
from my_plugin.hooks import on_agent_start, pre_tool_call, post_tool_call

async def activate(context):
    """插件激活回调。"""
    context.register_tools([WeatherLookupTool(), WeatherForecastTool()])
    context.register_hook("on_agent_start", on_agent_start)
    context.register_hook("pre_tool_call", pre_tool_call)
    context.register_hook("post_tool_call", post_tool_call)


async def deactivate(context):
    """插件停用回调。"""
    # 关闭插件自己拥有的非工具资源；注册项由 PluginManager 回收。
    ...
```

## 插件状态生命周期

```
discovered → loaded → activated → (running)
                ↓                      ↓
              failed               disabled
```

| 状态 | 说明 |
|------|------|
| `discovered` | 发现了 plugin.yaml |
| `loaded` | 模块导入成功 |
| `activated` | activate() 执行成功，工具/钩子已注册 |
| `failed` | 加载或激活失败 |
| `disabled` | 被用户手动停用 |

## 信任模型与权限声明

Python 插件作为受信任代码在 Echo Agent 进程内运行，当前机制不是 OS 级沙箱：

- **注册权限** — `tool.register` 和 `hook.register` 在插件注册能力时强制检查
- **声明性权限** — `network`、`subprocess` 与 `filesystem.*` 记录插件意图，不会阻止进程内 Python 代码直接访问这些资源
- **工具审批** — 插件注册的工具遵循与内置工具相同的审批流程
- **隔离不受信任代码** — 在独立进程或容器中运行，并通过 MCP 接入

## 开发流程

### 1. 初始化插件项目

```bash
mkdir my-echo-plugin && cd my-echo-plugin
```

### 2. 创建 plugin.yaml

定义插件元数据和能力声明。

### 3. 实现工具/钩子

按需实现 Tool 类和钩子函数。

### 4. 本地测试

```bash
# 将插件目录链接到用户插件路径
ln -s $(pwd) ~/.echo-agent/plugins/my-echo-plugin

# 启动 Echo Agent，观察插件加载日志
echo-agent --log-level DEBUG
```

### 5. 打包发布

```bash
pip install build
python -m build
pip install twine
twine upload dist/*
```

## 检查清单

- [ ] `plugin.yaml` 清单完整且有效
- [ ] `requires_env` 正确声明（缺少时给出清晰错误）
- [ ] `provides` 准确列出所有工具和钩子
- [ ] 钩子函数接受 `**kwargs`（向前兼容新参数）
- [ ] `activate()` / `deactivate()` 正确管理资源
- [ ] 错误不会泄漏到宿主（异常在插件边界捕获）
- [ ] entry point 正确注册（如需 PyPI 分发）
- [ ] 本地安装测试通过

目前没有脚手架命令，插件文件需手工创建。`echo-agent plugin` 支持的动作为 `list`、`info`、`enable`、`disable`、`check`，其中 `check` 可用于校验已写好的插件能否正确加载。
