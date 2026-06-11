"""Worker executor — runs worker agents with tool containment and structured results."""

from __future__ import annotations

import asyncio
import time
from collections.abc import Awaitable, Callable
from typing import Any

from loguru import logger

from echo_agent.agent.multi_agent.models import WorkerProfile, WorkerResult
from echo_agent.models.provider import LLMProvider, ToolCallRequest


ToolExecutorFn = Callable[[str, ToolCallRequest, int], Awaitable[str]]


class WorkerExecutor:
    """Executes a single worker agent to completion with tool calls."""

    def __init__(
        self,
        *,
        provider: LLMProvider,
        model_router: Any | None = None,
        default_model: str = "",
    ):
        self._provider = provider
        self._model_router = model_router
        self._default_model = default_model

    async def run(
        self,
        *,
        task_index: int,
        goal: str,
        profile: WorkerProfile | None = None,
        system_prompt: str = "",
        tool_defs: list[dict[str, Any]],
        tool_executor: ToolExecutorFn,
        max_iterations: int = 12,
        max_tokens: int = 4096,
        temperature: float = 0.4,
        model: str = "",
        timeout_seconds: float = 300.0,
    ) -> WorkerResult:
        """Run a worker agent loop until completion or limits reached."""
        started = time.monotonic()

        effective_model = model or (profile.model if profile else "") or self._default_model
        effective_temp = temperature if not profile else profile.temperature
        effective_max_tokens = max_tokens if not profile else profile.max_tokens
        effective_max_iter = max_iterations if not profile else min(max_iterations, profile.max_iterations)

        instructions = ""
        if profile and profile.instructions:
            instructions = profile.instructions
        if system_prompt:
            instructions = system_prompt

        system = self._build_system_prompt(goal, instructions, profile)
        messages: list[dict[str, Any]] = [
            {"role": "system", "content": system},
            {"role": "user", "content": goal},
        ]

        state = {"iterations": 0, "tool_calls": 0, "last_content": ""}

        try:
            result = await asyncio.wait_for(
                self._run_loop(
                    messages=messages,
                    tool_defs=tool_defs,
                    tool_executor=tool_executor,
                    max_iterations=effective_max_iter,
                    model=effective_model,
                    max_tokens=effective_max_tokens,
                    temperature=effective_temp,
                    state=state,
                ),
                timeout=timeout_seconds,
            )
            return WorkerResult(
                task_index=task_index,
                status="completed" if result["success"] else "failed",
                output=result["output"],
                error=result.get("error", ""),
                iterations=result["iterations"],
                tool_calls=result["tool_calls"],
                duration_seconds=time.monotonic() - started,
                model=effective_model,
            )
        except asyncio.TimeoutError:
            return WorkerResult(
                task_index=task_index,
                status="timeout",
                output=state["last_content"],
                error=f"Worker timed out after {timeout_seconds}s",
                iterations=state["iterations"],
                tool_calls=state["tool_calls"],
                duration_seconds=time.monotonic() - started,
                model=effective_model,
            )
        except Exception as e:
            logger.error("Worker execution failed: {}", e)
            return WorkerResult(
                task_index=task_index,
                status="failed",
                error=str(e),
                iterations=state["iterations"],
                tool_calls=state["tool_calls"],
                duration_seconds=time.monotonic() - started,
                model=effective_model,
            )

    _MAX_MESSAGES = 200

    async def _run_loop(
        self,
        *,
        messages: list[dict[str, Any]],
        tool_defs: list[dict[str, Any]],
        tool_executor: ToolExecutorFn,
        max_iterations: int,
        model: str,
        max_tokens: int,
        temperature: float,
        state: dict[str, Any],
    ) -> dict[str, Any]:
        for iteration in range(max_iterations):
            state["iterations"] = iteration + 1

            if len(messages) > self._MAX_MESSAGES:
                system_msg = messages[0]
                messages[:] = [system_msg] + messages[-(self._MAX_MESSAGES - 1):]

            response = await self._provider.chat_with_retry(
                messages=messages,
                tools=tool_defs if tool_defs else None,
                model=model or None,
                max_tokens=max_tokens,
                temperature=temperature,
            )

            if response.finish_reason == "error":
                return {
                    "success": False,
                    "output": response.content or "",
                    "error": response.content or "LLM error",
                    "iterations": state["iterations"],
                    "tool_calls": state["tool_calls"],
                }

            if response.content:
                state["last_content"] = response.content

            if not response.has_tool_calls:
                return {
                    "success": True,
                    "output": response.content or state["last_content"] or "(no response)",
                    "iterations": state["iterations"],
                    "tool_calls": state["tool_calls"],
                }

            messages.append({
                "role": "assistant",
                "content": response.content,
                "tool_calls": [tc.to_openai_format() for tc in response.tool_calls],
            })

            for idx, tc in enumerate(response.tool_calls):
                state["tool_calls"] += 1
                result_text = await tool_executor(tc.name, tc, idx)
                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "name": tc.name,
                    "content": result_text[:16000],
                })

        return {
            "success": False,
            "output": state["last_content"],
            "error": f"Reached max iterations ({max_iterations})",
            "iterations": state["iterations"],
            "tool_calls": state["tool_calls"],
        }

    @staticmethod
    def _build_system_prompt(goal: str, instructions: str, profile: WorkerProfile | None) -> str:
        parts = ["You are a focused worker agent. Complete the assigned task concisely and accurately."]
        if profile:
            parts.append(f"Role: {profile.name} — {profile.description}")
        if instructions:
            parts.append(f"Instructions: {instructions}")
        parts.append(
            "Rules:\n"
            "- Stay focused on the assigned task\n"
            "- Use available tools to gather information and take action\n"
            "- Report results clearly when done\n"
            "- If you cannot complete the task, explain why"
        )
        return "\n\n".join(parts)
