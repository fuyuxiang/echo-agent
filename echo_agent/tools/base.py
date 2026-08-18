"""Base tool class and execution context for the tool framework."""

from __future__ import annotations

import hashlib
import json
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Mapping


def _validate_json_schema(schema: Any, path: str = "parameters") -> None:
    if not isinstance(schema, dict):
        return

    schema_type = schema.get("type")
    if schema_type == "array":
        if "items" not in schema:
            raise ValueError(f"Invalid schema at '{path}': array schema missing items")
        items_schema = schema["items"]
        if isinstance(items_schema, list):
            for index, entry in enumerate(items_schema):
                _validate_json_schema(entry, f"{path}.items[{index}]")
        else:
            _validate_json_schema(items_schema, f"{path}.items")

    if schema_type == "object":
        properties = schema.get("properties", {})
        if isinstance(properties, dict):
            for name, prop_schema in properties.items():
                _validate_json_schema(prop_schema, f"{path}.properties.{name}")
        additional = schema.get("additionalProperties")
        if isinstance(additional, dict):
            _validate_json_schema(additional, f"{path}.additionalProperties")

    for key in ("anyOf", "oneOf", "allOf"):
        variants = schema.get(key)
        if isinstance(variants, list):
            for index, entry in enumerate(variants):
                _validate_json_schema(entry, f"{path}.{key}[{index}]")


@dataclass(frozen=True)
class ToolExecutionContext:
    execution_id: str = ""
    trace_id: str = ""
    session_key: str = ""
    # 记忆作用域键(owner-aware)。与 session_key 解耦:后者承载锁/历史/投递,
    # 记忆按人归一只用这个。由 AgentLoop 冻结后经 event 传入。
    memory_scope: str = ""
    user_id: str = ""
    agent_id: str = ""
    attempt_index: int = 0
    idempotency_key: str = ""
    is_replay: bool = False
    parent_execution_id: str | None = None
    credentials: dict[str, str] = field(default_factory=dict)
    approved_actions: frozenset[str] = field(default_factory=frozenset)
    # How this call got approved, for tools that issue *persistent* privileges
    # from a one-off consent. "human" only when a person answered an approval
    # prompt for this exact call; "auto" for every policy-based pass
    # (cli_auto_approve, trusted channel, mode=off, allowlist, unattended grant).
    #
    # approved_actions cannot carry this: it holds the same {tool_name} whether a
    # human confirmed or a config auto-approved, so the one fact a tool like
    # cronjob needs — "did anybody actually look at this?" — was unrecoverable
    # downstream. Tools MUST treat the empty default as not-human: a context
    # built by an older caller says nothing about consent.
    approval_source: str = ""
    allowed_tools: frozenset[str] = field(default_factory=frozenset)
    channel: str = ""
    chat_id: str = ""
    reply_to_id: str = ""
    # Trust signals inherited from the InboundEvent that started this turn, so a
    # nested call (a delegate/spawn worker running tools of its own) can be gated
    # on the SAME facts as the turn that dispatched it. The worker executor has no
    # InboundEvent to pass — it used to hand the gate event=None, which read as
    # "not unattended, not authorized" and, combined with channel="", let a
    # worker's exec call auto-approve on a path where the parent's own exec would
    # have needed a human. Carrying them here keeps the parent's context
    # authoritative.
    #
    # Both default False, which is the conservative reading: a context built by an
    # older caller claims no authorization. They are only ever set FROM the typed
    # InboundEvent fields (never from metadata) — see the note on
    # InboundEvent.unattended for why that distinction is load-bearing.
    unattended: bool = False
    cron_authorized: bool = False
    # The inbound event this turn is answering. Tools that publish an
    # OutboundEvent themselves (message / notify / send_file / tts) must stamp it
    # so the delivery layer can tell "this turn already sent something to this
    # target" — otherwise their message and the turn's own final reply are two
    # unrelated finals and both go out. Same key the progress/heartbeat events
    # use (`_inbound_event_id`), so all of one turn's traffic shares one identity.
    inbound_event_id: str = ""


def build_idempotency_key(trace_id: str, tool_name: str, index: int, params: Mapping[str, Any]) -> str:
    payload = json.dumps(params, ensure_ascii=False, sort_keys=True, separators=(",", ":"), default=str)
    digest = hashlib.sha256(f"{trace_id}:{tool_name}:{index}:{payload}".encode()).hexdigest()
    return digest[:24]


@dataclass
class ToolResult:
    success: bool = True
    output: str = ""
    error: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)
    # 结构化错误分类,熔断器据此决定是否计数:
    #   validation — 参数校验失败(LLM 传错参),工具本身健康
    #   business   — 业务性失败(记录不存在、权限不足等),工具本身健康
    #   timeout    — 执行超时
    #   dependency — 下游依赖故障(网络、外部 API)
    #   internal   — 工具内部异常
    # 只有 timeout/dependency/internal 属于基础设施故障,应触发熔断;
    # validation/business 是正常交互的一部分,不能污染全局健康状态。
    error_kind: str = ""

    INFRA_ERROR_KINDS = frozenset({"timeout", "dependency", "internal"})

    @property
    def text(self) -> str:
        return self.output if self.success else f"Error: {self.error}"

    @property
    def is_infra_failure(self) -> bool:
        """True 仅当失败源于基础设施(超时/依赖/内部异常)。未标注 error_kind 的
        失败视为业务失败——宁可少熔断,不可让参数错误熔断掉所有会话的工具。"""
        return not self.success and self.error_kind in self.INFRA_ERROR_KINDS


class Tool(ABC):
    """Subclasses define name, description, parameters schema, required permissions,
    and the execute method.
    """

    name: str = ""
    description: str = ""
    parameters: dict[str, Any] = {}
    timeout_seconds: int = 30
    max_retries: int = 0
    stream_capable: bool = False
    capabilities: tuple[str, ...] = ()
    risk_level: str = "write"

    def is_ready(self) -> bool:
        """Whether this tool is functional (e.g. API keys configured). Default True."""
        return True

    def readiness_detail(self) -> tuple[bool, str]:
        """Override for tools with external dependencies."""
        return True, "ok"

    @abstractmethod
    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        """Execute the tool with given parameters."""

    def validate_params(self, params: dict[str, Any]) -> list[str]:
        errors = []
        required = self.parameters.get("required", [])
        properties = self.parameters.get("properties", {})
        for key in required:
            if key not in params:
                errors.append(f"missing required parameter: {key}")
        for key, value in params.items():
            if key in properties:
                prop = properties[key]
                expected_type = prop.get("type")
                if expected_type == "string" and not isinstance(value, str):
                    errors.append(f"{key} must be a string")
                elif expected_type == "integer" and (
                    not isinstance(value, int) or isinstance(value, bool)
                ):
                    # bool is a subclass of int — exclude it explicitly so a
                    # tool that asks for an integer doesn't silently accept
                    # True/False from the LLM.
                    errors.append(f"{key} must be an integer")
                elif expected_type == "number" and (
                    not isinstance(value, (int, float)) or isinstance(value, bool)
                ):
                    errors.append(f"{key} must be a number")
                elif expected_type == "boolean" and not isinstance(value, bool):
                    errors.append(f"{key} must be a boolean")
                elif expected_type == "array" and not isinstance(value, (list, tuple)):
                    # A str is rejected here even though it is iterable. An LLM
                    # that emits an array parameter as its JSON *text* (the
                    # literal "['a','b']") used to pass validation and then get
                    # iterated character by character downstream — that is how a
                    # clarify ended up rendering "[", "'", "a" … as its choices.
                    # Item types are deliberately NOT checked: clarify documents
                    # (see cli/tui/blocks.py) that it tolerates dict-shaped
                    # options and coerces them at the render boundary.
                    errors.append(f"{key} must be an array")
                if "enum" in prop and value not in prop["enum"]:
                    errors.append(f"{key} must be one of {prop['enum']}")
        return errors

    def execution_mode(self, params: dict[str, Any]) -> str:
        """Classify as 'read_only' or 'side_effect' for replay guards."""
        return "side_effect"

    def description_for_channel(self, channel: str | None) -> str:
        """Description as the model should see it for *channel*.

        Base implementation ignores the channel. Tools whose behaviour actually
        differs per channel (clarify) override this so the schema never
        describes a capability the channel lacks."""
        return self.description

    def parameters_for_channel(self, channel: str | None) -> dict[str, Any]:
        """JSON schema for the parameters as the model should see it for *channel*.

        The model reads the WHOLE function schema, not just its description, so a
        per-parameter description that promises a capability the channel lacks
        misleads it just as much. Base implementation ignores the channel."""
        return self.parameters

    def to_schema(self, channel: str | None = None) -> dict[str, Any]:
        parameters = self.parameters_for_channel(channel)
        _validate_json_schema(parameters)
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description_for_channel(channel),
                "parameters": parameters,
            },
        }
