"""Inference stage — LLM call loop with tool execution and circuit breaking."""

from __future__ import annotations

import asyncio
import hashlib
import uuid
from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.degraded_notice import REASON_REPEAT_BLOCKED, notice_for
from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.agent.tools.base import ToolExecutionContext, build_idempotency_key
from echo_agent.agent.tools.circuit_breaker import ToolCircuitBreaker
from echo_agent.agent.planning.models import StepStatus
from echo_agent.bus.events import OutboundEvent
from echo_agent.cost.budget import BudgetExceeded
from echo_agent.models.provider import LLMResponse
from echo_agent.models.router import RouteDecision

if TYPE_CHECKING:
    from echo_agent.agent.approval_gate import ApprovalGate
    from echo_agent.agent.planning.planner import AgentPlanner
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.models.inference import InferenceController
    from echo_agent.models.provider import LLMProvider
    from echo_agent.models.router import ModelRouter
    from echo_agent.observability.monitor import TraceLogger
    from echo_agent.permissions.manager import CredentialManager


@dataclass
class _LoopResult:
    """Output of one tool loop pass, used by run() to orchestrate reflection/rerun."""

    response_text: str = ""
    total_tool_calls: int = 0
    loop_exhausted: bool = True
    # True when the daily cost budget halted the loop mid-task. The task is
    # NOT complete, so run() must not mark pending plan steps done.
    budget_halted: bool = False
    should_review_skills: bool = False
    should_review_memory: bool = False
    skill_iters: int = 0
    memory_iters: int = 0
    degraded_notices: list[str] = field(default_factory=list)
    # NOTE: memory_turns is intentionally absent — it's a turn-level counter
    # managed by run(), not the per-pass tool loop.


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
        planner: AgentPlanner | None = None,
        plan_run_store: Any = None,
        cost_tracker: Any = None,
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
        self._planner = planner
        self._plan_run_store = plan_run_store
        self._cost_tracker = cost_tracker

    def set_hook_registry(self, registry: Any) -> None:
        """Inject the plugin hook registry (attached after bootstrap)."""
        self._hook_registry = registry

    async def _emit_progress(self, ctx: PipelineContext, text: str, *, tool_hint: bool = False) -> None:
        if not ctx.publish_response:
            return
        event = ctx.event
        out = OutboundEvent.text_reply(
            channel=event.channel, chat_id=event.chat_id, text=text, reply_to_id=event.reply_to_id,
        )
        out.is_final = False
        out.message_kind = "tool" if tool_hint else "progress"
        out.metadata = dict(event.metadata)
        out.metadata.update({"_progress": True, "_tool_hint": tool_hint, "_inbound_event_id": event.event_id})
        await self._bus.publish_outbound(out)

    async def _emit_tool_event(self, ctx: PipelineContext, metadata: dict[str, Any]) -> None:
        if not ctx.publish_response:
            return
        if not getattr(self._config.gateway, 'emit_progress_events', True):
            return
        event = ctx.event
        out = OutboundEvent.text_reply(
            channel=event.channel, chat_id=event.chat_id, text="", reply_to_id=event.reply_to_id,
        )
        out.is_final = False
        out.message_kind = "progress"
        out.metadata = {"_progress": True, "_inbound_event_id": event.event_id}
        out.metadata.update(metadata)
        await self._bus.publish_outbound(out)

    async def run(self, ctx: PipelineContext) -> InferenceResult:
        """Execute the inference loop, returning the final result."""
        session = ctx.session
        messages = ctx.messages

        loop_result = await self._run_tool_loop(ctx, messages)

        # 反思闭环：仅在多步 plan 上触发，最多重跑 1 轮
        if (
            self._planner is not None
            and ctx.execution_plan is not None
            and len(ctx.execution_plan.steps) > 1
        ):
            # reflect 是注入依赖的外部调用，必须兜底：反思失败绝不能把
            # 一个已经拿到的第一轮结果搞坏。失败时记日志并按"不重跑"处理。
            try:
                feedback = await self._planner.reflect(
                    ctx.execution_plan, [loop_result.response_text]
                )
            except Exception as exc:  # noqa: BLE001 — 边界容错，任何反思异常都不该冒泡
                logger.warning("Reflection raised, skipping rerun: {}", exc)
                feedback = None
            if feedback is not None and feedback.should_replan:
                guidance = (
                    "[Reflection] 上一轮回复可能未完全达成目标。\n"
                    f"评估意见：{feedback.critique}"
                )
                if feedback.suggestions:
                    sug = "\n".join(f"- {s}" for s in feedback.suggestions)
                    guidance += f"\n建议：\n{sug}"
                guidance += "\n请据此改进并完成任务。"
                messages.append({"role": "user", "content": guidance})

                # 先把第一轮 nudge 计数写回 session，第二轮 helper 才能从第一轮
                # 末尾续起，使整个 turn 的工具调用被完整累计（而非只数第二轮）。
                session.metadata["_nudge_tool_iters_skill"] = loop_result.skill_iters
                session.metadata["_nudge_tool_iters_memory"] = loop_result.memory_iters

                # 第二轮重跑（硬上限 1，第二轮后不再反思）
                second = await self._run_tool_loop(ctx, messages)
                loop_result = _LoopResult(
                    response_text=second.response_text or loop_result.response_text,
                    total_tool_calls=loop_result.total_tool_calls + second.total_tool_calls,
                    loop_exhausted=second.loop_exhausted,
                    budget_halted=second.budget_halted,
                    should_review_skills=loop_result.should_review_skills or second.should_review_skills,
                    should_review_memory=loop_result.should_review_memory or second.should_review_memory,
                    skill_iters=second.skill_iters,
                    memory_iters=second.memory_iters,
                    degraded_notices=loop_result.degraded_notices + second.degraded_notices,
                )

        # Turn-based memory review trigger: fires even for pure chat (no tool calls).
        # The tool-iteration path only counts tool loops, so personal facts
        # shared in plain conversation would never be reviewed without this.
        _memory_turns = session.metadata.get("_nudge_turns_memory", 0)
        should_review_memory = loop_result.should_review_memory
        if self._memory_nudge_interval > 0 and self._tools.has("memory"):
            _memory_turns += 1
            if _memory_turns >= self._memory_nudge_interval:
                should_review_memory = True
                _memory_turns = 0

        # Persist nudge counters back to session metadata
        session.metadata["_nudge_tool_iters_skill"] = loop_result.skill_iters
        session.metadata["_nudge_tool_iters_memory"] = loop_result.memory_iters
        session.metadata["_nudge_turns_memory"] = _memory_turns

        # Persist plan execution state so progress is queryable and an
        # interrupted long task can be resumed. The reflect loop above is the
        # only execution feedback we have at turn granularity, so completing a
        # turn without a replan request marks the plan's steps done; a
        # should_replan turn leaves the plan running for the next turn.
        if (
            self._plan_run_store is not None
            and ctx.plan_run_id
            and ctx.execution_plan is not None
        ):
            try:
                plan = ctx.execution_plan
                if (
                    loop_result.response_text
                    and not loop_result.loop_exhausted
                    and not loop_result.budget_halted
                ):
                    for step in plan.steps:
                        if step.status == StepStatus.PENDING:
                            plan.mark_step_complete(step.index, "")
                status = "complete" if plan.is_complete else "running"
                if loop_result.loop_exhausted or loop_result.budget_halted:
                    status = "exhausted"
                await self._plan_run_store.update(ctx.plan_run_id, plan, status=status)
            except Exception as e:
                logger.debug("Plan run update failed: {}", e)

        return InferenceResult(
            response_text=loop_result.response_text,
            total_tool_calls=loop_result.total_tool_calls,
            should_review_skills=loop_result.should_review_skills,
            should_review_memory=should_review_memory,
            degraded_notices=loop_result.degraded_notices,
        )

    async def _run_tool_loop(self, ctx: PipelineContext, messages: list[dict[str, Any]]) -> _LoopResult:
        """Run one pass of the tool loop and return a _LoopResult.

        Nudge counters are read from session.metadata but NOT written back —
        run() is responsible for persisting them after all passes complete.
        """
        event = ctx.event
        session = ctx.session
        trace_id = ctx.trace_id
        tool_defs = ctx.tool_defs
        stream_publisher = ctx.stream_publisher

        # Standard inference loop
        response_text = ""
        should_review_skills = False
        should_review_memory = False
        total_tool_calls = 0
        _repeat_tracker: dict[str, int] = {}
        _REPEAT_BLOCK_THRESHOLD = 4
        loop_exhausted = True
        budget_halted = False
        degraded_notices: list[str] = []

        # Read nudge counters from session (do NOT write back — run() owns that)
        _skill_iters = session.metadata.get("_nudge_tool_iters_skill", 0)
        _memory_iters = session.metadata.get("_nudge_tool_iters_memory", 0)

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

            # Hard budget gate: stop before spending more once the daily cap is hit.
            if self._cost_tracker is not None:
                try:
                    self._cost_tracker.enforce()  # raises BudgetExceeded when daily hard cap hit
                except BudgetExceeded as e:
                    logger.warning("Budget gate stopped inference: {}", e)
                    self._tracer.end_span(llm_span, metadata={"budget_exceeded": True})
                    response_text = str(e)
                    loop_exhausted = False
                    budget_halted = True
                    break

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

            if self._cost_tracker is not None and response.usage:
                await self._cost_tracker.record(
                    route_decision.model, response.usage, route_decision.provider_name,
                    channel=event.channel,
                )

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

            if response.content:
                await self._emit_progress(ctx, response.content)

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
                    await self._emit_progress(ctx, f"Using tool: {tool_call.name}", tool_hint=True)

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
                        if approval_check.notify_user and approval_check.notice:
                            degraded_notices.append(approval_check.notice)
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
                        degraded_notices.append(notice_for(REASON_REPEAT_BLOCKED))
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
                    await self._emit_tool_event(ctx, _tool_start_meta)

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
                    await self._emit_tool_event(ctx, _tool_result_meta)

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

        # Safety net independent of loop_exhausted: empty content with no tool
        # calls breaks the loop with loop_exhausted=False, bypassing the guard
        # above. Ensure the user always receives a reply.
        if not response_text:
            response_text = "I encountered an issue processing your request. Please try again or rephrase your question."

        return _LoopResult(
            response_text=response_text,
            total_tool_calls=total_tool_calls,
            loop_exhausted=loop_exhausted,
            budget_halted=budget_halted,
            should_review_skills=should_review_skills,
            should_review_memory=should_review_memory,
            skill_iters=_skill_iters,
            memory_iters=_memory_iters,
            degraded_notices=degraded_notices,
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
