"""Stub provider — used when no real LLM provider could be initialized.

Logs at ERROR level on first invocation so the issue is visible in logs
and the user sees an explicit error message in the agent response.
"""

from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.models.provider import LLMProvider, LLMResponse


class StubProvider(LLMProvider):
    """Placeholder provider returned when all configured providers fail."""

    def __init__(self, message: str, **kwargs: Any):
        super().__init__()
        self._message = message
        self._notified = False

    @property
    def is_stub(self) -> bool:
        return True

    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        **kw: Any,
    ) -> LLMResponse:
        if not self._notified:
            self._notified = True
            logger.error("StubProvider invoked — no real LLM available: {}", self._message)
        return LLMResponse(content=self._message)

    def get_default_model(self) -> str:
        return "stub"
