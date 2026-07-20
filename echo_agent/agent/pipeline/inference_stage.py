"""Inference stage — LLM call loop with tool execution and circuit breaking."""

from __future__ import annotations

import asyncio
import hashlib
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.cognitive_emitter import should_emit_cognitive
from echo_agent.agent.degraded_notice import (
    REASON_LOOP_EXHAUSTED,
    REASON_OUTPUT_TRUNCATED,
    REASON_REPEAT_BLOCKED,
    notice_for,
)
from echo_agent.agent.pipeline.tool_concurrency import (
    ToolPlan,
    extract_paths,
    partition_concurrent,
)
from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.agent.tools.base import ToolExecutionContext, build_idempotency_key
from echo_agent.agent.tools.circuit_breaker import ToolCircuitBreaker
from echo_agent.agent.progress_heartbeat import ActivitySnapshot, friendly_activity
from echo_agent.bus.events import OutboundEvent
from echo_agent.cost.budget import BudgetExceeded
from echo_agent.models.provider import LLMResponse
from echo_agent.models.router import RouteDecision

if TYPE_CHECKING:
    from echo_agent.agent.approval_gate import ApprovalGate
    from echo_agent.agent.cognitive_emitter import CognitiveEmitter
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
    # True when the final iteration had to force a conclusion (tools stripped
    # at the iteration ceiling). The turn produced an answer, but the TASK is
    # not done — run() must keep the plan resumable, same as budget_halted.
    forced_convergence: bool = False
    # True when the user cooperatively stopped the turn (Ctrl+C interrupt frame
    # tripped a checkpoint). The turn produced only a partial answer + stop
    # notice; the TASK is not done — run() must keep the plan resumable AND skip
    # post-processing (planner reflection, is_complete) that assumes a finished
    # turn.
    interrupted: bool = False
    should_review_skills: bool = False
    should_review_memory: bool = False
    skill_iters: int = 0
    memory_iters: int = 0
    degraded_notices: list[str] = field(default_factory=list)
    # NOTE: memory_turns is intentionally absent — it's a turn-level counter
    # managed by run(), not the per-pass tool loop.


@dataclass
class _BatchCounters:
    """Mutable carrier threaded through _execute_tool_batch so the batch can
    update the loop's running totals/flags in place (counters live in
    _run_tool_loop, the batch mutates them)."""

    total_tool_calls: int
    skill_iters: int
    memory_iters: int
    should_review_skills: bool
    should_review_memory: bool
    degraded_notices: list


@dataclass
class _Decision:
    """Phase A outcome for one tool_call."""

    tool_call: object
    index: int
    verdict: str  # "RUN" | "BLOCKED"
    exec_ctx: object = None  # ToolExecutionContext, set when RUN
    blocked_message: str = ""  # tool message text, set when BLOCKED
    blocked_meta: dict = field(default_factory=dict)  # span end metadata
    read_only: bool = False
    approved: bool = True
    paths: list = field(default_factory=list)


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
        cognitive_emitter: "CognitiveEmitter | None" = None,
        compressor: Any = None,
        memory_store: Any = None,
        clarify_manager: Any = None,
        interrupt_manager: Any = None,
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
        self._cog = cognitive_emitter
        self._compressor = compressor
        self._memory_store = memory_store
        self._clarify = clarify_manager
        self._interrupt = interrupt_manager
        # Read-only tool concurrency config (Task 2 added the fields, marked
        # effective). getattr fallbacks because some test configs are MagicMock.
        _tc = getattr(getattr(config, "agent", None), "tool_concurrency", None)
        self._concurrency_enabled: bool = bool(getattr(_tc, "enabled", False))
        self._max_concurrent: int = int(getattr(_tc, "max_concurrent", 1) or 1)

    def set_hook_registry(self, registry: Any) -> None:
        """Inject the plugin hook registry (attached after bootstrap)."""
        self._hook_registry = registry

    async def _prepare_clarify(self, tool_call: Any, event: Any) -> None:
        """Wire a clarify tool call to the follow-up machinery.

        CLI channel: register a pending request, inject its id into the tool
        arguments, and emit a clarify_request frame so the TUI can render the
        choices. The tool then blocks on ClarifyManager.wait_for_answer.

        IM channels (non-CLI): the tool cannot block (callback-style transport,
        no long-held lock), so instead we remember the question per session via
        register_im_pending. The tool still returns the question as text this
        turn; _on_inbound consumes the *next* message on this session as the
        answer. No id injection, no frame — those are CLI/TUI-only."""
        if tool_call.name != "clarify" or self._clarify is None:
            return
        question = tool_call.arguments.get("question", "")
        options = tool_call.arguments.get("options", []) or []
        if not should_emit_cognitive(event.channel):
            # IM path: remember the question keyed by session so the next inbound
            # message is routed as its answer (see AgentLoop._on_inbound).
            session_key = getattr(event, "session_key", "")
            if session_key:
                self._clarify.register_im_pending(
                    session_key, question, options,
                    user_id=getattr(event, "sender_id", ""),
                )
            return
        req = self._clarify.request(
            question, options,
            user_id=getattr(event, "sender_id", ""),
            session_key=getattr(event, "session_key", ""),
        )
        tool_call.arguments["_clarify_id"] = req.id
        if self._cog is not None:
            await self._cog.emit(
                event, "clarify_request",
                {"clarify_id": req.id, "question": question, "options": options},
                question,
            )

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
        # Only count this as visible feedback if it actually reached the user.
        # On uneditable channels (e.g. weixin) the ChannelManager drops progress
        # events; counting a dropped event would starve the heartbeat throttle.
        if ctx.activity is not None and not out.metadata.get("_drop"):
            ctx.activity.mark_visible_feedback()

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
        # See _emit_progress: a dropped event is not visible feedback.
        if ctx.activity is not None and not out.metadata.get("_drop"):
            ctx.activity.mark_visible_feedback()

    async def _emit_tool_call(
        self, event, name, params, status, result_summary,
        *, tool_call_id: str = "", result_meta: dict | None = None,
        duration_ms: int | None = None,
    ) -> None:
        """Surface a tool invocation to the cognition stream (cli-gated).

        Emitted twice per call, paired by tool_call_id: once with
        status="running" before execution, once with the terminal status.
        result_meta carries producer-supplied counts (line/hit/entry totals)
        computed on the full result before truncation — the client turns them
        into Chinese words and must never recount the truncated result_text."""
        # Gate before slicing params: IM channels skip the comprehension too.
        if self._cog is None or not self._cog.active(event):
            return
        safe_params = {k: str(v)[:120] for k, v in (params or {}).items()}
        await self._cog.emit(
            event, "tool_call",
            {"name": name, "params": safe_params, "status": status,
             "tool_call_id": tool_call_id, "result_meta": result_meta,
             "result_text": str(result_summary)[:300],
             "duration_ms": duration_ms},
            f"🔧 {name} · {status}",
        )

    async def _emit_cost(self, event, turn_tokens, turn_cost, total_cost,
                         model: str = "", messages: list | None = None) -> None:
        """Surface a cost update to the cognition stream (cli-gated)."""
        if self._cog is None:
            return
        context_used = 0
        context_max = 0
        compressor = getattr(self, "_compressor", None)
        if compressor is not None:
            context_max = getattr(compressor, "context_window_tokens", 0) or 0
            if messages:
                context_used = compressor.estimate_tokens(messages)
        memory_count = 0
        memory_store = getattr(self, "_memory_store", None)
        if memory_store is not None:
            try:
                memory_count = len(memory_store.list_all())
            except Exception:
                pass
        await self._cog.emit(
            event, "cost_update",
            {"turn_tokens": int(turn_tokens), "turn_cost": round(turn_cost, 4),
             "total_cost": round(total_cost, 4),
             "model": model,
             "context_used": context_used,
             "context_max": context_max,
             "memory_count": memory_count},
            f"💰 ${round(total_cost, 4)}",
        )

    async def _emit_memory_written(self, event, items) -> None:
        """Surface written/reinforced memory items to the cognition stream."""
        if self._cog is None or not items:
            return
        norm = [{"content": str(i.get("content", ""))[:200],
                 "source": i.get("source", "model_inferred"),
                 "op": i.get("op", "write")} for i in items]
        n = len(norm)
        await self._cog.emit(
            event, "memory_written", {"items": norm},
            f"✍ 写入/强化 {n} 条记忆",
        )

    async def _emit_thinking(self, event, duration_ms, text) -> None:
        """Surface a reasoning/thinking span to the cognition stream."""
        if self._cog is None:
            return
        await self._cog.emit(
            event, "thinking",
            {"duration_ms": duration_ms, "text": str(text)[:2000]},
            f"💭 Thought for {round(duration_ms / 1000, 1)}s",
        )

    async def _emit_evolution(self, event, phase, skill, detail) -> None:
        """Surface an evolution-engine event to the cognition stream."""
        if self._cog is None:
            return
        await self._cog.emit(
            event, "evolution",
            {"phase": phase, "skill": skill, "detail": str(detail)[:300]},
            f"🧬 {skill}: {phase}",
        )

    async def run(self, ctx: PipelineContext) -> InferenceResult:
        """Execute the inference loop, returning the final result."""
        session = ctx.session
        messages = ctx.messages

        loop_result = await self._run_tool_loop(ctx, messages)

        # 反思闭环：仅在多步 plan 上触发，最多重跑 1 轮。
        # 用户中断的 turn 不反思也不重跑：中断意味着"现在就停"，再自动发起一轮
        # 推理会违背用户意图，也会覆盖掉已生成的停止文案。
        if (
            not loop_result.interrupted
            and self._planner is not None
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
                    # The rerun supersedes the first pass: if IT converged
                    # normally the turn is fine; only its own forcing counts.
                    forced_convergence=second.forced_convergence,
                    interrupted=second.interrupted,
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
                # NB: counter is NOT reset here. Clearing at the trigger meant a
                # failed background review left the counter at 0, so this batch
                # would never be reviewed again. The counter is now zeroed only
                # after the review SUCCEEDS (ResponseStage._background_memory_review),
                # so a failed/retried review keeps it elevated and next turn
                # re-triggers.

        # Persist nudge counters back to session metadata
        session.metadata["_nudge_tool_iters_skill"] = loop_result.skill_iters
        session.metadata["_nudge_tool_iters_memory"] = loop_result.memory_iters
        session.metadata["_nudge_turns_memory"] = _memory_turns

        # Persist plan execution state so progress is queryable and an
        # interrupted long task can be resumed. Honest status semantics: the
        # only execution feedback we have is turn-granular (the reflect loop
        # above), so the RUN status records the turn outcome — step statuses
        # are NOT batch-faked to COMPLETED anymore. A step only ever flips
        # status when something actually marks it (today: nothing does, so
        # steps stay pending; the run-level status carries the truth).
        if (
            self._plan_run_store is not None
            and ctx.plan_run_id
            and ctx.execution_plan is not None
        ):
            try:
                plan = ctx.execution_plan
                # forced_convergence means the loop hit its iteration ceiling
                # and squeezed out a conclusion — the ANSWER exists but the
                # TASK is unfinished. Treat it like exhaustion: keep the run
                # resumable.
                task_incomplete = (
                    loop_result.loop_exhausted
                    or loop_result.budget_halted
                    or loop_result.forced_convergence
                    or loop_result.interrupted
                )
                if task_incomplete:
                    status = "exhausted"
                elif loop_result.response_text:
                    status = "complete"
                else:
                    status = "running"
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
        _repeat_tracker: dict[str, int] = {}
        loop_exhausted = True
        budget_halted = False
        # Degraded-convergence controls: force_no_tools strips tools from the
        # NEXT call (set by the truncation retry below); truncation_retried
        # bounds that retry to once per pass. forcing_final marks the last
        # iteration's forced-conclusion call; forced_convergence records that
        # the pass ended that way so run() keeps the plan resumable.
        force_no_tools = False
        truncation_retried = False
        forcing_final = False
        forced_convergence = False
        interrupted = False

        # Read nudge counters from session (do NOT write back — run() owns that)
        _skill_iters = session.metadata.get("_nudge_tool_iters_skill", 0)
        _memory_iters = session.metadata.get("_nudge_tool_iters_memory", 0)

        # Mutable carrier for the running totals/flags the per-batch tool
        # executor updates in place. Persists across loop iterations.
        counters = _BatchCounters(
            total_tool_calls=0,
            skill_iters=_skill_iters,
            memory_iters=_memory_iters,
            should_review_skills=False,
            should_review_memory=False,
            degraded_notices=[],
        )

        on_delta = stream_publisher.on_delta if ctx.publish_response else None

        for iteration in range(self._max_iterations):
            # Cooperative interrupt checkpoint. A Ctrl+C interrupt frame set this
            # flag on a lock-free path; stop cleanly at the iteration boundary so
            # session history, memory writes and tool side effects are never left
            # half-applied (the reason we do NOT hard-cancel the task). Keep any
            # text produced so far and mark the turn as user-stopped, not
            # exhausted, so run() treats it as a real (if partial) result.
            if (
                self._interrupt is not None
                and self._interrupt.is_interrupted(getattr(event, "session_key", ""))
            ):
                notice = "⏹ 已按你的请求停止。"
                response_text = (
                    f"{response_text}\n\n{notice}" if response_text.strip() else notice
                )
                loop_exhausted = False
                interrupted = True
                break

            # Filter out circuit-broken tools
            unavailable = self._circuit_breaker.get_unavailable_tools()
            active_tool_defs = [
                t for t in tool_defs
                if t.get("function", {}).get("name") not in unavailable
            ] if unavailable else tool_defs

            if force_no_tools:
                active_tool_defs = []
            elif iteration == self._max_iterations - 1 and iteration > 0 and active_tool_defs:
                # Final-answer forcing: the last iteration must produce a
                # conclusion instead of yet another tool round. Without this,
                # exhausting the loop discards the entire turn's work and the
                # user gets a generic failure. Strip tools and tell the model
                # to wrap up. The LOOP_EXHAUSTED notice is appended only when
                # this forced call actually delivers content — attaching it
                # here would pair "以上是目前的进展" with a turn that may end
                # in a budget halt or provider error and show no progress.
                active_tool_defs = []
                forcing_final = True
                forced_convergence = True
                messages.append({
                    "role": "user",
                    "content": (
                        "[系统] 已达到本轮工具调用上限,不能再调用工具。"
                        "请基于已收集到的信息直接给出最终结论;"
                        "如果任务尚未完成,请说明当前进展和剩余缺口。"
                    ),
                })

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

            _llm_started = time.monotonic()
            response, route_decision = await self._chat_stream_with_routing(
                messages=messages,
                tools=active_tool_defs if active_tool_defs else None,
                on_delta=on_delta,
                task_type=ctx.task_type,
                content=event.text,
            )

            # Surface model reasoning as a thinking event when the provider
            # returns reasoning content (real signal on LLMResponse).
            _reasoning = getattr(response, "reasoning_content", None)
            if _reasoning:
                await self._emit_thinking(
                    event, int((time.monotonic() - _llm_started) * 1000), _reasoning,
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
                _cost_before = self._cost_tracker.spent_usd
                await self._cost_tracker.record(
                    route_decision.model, response.usage, route_decision.provider_name,
                    channel=event.channel,
                )
                _u = response.usage if isinstance(response.usage, dict) else {}
                _turn_tokens = int(
                    _u.get("total_tokens")
                    or (int(_u.get("prompt_tokens") or _u.get("input_tokens") or 0)
                        + int(_u.get("completion_tokens") or _u.get("output_tokens") or 0))
                )
                await self._emit_cost(
                    event, _turn_tokens,
                    self._cost_tracker.spent_usd - _cost_before,
                    self._cost_tracker.spent_usd,
                    model=route_decision.model,
                    messages=messages,
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

            if response.finish_reason == "length":
                # Output hit max_tokens. Provider-level reasoning promotion may
                # already have recovered text into content; whatever we have is
                # partial. Never treat this as a clean stop.
                logger.warning(
                    "LLM output truncated at max_tokens in iteration {} (model {}, content_len {})",
                    iteration, route_decision.model, len(response.content or ""),
                )
                if response.content:
                    # Deliver the partial answer with a truncation notice
                    # instead of discarding it.
                    response_text = response.content
                    counters.degraded_notices.append(notice_for(REASON_OUTPUT_TRUNCATED))
                    if forcing_final:
                        counters.degraded_notices.append(notice_for(REASON_LOOP_EXHAUSTED))
                    loop_exhausted = False
                    break
                if not truncation_retried and iteration + 1 < self._max_iterations:
                    # Nothing recoverable — retry once without tools, asking
                    # for a bounded plain-text conclusion. One extra call
                    # salvages the whole turn's collected context. Requires a
                    # remaining iteration: on the last one, continue would fall
                    # off the range and the injected instruction would never
                    # be answered, so give up below instead.
                    truncation_retried = True
                    force_no_tools = True
                    messages.append({
                        "role": "user",
                        "content": (
                            "[系统] 你上一次的回复超出输出长度上限且未产生任何正文。"
                            "请不要调用工具,直接用简短的篇幅给出最终结论。"
                        ),
                    })
                    continue
                # Retry also truncated empty (or no iterations left to retry
                # in) — give up with the truncation notice.
                counters.degraded_notices.append(notice_for(REASON_OUTPUT_TRUNCATED))
                loop_exhausted = False
                break

            if response.content:
                response_text = response.content

            # Post-LLM interrupt checkpoint. An interrupt may have arrived DURING
            # the (long) LLM call above. Check here — BEFORE the has_tool_calls
            # branch — so BOTH paths are covered: a plain-text reply (the common
            # case, which breaks just below) and a tool-call batch (discarded
            # before it runs). Keep whatever text the model produced and mark the
            # turn user-stopped, not exhausted. Placed before the assistant
            # tool_calls message is appended, so pending calls are dropped cleanly
            # with no dangling tool_calls message to corrupt the next turn.
            if (
                self._interrupt is not None
                and self._interrupt.is_interrupted(getattr(event, "session_key", ""))
            ):
                notice = "⏹ 已按你的请求停止。"
                response_text = (
                    f"{response_text}\n\n{notice}" if response_text.strip() else notice
                )
                loop_exhausted = False
                interrupted = True
                break

            if not response.has_tool_calls:
                if forcing_final:
                    # The forced final call delivered its conclusion — now the
                    # "以上是目前的进展" notice is truthful. Gate on actual
                    # text so an empty forced reply doesn't claim progress
                    # that was never shown. The plan stays NOT complete: the
                    # model was cut off, not finished.
                    if response_text:
                        counters.degraded_notices.append(notice_for(REASON_LOOP_EXHAUSTED))
                elif ctx.execution_plan and not ctx.execution_plan.is_complete:
                    ctx.execution_plan.is_complete = True
                if ctx.activity is not None:
                    ctx.activity.set_generating()
                loop_exhausted = False
                break

            if response.content:
                await self._emit_progress(ctx, response.content)

            assistant_msg: dict[str, Any] = {"role": "assistant", "content": response.content}
            assistant_msg["tool_calls"] = [tc.to_openai_format() for tc in response.tool_calls]
            messages.append(assistant_msg)

            tool_call_fmts = [tc.to_openai_format() for tc in response.tool_calls]
            session.add_message("assistant", response.content or "", tool_calls=tool_call_fmts)

            await self._execute_tool_batch(
                ctx=ctx, response=response, messages=messages, session=session,
                trace_id=trace_id, iteration=iteration, event=event,
                repeat_tracker=_repeat_tracker, counters=counters,
            )

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
            total_tool_calls=counters.total_tool_calls,
            loop_exhausted=loop_exhausted,
            budget_halted=budget_halted,
            forced_convergence=forced_convergence,
            interrupted=interrupted,
            should_review_skills=counters.should_review_skills,
            should_review_memory=counters.should_review_memory,
            skill_iters=counters.skill_iters,
            memory_iters=counters.memory_iters,
            degraded_notices=counters.degraded_notices,
        )

    async def _run_pre_tool_hook(self, tool_call: Any, exec_ctx: Any) -> tuple[Any, Any]:
        """Dispatch the pre_tool_call hook chain.

        Mutates ``tool_call.arguments`` in place for any hook returning modified
        args (faithful to the original loop), and returns
        ``(cancel_reason_or_None, modified_args_or_None)``. A non-None cancel
        reason signals the caller to mark the decision BLOCKED.
        """
        hook_results = await self._hook_registry.dispatch(
            "pre_tool_call", tool_call.name, tool_call.arguments, exec_ctx,
        )
        modified_args = None
        for hr in hook_results:
            if hr.cancel:
                return hr.cancel_reason, modified_args
            if hr.modified is not None:
                tool_call.arguments = hr.modified
                modified_args = hr.modified
        return None, modified_args

    async def _execute_tool_batch(self, *, ctx, response, messages, session,
                                  trace_id, iteration, event, repeat_tracker, counters):
        """Execute one batch of tool_calls in three phases:

        A. serial decision (approval / repeat guard / pre_tool_call hook) ->
           a _Decision per tool_call;
        B. execution split into a concurrent read-only group and a serial group
           (Task 3: conc_idx is always empty so everything is serial);
        C. serial writeback in ORIGINAL tool_call order so message/session/span
           ordering and circuit/nudge counters stay byte-equivalent.

        Mutates ``messages``/``session`` in place and updates ``counters``.
        """
        _REPEAT_BLOCK_THRESHOLD = 4  # was a local in _run_tool_loop; value kept identical
        decisions: list[_Decision] = []

        # ---- Phase A: serial decision (approval / repeat / pre-hook) ----
        for tool_index, tool_call in enumerate(response.tool_calls):
            d = _Decision(tool_call=tool_call, index=tool_index, verdict="RUN")

            # Emit "Using tool" BEFORE approval so BLOCKED tools (denied /
            # repeat-guarded / hook-cancelled) still surface this progress,
            # matching the pre-refactor loop where it fired at the top of the
            # body. Phase B must NOT re-emit it for RUN tools.
            _friendly = friendly_activity(ActivitySnapshot(0.0, "calling_tool", tool_call.name))
            await self._emit_progress(ctx, _friendly, tool_hint=True)

            approval_check = await self._approval_gate.check(
                tool_call.name,
                tool_call.arguments,
                event.sender_id,
                channel=event.channel,
                event=event,
                running=True,
            )
            if approval_check.denial:
                d.verdict = "BLOCKED"
                d.blocked_message = approval_check.denial.text
                d.blocked_meta = {"success": False, "denied": True}
                if approval_check.notify_user and approval_check.notice:
                    counters.degraded_notices.append(approval_check.notice)
                decisions.append(d)
                continue
            d.approved = True

            # Circuit-breaker probe permit. Schema filtering above uses the
            # side-effect-free peek, so the OPEN→HALF_OPEN transition and the
            # half_open_max probe budget are enforced HERE, on the real call
            # path — without this acquire the recovery state machine never
            # runs and an opened circuit can only "recover" by accident.
            # Runs serially in Phase A so probe accounting has no races.
            if not self._circuit_breaker.is_available(tool_call.name):
                d.verdict = "BLOCKED"
                d.blocked_message = (
                    f"[Blocked] Tool '{tool_call.name}' is temporarily disabled "
                    "(circuit open after repeated failures). Try again later or "
                    "use a different approach."
                )
                d.blocked_meta = {"success": False, "circuit_open": True}
                logger.warning("Circuit breaker blocked tool call: {}", tool_call.name)
                decisions.append(d)
                continue

            # Repeat-call guard: count BEFORE executing so identical calls don't
            # keep firing side effects. The N-th identical call is short-circuited
            # with an error message instead.
            _call_key = f"{tool_call.name}:{hashlib.md5(str(sorted(tool_call.arguments.items())).encode()).hexdigest()[:8]}"
            repeat_tracker[_call_key] = repeat_tracker.get(_call_key, 0) + 1
            if repeat_tracker[_call_key] >= _REPEAT_BLOCK_THRESHOLD:
                d.verdict = "BLOCKED"
                d.blocked_message = (
                    f"[Blocked] Tool '{tool_call.name}' called with identical arguments "
                    f"{repeat_tracker[_call_key]} times. Stopping repeated calls — "
                    "vary the arguments or take a different action."
                )
                d.blocked_meta = {"success": False, "repeat_blocked": True}
                logger.warning(
                    "Blocked repeated tool call: {} ({}x)",
                    tool_call.name, repeat_tracker[_call_key],
                )
                counters.degraded_notices.append(notice_for(REASON_REPEAT_BLOCKED))
                decisions.append(d)
                continue

            d.exec_ctx = ToolExecutionContext(
                execution_id=uuid.uuid4().hex[:12],
                trace_id=trace_id,
                session_key=event.session_key,
                memory_scope=getattr(event, "memory_scope", ""),
                user_id=event.sender_id,
                attempt_index=0,
                idempotency_key=build_idempotency_key(trace_id, tool_call.name, tool_index, tool_call.arguments),
                credentials=self._credentials.get_for_tool(tool_call.name),
                approved_actions=approval_check.approved_actions,
                channel=event.channel,
                chat_id=event.chat_id,
                reply_to_id=event.reply_to_id or "",
            )

            # pre_tool_call hook (may cancel/modify); modifications applied in place
            if self._hook_registry and self._hook_registry.has_hooks("pre_tool_call"):
                cancelled, _modified = await self._run_pre_tool_hook(tool_call, d.exec_ctx)
                if cancelled:
                    d.verdict = "BLOCKED"
                    d.blocked_message = f"Blocked by plugin: {cancelled}"
                    d.blocked_meta = {"success": False, "hook_cancelled": True}
                    decisions.append(d)
                    continue

            # classify for concurrency (Task 4 consumes this; harmless in Task 3)
            tool_obj = self._tools.get(tool_call.name)
            try:
                d.read_only = bool(tool_obj and tool_obj.execution_mode(tool_call.arguments) == "read_only")
            except Exception:
                d.read_only = False
            d.paths = extract_paths(tool_call.arguments)
            decisions.append(d)

        # ---- Phase B: split RUN decisions into concurrent / serial groups ----
        run_decisions = [d for d in decisions if d.verdict == "RUN"]
        results: dict[int, object] = {}  # index -> ToolResult (or Exception)

        if self._concurrency_enabled and self._max_concurrent > 1 and len(run_decisions) > 1:
            plans = [ToolPlan(index=d.index, name=d.tool_call.name,
                              read_only=d.read_only, paths=d.paths, approved=d.approved)
                     for d in run_decisions]
            conc_plans, _serial_plans = partition_concurrent(plans)
            conc_idx = {p.index for p in conc_plans}
        else:
            conc_idx = set()  # Task 3: everything serial. Task 4 flips this on.

        by_index = {d.index: d for d in run_decisions}

        # concurrent group (Task 3: conc_idx is empty, this never runs)
        if conc_idx:
            await self._emit_progress(
                ctx, f"并行执行 {len(conc_idx)} 个只读工具", tool_hint=True,
            )
            sem = asyncio.Semaphore(self._max_concurrent)

            async def _run_one(d):
                async with sem:
                    # Emit a "running" frame before execution so the cli TUI can
                    # flip this tool line into an in-progress state; the terminal
                    # frame below shares tool_call_id to pair with it.
                    await self._emit_tool_call(
                        ctx.event, d.tool_call.name, d.tool_call.arguments,
                        "running", "", tool_call_id=d.tool_call.id,
                    )
                    result = await self._tools.execute(
                        d.tool_call.name, d.tool_call.arguments, d.exec_ctx)
                    # post_tool_call hook — kept consistent with the serial group
                    # below so concurrently-executed read-only tools still honor
                    # the hook contract (e.g. evolution trajectory recording).
                    # `result` is a local var per decision; the gather'd tasks
                    # share no mutable state here. If a hook raises, it propagates
                    # out and is captured by gather(return_exceptions=True).
                    if self._hook_registry and self._hook_registry.has_hooks("post_tool_call"):
                        result = await self._hook_registry.dispatch_modify(
                            "post_tool_call", result, d.tool_call.name,
                            d.tool_call.arguments, d.exec_ctx,
                        )
                    return result

            conc_order = list(conc_idx)
            gathered = await asyncio.gather(
                *[_run_one(by_index[i]) for i in conc_order],
                return_exceptions=True,
            )
            for i, res in zip(conc_order, gathered):
                results[i] = res
                # Emit a tool_call frame per concurrent tool too — the serial
                # group emits inline, so without this the cli TUI would drop
                # every concurrently-executed tool once Task 4 turns concurrency
                # on. Exceptions surface as status="err".
                d = by_index[i]
                if isinstance(res, BaseException):
                    await self._emit_tool_call(
                        ctx.event, d.tool_call.name, d.tool_call.arguments,
                        "err", f"{type(res).__name__}: {res}"[:500],
                        tool_call_id=d.tool_call.id,
                    )
                else:
                    await self._emit_tool_call(
                        ctx.event, d.tool_call.name, d.tool_call.arguments,
                        "ok" if res.success else "err", res.text,
                        tool_call_id=d.tool_call.id,
                        result_meta=dict(res.metadata) if res.metadata else None,
                    )

        # serial group (runs after concurrent batch; preserves read-before-write
        # ordering because overlapping readers were demoted into this group).
        # Per-tool progress/started/done events and the post_tool_call hook are
        # kept here so serial execution stays behavior-equivalent to the original
        # loop.
        for d in run_decisions:
            if d.index in conc_idx:
                continue
            tool_call = d.tool_call

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
            if ctx.activity is not None:
                ctx.activity.enter_tool(tool_call.name)
            await self._emit_tool_event(ctx, _tool_start_meta)
            # Emit a "running" frame before execution so the cli TUI can flip
            # this tool line into an in-progress state; the terminal frame below
            # shares tool_call_id to pair with it.
            await self._emit_tool_call(
                ctx.event, tool_call.name, tool_call.arguments,
                "running", "", tool_call_id=tool_call.id,
            )
            await self._prepare_clarify(tool_call, ctx.event)

            try:
                result = await self._tools.execute(
                    tool_call.name, tool_call.arguments, d.exec_ctx)

                # post_tool_call hook
                if self._hook_registry and self._hook_registry.has_hooks("post_tool_call"):
                    result = await self._hook_registry.dispatch_modify(
                        "post_tool_call", result, tool_call.name, tool_call.arguments, d.exec_ctx,
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
                await self._emit_tool_call(
                    ctx.event, tool_call.name, tool_call.arguments,
                    "ok" if result.success else "err", result.text,
                    tool_call_id=tool_call.id,
                    result_meta=dict(result.metadata) if result.metadata else None,
                    duration_ms=_tool_duration_ms,
                )
                if ctx.activity is not None:
                    ctx.activity.exit_tool()

                results[d.index] = result
            except BaseException as exc:  # noqa: BLE001 — recorded per-tool in Phase C
                # Fail-fast: mirror the original serial loop's `raise` on the
                # first tool exception. Record this failure but DO NOT run any
                # later serial tool — their side effects must not fire once a
                # sibling has crashed. Phase C raises at this decision's index
                # (in original order) before reaching the unexecuted ones.
                # Emit a terminal tool_call frame so the cli TUI doesn't leave
                # the "started" progress hanging with no resolution — a real
                # crash was previously invisible in the cognitive stream.
                if ctx.activity is not None:
                    ctx.activity.exit_tool()
                await self._emit_tool_call(
                    ctx.event, tool_call.name, tool_call.arguments,
                    "err", f"{type(exc).__name__}: {exc}"[:500],
                    tool_call_id=tool_call.id,
                )
                results[d.index] = exc
                break

        # ---- Phase C: serial writeback in ORIGINAL tool_call order ----
        for d in decisions:
            tool_call = d.tool_call
            tool_span = self._tracer.start_span(
                trace_id, f"tool_{iteration}_{d.index}", f"tool:{tool_call.name}", "tool_call")

            if d.verdict == "BLOCKED":
                messages.append({"role": "tool", "tool_call_id": tool_call.id,
                                 "name": tool_call.name, "content": d.blocked_message})
                session.add_message("tool", d.blocked_message,
                                    tool_call_id=tool_call.id, name=tool_call.name)
                self._tracer.end_span(tool_span, metadata=d.blocked_meta)
                counters.total_tool_calls += 1
                continue

            res = results.get(d.index)
            if isinstance(res, BaseException):
                # Every announced tool_call must get a paired tool message, and
                # the failure is counted by the circuit breaker. What differs is
                # whether the exception propagates:
                #   - concurrent group -> failure isolation: append an error tool
                #     message, record the failure, but DO NOT raise — sibling
                #     read-only tools have already completed and the batch must
                #     not be aborted by one tool crashing.
                #   - serial group -> fail-fast: preserve the original loop's
                #     behavior and re-raise so later writeback is skipped.
                # A bare asyncio.CancelledError must NEVER be swallowed
                # regardless of source: a cancellation signal is not an ordinary
                # tool failure and must always propagate.
                err_text = "Tool execution interrupted before producing a result."
                messages.append({"role": "tool", "tool_call_id": tool_call.id,
                                 "name": tool_call.name, "content": err_text})
                try:
                    session.add_message("tool", err_text, tool_call_id=tool_call.id, name=tool_call.name)
                except Exception as e:
                    logger.debug("Failed to record interrupted-tool message for {}: {}", tool_call.name, e)
                try:
                    self._tracer.end_span(tool_span, error="interrupted")
                except Exception as e:
                    logger.debug("Failed to end span for interrupted tool {}: {}", tool_call.name, e)
                try:
                    self._circuit_breaker.record_failure(tool_call.name)
                except Exception as e:
                    logger.debug("Failed to record circuit-breaker failure for {}: {}", tool_call.name, e)
                counters.total_tool_calls += 1
                if isinstance(res, asyncio.CancelledError):
                    raise res
                if d.index in conc_idx:
                    # Isolated concurrent-group failure: keep writing back the
                    # remaining decisions instead of aborting the batch.
                    continue
                raise res

            result = res
            result_text = result.text
            if len(result_text) > self._MAX_TOOL_RESULT_CHARS:
                result_text = result_text[:self._MAX_TOOL_RESULT_CHARS] + "\n...(truncated)"

            self._tracer.end_span(tool_span, metadata={"success": result.success})

            if self._telemetry and self._telemetry.available:
                from echo_agent.observability.spans import start_tool_span, end_tool_span
                otel_tool = start_tool_span(self._telemetry.get_tracer(), tool_call.name)
                end_tool_span(otel_tool, error=None if result.success else result.error)

            messages.append({"role": "tool", "tool_call_id": tool_call.id,
                             "name": tool_call.name, "content": result_text})
            session.add_message("tool", result_text, tool_call_id=tool_call.id, name=tool_call.name)

            counters.total_tool_calls += 1
            counters.skill_iters += 1
            counters.memory_iters += 1

            # Per-tool circuit breaker. Only infrastructure failures (timeout/
            # dependency/internal) count toward opening the circuit — validation
            # and business failures are part of normal interaction; counting
            # them lets one session's bad arguments disable the tool for every
            # session (the breaker is process-global).
            if result.success:
                self._circuit_breaker.record_success(tool_call.name)
            elif result.is_infra_failure:
                self._circuit_breaker.record_failure(tool_call.name)

            if (self._nudge_interval > 0 and counters.skill_iters >= self._nudge_interval
                    and self._tools.has("skill_manage")):
                counters.should_review_skills = True
                counters.skill_iters = 0
            if (self._memory_nudge_interval > 0 and counters.memory_iters >= self._memory_nudge_interval
                    and self._tools.has("memory")):
                counters.should_review_memory = True
                # Counter NOT reset here — see run(): it is zeroed only after the
                # background review succeeds, so a failed review re-triggers next
                # turn instead of dropping this batch permanently.

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
