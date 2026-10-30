# Tools, skills, and plugins

Tools, skills, plugins, MCP, and A2A operate at different levels. Choose based on whether you need an action, a method, extension logic, or an integration protocol.

## Comparison

| Mechanism | Purpose | Example | Boundary |
| --- | --- | --- | --- |
| Tool | Parameterized operation with a result | Read a file, run a command, query a service | Visibility, validation, execution permissions |
| Skill | Domain knowledge and workflow | Writing standards, investigation steps | Guides the model; tools perform actions |
| Plugin | Load a set of capabilities | Register tools or event handling | Product-specific packaging and trust |
| MCP | Connect tool services | External database tools | Service authentication and authorization still apply |
| A2A | Receive external Agent tasks | External orchestrator submission | Runtime production support is inbound |

## Tool execution

Models read tool descriptions and propose parameters. Execution validates requests, checks policy, runs the operation, and returns feedback. Errors can guide the next step. A name appearing in a policy table does not establish an implemented or enabled tool.

## Skill use

Skills organize applicability, methods, and supporting resources. Relevant content helps the model plan and use available tools.

Skill text does not grant permissions. Scripts and external services still require dependencies and authorization. Manual installation, staged skills, and evolution candidates have different admission conditions; loading a skill does not always imply a full automated evaluation.

## Plugin trust

Runtime discovers Python plugins through registered entry points and configured directories. They are **trusted in-process code**. Registration and hook declarations are not an OS sandbox.

Desktop manages its own plugins and skills through the embedded engine and host, with source-aware installation and removal. Packages and configuration are not automatically compatible between products.

## MCP versus A2A

MCP lets the current Agent call a tool service. Runtime A2A lets external Agents submit tasks, inspect status, and cancel work. Internal Worker delegation is a separate local mechanism.

There is no outbound A2A delegation entry point in the current Agent runtime. A2A intake does not automatically create a cross-service Agent network.

## Choosing a mechanism

Use a skill to standardize methods over existing tools. Add a tool or MCP connection for a missing operation. Use product-specific plugins to extend behavior. Evaluate Gateway or inbound Runtime A2A for external task submission.

See [Runtime integrations](../integrations/index.md) and [Desktop models and extensions](../desktop/models-extensions.md).
