# Architecture Overview

Echo Agent uses an event-driven, channel-agnostic architecture built around a unified
AgentLoop. The same inference and tool execution logic serves Telegram, Discord, Slack,
WhatsApp, WeChat, CLI, Webhook, and other channel integrations.

## System Context Diagram (C4 Style)

```mermaid
C4Context
    title Echo Agent - System Context

    Person(user, "User", "Interacts with Agent via various channels")

    System(echo, "Echo Agent", "Event-driven self-evolving AI Agent platform")

    System_Ext(llm, "LLM Providers", "OpenAI / Anthropic / Local models")
    System_Ext(mcp, "MCP Servers", "Model Context Protocol tool services")
    System_Ext(a2a, "A2A Peers", "Agent-to-Agent collaboration nodes")
    System_Ext(api, "External APIs", "Third-party services and data sources")

    System_Ext(tg, "Telegram")
    System_Ext(dc, "Discord")
    System_Ext(sl, "Slack")
    System_Ext(wa, "WhatsApp")
    System_Ext(wx, "WeChat")
    System_Ext(cli, "CLI / Webhook")

    Rel(user, tg, "sends message")
    Rel(user, dc, "sends message")
    Rel(user, sl, "sends message")
    Rel(user, wa, "sends message")
    Rel(user, wx, "sends message")
    Rel(user, cli, "sends message")

    Rel(tg, echo, "InboundEvent")
    Rel(dc, echo, "InboundEvent")
    Rel(sl, echo, "InboundEvent")
    Rel(wa, echo, "InboundEvent")
    Rel(wx, echo, "InboundEvent")
    Rel(cli, echo, "InboundEvent")

    Rel(echo, llm, "inference request")
    Rel(echo, mcp, "tool invocation")
    Rel(echo, a2a, "agent collaboration")
    Rel(echo, api, "external call")
```

## Message Sequence Diagram

```mermaid
sequenceDiagram
    participant U as User
    participant CH as Channel
    participant EB as EventBus
    participant AL as AgentLoop
    participant SM as SessionManager
    participant CB as ContextBuilder
    participant MS as MemoryService
    participant IC as InferenceController
    participant LLM as LLM Provider
    participant AG as ApprovalGate
    participant TR as ToolRegistry
    participant RS as ResponseStage

    U->>CH: send message
    CH->>EB: emit InboundEvent
    EB->>AL: dispatch to AgentLoop
    AL->>SM: getOrCreate session
    SM-->>AL: Session
    AL->>CB: buildContext(session, event)
    CB->>MS: retrieve memories (4-tier)
    MS-->>CB: relevant memories
    CB-->>AL: enriched context
    AL->>IC: infer(context)
    IC->>LLM: completion request
    LLM-->>IC: response (text / tool_calls)
    IC-->>AL: InferenceResult

    alt contains tool calls
        AL->>AG: checkApproval(tool_calls)
        AG-->>AL: approved / denied
        AL->>TR: execute(approved_calls)
        TR-->>AL: tool results
        AL->>IC: re-infer with results
        IC->>LLM: follow-up request
        LLM-->>IC: final response
    end

    AL->>RS: formatResponse
    RS->>EB: emit OutboundEvent
    EB->>CH: deliver to channel
    CH->>U: reply message
```

## Module Relationship Diagram

```mermaid
graph TB
    subgraph "agent/"
        loop[loop]
        context[context]
        pipeline[pipeline]
        tools[tools]
        multi_agent[multi_agent]
        approval[approval_gate]
        compression[compression]
        consolidation[consolidation]
    end

    subgraph "bus/"
        events[events]
        queue[queue]
    end

    subgraph "channels/"
        telegram[telegram]
        discord[discord]
        slack[slack]
        whatsapp[whatsapp]
        weixin[weixin]
        webhook[webhook]
        cli_ch[cli]
        cron[cron]
    end

    subgraph "session/"
        manager[manager]
    end

    subgraph "memory/"
        mem_store[store]
        mem_service[service]
        mem_consolidator[consolidator]
        mem_types[types]
        mem_retrieval[retrieval]
    end
    subgraph "spill/"
        spill_store[store]
        spill_policy[policy]
        spill_preview[preview]
    end

    subgraph "security/"
        guards[guards]
        tool_policy[tool_policy]
        capabilities[capabilities]
        path_policy[path_policy]
        net_guard[net_guard]
    end

    subgraph "evolution/"
        engine[engine]
        evolver[evolver]
        recorder[recorder]
        evo_types[types]
        gate[gate]
        validation[validation]
    end

    subgraph "models/"
        provider[provider]
        inference[inference]
        router[router]
    end

    subgraph "Other Modules"
        config[config/schema]
        skills[skills/]
        tools_pkg[tools/]
        plugins[plugins/]
        mcp_pkg[mcp/]
        a2a_pkg[a2a/]
        gateway[gateway/]
        observability[observability/]
        cost[cost/]
    end

    channels --> events
    events --> loop
    loop --> manager
    loop --> pipeline
    pipeline --> context
    context --> mem_service
    context --> spill_preview
    pipeline --> inference
    inference --> provider
    inference --> router
    pipeline --> approval
    approval --> tool_policy
    approval --> guards
    loop --> tools
    tools --> tools_pkg
    tools --> mcp_pkg
    loop --> multi_agent
    multi_agent --> a2a_pkg
    loop --> compression
    loop --> consolidation
    consolidation --> mem_consolidator
    mem_service --> mem_store
    mem_service --> mem_retrieval
    evolver --> recorder
    evolver --> engine
    engine --> gate
    engine --> validation
```

## Core Architectural Principles

| Principle | Description |
|-----------|-------------|
| Event-driven | All I/O normalized to InboundEvent / OutboundEvent, decoupled via EventBus |
| Channel-agnostic core | AgentLoop is fully decoupled from channel implementations; same logic handles messages from any source |
| Pipeline stages | Processing pipeline divided into ContextStage -> InferenceStage -> ResponseStage |
| Security-by-default | ToolPolicy filtering, ShellGuard sandboxing, ApprovalGate human-in-the-loop review |
| Memory-augmented | 4-tier memory (working / episodic / semantic / procedural) injected into every context build |
| Self-evolving | Trajectory capture and evolution engine enable autonomous skill improvement |
