# Glossary

术语表。

---

## A

**Agent Loop** — Agent 的核心处理循环，从接收事件到产生响应的完整流程。

**Agent Runtime** — Echo Agent 的核心执行环境，管理 Agent 生命周期。

**A2A (Agent-to-Agent)** — A JSON-RPC task protocol. Echo Agent currently exposes an inbound service for external peers; it does not yet provide a production outbound A2A delegation entry point.

**Approval** — 工具执行前的用户确认机制。

## C

**Channel** — 消息接入通道适配器，如 Telegram、Discord 等。

**Capability** — 工具声明的能力标签，用于权限控制。

**Checkpoint** — 文件级别的变更快照，支持回滚。

**Compression** — 当上下文接近模型窗口时的历史消息压缩。

**Consolidation** — 记忆整合过程，合并重复或相关的记忆片段。

## E

**Evolution** — 自进化机制，通过轨迹捕获和反思生成候选改进。

**Evaluation** — 评估框架，用于测试 Agent 行为质量。

## G

**Gateway** — HTTP/WebSocket 服务器，提供 API 和多通道接入。

## K

**Knowledge** — 知识库系统，支持文档上传、向量索引和语义搜索。

## M

**MCP (Model Context Protocol)** — 模型上下文协议，标准化的外部工具接入方式。

**Memory** — 记忆系统，分四层：Working、Episodic、Semantic、Archival。

## O

**Owner/Scope** — 记忆和数据的归属与隔离范围。

## P

**Plugin** — 可加载的 Python 扩展包。

**Profile** — 预设的安全/工具/认知配置档位。

## S

**Session** — 由 channel + user + chat + thread 组成的会话上下文。

**Skill** — 提供领域知识和工作流的能力包（SKILL.md 格式）。

**Spill** — 工具输出超阈值时的溢出存储机制。

## T

**Task** — 任务管理单元，支持 Kanban 状态流转。

**Tool** — Agent 可调用的可执行能力接口。

## W

**Workflow** — 多步骤任务编排。

**Workspace** — Echo Agent 的工作目录，包含配置和运行时状态。
