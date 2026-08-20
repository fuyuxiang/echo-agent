# 环境变量参考

Echo Agent 的环境变量分两类：一类是**配置覆盖变量**，由 `ECHO_AGENT_` 前缀按规则映射到配置项；另一类是少数**独立运行时变量**，由代码直接读取。本页机制与取值均以 `echo_agent/config/loader.py` 为准。

## 配置覆盖变量

### 命名规则

任何配置项都可以用环境变量覆盖，无需在代码中逐个声明。规则由 `_env_overrides()` 定义：

1. 以 `ECHO_AGENT_` 开头；
2. 去掉前缀后转为小写；
3. 用双下划线 `__` 分隔配置层级。

因此 `gateway.port` 对应 `ECHO_AGENT_GATEWAY__PORT`，`permissions.approval.mode` 对应 `ECHO_AGENT_PERMISSIONS__APPROVAL__MODE`。

```bash
export ECHO_AGENT_GATEWAY__PORT=9000
export ECHO_AGENT_TOOLS__PROFILE=coding
export ECHO_AGENT_SECURITY__PROFILE=daemon
```

!!! important "层级分隔必须是双下划线"
    单下划线是字段名的一部分，不是层级分隔符。`gateway.api_prefix` 对应 `ECHO_AGENT_GATEWAY__API_PREFIX` —— `API_PREFIX` 里的单下划线属于字段名本身。写成 `ECHO_AGENT_GATEWAY_PORT`（单下划线）会被解析为顶层字段 `gateway_port`，该字段不存在，因此设置被静默忽略。

### 可用变量取决于 schema

由于映射是机械推导的，可用变量就是配置树上的全部字段，本页不再逐一罗列 —— 请查[配置参考](configuration.md)，把其中的配置路径按上述规则转写即可。顶层配置节共 34 个：

`a2a`、`agent`、`bus`、`channels`、`checkpoint`、`circuit_breaker`、`compression`、`cost`、`credentials`、`evaluation`、`evolution`、`execution`、`gateway`、`knowledge`、`media_understanding`、`memory`、`models`、`multi_agent`、`observability`、`permissions`、`planning`、`plugins`、`rate_limit`、`runtime`、`scheduler`、`security`、`session`、`skills`、`spill`、`storage`、`tools`、`ui`、`validation`、`workspace`

常用示例：

| 配置项 | 环境变量 | 说明 |
|--------|----------|------|
| `gateway.host` | `ECHO_AGENT_GATEWAY__HOST` | 网关监听地址，默认 `127.0.0.1` |
| `gateway.port` | `ECHO_AGENT_GATEWAY__PORT` | 网关端口，默认 `58123` |
| `gateway.api_prefix` | `ECHO_AGENT_GATEWAY__API_PREFIX` | API 路径前缀，默认 `/api/v1` |
| `tools.profile` | `ECHO_AGENT_TOOLS__PROFILE` | 工具档位，默认 `full` |
| `security.profile` | `ECHO_AGENT_SECURITY__PROFILE` | 运行形态，默认 `personal_cli` |
| `permissions.approval.mode` | `ECHO_AGENT_PERMISSIONS__APPROVAL__MODE` | 审批模式，默认 `smart` |
| `execution.network_policy` | `ECHO_AGENT_EXECUTION__NETWORK_POLICY` | 出站网络策略，默认 `deny` |
| `models.default_model` | `ECHO_AGENT_MODELS__DEFAULT_MODEL` | 默认模型 |

### 类型转换

`_env_overrides()` 收集到的值一律是字符串，类型转换交由 pydantic 在校验阶段完成。因此布尔值写 `true` / `false`，整数直接写数字即可：

```bash
export ECHO_AGENT_GATEWAY__PORT=9000              # 转为 int
export ECHO_AGENT_PERMISSIONS__ELEVATED__ENABLED=true  # 转为 bool
```

无法转换的值会在启动时以配置校验错误的形式报出，不会被静默忽略。

### 列表与嵌套结构

列表类型的配置项无法通过环境变量可靠表达。`tools.deny`、`permissions.approval.require_approval` 这类字段请写在 YAML 配置文件里。

## 配置加载优先级

`load_config()` 依次合并四个来源，后者覆盖前者：

1. 包内默认配置 `echo_agent/config/default.yaml`
2. 用户配置文件（见下）
3. `ECHO_AGENT_` 前缀的环境变量
4. 调用方传入的显式 overrides

合并是深合并：只覆盖同名叶子字段，同级的其他字段保留。

用户配置文件按以下文件名在搜索目录中依次查找：`echo-agent.yaml`、`echo-agent.yml`、`config.yaml`、`config.yml`。

## 独立运行时变量

以下变量不走配置树，由代码直接读取：

| 变量 | 作用 |
|------|------|
| `ECHO_AGENT_CREDENTIAL_KEY` | 凭据加密密钥。变量名本身可通过 `credentials.encryption_key_env` 改写，此处为其默认值 |
| `ECHO_AGENT_DISABLE_LAZY_INSTALLS` | 禁用运行时按需安装依赖 |
| `ECHO_AGENT_SETUP_HANDLES_SERVICE` | 由安装向导设置，标记服务注册已由向导接管 |

## 凭据变量

模型供应商的 API Key 既可直接写在配置里，也可从环境变量自动发现（推荐后者）。发现规则定义在 `echo_agent/models/providers/__init__.py` 的 `_API_KEY_ENV`：

| 供应商 | 环境变量 |
|--------|----------|
| `openai` | `OPENAI_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |
| `gemini` / `google` | `GOOGLE_API_KEY` 或 `GEMINI_API_KEY` |
| `openrouter` | `OPENROUTER_API_KEY` |

`bedrock` / `aws` 不使用上述机制，而是走 AWS SDK 的标准凭据链，可用 `AWS_ACCESS_KEY_ID`、`AWS_REGION`、`AWS_PROFILE`、`AWS_WEB_IDENTITY_TOKEN_FILE` 等标准变量。

其他工具类凭据：`FAL_KEY` 用于 FAL.ai 图像生成。

!!! warning "配置文件不支持 ${VAR} 替换"
    不要在配置文件中写 `api_key: "${ANTHROPIC_API_KEY}"` —— 配置加载器不做变量替换，这串字符会被原样当作 API Key。要么依赖上表的自动发现，要么直接写入受权限保护的配置文件（`chmod 600`）。

## 使用示例

### Docker Compose

```yaml
services:
  echo-agent:
    image: echo-agent:latest
    environment:
      ECHO_AGENT_GATEWAY__HOST: 0.0.0.0     # 容器内需监听所有网卡
      ECHO_AGENT_GATEWAY__PORT: 58123
      ECHO_AGENT_SECURITY__PROFILE: public_gateway
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY}
    ports:
      - "58123:58123"
```

将 `gateway.host` 改为 `0.0.0.0` 意味着对外暴露，务必同时启用鉴权并收紧来源，详见[安全加固](../operations/security-hardening.md)。

### systemd

```ini
[Service]
Environment=ECHO_AGENT_SECURITY__PROFILE=daemon
Environment=ECHO_AGENT_GATEWAY__PORT=58123
EnvironmentFile=/etc/echo-agent/credentials.env
```

### 临时覆盖

```bash
ECHO_AGENT_TOOLS__PROFILE=minimal echo-agent run
```

## 相关页面

- [配置参考](configuration.md) — 由 schema 自动生成的逐项说明
- [安全档位矩阵](security-profile-matrix.md) — 档位与运行形态