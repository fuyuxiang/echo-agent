"""Inference controller — constrains and validates LLM outputs.

Handles: system prompt layering, tool call constraints, output format enforcement,
hallucination checks, self-verification, and critical step confirmation.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any

from loguru import logger

from echo_agent.models.provider import LLMResponse


@dataclass
class InferenceConstraints:
    allowed_tools: list[str] | None = None
    blocked_tools: list[str] | None = None
    output_format: str | None = None  # "json", "markdown", "text"
    max_output_tokens: int = 4096
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

        return issues

    def check_hallucination_markers(self, content: str) -> list[str]:
        """Detect common hallucination patterns in LLM output."""
        markers = []
        patterns = [
            (r"as an ai", "Self-reference as AI"),
            (r"i cannot.*browse", "Claims inability that may be false"),
            (r"as of my.*cutoff", "Knowledge cutoff reference"),
            (r"i don't have access to", "False access claim"),
        ]
        lower = content.lower()
        for pattern, description in patterns:
            if re.search(pattern, lower):
                markers.append(description)
        return markers

    def needs_confirmation(self, tool_name: str) -> bool:
        return tool_name in self._constraints.require_confirmation_for

    def build_verification_prompt(self, original_question: str, answer: str) -> str:
        return (
            "Verify this answer for accuracy. Point out any errors or unsupported claims.\n\n"
            f"Question: {original_question}\n\n"
            f"Answer: {answer}\n\n"
            "Is this answer correct? Reply with CORRECT or list specific issues."
        )

    def layer_system_prompts(self, *layers: str) -> str:
        """Combine multiple system prompt layers with clear separation."""
        parts = [layer for layer in layers if layer.strip()]
        return "\n\n---\n\n".join(parts)
