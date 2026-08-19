# Usage Guide

This section covers all core capabilities of Echo Agent, from model integration to task scheduling, helping you unlock the full potential of the Agent runtime.

---

## Contents

| Section | Description |
|---------|-------------|
| [Model Integration](models/index.en.md) | Multi-model configuration, load balancing, custom endpoints |
| [Tools & Permissions](tools-permissions.en.md) | Built-in tool registration, permission policies, sandbox isolation |
| [Execution Backends](execution-backends.en.md) | Local/remote/containerized execution environments |
| [Browser & Media](browser-media.en.md) | Web interaction, screenshots, file upload & media processing |
| [Memory Management](memory-management.en.md) | Short/long-term memory, vector storage, retrieval strategies |
| [Knowledge Base](knowledge-base.en.md) | Document import, index building, RAG-enhanced retrieval |
| [Sessions](sessions.en.md) | Parallel sessions, context isolation, session persistence |
| [Tasks & Planning](tasks-planning.en.md) | Task decomposition, execution plans, multi-step reasoning |
| [Scheduled Jobs](scheduled-jobs.en.md) | Cron expressions, scheduled triggers, failure retries |
| [Dashboard](dashboard.en.md) | Web dashboard, real-time monitoring, operation auditing |
| [Cost Control](cost-control.en.md) | Token budgets, usage alerts, model fallback strategies |

---

## Overview

Echo Agent combines the reasoning capabilities of large language models with an orchestrable tool system, delivering a complete Agent runtime. The sections here are organized by functional domain:

- **Model Layer**: Configure one or more LLM backends with support for OpenAI, Anthropic, local models, and other providers.
- **Tool Layer**: Declare which tools the Agent can invoke, and control scope and rate through permission policies.
- **Execution Layer**: Choose where code and commands run — local processes, Docker containers, or remote sandboxes.
- **Extended Capabilities**: Browser automation, media processing, and knowledge base retrieval give the Agent richer perception and action.
- **State Management**: The memory system and session management ensure context coherence across multi-turn, multi-session scenarios.
- **Scheduling & Monitoring**: Scheduled jobs, the Dashboard, and cost controls help you run Agents safely and efficiently in production.

!!! tip "Suggested Reading Order"
    If you just finished installation, start with [Model Integration](models/index.en.md), then read [Tools & Permissions](tools-permissions.en.md) to understand the Agent's action boundaries.
