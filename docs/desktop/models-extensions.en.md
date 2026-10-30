# Models and extensions

**Scope: Desktop.** Models provide reasoning, MCP provides callable tools, Skills provide methods, and experts define roles. Task permissions and service availability still govern actual use.

## Configure a model

1. Open **Settings → Models**.
2. Add a connection with endpoint, interface type, and API key.
3. Fetch or add models and test the connection.
4. Select a model for a task and verify text responses and tool calls.

Custom connections support OpenAI, Anthropic, and compatible interfaces. Compatibility does not guarantee identical image, tool, context-window, or streaming capabilities.

Personal connections require HTTPS by default. Built-in models use the project's remote HTTP service. No personal key does not imply offline execution or unlimited usage. Local usage estimates are not provider bills.

## Choose an extension

| Mechanism | Purpose | Check first |
| --- | --- | --- |
| MCP | External tools and business systems | Connectivity, credentials, authorization |
| Skill | Reusable methods and workflows | Source, scope, required capabilities |
| Plugin | Install a set of capabilities | Source, enabled state, affected features |
| Connector | Selected references or services | Account, data source, access scope |
| Expert | Saved role, instructions, and model | Responsibilities, workspace, knowledge |

Manage capabilities from the experts, skills, and connectors area. A connected MCP server may still have disabled tools. A visible skill may require unavailable tools, commands, or services.

Plugins can be enabled, disabled, or managed by source. Before uninstalling a managed package, the application lists affected plugins. External sources can be managed through their directory. Failures retain error information for retry.

## Diagnose model and tool failures

For connection failures, check the URL, key, and network. If text works but tool calls fail, check model support and parameters. For a single extension failure, check that service, dependencies, and authorization first.

See [Data management](operations.md) for configuration and credentials, and [Tools, skills, and plugins](../concepts/skills-tools-plugins.md) for concepts.
