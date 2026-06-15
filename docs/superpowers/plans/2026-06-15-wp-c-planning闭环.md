# WP-C：Planning 反思闭环实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 InferenceStage 补入轻量反思闭环——循环结束后对多步 plan 调 `planner.reflect()`，结果显示需重跑时最多重跑 1 轮，同时清理假打勾并修正类型注解（P1 #1）。

**Architecture:** 纯增量：抽取 `_run_tool_loop` helper + `_LoopResult` dataclass（零行为变更，等价性靠现有 5 个测试证明）；再在 `run()` 里串接反思—重跑编排；loop.py 注入 planner。

**Tech Stack:** Python 3.11、asyncio、pytest（`python -m pytest`，ruff lint）。

**Spec:** `docs/superpowers/specs/2026-06-15-wp-c-planning闭环-design.md`

---

## 背景

对应评估报告 P1 #1：当前 planning 创建了 `Plan` 但执行端是自主 ReAct 循环，循环结束后没有任何"反思—验证—重跑"闭环，`Plan` 实际上只是装饰。同时 `inference_stage.py:191-192` 存在"假打勾"——用 `iteration` 当 step 下标盲目 `mark_step_complete`，下标语义与真实 plan 步骤无关。

2026 的架构判断：自主 ReAct 是正确底座（2093 个测试依赖它，运行良好），不应改回固定 Plan-Execute。正确的补充是在循环**结束后**接入轻量反思（generator-verifier gap：验证比规划更可靠），仅在多步 plan 上触发，最多重跑 1 轮。

## 设计决策（已与用户确认）

1. **方向**：保留自主 ReAct，循环结束后接 `planner.reflect()`，`should_replan` 为真时最多重跑 1 轮。
2. **触发条件**：`planner is not None AND ctx.execution_plan is not None AND len(ctx.execution_plan.steps) > 1`。单步/无 plan 不反思（零额外开销）。
3. **重跑机制**：把 `feedback.critique` + `feedback.suggestions` 拼成一条 `user` 引导消息追加进 `messages`，再调一次工具循环 helper。第二轮后硬上限，不再反思。
4. **实现手法**：把 `inference_stage.py` 的工具循环抽成 `_run_tool_loop` helper；把闭包 `_emit_progress`/`_emit_tool_event` 提升为接收 `ctx` 的实例方法。
5. **清理**：删除 `inference_stage.py:191-192` 的假打勾。
6. **注入**：`loop.py` 构造 `InferenceStage` 时传 `planner=self.planner`；`__init__` 增加 `planner: AgentPlanner | None = None`。
7. **配置**：复用已有的 `PlanningConfig.reflection_enabled`（默认 True），不加新配置；重跑上限 1 硬编码。
8. **类型修正**：`types.py` 把悬空注解 `ExecutionPlan` 改为真实的 `Plan`。

## 计数器边界（关键，避免行为漂移）

- **tool-based 计数器**（`_skill_iters`/`_memory_iters`）：进 helper，跨两轮**累加**（工具确实多调了，频率应反映这一点）。helper 从 `session.metadata` 读起始值，返回最终值，**不写回** session。
- **turn-based memory review**（当前 L406-410 的 `_memory_turns += 1`）：**留在 `run()`**，整个用户 turn 只触发一次。反思重跑是同一 turn 内的二次推理，不应让它 +2。
- `run()` 在所有 helper 调用结束后，用最终计数统一写回 `session.metadata`。

## 等价性验证策略

Task 2（纯重构，零行为变更）完成后，`tests/test_inference_stage.py` 现有 5 个测试类必须**全部不改动地通过**，作为重构等价性的证明。

## 测试命令约定

项目 editable install 指向旧路径，直接 `pytest` 会 `ModuleNotFoundError`。**一律用 `python -m pytest`**（把 cwd 放进 sys.path）。

---

## Task 1：types.py 注解修正

最小独立改动，修正悬空类型注解（`ExecutionPlan` 类不存在，真实对象是 `Plan`）。

- [ ] 编辑 `echo_agent/agent/pipeline/types.py:9`，把
  ```python
      from echo_agent.agent.planning.models import ExecutionPlan
  ```
  改为
  ```python
      from echo_agent.agent.planning.models import Plan
  ```
- [ ] 编辑 `echo_agent/agent/pipeline/types.py:28`，把
  ```python
      execution_plan: ExecutionPlan | None = None
  ```
  改为
  ```python
      execution_plan: Plan | None = None
  ```
- [ ] 运行 `python -m pytest tests/test_inference_stage.py -q`，确认导入无误、现有测试仍通过。
- [ ] 运行 `python -m ruff check echo_agent/agent/pipeline/types.py`，确认无 lint 错误。
- [ ] 提交：`git add echo_agent/agent/pipeline/types.py && git commit -m "修正 PipelineContext.execution_plan 的类型注解为 Plan"`

---

## Task 2：抽取 _run_tool_loop helper + 提升 emit 方法（纯重构）

把 `run()` 里的工具循环整体抽进 `_run_tool_loop`，闭包提升为实例方法。**零行为变更**，靠现有 5 个测试证明等价。

### 2.1 新增 `_LoopResult` dataclass

- [ ] 在 `inference_stage.py` 顶部 import 区（第 8 行 `from typing import...` 之后）确认有 `from dataclasses import dataclass`，没有则加上。
- [ ] 在 `class InferenceStage` 定义**之前**（约第 30 行）新增内部结果容器：
  ```python
  @dataclass
  class _LoopResult:
      """一次工具循环的产出，供 run() 编排反思重跑时合并。"""
      response_text: str = ""
      total_tool_calls: int = 0
      loop_exhausted: bool = True
      should_review_skills: bool = False
      should_review_memory: bool = False
      skill_iters: int = 0
      memory_iters: int = 0
  ```
  注意：`_LoopResult` **不含** `memory_turns`——turn-based 计数留在 `run()`。

### 2.2 提升 `_emit_progress` / `_emit_tool_event` 为实例方法

- [ ] 在 `set_hook_registry` 之后（约 L73）新增两个实例方法，签名接收 `ctx`，方法体从原闭包搬来，把闭包捕获的 `event` 改为 `ctx.event`：
  ```python
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
  ```

### 2.3 抽取 `_run_tool_loop`

- [ ] 新增方法 `_run_tool_loop`，签名：
  ```python
  async def _run_tool_loop(self, ctx: PipelineContext, messages: list[dict[str, Any]]) -> _LoopResult:
  ```
- [ ] 方法体：把原 `run()` 中 **L109-399** 的逻辑搬入（从 `# Standard inference loop` 到 `loop_exhausted` 的 warning 块结束），但做如下调整：
  - 开头解构 `event = ctx.event`、`session = ctx.session`、`trace_id = ctx.trace_id`、`tool_defs = ctx.tool_defs`、`stream_publisher = ctx.stream_publisher`、`on_delta = stream_publisher.on_delta if ctx.publish_response else None`。
  - **nudge 计数器只读起始值**：`_skill_iters = session.metadata.get("_nudge_tool_iters_skill", 0)`、`_memory_iters = session.metadata.get("_nudge_tool_iters_memory", 0)`。**不读 `_memory_turns`**，**循环内不写回 session.metadata**。
  - 所有 `await _emit_progress(...)` 改为 `await self._emit_progress(ctx, ...)`；所有 `await _emit_tool_event(...)` 改为 `await self._emit_tool_event(ctx, ...)`。
  - **删除假打勾**：原 L191-192 的
    ```python
    if ctx.execution_plan and iteration < len(ctx.execution_plan.steps):
        ctx.execution_plan.mark_step_complete(iteration, response.content or "")
    ```
    整段删除。
  - 保留 L186-188 的正常退出逻辑（`if not response.has_tool_calls:` 里把 `ctx.execution_plan.is_complete = True`）。
  - 把原 L395-401 的 `if loop_exhausted:` warning 块**留在 helper 内**（它依赖循环局部的 `loop_exhausted`/`response_text`）。
  - 方法末尾 `return _LoopResult(response_text=response_text, total_tool_calls=total_tool_calls, loop_exhausted=loop_exhausted, should_review_skills=should_review_skills, should_review_memory=should_review_memory, skill_iters=_skill_iters, memory_iters=_memory_iters)`。

### 2.4 改写 `run()` 调用 helper（本任务仅单轮，等价于旧行为）

- [ ] 把 `run()` 方法体替换为（**本任务先不接反思**，保证纯等价）：
  ```python
  async def run(self, ctx: PipelineContext) -> InferenceResult:
      """Execute the inference loop, returning the final result."""
      session = ctx.session
      messages = ctx.messages

      loop_result = await self._run_tool_loop(ctx, messages)

      # Turn-based memory review trigger: fires even for pure chat (no tool calls).
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

      return InferenceResult(
          response_text=loop_result.response_text,
          total_tool_calls=loop_result.total_tool_calls,
          should_review_skills=loop_result.should_review_skills,
          should_review_memory=should_review_memory,
      )
  ```
  注意：turn-based memory 触发逻辑搬回 `run()`，且 `should_review_memory` 以 helper 结果为起点再叠加 turn 触发——与旧版完全等价。

### 2.5 等价性验证

- [ ] 运行 `python -m pytest tests/test_inference_stage.py -q`，**5 个测试类全部通过且未改动测试代码**。若有失败，说明重构引入了行为差异，必须修到通过。
- [ ] 运行 `python -m ruff check echo_agent/agent/pipeline/inference_stage.py`，确认无 lint 错误（特别注意未使用的 import / 局部变量）。
- [ ] 提交：`git add echo_agent/agent/pipeline/inference_stage.py && git commit -m "抽取工具循环为 _run_tool_loop 并提升进度事件为实例方法"`

---

## Task 3：注入 planner + 实现反思闭环

接线 planner，在多步 plan 上接入反思重跑。

### 3.1 InferenceStage 接受 planner

- [ ] `inference_stage.py` 的 `TYPE_CHECKING` 块（约 L19-28）新增：
  ```python
      from echo_agent.agent.planning.planner import AgentPlanner
  ```
- [ ] `__init__` 参数列表末尾（`max_iterations: int,` 之后）新增：
  ```python
          planner: AgentPlanner | None = None,
  ```
- [ ] `__init__` 方法体末尾（约 L68 之后）新增：`self._planner = planner`

### 3.2 run() 接入反思重跑

- [ ] 在 `run()` 里，把 `loop_result = await self._run_tool_loop(ctx, messages)` 之后、turn-based memory 触发**之前**，插入反思编排：
  ```python
      # 反思闭环：仅在多步 plan 上触发，最多重跑 1 轮
      if (
          self._planner is not None
          and ctx.execution_plan is not None
          and len(ctx.execution_plan.steps) > 1
      ):
          feedback = await self._planner.reflect(
              ctx.execution_plan, [loop_result.response_text]
          )
          if feedback.should_replan:
              guidance = (
                  "[Reflection] 上一轮回复可能未完全达成目标。\n"
                  f"评估意见：{feedback.critique}"
              )
              if feedback.suggestions:
                  sug = "\n".join(f"- {s}" for s in feedback.suggestions)
                  guidance += f"\n建议：\n{sug}"
              guidance += "\n请据此改进并完成任务。"
              messages.append({"role": "user", "content": guidance})

              # 第二轮重跑（helper 会从 session.metadata 重新读取 nudge 起始值，
              # 而此时 session 尚未写回，故起始值与第一轮一致——计数不重复累计）
              second = await self._run_tool_loop(ctx, messages)
              loop_result = _LoopResult(
                  response_text=second.response_text or loop_result.response_text,
                  total_tool_calls=loop_result.total_tool_calls + second.total_tool_calls,
                  loop_exhausted=second.loop_exhausted,
                  should_review_skills=loop_result.should_review_skills or second.should_review_skills,
                  should_review_memory=loop_result.should_review_memory or second.should_review_memory,
                  skill_iters=second.skill_iters,
                  memory_iters=second.memory_iters,
              )
  ```
  说明：`total_tool_calls` 两轮相加（真实调用次数）；`skill_iters`/`memory_iters` 取第二轮结果（第二轮 helper 已基于同一起始值累计了两轮工具调用，因为第一轮没写回 session）；`response_text` 第二轮优先、空则回退第一轮。

### 3.3 loop.py 注入 planner

- [ ] 编辑 `echo_agent/agent/loop.py` 的 `InferenceStage(...)` 构造（L264-278），在 `max_iterations=self._max_iterations,` 之后新增一行：
  ```python
              planner=self.planner,
  ```

### 3.4 新增反思闭环测试

- [ ] 在 `tests/test_inference_stage.py` 末尾新增测试类 `TestInferenceStageReflection`。先扩展 `_make_stage` 支持可选 planner：把签名改为 `def _make_stage(provider=None, tools=None, approval_gate=None, max_iterations=10, planner=None):`，并在 `InferenceStage(...)` 构造里加 `planner=planner,`。现有 5 个测试不传该参数，默认 `None`，行为不变。
- [ ] 新增辅助构造多步 plan 的 ctx（在测试类内联即可）：
  ```python
  from echo_agent.agent.planning.models import Plan, PlanStep, StrategyType
  from echo_agent.agent.planning.models import Feedback

  def _make_multistep_plan():
      return Plan(
          strategy=StrategyType.PLAN_EXECUTE,
          steps=[PlanStep(index=0, description="a"), PlanStep(index=1, description="b")],
          goal="multi",
      )
  ```
- [ ] 测试 A：`test_reflection_triggers_rerun_when_should_replan` —— provider 第一次返回 `LLMResponse(content="partial", finish_reason="stop")`，第二次返回 `LLMResponse(content="final", finish_reason="stop")`（用 `side_effect=[...]`）；planner = `AsyncMock`，`planner.reflect = AsyncMock(return_value=Feedback(should_replan=True, critique="incomplete", suggestions=["do x"]))`；ctx.execution_plan = 多步 plan。断言：`result.response_text == "final"`；`planner.reflect` 被调用 1 次；`provider.chat_stream_with_retry` 被调用 2 次；`messages` 中新增了含 `[Reflection]` 的 user 消息。
- [ ] 测试 B：`test_reflection_no_rerun_when_satisfied` —— `planner.reflect` 返回 `Feedback(should_replan=False)`；断言 provider 只调 1 次，`response_text` 为第一轮内容。
- [ ] 测试 C：`test_reflection_skipped_for_single_step_plan` —— execution_plan 只有 1 个 step；断言 `planner.reflect` **未被调用**（`assert not planner.reflect.called`）。
- [ ] 测试 D：`test_reflection_skipped_when_no_planner` —— planner 不传（None）、execution_plan 为多步；断言只调 1 次 provider（无反思分支）。
- [ ] 运行 `python -m pytest tests/test_inference_stage.py -q`，全部通过。
- [ ] 运行 `python -m ruff check echo_agent/agent/pipeline/inference_stage.py echo_agent/agent/loop.py tests/test_inference_stage.py`。
- [ ] 提交：`git add -A && git commit -m "InferenceStage 接入多步 plan 反思重跑闭环"`

---

## Task 4：全量回归 + lint

- [ ] 运行 `python -m pytest -q`，确认全部通过（基线约 2093 passed，本次新增 4 个反思测试）。若有失败，定位是否本次改动引入，修到通过。
- [ ] 运行 `python -m ruff check echo_agent tests`，确认整体无新增 lint 错误。
- [ ] 如全绿，无新文件需提交则跳过；否则 `git add -A && git commit -m "WP-C 全量回归通过"`（一般 Task 1-3 已分别提交，此步通常无改动）。

---

## 完成标准

- 假打勾（旧 L191-192）已删除。
- `_run_tool_loop` / `_LoopResult` / 实例化的 emit 方法就位，现有 5 个测试未改动通过（重构等价性）。
- 多步 plan 上 `should_replan=True` 触发一次重跑；单步/无 plan/无 planner 不触发。
- 新增 4 个反思测试通过，全量回归绿。
- `types.py` 注解修正为 `Plan`。

