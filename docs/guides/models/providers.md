# Provider 总览

Echo Agent 支持多种 LLM Provider，通过统一的 YAML 配置格式接入不同模型服务。

## Provider 对比

| Provider | 认证方式 | 流式输出 | 凭证池 | 自定义端点 | 备注 |
|----------|----------|----------|--------|------------|------|
| openai | API Key | ✅ | ✅ | ✅ | 同时兼容 OpenAI 协议的第三方服务 |
| anthropic | API Key | ✅ | ✅ | ❌ | Claude 系列模型 |
| gemini | API Key | ✅ | ✅ | ❌ | Google Gemini 系列 |
| bedrock | AWS 凭证 | ✅ | ❌ | ❌ | 通过 AWS 区域端点访问 |
| openrouter | API Key | ✅ | ✅ | ✅ | 聚合路由，支持多家模型 |

## 基础配置

在 `config.yaml` 中配置模型与 Provider：

```yaml
models:
  default_model: "gpt-4o"
  fallback_model: "gpt-4o-mini"
  providers:
    - name: "openai"
      api_key: "sk-xxx"
      models: ["gpt-4o", "gpt-4o-mini", "o1-preview"]
```

- `default_model`：默认使用的模型
- `fallback_model`：主模型不可用时的回退模型
- `providers`：Provider 列表，每个 Provider 包含名称、密钥和可用模型

## 各 Provider 配置示例

### OpenAI

```yaml
- name: "openai"
  api_key: "sk-xxx"
  api_base: ""  # 可选，留空使用官方端点
  models: ["gpt-4o", "gpt-4o-mini", "o1-preview"]
```

### Anthropic

```yaml
- name: "anthropic"
  api_key: "sk-ant-xxx"
  models: ["claude-sonnet-4-20250514", "claude-haiku-4-20250414"]
```

### Gemini

```yaml
- name: "gemini"
  api_key: "AIza..."
  models: ["gemini-2.0-flash", "gemini-2.5-pro"]
```

### Bedrock

```yaml
- name: "bedrock"
  api_key: ""  # 使用本地 AWS 凭证（环境变量或 ~/.aws/credentials）
  api_base: "us-east-1"  # AWS 区域
  models: ["anthropic.claude-sonnet-4-20250514-v1:0"]
```

!!! note "Bedrock 认证"
    Bedrock 不使用 API Key，而是依赖 AWS 凭证链（环境变量 `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`，或 IAM 角色）。`api_base` 字段用于指定 AWS 区域。

### OpenRouter

```yaml
- name: "openrouter"
  api_key: "sk-or-xxx"
  api_base: "https://openrouter.ai/api/v1"
  models: ["openai/gpt-4o", "anthropic/claude-sonnet-4-20250514"]
```

## OpenAI 兼容端点

对于兼容 OpenAI API 协议的第三方服务，使用 `openai` 类型并设置自定义 `api_base`：

### DeepSeek

```yaml
- name: "deepseek"
  api_key: "sk-xxx"
  api_base: "https://api.deepseek.com/v1"
  models: ["deepseek-chat", "deepseek-coder"]
```

### 通义千问 (Qwen)

```yaml
- name: "qwen"
  api_key: "sk-xxx"
  api_base: "https://dashscope.aliyuncs.com/compatible-mode/v1"
  models: ["qwen-turbo", "qwen-plus", "qwen-max"]
```

### Kimi (Moonshot)

```yaml
- name: "kimi"
  api_key: "sk-xxx"
  api_base: "https://api.moonshot.cn/v1"
  models: ["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"]
```

### 智谱 GLM

```yaml
- name: "glm"
  api_key: "xxx.xxx"
  api_base: "https://open.bigmodel.cn/api/paas/v4"
  models: ["glm-4", "glm-4-flash"]
```

### MiniMax

```yaml
- name: "minimax"
  api_key: "xxx"
  api_base: "https://api.minimax.chat/v1"
  models: ["abab6.5-chat", "abab5.5-chat"]
```

### SiliconFlow

```yaml
- name: "siliconflow"
  api_key: "sk-xxx"
  api_base: "https://api.siliconflow.cn/v1"
  models: ["deepseek-ai/DeepSeek-V2.5", "Qwen/Qwen2.5-72B-Instruct"]
```

!!! tip "OpenAI 兼容服务通用规则"
    所有兼容 OpenAI Chat Completions API 的服务均可通过设置 `api_base` 接入。`name` 字段可自定义命名，仅用于标识。

## 凭证池（Credential Pool）

当单个 API Key 的速率限制不足时，可配置凭证池实现多 Key 轮询：

```yaml
- name: "openai"
  api_key: "sk-primary"  # 主密钥，凭证池不可用时回退
  models: ["gpt-4o", "gpt-4o-mini"]
  credential_pool: ["sk-key1", "sk-key2", "sk-key3"]
```

### 轮询机制

- 请求按 Round-Robin 策略在凭证池中的 Key 之间轮转
- 当某个 Key 连续产生 **3 次错误**后，进入冷却期
- 冷却时间为 **300 秒**（5 分钟），冷却期间该 Key 不参与轮询
- 冷却结束后 Key 自动恢复可用

!!! warning "凭证池注意事项"
    - 凭证池中的所有 Key 应属于同一 Provider 且具有相同的模型访问权限
    - `api_key` 字段的主密钥作为最终回退，即使不在 `credential_pool` 列表中也会被使用

!!! question "需维护者确认"
    凭证池的错误计数是否在冷却恢复后重置？是否支持为不同模型配置不同的凭证池？

## 完整配置示例

```yaml
models:
  default_model: "gpt-4o"
  fallback_model: "gpt-4o-mini"
  providers:
    - name: "openai"
      api_key: "sk-xxx"
      models: ["gpt-4o", "gpt-4o-mini", "o1-preview"]
      credential_pool: ["sk-key1", "sk-key2", "sk-key3"]
    - name: "anthropic"
      api_key: "sk-ant-xxx"
      models: ["claude-sonnet-4-20250514", "claude-haiku-4-20250414"]
    - name: "gemini"
      api_key: "AIza..."
      models: ["gemini-2.0-flash", "gemini-2.5-pro"]
    - name: "bedrock"
      api_key: ""
      api_base: "us-east-1"
      models: ["anthropic.claude-sonnet-4-20250514-v1:0"]
    - name: "openrouter"
      api_key: "sk-or-xxx"
      api_base: "https://openrouter.ai/api/v1"
      models: ["openai/gpt-4o", "anthropic/claude-sonnet-4-20250514"]
    - name: "deepseek"
      api_key: "sk-xxx"
      api_base: "https://api.deepseek.com/v1"
      models: ["deepseek-chat", "deepseek-coder"]
```
