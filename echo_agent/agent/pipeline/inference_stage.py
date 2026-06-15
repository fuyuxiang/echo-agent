"""Inference stage — LLM call loop with tool execution and circuit breaking."""

from __future__ import annotations

import asyncio
import hashlib
import uuid
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.agent.tools.base import ToolExecutionContext, build_idempotency_key
from echo_agent.agent.tools.circuit_breaker import ToolCircuitBreaker
from echo_agent.bus.events import OutboundEvent
from echo_agent.models.provider import LLMResponse
from echo_agent.models.router import RouteDecision

if TYPE_CHECKING:
    from echo_agent.agent.approval_gate import ApprovalGate
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.models.inference import InferenceController
    from echo_agent.models.provider import LLMProvider
    from echo_agent.models.router import ModelRouter
    from echo_agent.observability.monitor import TraceLogger
    from echo_agent.permissions.manager import CredentialManager


class InferenceStage:
    """Runs the LLM inference loop: call model, execute tools, repeat until done."""

    _MAX_TOOL_RESULT_CHARS = 16000

    def __init__(
        self,
        *,
        config: Config,
        bus: MessageBus,
        provider: LLMProvider,
        router: ModelRouter | None,
        tools: ToolRegistry,
        approval_gate: ApprovalGate,
        credentials: CredentialManager,
        tracer: TraceLogger,
        telemetry: Any,
        inference: InferenceController,
        circuit_breaker: ToolCircuitBreaker,
        default_model: str,
        max_iterations: int,
    ):
        self._config = config
        self._bus = bus
        self._provider = provider
        self._router = router
        self._tools = tools
        self._approval_gate = approval_gate
        self._credentials = credentials
        self._tracer = tracer
        self._telemetry = telemetry
        self._inference = inference
        self._circuit_breaker = circuit_breaker
        self._default_model = default_model
        self._max_iterations = max_iterations
        self._hook_registry: Any = None
        self._nudge_interval: int = config.skills.creation_nudge_interval if hasattr(config, 'skills') and hasattr(config.skills, 'creation_nudge_interval') else 0
        self._memory_nudge_interval: int = config.memory.memory_nudge_interval if hasattr(config.memory, 'memory_nudge_interval') else 0

    def set_hook_registry(self, registry: Any) -> None:
        """Inject the plugin hook registry (attached after bootstrap)."""
        self._hook_registry = registry

    async def run(self, ctx: PipelineContext) -> InferenceResult:
        """Execute the inference loop, returning the final result."""
        event = ctx.event
        session = ctx.session
        trace_id = ctx.trace_id
        messages = ctx.messages
        tool_defs = ctx.tool_defs
        stream_publisher = ctx.stream_publisher

        async def _emit_progress(text: str, *, tool_hint: bool = False) -> None:
            if not ctx.publish_response:
                return
            out = OutboundEvent.text_reply(
                channel=event.channel, chat_id=event.chat_id, text=text, reply_to_id=event.reply_to_id,
            )
            out.is_final = False
            out.message_kind = "tool" if tool_hint else "progress"
            out.metadata = dict(event.metadata)
            out.metadata.update({"_progress": True, "_tool_hint": tool_hint, "_inbound_event_id": event.event_id})
            await self._bus.publish_outbound(out)

        async def _emit_tool_event(metadata: dict[str, Any]) -> None:
            if not ctx.publish_response:
                return
            if not getattr(self._config.gateway, 'emit_progress_events', True):
                return
            out = OutboundEvent.text_reply(
                channel=event.channel, chat_id=event.chat_id, text="", reply_to_id=event.reply_to_id,
            )
            out.is_final = False
            out.message_kind = "progress"
            out.metadata = {"_progress": True, "_inbound_event_id": event.event_id}
            out.metadata.update(metadata)
            await self._bus.publish_outbound(out)

        # Standard inference loop
        response_text = ""
        should_review_skills = False
        should_review_memory = False
        total_tool_calls = 0
        _repeat_tracker: dict[str, int] = {}
        _REPEAT_BLOCK_THRESHOLD = 4
        loop_exhausted = True

        # Load nudge counters from session (persisted across turns)
        _skill_iters = session.metadata.get("_nudge_tool_iters_skill", 0)
        _memory_iters = session.metadata.get("_nudge_tool_iters_memory", 0)
        _memory_turns = session.metadata.get("_nudge_turns_memory", 0)

        on_delta = stream_publisher.on_delta if ctx.publish_response else None

        for iteration in range(self._max_iterations):
            # Filter out circuit-broken tools
            unavailable = self._circuit_breaker.get_unavailable_tools()
            active_tool_defs = [
                t for t in tool_defs
                if t.get("function", {}).get("name") not in unavailable
            ] if unavailable else tool_defs

            llm_span = self._tracer.start_span(trace_id, f"llm_{iteration}", "llm_call", "llm_call")

            # pre_llm_call hook
            if self._hook_registry and self._hook_registry.has_hooks("pre_llm_call"):
                messages = await self._hook_registry.dispatch_modify(
                    "pre_llm_call", messages, active_tool_defs, self._default_model,
                )

            response, route_decision = await self._chat_stream_with_routing(
                messages=messages,
                tools=active_tool_defs if active_tool_defs else None,
                on_delta=on_delta,
                task_type=ctx.task_type,
                content=event.text,
            )

            # post_llm_call hook
            if self._hook_registry and self._hook_registry.has_hooks("post_llm_call"):
                response = await self._hook_registry.dispatch_modify(
                    "post_llm_call", response, messages,
                )

            self._tracer.end_span(
                llm_span,
                metadata={
                    "model": route_decision.model,
                    "provider": route_decision.provider_name,
                    "route_reason": route_decision.reason,
                    "finish": response.finish_reason,
                },
            )

            if self._telemetry and self._telemetry.available and response.usage:
                from echo_agent.observability.spans import start_llm_span, record_llm_usage, end_llm_span
                otel_span = start_llm_span(self._telemetry.get_tracer(), route_decision.model, route_decision.provider_name)
                record_llm_usage(otel_span, response.usage, route_decision.model)
                end_llm_span(otel_span)

            issues = self._inference.validate_response(response)
            if issues:
                logger.warning("Inference issues: {}", issues)

            if response.finish_reason == "error":
                logger.warning("LLM returned error in iteration {}: {}", iteration, response.content)
                if not response_text:
                    response_text = "I encountered an issue processing your request. Please try again."
                loop_exhausted = False
                break

            if response.content:
                response_text = response.content

            if not response.has_tool_calls:
                if ctx.execution_plan and not ctx.execution_plan.is_complete:
                    ctx.execution_plan.is_complete = True
                loop_exhausted = False
                break

            if ctx.execution_plan and iteration < len(ctx.execution_plan.steps):
                ctx.execution_plan.mark_step_complete(iteration, response.content or "")

            if response.content:
                await _emit_progress(response.content)

            assistant_msg: dict[str, Any] = {"role": "assistant", "content": response.content}
            assistant_msg["tool_calls"] = [tc.to_openai_format() for tc in response.tool_calls]
            messages.append(assistant_msg)

            tool_call_fmts = [tc.to_openai_format() for tc in response.tool_calls]
            session.add_message("assistant", response.content or "", tool_calls=tool_call_fmts)

            for tool_index, tool_call in enumerate(response.tool_calls):
                tool_span = self._tracer.start_span(
                    trace_id, f"tool_{iteration}_{tool_index}", f"tool:{tool_call.name}", "tool_call"
                )
                tool_message_appended = False
                try:
                    await _emit_progress(f"Using tool: {tool_call.name}", tool_hint=True)

                    approval_check = await self._approval_gate.check(
                        tool_call.name,
                        tool_call.arguments,
                        event.sender_id,
                        channel=event.channel,
                        event=event,
                        running=True,
                    )
                    if approval_check.denial:
                        self._tracer.end_span(tool_span, metadata={"success": False, "denied": True})
                        messages.append({
                            "role": "tool", "tool_call_id": tool_call.id,
                            "name": tool_call.name, "content": approval_check.denial.text,
                        })
                        tool_message_appended = True
                        session.add_message("tool", approval_check.denial.text, tool_call_id=tool_call.id, name=tool_call.name)
                        total_tool_calls += 1
                        continue

                    # Repeat-call guard: count BEFORE executing so identical
                    # calls don't keep firing side effects. The N-th identical
                    # call is short-circuited with an error message instead.
                    _call_key = f"{tool_call.name}:{hashlib.md5(str(sorted(tool_call.arguments.items())).encode()).hexdigest()[:8]}"
                    _repeat_tracker[_call_key] = _repeat_tracker.get(_call_key, 0) + 1
                    if _repeat_tracker[_call_key] >= _REPEAT_BLOCK_THRESHOLD:
                        result_text_blocked = (
                            f"[Blocked] Tool '{tool_call.name}' called with identical arguments "
                            f"{_repeat_tracker[_call_key]} times. Stopping repeated calls — "
                            "vary the arguments or take a different action."
                        )
                        messages.append({
                            "role": "tool", "tool_call_id": tool_call.id,
                            "name": tool_call.name, "content": result_text_blocked,
                        })
                        tool_message_appended = True
                        session.add_message(
                            "tool", result_text_blocked,
                            tool_call_id=tool_call.id, name=tool_call.name,
                        )
                        self._tracer.end_span(tool_span, metadata={"success": False, "repeat_blocked": True})
                        logger.warning(
                            "Blocked repeated tool call: {} ({}x)",
                            tool_call.name, _repeat_tracker[_call_key],
                        )
                        total_tool_calls += 1
                        continue

                    tool_exec_ctx = ToolExecutionContext(
                        execution_id=uuid.uuid4().hex[:12],
                        trace_id=trace_id,
                        session_key=event.session_key,
                        user_id=event.sender_id,
                        attempt_index=0,
                        idempotency_key=build_idempotency_key(trace_id, tool_call.name, tool_index, tool_call.arguments),
                        credentials=self._credentials.get_for_tool(tool_call.name),
                        approved_actions=approval_check.approved_actions,
                    )

                    # pre_tool_call hook
                    if self._hook_registry and self._hook_registry.has_hooks("pre_tool_call"):
                        hook_results = await self._hook_registry.dispatch(
                            "pre_tool_call", tool_call.name, tool_call.arguments, tool_exec_ctx,
                        )
                        _hook_cancelled = False
                        for hr in hook_results:
                            if hr.cancel:
                                result = type("_R", (), {"success": False, "text": f"Blocked by plugin: {hr.cancel_reason}", "error": hr.cancel_reason, "metadata": {}})()
                                messages.append({
                                    "role": "tool", "tool_call_id": tool_call.id,
                                    "name": tool_call.name, "content": result.text,
                                })
                                tool_message_appended = True
                                session.add_message("tool", result.text, tool_call_id=tool_call.id, name=tool_call.name)
                                self._tracer.end_span(tool_span, metadata={"success": False, "hook_cancelled": True})
                                total_tool_calls += 1
                                _hook_cancelled = True
                                break
                            if hr.modified is not None:
                                tool_call.arguments = hr.modified
                        if _hook_cancelled:
                            continue

                    import time as _time
                    _tool_start_ts = _time.monotonic()

                    _debug_progress = getattr(self._config.gateway, 'progress_debug', False)
                    _tool_start_meta: dict[str, Any] = {
                        "progress_type": "tool_call",
                        "tool": tool_call.name,
                        "status": "started",
                    }
                    if _debug_progress:
                        _tool_start_meta["args"] = str(tool_call.arguments)[:500]
                    await _emit_tool_event(_tool_start_meta)

                    result = await self._tools.execute(tool_call.name, tool_call.arguments, tool_exec_ctx)

                    # post_tool_call hook
                    if self._hook_registry and self._hook_registry.has_hooks("post_tool_call"):
                        result = await self._hook_registry.dispatch_modify(
                            "post_tool_call", result, tool_call.name, tool_call.arguments, tool_exec_ctx,
                        )

                    _tool_duration_ms = int((_time.monotonic() - _tool_start_ts) * 1000)
                    _tool_result_meta: dict[str, Any] = {
                        "progress_type": "tool_result",
                        "tool": tool_call.name,
                        "duration_ms": _tool_duration_ms,
                        "status": "done" if result.success else "error",
                    }
                    if _debug_progress:
                        _tool_result_meta["result_preview"] = result.text[:500]
                    await _emit_tool_event(_tool_result_meta)

                    result_text = result.text
                    if len(result_text) > self._MAX_TOOL_RESULT_CHARS:
                        result_text = result_text[:self._MAX_TOOL_RESULT_CHARS] + "\n...(truncated)"

                    self._tracer.end_span(tool_span, metadata={"success": result.success})

                    if self._telemetry and self._telemetry.available:
                        from echo_agent.observability.spans import start_tool_span, end_tool_span
                        otel_tool = start_tool_span(self._telemetry.get_tracer(), tool_call.name)
                        end_tool_span(otel_tool, error=None if result.success else result.error)

                    messages.append({
                        "role": "tool", "tool_call_id": tool_call.id,
                        "name": tool_call.name, "content": result_text,
                    })
                    tool_message_appended = True
                    session.add_message("tool", result_text, tool_call_id=tool_call.id, name=tool_call.name)

                    total_tool_calls += 1
                    _skill_iters += 1
                    _memory_iters += 1

                    # Per-tool circuit breaker
                    if result.success:
                        self._circuit_breaker.record_success(tool_call.name)
                    else:
                        self._circuit_breaker.record_failure(tool_call.name)

                    if (
                        self._nudge_interval > 0
                        and _skill_iters >= self._nudge_interval
                        and self._tools.has("skill_manage")
                    ):
                        should_review_skills = True
                        _skill_iters = 0
                    if (
                        self._memory_nudge_interval > 0
                        and _memory_iters >= self._memory_nudge_interval
                        and self._tools.has("memory")
                    ):
                        should_review_memory = True
                        _memory_iters = 0
                except BaseException:
                    # Ensure every announced tool_call gets a paired tool message,
                    # otherwise the next LLM turn will reject the conversation
                    # ("tool_calls must be followed by tool messages").
                    if not tool_message_appended:
                        err_text = "Tool execution interrupted before producing a result."
                        messages.append({
                            "role": "tool", "tool_call_id": tool_call.id,
                            "name": tool_call.name, "content": err_text,
                        })
                        try:
                            session.add_message("tool", err_text, tool_call_id=tool_call.id, name=tool_call.name)
                        except Exception:
                            pass
                        try:
                            self._tracer.end_span(tool_span, error="interrupted")
                        except Exception:
                            pass
                        # Mark this tool as failing so the circuit breaker
                        # eventually disables it instead of letting interrupted
                        # calls silently skew failure stats.
                        try:
                            self._circuit_breaker.record_failure(tool_call.name)
                        except Exception:
                            pass
                    raise

        if loop_exhausted:
            logger.warning(
                "Agent loop exhausted max iterations ({}) for session {}",
                self._max_iterations, event.session_key,
            )
            if not response_text:
                response_text = "I encountered an issue processing your request. Please try again or rephrase your question."

        # Turn-based memory review trigger: fires even for pure chat (no tool calls).
        # The tool-iteration path above only counts tool loops, so personal facts
        # shared in plain conversation would never be reviewed without this.
        if self._memory_nudge_interval > 0 and self._tools.has("memory"):
            _memory_turns += 1
            if _memory_turns >= self._memory_nudge_interval:
                should_review_memory = True
                _memory_turns = 0

        # Persist nudge counters back to session metadata
        session.metadata["_nudge_tool_iters_skill"] = _skill_iters
        session.metadata["_nudge_tool_iters_memory"] = _memory_iters
        session.metadata["_nudge_turns_memory"] = _memory_turns

        return InferenceResult(
            response_text=response_text,
            total_tool_calls=total_tool_calls,
            should_review_skills=should_review_skills,
            should_review_memory=should_review_memory,
        )

    async def _chat_stream_with_routing(
        self,
        *,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None,
        on_delta: Any | None,
        task_type: str,
        content: str,
    ) -> tuple[LLMResponse, RouteDecision]:
        """Route to appropriate model with fallback chain and streaming."""
        if not self._router:
            response = await self._provider.chat_stream_with_retry(
                messages=messages,
                tools=tools,
                model=self._default_model or None,
                on_delta=on_delta,
            )
            return response, RouteDecision(provider_name="default", model=self._default_model, reason="no router")

        emitted = False

        async def routed_delta(delta: str) -> None:
            nonlocal emitted
            emitted = True
            if on_delta:
                maybe = on_delta(delta)
                if asyncio.iscoroutine(maybe):
                    await maybe

        last_response: LLMResponse | None = None
        last_decision: RouteDecision | None = None
        for provider_name, provider, decision in self._router.route_candidates(task_type, content):
            response = await provider.chat_stream_with_retry(
                messages=messages,
                tools=tools,
                model=decision.model,
                on_delta=routed_delta if on_delta else None,
                max_tokens=decision.max_tokens,
                temperature=decision.temperature,
            )
            last_response = response
            last_decision = decision
            if response.finish_reason != "error":
                self._router.mark_success(provider_name)
                return response, decision
            self._router.mark_failure(provider_name, response.content or "LLM error")
            logger.warning("LLM provider '{}' failed for model '{}': {}", provider_name, decision.model, response.content)
            if emitted:
                return response, decision

        if last_response and last_decision:
            return last_response, last_decision
        response = await self._provider.chat_stream_with_retry(
            messages=messages,
            tools=tools,
            model=self._default_model or None,
            on_delta=on_delta,
        )
        return response, RouteDecision(provider_name="default", model=self._default_model, reason="router empty")
