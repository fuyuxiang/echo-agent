# Provider Overview

Echo Agent supports multiple LLM providers through a unified YAML configuration format.

## Provider Comparison

| Provider | Auth Method | Streaming | Credential Pool | Custom Endpoint | Notes |
|----------|-------------|-----------|-----------------|-----------------|-------|
| openai | API Key | ✅ | ✅ | ✅ | Also compatible with third-party OpenAI-protocol services |
| anthropic | API Key | ✅ | ✅ | ❌ | Claude model family |
| gemini | API Key | ✅ | ✅ | ❌ | Google Gemini series |
| bedrock | AWS Credentials | ✅ | ❌ | ❌ | Access via AWS regional endpoints |
| openrouter | API Key | ✅ | ✅ | ✅ | Aggregation router supporting multiple providers |

## Basic Configuration

Configure models and providers in `config.yaml`:

```yaml
models:
  default_model: "gpt-4o"
  fallback_model: "gpt-4o-mini"
  providers:
    - name: "openai"
      api_key: "sk-xxx"
      models: ["gpt-4o", "gpt-4o-mini", "o1-preview"]
```

- `default_model`: The model used by default
- `fallback_model`: Fallback model when the primary is unavailable
- `providers`: List of providers, each containing a name, credentials, and available models

## Provider Configuration Examples

### OpenAI

```yaml
- name: "openai"
  api_key: "sk-xxx"
  api_base: ""  # optional, defaults to official endpoint
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
  api_key: ""  # uses local AWS credentials (env vars or ~/.aws/credentials)
  api_base: "us-east-1"  # AWS region
  models: ["anthropic.claude-sonnet-4-20250514-v1:0"]
```

!!! note "Bedrock Authentication"
    Bedrock does not use an API key. It relies on the AWS credential chain (environment variables `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`, or IAM roles). The `api_base` field specifies the AWS region.

### OpenRouter

```yaml
- name: "openrouter"
  api_key: "sk-or-xxx"
  api_base: "https://openrouter.ai/api/v1"
  models: ["openai/gpt-4o", "anthropic/claude-sonnet-4-20250514"]
```

## OpenAI-Compatible Endpoints

Third-party services that implement the OpenAI API protocol can be configured using the `openai` provider type with a custom `api_base`:

### DeepSeek

```yaml
- name: "deepseek"
  api_key: "sk-xxx"
  api_base: "https://api.deepseek.com/v1"
  models: ["deepseek-chat", "deepseek-coder"]
```

### Qwen (Tongyi Qianwen)

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

### GLM (Zhipu AI)

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

!!! tip "General Rule for OpenAI-Compatible Services"
    Any service implementing the OpenAI Chat Completions API can be integrated by setting `api_base`. The `name` field is a custom identifier for your reference only.

## Credential Pool

When a single API key's rate limit is insufficient, configure a credential pool for multi-key round-robin:

```yaml
- name: "openai"
  api_key: "sk-primary"  # primary key, used as fallback when pool is exhausted
  models: ["gpt-4o", "gpt-4o-mini"]
  credential_pool: ["sk-key1", "sk-key2", "sk-key3"]
```

### Round-Robin Behavior

- Requests rotate across keys in the credential pool using a round-robin strategy
- When a key accumulates **3 consecutive errors**, it enters a cooldown period
- Cooldown duration is **300 seconds** (5 minutes); the key is excluded from rotation during this time
- After cooldown expires, the key is automatically restored to the pool

!!! warning "Credential Pool Considerations"
    - All keys in the pool should belong to the same provider and have identical model access permissions
    - The `api_key` field serves as the ultimate fallback, even if it is not listed in `credential_pool`

!!! question "Maintainer Confirmation Needed"
    Does the error counter reset after cooldown recovery? Is it possible to configure separate credential pools for different models?

## Full Configuration Example

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
