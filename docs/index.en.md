# Echo Agent Documentation

**Echo Agent** is a self-hosted, long-running AI Agent runtime with persistent memory, skills evolution, and multi-channel integrations.

---

## Quick Navigation

<div class="grid cards" markdown>

-   :material-rocket-launch:{ .lg .middle } **Getting Started**

    ---

    Install and have your first conversation in 5 minutes

    [:octicons-arrow-right-24: Get started](getting-started/index.en.md)

-   :material-server-network:{ .lg .middle } **Background Deployment**

    ---

    Run as a daemon or systemd service for 24/7 availability

    [:octicons-arrow-right-24: Deployment guide](operations/deployment.md)

-   :material-chat-processing:{ .lg .middle } **Platform Integrations**

    ---

    Connect your Agent to DingTalk, Feishu, WeChat, Slack, Telegram, and 14 channels total

    [:octicons-arrow-right-24: Channel configuration](integrations/channels/index.md)

-   :material-puzzle:{ .lg .middle } **Extend & Develop**

    ---

    Write custom skills, plugins, and channel adapters

    [:octicons-arrow-right-24: Developer guide](development/index.en.md)

</div>

---

## Core Capabilities

| Capability | Description |
|------------|-------------|
| **Multi-model support** | OpenAI, Anthropic, Gemini, Bedrock, OpenRouter, plus OpenAI-compatible endpoints (DeepSeek, Qwen, Kimi, GLM, MiniMax, SiliconFlow, Ollama, LM Studio, vLLM) |
| **14 channel adapters** | CLI, Cron, DingTalk, Discord, Email, Feishu, Matrix, QQ Bot, Slack, Telegram, Webhook, WeCom, WeChat, WhatsApp |
| **Long-term memory** | Vector-based conversational memory with cross-session persistence |
| **Skills evolution** | Agent autonomously distills and accumulates reusable skills over interactions |
| **Plugin system** | Register external plugins via entry-points with hot-reload support |
| **Dashboard** | Built-in web management panel for real-time conversations, cost tracking, and status monitoring |
| **Scheduled tasks** | Built-in cron scheduler for time-triggered Agent execution |

---

## Project Status

!!! warning "Beta Stage"
    Echo Agent is currently at **v0.3.7** in Beta. The core API is stabilizing, but breaking changes may still occur in:

    - Configuration file format (`config.yaml` schema)
    - Plugin / skill API interfaces
    - Database schema (migration provided via `echo-agent migrate`)

    Review the [CHANGELOG](https://github.com/fuyuxiang/echo-agent/blob/master/CHANGELOG.md) and back up your data before upgrading.

---

## System Requirements

- Python 3.11+
- Linux / macOS / Windows (WSL2 recommended)
- At least one model API key (OpenAI, Anthropic, etc.)

---

## License

Echo Agent is open-source under the [MIT License](https://github.com/fuyuxiang/echo-agent/blob/master/LICENSE).
