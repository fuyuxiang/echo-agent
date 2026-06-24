"""Inference controller — constrains and validates LLM outputs.

Handles: tool call constraints, output format soft-checks, and critical step
confirmation. Validation is advisory only (see validate_response).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any


from echo_agent.models.provider import LLMResponse


@dataclass
class InferenceConstraints:
    allowed_tools: list[str] | None = None
    blocked_tools: list[str] | None = None
    output_format: str | None = None  # "json", "markdown", "text"
    require_tool_call: bool = False
    require_confirmation_for: list[str] = field(default_factory=list)


class InferenceController:
    """Controls and validates LLM inference behavior."""

    def __init__(self):
        self._constraints = InferenceConstraints()

    def set_constraints(self, constraints: InferenceConstraints) -> None:
        self._constraints = constraints

    def filter_tools(self, tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
        if self._constraints.allowed_tools is not None:
            tools = [t for t in tools if t.get("function", {}).get("name") in self._constraints.allowed_tools]
        if self._constraints.blocked_tools:
            tools = [t for t in tools if t.get("function", {}).get("name") not in self._constraints.blocked_tools]
        return tools

    def validate_response(self, response: LLMResponse) -> list[str]:
        """Advisory soft-check: returns a list of issue strings for logging.

        This NEVER blocks or mutates the response — callers (inference_stage)
        only log the issues. Tool allow/block enforcement happens in
        filter_tools(); this is observability, not a gate.
        """
        issues = []

        if self._constraints.require_tool_call and not response.has_tool_calls:
            issues.append("Expected tool call but none received")

        if response.has_tool_calls and self._constraints.allowed_tools is not None:
            for tc in response.tool_calls:
                if tc.name not in self._constraints.allowed_tools:
                    issues.append(f"Tool '{tc.name}' not in allowed list")

        if response.has_tool_calls and self._constraints.blocked_tools:
            for tc in response.tool_calls:
                if tc.name in self._constraints.blocked_tools:
                    issues.append(f"Tool '{tc.name}' is blocked")

        if self._constraints.output_format == "json" and response.content:
            try:
                json.loads(response.content)
            except (json.JSONDecodeError, TypeError):
                issues.append("Expected JSON output but got non-JSON")

        if not response.content and not response.has_tool_calls:
            issues.append("Empty content with no tool calls")

        return issues

    def needs_confirmation(self, tool_name: str) -> bool:
        return tool_name in self._constraints.require_confirmation_for
