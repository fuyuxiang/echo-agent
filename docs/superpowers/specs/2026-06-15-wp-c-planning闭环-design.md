# WP-C：Planning 反思闭环 设计文档

**日期**：2026-06-15
**范围**：v0.3.0 路线图中 P1 核心能力修复的第三个工作包
**覆盖问题**：P1 #1（Planning 无执行闭环）

---

## 一、背景与架构判断

架构评估报告指出 Planning 模块的 `execute_step`/`reflect` 是死代码、与主链路脱节。核实真实代码后，得出一个比报告更精确、也更重要的判断：

**当前执行层已经是一个工作良好的自主 ReAct 循环**（inference_stage.py:125 的工具循环：LLM 自主决定调用工具、看结果、再决策、无工具调用即结束）。它跑过 2093 个测试。planning 模块那套 `execute_step`/`StepAction`/`strategy.step()` 是另一套平行的、未接线的"显式步骤控制器"。

**架构决策：WP-C 补"反思（Reflect）"，不接通"固定步骤执行（Plan-Execute）"。**

依据（2026 年 agent 架构视角）：
1. **验证比规划可靠**（generator-verifier gap）：模型校验自己输出的能力系统性强于一次性规划正确。事后反思押在更可靠的一侧；预先固定 plan 假设"现在就知道正确路径"，对强模型常不成立。
2. **顺应 test-time compute 趋势**：推理时多花算力做自我批评/重试，优于把流程写死。
3. **保留已验证可用的核心**：反思加在 ReAct 循环之外，不动循环内部，回归面小。

固定步骤执行（`execute_step`/`StepAction`）保留为未来可选模式，不在本轮接线。跨轮次目标编排属 WP-D。

### 本轮明确不做的事

- 固定 plan 步骤强制执行（`execute_step`/`strategy.step()`/`StepAction` 不接线）
- 多轮反思（重跑硬上限 1 次）
- plan 生成逻辑改动（保留现状）
- 新增配置项（复用现有 `PlanningConfig`）
- 真实 LLM 的 E2E 反思测试
- 其余 P1（WP-D 独立）

---

## 二、当前现状（核实结论）

- **plan 会生成但只当提示文字**：context_stage.py:231 调 `create_plan()`；仅当 `steps > 1` 时把 `plan.to_prompt()` 拼进 prompt 末尾（context_stage.py:239-247）。
- **执行完全自主**：inference_stage.py:125 工具循环不读 plan 步骤，LLM 自主驱动。
- **plan 在执行期只被假打勾**：inference_stage.py:191-192 用循环轮次号 `iteration` 当索引调 `mark_step_complete`，轮次与 plan 步骤不对应，是装饰性假数据。
- **`execute_step`/`reflect` 从无调用者**：全仓 grep 确认。
- **InferenceStage 拿不到 planner**：planner 仅注入 ContextStage（loop.py:256），InferenceStage 只能访问 `ctx.execution_plan`（一个 `Plan` 数据对象）。
- **现成可用**：`AgentPlanner.reflect(plan, results) -> Feedback`（planner.py:57，reflection 关闭时返回 `Feedback(score=0.5, should_replan=False)`）；`ReflectionModule.critique`（reflection.py:30）。

---

## 三、关键设计决策（已逐项确认）

| # | 决策 | 选择 |
|---|---|---|
| 1 | 闭环方向 | 轻量反思闭环（补 Reflect，不接固定步骤执行） |
| 2 | 反思触发 | 仅对多步 plan 请求（复用 context_stage 的 `steps>1` 判据） |
| 3 | 重跑预算 | 最多重跑 1 轮，把 critique/suggestions 喂回；之后不再反思 |
| 4 | 实现方式 | 抽出工具循环为 helper，run() 编排反思+重跑 |
| 5 | 配置 | 复用 `PlanningConfig.reflection_enabled`，不新增 |

---

## 四、详细设计

### 4.1 架构与控制流

**注入 planner**：loop.py:264 构造 InferenceStage 时增加 `planner=self.planner`（可选）；`InferenceStage.__init__` 增加 `planner: AgentPlanner | None = None`。planning 关闭时 planner=None，反思整段跳过，行为等价现状。

**新 run() 控制流**：

```
run(ctx):
    messages = ctx.messages
    result = await self._run_tool_loop(ctx, messages)        # 第一轮，逻辑同现状

    if self._should_reflect(ctx):
        feedback = await self._planner.reflect(plan, [result.response_text])
        if feedback.should_replan and rerun_budget > 0:
            messages.append(_build_reflection_message(feedback))
            result = await self._run_tool_loop(ctx, messages) # 第二轮，预算耗尽
            # 第二轮后不再反思（硬上限 1）

    return InferenceResult(...)
```

**`_run_tool_loop(ctx, messages)`**：内部即现 inference_stage.py:125-393 的 `for iteration in range(max_iterations)` 循环体（LLM 调用、工具执行、ApprovalGate、熔断、重复守卫、nudge 全部原样），返回 `(response_text, total_tool_calls, exhausted, should_review_skills, should_review_memory)`。**循环体逻辑一字不改，只是位移进方法。**

**约束**：
- planner=None → 不进反思分支，等价现状。
- `planner.reflect()` 在 reflection 关闭时返回 `should_replan=False`，永不误触发重跑。
- 重跑上限硬编码 1，杜绝无限循环。

### 4.2 触发条件

```python
def _should_reflect(self, ctx) -> bool:
    return (
        self._planner is not None
        and ctx.execution_plan is not None
        and len(ctx.execution_plan.steps) > 1
    )
```

`steps > 1` 与 context_stage.py:239 注入 plan 提示的判据一致：被判为多步、已把 plan 提示喂给模型的请求才反思。简单问答/闲聊（单步 ReAct，无 plan 注入）零额外成本。

### 4.3 反思输入与反馈回灌

**输入 results**：传第一轮最终回复作单元素列表 `[result.response_text]`。`ReflectionModule.critique` 把 plan 摘要与 results 拼进 prompt 判断目标是否达成；关心的是"最终回复是否解决原始诉求"，传最终回复最贴切，不传逐工具结果（噪声大）。

**反馈回灌消息**（`should_replan=True` 时追加进 messages，user 角色）：

```
[Reflection] 你的上一轮回复可能未完全达成目标。
评估意见：{feedback.critique}
建议：
- {suggestion 1}
- {suggestion 2}
请据此改进并完成任务。
```

`suggestions` 为空时只含 critique。第二轮工具循环看到引导后**仍自主决定如何补救**——反思只提供方向，不强制步骤。

**边界**：reflect 调用抛异常 → inference_stage 在调用处 try/except 兜底（记 warning、按 `should_replan=False` 处理，返回第一轮结果）。这层保护放在调用边界而非依赖 `planner` 内部实现：`planner` 是注入依赖，不应假设它永不抛。`ReflectionModule.critique` 内部对 LLM 调用也有兜底，二者叠加确保"反思永不破坏已有结果"这一不变量结构上成立。

### 4.4 清理假打勾

删除 inference_stage.py:191-192 用 `iteration` 当索引的 `mark_step_complete(iteration, ...)`（轮次与步骤不对应的假数据）。

保留 inference_stage.py:186-187 现有的"正常退出（`not has_tool_calls`）时 `is_complete=True`"——这是正确的。reflect 用 `plan.is_complete` + `to_prompt()` 的目标描述 + 最终回复来判断，而非伪造的逐步打勾。循环耗尽（exhausted）时不标完成，反思能据此感知"没跑完"。

### 4.5 配置（复用现有，不新增）

`PlanningConfig.reflection_enabled`（schema.py，默认 True）已存在，`AgentPlanner` 构造时据此决定是否创建 ReflectionModule（planner.py:23）。

- `planning.enabled=False` → planner=None → 无反思（现状）
- `planning.enabled=True, reflection_enabled=False` → planner 存在，`reflect()` 返回 `should_replan=False` → 永不重跑，只有 plan 生成
- 两者 True → 完整反思闭环

重跑上限（1）硬编码，不做配置（YAGNI）。

### 4.6 plan 生成取舍（保留）

预先 plan 生成（context_stage.py:231）保留：(1) `to_prompt()` 给模型"建议步骤"提示，利于复杂任务首轮质量；(2) reflect 需要 plan 作为"原始目标"对照基准。本轮不动。

---

## 五、测试策略

### _run_tool_loop 抽取等价性（最关键回归保护）

抽取后行为须与抽取前完全等价：现有 `test_inference_stage.py`（5 个）+ 依赖工具循环的其它测试必须全绿、零修改。只是位移代码，这些测试不该变化——这是抽取正确的硬证据。

### 反思闭环新测试（mock provider/planner）

| 场景 | 设置 | 断言 |
|---|---|---|
| planner=None 不反思 | planner 不注入 | reflect 调用次数=0，helper 调用=1 |
| plan 为 None/单步不反思 | planner 存在，plan None 或 steps≤1 | 不反思 |
| 触发但不重跑 | 多步 plan，reflect 返回 should_replan=False | reflect 调 1 次，helper 调 1 次 |
| 触发且重跑 | 多步 plan，should_replan=True | helper 调 2 次，第二次 messages 含 critique 文本 |
| 重跑硬上限 | should_replan 恒 True | helper 最多调 2 次（无第三次） |
| reflect 异常兜底 | reflect mock 抛异常 | 不崩溃、不重跑、返回第一轮结果 |
| reflection 关闭 | reflect 返回 should_replan=False | 不重跑 |

### 清理验证

多步 plan 跑完场景，断言不再按轮次假打勾（plan.steps 的 status 不被错误索引污染）。

### 回归（硬约束）

- `python -m pytest` 全量通过（2093 基线 + 新增反思测试）
- `ruff check .` 通过
- inference/pipeline/planning 相关测试无回归

### 不做

不写真实跑两轮 + 真实 LLM 反思的 E2E——成本高，mock 已覆盖编排所有分支。

---

## 六、影响面与提交策略

### 改动文件清单

| 文件 | 操作 | 说明 |
|---|---|---|
| `echo_agent/agent/pipeline/inference_stage.py` | 修改 | 抽出 `_run_tool_loop`；run() 编排反思+重跑；增 planner 参数；删假打勾；加 `_should_reflect`/`_build_reflection_message` |
| `echo_agent/agent/loop.py:264` | 修改 | 构造 InferenceStage 时传 `planner=self.planner` |
| `echo_agent/agent/pipeline/types.py:9,28` | 修改（小） | `execution_plan` 注解 `ExecutionPlan` 修正为 `Plan`（悬空注解清理，顺手） |
| `tests/test_inference_stage.py` | 修改 | 新增反思闭环测试 |

### 提交策略（拆 2 个 commit）

1. 抽出 `_run_tool_loop` helper（纯重构，现有测试全绿验证等价）+ 修正 types.py 注解
2. 接入反思闭环（planner 注入、_should_reflect、reflect+重跑编排、删假打勾）+ 新测试

拆分理由：commit 1 是纯位移重构，可由"现有测试零修改全绿"独立验证；commit 2 才是新行为。两步分离便于定位回归。

每个 commit 前 `python -m pytest` + `ruff check .` 通过才提。

---

## 七、后续工作包

- **WP-D**：Goal/Objective 跨轮次编排层（P1 #4，路线图 v0.4.0）——跨请求的目标持久化与进度追踪，建立在本轮单次请求内闭环之上。
