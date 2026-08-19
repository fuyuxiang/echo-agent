# 模型接入概述

Echo Agent 支持多种大模型供应商接入，并提供统一的路由、容错与密钥轮换机制。

---

## 章节目录

| 章节 | 说明 |
|------|------|
| [Provider 总览](providers.md) | 支持的供应商类型与配置方式 |
| [本地模型](local-models.md) | Ollama、LM Studio、vLLM 等本地部署方案 |
| [路由与 Fallback](routing-fallback.md) | 智能路由策略、健康检测与自动降级 |

---

## 架构概览

模型接入层位于 Echo Agent 核心与外部 LLM API 之间，负责请求分发、密钥管理与故障恢复：

```
┌─────────────────────────────────────────────────┐
│                 Echo Agent Core                  │
└────────────────────┬────────────────────────────┘
                     │ 请求
                     ▼
┌─────────────────────────────────────────────────┐
│              Model Router (路由层)               │
│  ┌───────────┐  ┌────────────┐  ┌───────────┐  │
│  │ 健康检测  │  │ 任务路由   │  │ Fallback  │  │
│  └───────────┘  └────────────┘  └───────────┘  │
└────────────────────┬────────────────────────────┘
                     │ 分发
        ┌────────────┼────────────┐
        ▼            ▼            ▼
┌─────────────┐ ┌─────────┐ ┌──────────┐
│  Provider A │ │Provider B│ │Provider C│
│ (密钥池轮换)│ │          │ │          │
└─────────────┘ └─────────┘ └──────────┘
```

---

## 支持的 Provider 类型

Echo Agent 内置 5 种 Provider 类型：

| Provider | 说明 |
|----------|------|
| **OpenAI** | GPT 系列，同时兼容 DeepSeek、Qwen、Kimi、GLM、MiniMax、SiliconFlow 等 OpenAI 兼容端点 |
| **Anthropic** | Claude 系列模型 |
| **Gemini** | Google Gemini 系列 |
| **Bedrock** | AWS Bedrock 托管模型 |
| **OpenRouter** | OpenRouter 统一网关 |

!!! info "OpenAI 兼容端点"
    任何提供 OpenAI 兼容 API 的服务（包括 Ollama、LM Studio、vLLM）均可通过 `openai` 类型接入，只需修改 `api_base` 即可。

---

## 基础配置示例

模型配置位于 `models` 段，核心结构如下：

```yaml
models:
  default_model: gpt-4o
  fallback_model: gpt-4o-mini

  providers:
    - name: openai-main
      type: openai
      api_key: ${OPENAI_API_KEY}
      api_base: https://api.openai.com/v1
      models:
        - gpt-4o
        - gpt-4o-mini

    - name: anthropic
      type: anthropic
      api_key: ${ANTHROPIC_API_KEY}
      models:
        - claude-sonnet-4-20250514

    - name: local-ollama
      type: openai
      api_base: http://localhost:11434/v1
      models:
        - llama3:8b

  routes:
    - model: gpt-4o
      provider: openai-main
      task_types: [chat, agent]
      fallback_models: [claude-sonnet-4-20250514, gpt-4o-mini]

  model_windows:
    gpt-4o: 128000
    claude-sonnet-4-20250514: 200000
```

---

## 密钥池与轮换

当单个 Provider 配置多个 API Key 时，系统自动启用轮换机制：

```yaml
providers:
  - name: openai-pool
    type: openai
    credential_pool:
      - key: sk-key-1
      - key: sk-key-2
      - key: sk-key-3
    models:
      - gpt-4o
```

!!! tip "密钥轮换策略"
    密钥池采用 round-robin 策略轮换。当某个密钥触发限流或错误时，系统自动将其置入冷却期，并切换至下一个可用密钥。

---

## 健康状态机

路由器为每个 Provider 维护健康状态，用于智能分发与自动降级：

| 状态 | 含义 |
|------|------|
| `HEALTHY` | 正常工作，接受所有请求 |
| `DEGRADED` | 部分失败，降低权重 |
| `COOLDOWN` | 连续失败，暂停请求，等待冷却 |
| `HALF_OPEN` | 冷却结束，试探性发送少量请求 |
| `DISABLED` | 手动禁用，不参与路由 |

!!! warning "自动降级"
    当所有主路由均不可用时，系统将自动尝试 `fallback_models` 列表中的备选模型，确保服务可用性。

---

## 下一步

- 查看 [Provider 总览](providers.md) 了解各供应商的详细配置
- 参考 [本地模型](local-models.md) 部署私有化方案
- 阅读 [路由与 Fallback](routing-fallback.md) 配置高可用策略
