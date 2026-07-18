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
    allowed_tools: frozenset[str] = field(default_factory=frozenset)
    channel: str = ""
    chat_id: str = ""
    reply_to_id: str = ""


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
    """Abstract base class for all agent tools.

    Subclasses define name, description, parameters schema, required permissions,
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
        """Returns (ready, reason). Override for tools with external dependencies."""
        return True, "ok"

    @abstractmethod
    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        """Execute the tool with given parameters."""

    def validate_params(self, params: dict[str, Any]) -> list[str]:
        """Validate parameters against the JSON schema."""
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
                if "enum" in prop and value not in prop["enum"]:
                    errors.append(f"{key} must be one of {prop['enum']}")
        return errors

    def execution_mode(self, params: dict[str, Any]) -> str:
        """Classify as 'read_only' or 'side_effect' for replay guards."""
        return "side_effect"

    def to_schema(self) -> dict[str, Any]:
        _validate_json_schema(self.parameters)
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        }
