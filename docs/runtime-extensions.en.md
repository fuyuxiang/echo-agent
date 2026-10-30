# Runtime session extensions

**Scope: standalone Python Runtime; extension developers.** This interface gives a trusted plugin ownership of session policy and native message handling. Ordinary tool plugins continue to use tool registration and observation hooks.

## Registration and ownership

Implement `RuntimeExtension` and register it through `PluginContext.register_runtime_extension`. Only one extension can own session policy. Duplicate ownership fails. Registration requires `hook.register` in both strict and compatibility modes.

Declare the extension in `plugins.required`. A missing, disabled, unauthorized, or failed required plugin prevents startup and triggers cleanup of activated plugins. Install through the existing plugin entry-point or directory mechanisms.

## Lifecycle and policy

Configuration validation and recovery run before message handling. Event, before-turn, and presentation methods coordinate input and response behavior. Session scope, memory namespace, streaming, and system instructions are owned by the extension.

The allowed-tool patterns define its tool catalog. Operator denials, security profiles, and execution network policy remain effective. Extension state is private runtime state rather than serialized configuration or model context.

Failures propagate to the host. Binding failure prevents startup. Tool-catalog failure remains enforced during model catalog selection and before tool execution until a trusted refresh succeeds. These controls must not use observation hooks' fail-open behavior.

## MCP and delivery

The MCP integration calls the extension's binding, connection-configuration, and adapter-building interfaces. A private operation can be hidden. Adapters must preserve verified registered names. The complete validated catalog is replaced atomically, without leaving a partial catalog after failure.

Delivery recording receives actual final channel receipts for ordinary and final streaming sends. Operator recovery uses the persisted original input and records the recovery receipt. Clarification must retain the actual question and options rather than replace them with a status summary.

Plugin routes supply their own authentication. The generic host does not interpret application-specific endpoint protocols.

## Trust boundary

An extension is trusted in-process Python code. Registration permissions are not OS isolation. Business services remain responsible for application authorization and transactions. See [Plugin API](development/plugin-api.md) and [Developing plugins](integrations/plugins/developing-plugins.md).
