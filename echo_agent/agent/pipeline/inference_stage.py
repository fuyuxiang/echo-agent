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
from echo_agent.models.provider import LLMResponse, ToolCallRequest
from echo_agent.models.router import RouteDecision
from echo_agent.utils.text import strip_thinking

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
        multi_agent: Any | None,
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
        self._multi_agent = multi_agent
        self._nudge_interval: int = config.skills.nudge_interval if hasattr(config, 'skills') and hasattr(config.skills, 'nudge_interval') else 0
        self._memory_nudge_interval: int = config.memory.nudge_interval if hasattr(config.memory, 'nudge_interval') else 0
        self._tool_iters_since_skill_check = 0
        self._tool_iters_since_memory_check = 0

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

        # Multi-agent auto dispatch path
        if (
            self._multi_agent
            and self._config.multi_agent.mode == "auto"
            and ctx.dispatch_plan
            and ctx.dispatch_plan.should_dispatch
        ):
            return await self._run_multi_agent_dispatch(ctx, _emit_progress)

        # Standard inference loop
        response_text = ""
        should_review_skills = False
        should_review_memory = False
        total_tool_calls = 0
        consecutive_failures = 0
        _repeat_tracker: dict[str, int] = {}
        loop_exhausted = True

        on_delta = stream_publisher.on_delta if ctx.publish_response else None

        for iteration in range(self._max_iterations):
            # Filter out circuit-broken tools
            unavailable = self._circuit_breaker.get_unavailable_tools()
            active_tool_defs = [
                t for t in tool_defs
                if t.get("function", {}).get("name") not in unavailable
            ] if unavailable else tool_defs

            llm_span = self._tracer.start_span(trace_id, f"llm_{iteration}", "llm_call", "llm_call")
            response, route_decision = await self._chat_stream_with_routing(
                messages=messages,
                tools=active_tool_defs if active_tool_defs else None,
                on_delta=on_delta,
                task_type=ctx.task_type,
                content=event.text,
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
                    session.add_message("tool", approval_check.denial.text, tool_call_id=tool_call.id, name=tool_call.name)
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
                result = await self._tools.execute(tool_call.name, tool_call.arguments, tool_exec_ctx)
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
                session.add_message("tool", result_text, tool_call_id=tool_call.id, name=tool_call.name)

                total_tool_calls += 1
                self._tool_iters_since_skill_check += 1
                self._tool_iters_since_memory_check += 1

                # Per-tool circuit breaker
                if result.success:
                    consecutive_failures = 0
                    self._circuit_breaker.record_success(tool_call.name)
                else:
                    consecutive_failures += 1
                    self._circuit_breaker.record_failure(tool_call.name)

                _call_key = f"{tool_call.name}:{hashlib.md5(str(sorted(tool_call.arguments.items())).encode()).hexdigest()[:8]}"
                _repeat_tracker[_call_key] = _repeat_tracker.get(_call_key, 0) + 1
                if _repeat_tracker[_call_key] >= 4:
                    result_text_blocked = (
                        f"[Blocked] Tool '{tool_call.name}' called with identical arguments "
                        f"{_repeat_tracker[_call_key]} times. Stopping repeated calls."
                    )
                    messages[-1]["content"] = result_text_blocked
                    logger.warning("Blocked repeated tool call: {} ({}x)", tool_call.name, _repeat_tracker[_call_key])

                if (
                    self._nudge_interval > 0
                    and self._tool_iters_since_skill_check >= self._nudge_interval
                    and self._tools.has("skill_manage")
                ):
                    should_review_skills = True
                    self._tool_iters_since_skill_check = 0
                if (
                    self._memory_nudge_interval > 0
                    and self._tool_iters_since_memory_check >= self._memory_nudge_interval
                    and self._tools.has("memory")
                ):
                    should_review_memory = True
                    self._tool_iters_since_memory_check = 0

        if loop_exhausted:
            logger.warning(
                "Agent loop exhausted max iterations ({}) for session {}",
                self._max_iterations, event.session_key,
            )
            if not response_text:
                response_text = "I encountered an issue processing your request. Please try again or rephrase your question."

        return InferenceResult(
            response_text=response_text,
            total_tool_calls=total_tool_calls,
            should_review_skills=should_review_skills,
            should_review_memory=should_review_memory,
        )

    async def _run_multi_agent_dispatch(self, ctx: PipelineContext, emit_progress: Any) -> InferenceResult:
        """Handle multi-agent auto dispatch."""
        event = ctx.event
        dispatch_plan = ctx.dispatch_plan
        trace_id = ctx.trace_id

        selected = ", ".join(dispatch_plan.selected_agent_ids)
        await emit_progress(f"Dispatching to specialist agent(s): {selected}", tool_hint=True)
        dispatch_span = self._tracer.start_span(trace_id, "multi_agent_dispatch", "multi_agent_dispatch", "agent_dispatch")

        async def _child_tool_executor(
            agent_id: str,
            tool_call: ToolCallRequest,
            index: int,
            child_messages: list[dict[str, Any]],
            allowed_tools: set[str],
        ) -> str:
            if tool_call.name not in allowed_tools:
                return f"Error: Tool '{tool_call.name}' is not allowed for agent '{agent_id}'"
            approval_check = await self._approval_gate.check(
                tool_call.name, tool_call.arguments, event.sender_id,
                channel=event.channel, event=event, running=True,
            )
            if approval_check.denial:
                return approval_check.denial.text
            tool_exec_ctx = ToolExecutionContext(
                execution_id=uuid.uuid4().hex[:12],
                trace_id=trace_id,
                session_key=event.session_key,
                user_id=event.sender_id,
                agent_id=agent_id,
                attempt_index=0,
                idempotency_key=build_idempotency_key(trace_id, tool_call.name, index, tool_call.arguments),
                credentials=self._credentials.get_for_tool(tool_call.name),
                approved_actions=approval_check.approved_actions,
            )
            result = await self._tools.execute(
                tool_call.name, tool_call.arguments, tool_exec_ctx,
                replay_scope=f"dispatch_{trace_id}_{agent_id}",
            )
            result_text = result.text
            if len(result_text) > self._MAX_TOOL_RESULT_CHARS:
                result_text = result_text[:self._MAX_TOOL_RESULT_CHARS] + "\n...(truncated)"
            return result_text

        dispatch_result = await self._multi_agent.dispatch(
            query=event.text,
            plan=dispatch_plan,
            base_messages=ctx.messages,
            retrieval_context=ctx.retrieval,
            tool_executor=_child_tool_executor,
            trace_id=trace_id,
        )
        self._tracer.end_span(
            dispatch_span,
            metadata={
                "strategy": dispatch_plan.strategy,
                "selected": dispatch_plan.selected_agent_ids,
                "confidence": dispatch_plan.confidence,
                "success": dispatch_result.success,
                "duration_ms": dispatch_result.metadata.get("duration_ms", 0),
            },
        )

        response_text = dispatch_result.final_output
        if response_text:
            response_text = strip_thinking(response_text)

        return InferenceResult(
            response_text=response_text,
            dispatched=True,
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
