"""Runtime for executing precisely routed specialist agents."""

from __future__ import annotations

import asyncio
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.multi_agent.audit import DispatchAuditLog
from echo_agent.agent.multi_agent.models import AgentProfile, AgentRunResult, DispatchPlan, DispatchResult
from echo_agent.agent.multi_agent.registry import AgentRegistry
from echo_agent.agent.multi_agent.router import IntentRouter
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.models.provider import LLMProvider
from echo_agent.models.router import ModelRouter


ToolExecutor = Callable[[str, Any, int, list[dict[str, Any]], set[str]], Awaitable[str]]


class MultiAgentRuntime:
    """Plans, runs, and aggregates specialist agents."""

    def __init__(
        self,
        *,
        registry: AgentRegistry,
        router: IntentRouter,
        provider: LLMProvider,
        model_router: ModelRouter | None,
        tools: ToolRegistry,
        audit_path: Path,
        max_iterations: int = 8,
        synthesize_results: bool = True,
    ):
        self.registry = registry
        self.router = router
        self._provider = provider
        self._model_router = model_router
        self._tools = tools
        self._audit = DispatchAuditLog(audit_path)
        self._max_iterations = max_iterations
        self._synthesize_results = synthesize_results

    def plan(self, query: str, *, task_type: str = "chat") -> DispatchPlan:
        return self.router.build_plan(
            query,
            task_type=task_type,
            available_tools=self._tools.tool_names,
            ready_tools=set(self._tools.ready_tool_names),
        )

    async def dispatch(
        self,
        *,
        query: str,
        plan: DispatchPlan,
        base_messages: list[dict[str, Any]],
        retrieval_context: str = "",
        tool_executor: ToolExecutor,
        trace_id: str = "",
    ) -> DispatchResult:
        started = time.monotonic()
        selected = [self.registry.require(agent_id) for agent_id in plan.selected_agent_ids]
        if not selected:
            return DispatchResult(plan=plan, final_output="", success=False, metadata={"reason": "no agents selected"})

        if plan.strategy == "parallel" and len(selected) > 1:
            raw_results = await asyncio.gather(*[
                self._run_agent(
                    profile,
                    query=query,
                    base_messages=base_messages,
                    retrieval_context=retrieval_context,
                    tool_executor=tool_executor,
                    trace_id=trace_id,
                )
                for profile in selected
            ], return_exceptions=True)
            results = []
            for i, r in enumerate(raw_results):
                if isinstance(r, Exception):
                    logger.error("Agent {} failed: {}", selected[i].id, r)
                    results.append(AgentRunResult(
                        agent_id=selected[i].id,
                        output=f"Agent failed: {r}",
                        success=False,
                    ))
                else:
                    results.append(r)
        else:
            results = [
                await self._run_agent(
                    selected[0],
                    query=query,
                    base_messages=base_messages,
                    retrieval_context=retrieval_context,
                    tool_executor=tool_executor,
                    trace_id=trace_id,
                )
            ]

        # Fallback to general agent on config/auth failures
        if len(results) == 1 and not results[0].success and results[0].error_type in ("config", "auth"):
            failed = results[0]
            logger.info(
                "Specialist '{}' failed with {}, falling back to general agent",
                failed.agent_id, failed.error_type,
            )
            fallback = await self._fallback_to_general(
                query=query,
                failed_result=failed,
                base_messages=base_messages,
                retrieval_context=retrieval_context,
                tool_executor=tool_executor,
                trace_id=trace_id,
            )
            results = [fallback]

        final_output = await self._aggregate(query=query, plan=plan, results=results)
        dispatch_result = DispatchResult(
            plan=plan,
            results=results,
            final_output=final_output,
            success=any(r.success for r in results),
            metadata={"duration_ms": int((time.monotonic() - started) * 1000)},
        )
        self._audit.write(self._audit_record(dispatch_result))
        return dispatch_result

    async def _run_agent(
        self,
        profile: AgentProfile,
        *,
        query: str,
        base_messages: list[dict[str, Any]],
        retrieval_context: str,
        tool_executor: ToolExecutor,
        trace_id: str,
    ) -> AgentRunResult:
        from echo_agent.agent.multi_agent.error_types import ToolErrorType, classify_tool_error

        allowed_tools = set(self.registry.filter_tool_names(profile.id, self._tools.ready_tool_names))
        tool_defs = [
            schema for schema in self._tools.get_ready_definitions()
            if schema.get("function", {}).get("name") in allowed_tools
        ]
        messages = self._build_agent_messages(profile, query, base_messages, retrieval_context)
        provider_name, provider, model = self._select_provider(profile, query)
        iterations = 0
        tool_calls = 0
        last_content = ""
        config_error_count = 0
        last_error_text = ""

        for iteration in range(min(profile.max_iterations, self._max_iterations)):
            iterations = iteration + 1
            response = await provider.chat_with_retry(
                messages=messages,
                tools=tool_defs if tool_defs else None,
                model=model,
                max_tokens=profile.max_tokens,
                temperature=profile.temperature,
            )
            if response.finish_reason == "error":
                return AgentRunResult(
                    agent_id=profile.id,
                    success=False,
                    error=response.content or "LLM error",
                    error_type=ToolErrorType.TRANSIENT.value,
                    iterations=iterations,
                    model=model,
                    provider_name=provider_name,
                    tool_calls=tool_calls,
                )
            if response.content:
                last_content = response.content
            if not response.has_tool_calls:
                return AgentRunResult(
                    agent_id=profile.id,
                    success=True,
                    output=response.content or last_content or "(no response)",
                    iterations=iterations,
                    model=model,
                    provider_name=provider_name,
                    tool_calls=tool_calls,
                    metadata={"allowed_tools": sorted(allowed_tools), "trace_id": trace_id},
                )

            messages.append({
                "role": "assistant",
                "content": response.content,
                "tool_calls": [tc.to_openai_format() for tc in response.tool_calls],
            })
            for index, tc in enumerate(response.tool_calls):
                tool_calls += 1
                result_text = await tool_executor(profile.id, tc, index, messages, allowed_tools)
                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "name": tc.name,
                    "content": result_text[:16000],
                })

                # Fast-fail on repeated config/auth errors
                if result_text.startswith("Error:"):
                    error_type = classify_tool_error(result_text)
                    if error_type in (ToolErrorType.CONFIG, ToolErrorType.AUTH):
                        if result_text == last_error_text:
                            config_error_count += 1
                        else:
                            config_error_count = 1
                            last_error_text = result_text
                        if config_error_count >= 2:
                            logger.warning(
                                "Agent '{}' fast-fail: repeated {} error on tool '{}': {}",
                                profile.id, error_type.value, tc.name, result_text[:200],
                            )
                            return AgentRunResult(
                                agent_id=profile.id,
                                success=False,
                                output=last_content,
                                error=result_text,
                                error_type=error_type.value,
                                iterations=iterations,
                                model=model,
                                provider_name=provider_name,
                                tool_calls=tool_calls,
                            )
                    else:
                        config_error_count = 0

        return AgentRunResult(
            agent_id=profile.id,
            success=False,
            output=last_content,
            error=f"Agent '{profile.id}' reached max iterations ({min(profile.max_iterations, self._max_iterations)})",
            iterations=iterations,
            model=model,
            provider_name=provider_name,
            tool_calls=tool_calls,
        )

    async def _fallback_to_general(
        self,
        *,
        query: str,
        failed_result: AgentRunResult,
        base_messages: list[dict[str, Any]],
        retrieval_context: str,
        tool_executor: ToolExecutor,
        trace_id: str,
    ) -> AgentRunResult:
        """Re-run with general agent when a specialist fails due to config/capability issues."""
        general = self.registry.require("general")
        degradation_note = (
            f"Note: The specialist agent '{failed_result.agent_id}' could not complete this task "
            f"because required tools are unavailable ({failed_result.error_type}). "
            f"Please answer using your general knowledge and any available tools. "
            f"If you cannot fully answer, explain what information you would need."
        )
        augmented_retrieval = (
            f"{retrieval_context}\n\n[Degradation Context]\n{degradation_note}"
            if retrieval_context else degradation_note
        )
        return await self._run_agent(
            general,
            query=query,
            base_messages=base_messages,
            retrieval_context=augmented_retrieval,
            tool_executor=tool_executor,
            trace_id=trace_id,
        )

    async def _aggregate(self, *, query: str, plan: DispatchPlan, results: list[AgentRunResult]) -> str:
        from echo_agent.agent.multi_agent.error_messages import sanitize_error_for_user

        if len(results) == 1:
            result = results[0]
            if result.success:
                return result.output
            return sanitize_error_for_user(result.error, result.output)

        sections = []
        all_failed = True
        for result in results:
            label = f"[{result.agent_id}]"
            if result.success:
                all_failed = False
                sections.append(f"{label}\n{result.output}")
            else:
                sections.append(f"{label}\n{sanitize_error_for_user(result.error, result.output)}")
        combined = "\n\n".join(sections)

        if all_failed:
            return sanitize_error_for_user("", "")

        if not self._synthesize_results:
            return combined

        supervisor = self.registry.get("planner") or self.registry.get(plan.primary_agent_id) or self.registry.require("general")
        provider_name, provider, model = self._select_provider(supervisor, query)
        prompt = (
            "You are the supervisor synthesizing multiple specialist agent results. "
            "Produce one concise final answer, resolve conflicts explicitly, and preserve source labels when useful.\n\n"
            f"User request:\n{query}\n\nSpecialist outputs:\n{combined}"
        )
        response = await provider.chat_with_retry(
            messages=[{"role": "user", "content": prompt}],
            model=model,
            max_tokens=supervisor.max_tokens,
            temperature=0.2,
        )
        if response.finish_reason == "error" or not response.content:
            logger.warning("Multi-agent synthesis failed via {}:{}: {}", provider_name, model, response.content)
            return f"Multi-agent results for: {query}\n\n{combined}"
        return response.content

    def _select_provider(self, profile: AgentProfile, query: str) -> tuple[str, LLMProvider, str]:
        if self._model_router:
            for provider_name, provider, decision in self._model_router.route_candidates(
                profile.task_types[0] if profile.task_types else "",
                query,
                preferred_model=profile.model,
            ):
                if profile.provider and provider_name != profile.provider:
                    continue
                model = profile.model or decision.model
                return provider_name, provider, model
        return "default", self._provider, profile.model or self._provider.get_default_model()

    def _build_agent_messages(
        self,
        profile: AgentProfile,
        query: str,
        base_messages: list[dict[str, Any]],
        retrieval_context: str,
    ) -> list[dict[str, Any]]:
        base_system = ""
        history: list[dict[str, Any]] = []
        if base_messages and base_messages[0].get("role") == "system":
            base_system = str(base_messages[0].get("content") or "")
            history = base_messages[1:-1]
        else:
            history = base_messages[:-1]

        instructions = profile.instructions or profile.description
        system = (
            f"{base_system}\n\n"
            f"# Specialist Agent\n"
            f"Agent ID: {profile.id}\n"
            f"Name: {profile.name}\n"
            f"Mission: {profile.description}\n"
            f"Instructions: {instructions}\n\n"
            "Work only within your assigned mission. Use only exposed tools. "
            "If the task belongs to another specialist, state the gap instead of guessing."
        ).strip()
        if retrieval_context:
            system += "\n\n# Shared Retrieval Context\n" + retrieval_context

        messages = [{"role": "system", "content": system}]
        messages.extend(history[-12:])
        messages.append({"role": "user", "content": query})
        return messages

    def _audit_record(self, result: DispatchResult) -> dict[str, Any]:
        return {
            "query": result.plan.query,
            "strategy": result.plan.strategy,
            "task_type": result.plan.task_type,
            "selected": result.plan.selected_agent_ids,
            "confidence": result.plan.confidence,
            "rationale": result.plan.rationale,
            "success": result.success,
            "duration_ms": result.metadata.get("duration_ms", 0),
            "results": [
                {
                    "agent_id": item.agent_id,
                    "success": item.success,
                    "error": item.error,
                    "iterations": item.iterations,
                    "tool_calls": item.tool_calls,
                    "model": item.model,
                    "provider": item.provider_name,
                }
                for item in result.results
            ],
        }
