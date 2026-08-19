# Model Integration Overview

Echo Agent supports multiple LLM providers with unified routing, failover, and credential rotation mechanisms.

---

## Contents

| Section | Description |
|---------|-------------|
| [Provider Overview](providers.en.md) | Supported provider types and configuration |
| [Local Models](local-models.en.md) | Ollama, LM Studio, vLLM and other local deployments |
| [Routing & Fallback](routing-fallback.en.md) | Smart routing strategies, health tracking, and auto-degradation |

---

## Architecture Overview

The model integration layer sits between the Echo Agent core and external LLM APIs, handling request dispatch, credential management, and failure recovery:

```
┌─────────────────────────────────────────────────┐
│                 Echo Agent Core                  │
└────────────────────┬────────────────────────────┘
                     │ Request
                     ▼
┌─────────────────────────────────────────────────┐
│              Model Router                        │
│  ┌───────────┐  ┌────────────┐  ┌───────────┐  │
│  │  Health   │  │   Task     │  │ Fallback  │  │
│  │ Tracking  │  │  Routing   │  │  Chain    │  │
│  └───────────┘  └────────────┘  └───────────┘  │
└────────────────────┬────────────────────────────┘
                     │ Dispatch
        ┌────────────┼────────────┐
        ▼            ▼            ▼
┌─────────────┐ ┌─────────┐ ┌──────────┐
│  Provider A │ │Provider B│ │Provider C│
│ (Key Pool)  │ │          │ │          │
└─────────────┘ └─────────┘ └──────────┘
```

---

## Supported Provider Types

Echo Agent ships with 5 built-in provider types:

| Provider | Description |
|----------|-------------|
| **OpenAI** | GPT series; also compatible with DeepSeek, Qwen, Kimi, GLM, MiniMax, SiliconFlow endpoints |
| **Anthropic** | Claude series models |
| **Gemini** | Google Gemini series |
| **Bedrock** | AWS Bedrock managed models |
| **OpenRouter** | OpenRouter unified gateway |

!!! info "OpenAI-Compatible Endpoints"
    Any service exposing an OpenAI-compatible API (including Ollama, LM Studio, vLLM) can be configured using the `openai` type — just change the `api_base`.

---

## Basic Configuration

Model configuration lives under the `models` section. Here is the core structure:

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

## Credential Pool & Rotation

When a provider is configured with multiple API keys, the system automatically enables rotation:

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

!!! tip "Rotation Strategy"
    The credential pool uses round-robin rotation. When a key triggers rate limiting or errors, the system automatically places it in a cooldown period and switches to the next available key.

---

## Health State Machine

The router maintains a health state for each provider, enabling smart dispatch and auto-degradation:

| State | Meaning |
|-------|---------|
| `HEALTHY` | Operating normally, accepts all requests |
| `DEGRADED` | Partial failures, weight reduced |
| `COOLDOWN` | Consecutive failures, requests paused pending cooldown |
| `HALF_OPEN` | Cooldown expired, sending probe requests |
| `DISABLED` | Manually disabled, excluded from routing |

!!! warning "Auto-Degradation"
    When all primary routes are unavailable, the system automatically tries models in the `fallback_models` list to maintain service availability.

---

## Next Steps

- See [Provider Overview](providers.en.md) for detailed configuration of each provider
- Check [Local Models](local-models.en.md) for private deployment options
- Read [Routing & Fallback](routing-fallback.en.md) to configure high-availability strategies
