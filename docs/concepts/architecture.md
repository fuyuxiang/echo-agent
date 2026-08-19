# 架构概览

Echo Agent 采用事件驱动、通道无关的架构设计，围绕统一的 AgentLoop 构建，
使同一套推理与工具执行逻辑能够服务于 Telegram、Discord、Slack、WhatsApp、
微信、CLI、Webhook 等多种接入方式。

## 系统上下文图 (C4 Style)

```mermaid
C4Context
    title Echo Agent - System Context

    Person(user, "用户", "通过各类通道与 Agent 交互")

    System(echo, "Echo Agent", "事件驱动的自进化 AI Agent 平台")

    System_Ext(llm, "LLM Providers", "OpenAI / Anthropic / 本地模型等")
    System_Ext(mcp, "MCP Servers", "Model Context Protocol 工具服务")
    System_Ext(a2a, "A2A Peers", "Agent-to-Agent 协作节点")
    System_Ext(api, "External APIs", "第三方服务与数据源")

    System_Ext(tg, "Telegram")
    System_Ext(dc, "Discord")
    System_Ext(sl, "Slack")
    System_Ext(wa, "WhatsApp")
    System_Ext(wx, "WeChat / 微信")
    System_Ext(cli, "CLI / Webhook")

    Rel(user, tg, "发送消息")
    Rel(user, dc, "发送消息")
    Rel(user, sl, "发送消息")
    Rel(user, wa, "发送消息")
    Rel(user, wx, "发送消息")
    Rel(user, cli, "发送消息")

    Rel(tg, echo, "InboundEvent")
    Rel(dc, echo, "InboundEvent")
    Rel(sl, echo, "InboundEvent")
    Rel(wa, echo, "InboundEvent")
    Rel(wx, echo, "InboundEvent")
    Rel(cli, echo, "InboundEvent")

    Rel(echo, llm, "推理请求")
    Rel(echo, mcp, "工具调用")
    Rel(echo, a2a, "Agent 协作")
    Rel(echo, api, "外部调用")
```

## 消息处理时序图

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

    U->>CH: 发送消息
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

    alt 包含工具调用
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
    CH->>U: 回复消息
```

## 模块关系图

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

    subgraph "其他模块"
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

## 核心架构原则

| 原则 | 说明 |
|------|------|
| Event-driven | 所有 I/O 统一为 InboundEvent / OutboundEvent，通过 EventBus 解耦 |
| Channel-agnostic core | AgentLoop 与通道实现完全解耦，同一套逻辑处理任何来源的消息 |
| Pipeline stages | 处理流水线分为 ContextStage -> InferenceStage -> ResponseStage 三阶段 |
| Security-by-default | ToolPolicy 过滤、ShellGuard 沙箱、ApprovalGate 人机协作审批 |
| Memory-augmented | 4-tier memory (working / episodic / semantic / procedural) 注入每次 context 构建 |
| Self-evolving | 通过 trajectory 捕获与 evolution engine 实现自主技能改进 |
