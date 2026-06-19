# 消除"静默挂起":回合降级通知收敛层 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 agent 在审批受阻、审批超时、provider 抖动、重复调用被拦等异常终态下,始终向用户送达一条有意义的中文降级通知,消除"已读不回"的静默挂起。

**Architecture:** 在工具循环内收集"待通知降级事件",经四层结果对象(`_LoopResult` → `InferenceResult` → `ProcessResult` → `_ProcessResult`,后两者实为同一类)透传到 `AgentLoop._on_inbound` 出口的统一收敛点;收敛点保证无论回合产出什么,用户都收到一条 `is_final=True` 的中文消息。smart approval 在 provider 失灵时返回新状态 `unavailable`,快速失败而非阻塞等待。

**Tech Stack:** Python 3.11+,dataclass,asyncio,pytest,ruff,loguru。

## Global Constraints

- Python >= 3.11(`pyproject.toml`)。
- 代码注释、变量名用英文;面向用户的文案用简体中文。
- 提交前 `ruff check .` 与 `pytest` 必须通过(README "开发与贡献")。
- 不降低现有覆盖率(当前约 75%)。
- 不修改 session 锁逻辑(2.2 留作独立后续)。
- 不扩展 provider 重试/熔断框架,不改微信通道出站逻辑。
- 降级通知必须 `is_final=True`,否则被 `channels/base.py:56-68` 的 `should_deliver` 在无 edit 能力的通道(如微信)丢弃。
- `degraded_notices` 按原因去重,同一原因一回合只发一次。
- 防双发:正常回复已发出时,通知作为补充紧随其后,不重复发送主回复。

## 数据流总览

```
工具循环 (inference_stage._run_tool_loop)
  ├─ approval_check.denial.notify_user → 收集 denial.notice
  └─ repeat_blocked                    → 收集 repeat_blocked 文案
        ↓ _LoopResult.degraded_notices
推理阶段 (inference_stage.run → InferenceResult.degraded_notices)
        ↓
收尾阶段 (response_stage.finalize → ProcessResult.degraded_notices)
        ↓
出口 (loop._process_event → _ProcessResult.degraded_notices)
        ↓
收敛点 (loop._on_inbound):决定发什么、怎么发
```

## 文件结构

| 文件 | 责任 | 改动类型 |
|---|---|---|
| `echo_agent/security/smart_approval.py` | smart 预筛;新增 `unavailable` 状态 | Modify |
| `echo_agent/agent/approval_gate.py` | `ApprovalCheck` 加通知字段;处理 `unavailable`;超时 denial 带通知 | Modify |
| `echo_agent/agent/pipeline/inference_stage.py` | `_LoopResult` 加 `degraded_notices`;denial/repeat 冒泡;透传到 `InferenceResult` | Modify |
| `echo_agent/agent/pipeline/types.py` | `InferenceResult` 加 `degraded_notices` | Modify |
| `echo_agent/agent/pipeline/response_stage.py` | `ProcessResult` 加 `degraded_notices`;`finalize` 透传 | Modify |
| `echo_agent/agent/loop.py` | `_process_event` 透传;`_on_inbound` 收敛决策 + 中文文案 | Modify |
| `echo_agent/agent/degraded_notice.py` | 中文降级文案常量与合成函数(集中一处) | Create |
| `tests/test_approval_degraded_notice.py` | 不变量测试:每条静默路径都断言有通知 | Create |
| `tests/test_degraded_notice_copy.py` | 文案合成/去重单元测试 | Create |

---

### Task 1: 中文降级文案模块

集中存放面向用户的降级文案与合成/去重逻辑,后续所有任务从这里取文案,保证 DRY。

**Files:**
- Create: `echo_agent/agent/degraded_notice.py`
- Test: `tests/test_degraded_notice_copy.py`

**Interfaces:**
- Produces:
  - `REASON_APPROVAL_UNAVAILABLE: str = "approval_unavailable"`
  - `REASON_APPROVAL_TIMEOUT: str = "approval_timeout"`
  - `REASON_REPEAT_BLOCKED: str = "repeat_blocked"`
  - `GENERIC_FALLBACK_TEXT: str`(通用中文兜底句)
  - `notice_for(reason: str, *, tool: str = "", request_id: str = "") -> str` — 按原因返回中文文案;未知原因返回 `GENERIC_FALLBACK_TEXT`。
  - `combine_notices(notices: list[str]) -> str` — 按出现顺序去重后用换行拼接;空列表返回 `""`。

- [ ] **Step 1: 写失败测试**

`tests/test_degraded_notice_copy.py`:

```python
from __future__ import annotations

from echo_agent.agent.degraded_notice import (
    GENERIC_FALLBACK_TEXT,
    REASON_APPROVAL_TIMEOUT,
    REASON_APPROVAL_UNAVAILABLE,
    REASON_REPEAT_BLOCKED,
    combine_notices,
    notice_for,
)


def test_notice_approval_unavailable_is_chinese():
    text = notice_for(REASON_APPROVAL_UNAVAILABLE)
    assert "安全审批暂时不可用" in text
    assert text.startswith("⚠️")


def test_notice_approval_timeout_includes_tool_and_id():
    text = notice_for(REASON_APPROVAL_TIMEOUT, tool="exec", request_id="abc123")
    assert "exec" in text
    assert "abc123" in text
    assert "/approve" in text


def test_notice_repeat_blocked_is_chinese():
    text = notice_for(REASON_REPEAT_BLOCKED)
    assert "多次尝试" in text


def test_notice_unknown_reason_falls_back():
    assert notice_for("something_else") == GENERIC_FALLBACK_TEXT


def test_combine_dedupes_preserving_order():
    a = notice_for(REASON_APPROVAL_UNAVAILABLE)
    b = notice_for(REASON_REPEAT_BLOCKED)
    combined = combine_notices([a, b, a])
    assert combined.count(a) == 1
    assert combined.index(a) < combined.index(b)


def test_combine_empty_returns_empty():
    assert combine_notices([]) == ""
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `pytest tests/test_degraded_notice_copy.py -v`
Expected: FAIL,`ModuleNotFoundError: No module named 'echo_agent.agent.degraded_notice'`

- [ ] **Step 3: 写最小实现**

`echo_agent/agent/degraded_notice.py`:

```python
"""User-facing degraded-mode notices (Chinese copy) and helpers.

Single source of truth for the messages the agent sends when a turn cannot
produce a normal answer (approval blocked/timed out, repeated tool failures,
provider outage). Keeping the copy here avoids scattering user-facing strings
across the pipeline.
"""

from __future__ import annotations

REASON_APPROVAL_UNAVAILABLE = "approval_unavailable"
REASON_APPROVAL_TIMEOUT = "approval_timeout"
REASON_REPEAT_BLOCKED = "repeat_blocked"

GENERIC_FALLBACK_TEXT = (
    "⚠️ 处理你的请求时遇到问题,已中止。可以稍后重试或换个说法。"
)


def notice_for(reason: str, *, tool: str = "", request_id: str = "") -> str:
    """Return the Chinese user-facing notice for a degraded-mode reason."""
    if reason == REASON_APPROVAL_UNAVAILABLE:
        return (
            "⚠️ 这步需要执行命令,但安全审批暂时不可用(模型服务异常),已暂停。"
            "请稍后回复让我重试。"
        )
    if reason == REASON_APPROVAL_TIMEOUT:
        tool_label = tool or "该操作"
        rid = request_id or "<id>"
        return (
            f"⚠️ 这步需要你确认执行 `{tool_label}`,等待超时已暂停。"
            f"回复 `/approve {rid}` 继续,或 `/deny {rid}` 取消。"
        )
    if reason == REASON_REPEAT_BLOCKED:
        return (
            "⚠️ 我多次尝试同一操作未成功,已停止。可能是工具或服务异常,请稍后重试。"
        )
    return GENERIC_FALLBACK_TEXT


def combine_notices(notices: list[str]) -> str:
    """Dedupe (preserving first-seen order) and join notices with newlines."""
    seen: set[str] = set()
    ordered: list[str] = []
    for n in notices:
        if n and n not in seen:
            seen.add(n)
            ordered.append(n)
    return "\n".join(ordered)
```

- [ ] **Step 4: 运行测试,确认通过**

Run: `pytest tests/test_degraded_notice_copy.py -v`
Expected: PASS(6 passed)

- [ ] **Step 5: 提交**

```bash
git add echo_agent/agent/degraded_notice.py tests/test_degraded_notice_copy.py
git commit -m "新增降级通知文案模块:集中管理中文降级文案与去重合成"
```

---

### Task 2: smart approval 新增 `unavailable` 状态(provider 失灵快速失败)

精确区分两种非裁决结果:**provider 返回空/调用异常**(故障真实签名,如 "No embedding data received")返回新状态 `unavailable`;**非空但首词不是裁决词**(模型回了话只是没守格式)保持现有 `escalate`。前者让上层快速失败并通知用户,后者仍走人工审批。

**Files:**
- Modify: `echo_agent/security/smart_approval.py:37-81`
- Test: 扩展 `tests/test_security_new_features.py`(沿用其 MagicMock provider 惯例)

**Interfaces:**
- Consumes: `provider.chat_with_retry(...)` 返回带 `.content` 的对象(空或 None 表示 provider 无输出)。
- Produces: `smart_approve(...) -> Literal["approve", "deny", "escalate", "unavailable"]`
  - `unavailable`:provider 返回空/None,或调用抛异常。
  - `escalate`:模型显式 ESCALATE,或非空但首词无法识别(行为不变)。

- [ ] **Step 1: 写失败测试**

在 `tests/test_security_new_features.py` 末尾追加(与现有 `TestSmartApprovalParsing` 同风格):

```python
class TestSmartApprovalUnavailable:
    """Provider outage (empty/None/exception) → 'unavailable', not silent escalate."""

    @pytest.mark.asyncio
    async def test_empty_content_is_unavailable(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=""))
        result = await smart_approve("exec", "curl x", "test", provider)
        assert result == "unavailable"

    @pytest.mark.asyncio
    async def test_none_content_is_unavailable(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=None))
        result = await smart_approve("exec", "curl x", "test", provider)
        assert result == "unavailable"

    @pytest.mark.asyncio
    async def test_exception_is_unavailable(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(side_effect=RuntimeError("provider down"))
        result = await smart_approve("exec", "curl x", "test", provider)
        assert result == "unavailable"

    @pytest.mark.asyncio
    async def test_nonempty_unrecognized_still_escalates(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="I would APPROVE this but let me think")
        )
        result = await smart_approve("exec", "ls", "test", provider)
        assert result == "escalate"
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `pytest tests/test_security_new_features.py::TestSmartApprovalUnavailable -v`
Expected: FAIL,前三个用例返回 `escalate` 而非 `unavailable`。

- [ ] **Step 3: 改实现**

`echo_agent/security/smart_approval.py`,修改函数签名返回类型(行 44)与解析分支(行 66-81):

签名行(44)改为:
```python
) -> Literal["approve", "deny", "escalate", "unavailable"]:
```

将 `try` 块内 `raw_text` 解析段(原 66-81)替换为:
```python
        response = await provider.chat_with_retry(
            messages=[{"role": "user", "content": prompt}],
            model=model or None,
            max_tokens=16,
            temperature=0.0,
        )
        raw_text = (response.content or "").strip()
        if not raw_text:
            # Empty/None content is the signature of a provider outage
            # (e.g. "No embedding data received"). Fail closed but loud:
            # surface 'unavailable' so the gate can notify the user instead
            # of silently escalating into a blocking wait.
            logger.warning("Smart approval: empty response (provider unavailable) for '{}'", tool_name)
            return "unavailable"
        first_word = raw_text.split()[0].upper() if raw_text.split() else ""
        if first_word == "APPROVE":
            logger.info("Smart approval: APPROVE for '{}' — {}", tool_name, command[:100])
            return "approve"
        if first_word == "DENY":
            logger.warning("Smart approval: DENY for '{}' — {}", tool_name, command[:100])
            return "deny"
        if first_word == "ESCALATE":
            logger.info("Smart approval: ESCALATE for '{}' — {}", tool_name, raw_text[:50])
            return "escalate"
        logger.info("Smart approval: unrecognized response (escalating) for '{}' — {}", tool_name, raw_text[:50])
        return "escalate"
    except Exception as e:
        logger.warning("Smart approval failed (provider unavailable): {}", e)
        return "unavailable"
```

- [ ] **Step 4: 运行测试,确认通过**

Run: `pytest tests/test_security_new_features.py -v`
Expected: PASS(新 4 个用例 + 原有 `TestSmartApprovalParsing` 全绿,`test_embedded_approve_not_matched` 仍返回 escalate)。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/security/smart_approval.py tests/test_security_new_features.py
git commit -m "smart approval 在 provider 返回空/异常时返回 unavailable,快速失败而非静默 escalate"
```

---

### Task 3: ApprovalCheck 携带通知语义 + gate 处理 unavailable/超时

让 gate 产出的 denial 带上"是否需要通知用户"和"中文文案",供工具循环冒泡。覆盖两条审批 denial 路径:smart `unavailable` 与 manual flow 超时。

**Files:**
- Modify: `echo_agent/agent/approval_gate.py`(`ApprovalCheck` 行 20-23;Step 12 行 138-148;`_manual_approval_flow` 超时分支行 219-227)
- Test: 扩展 `tests/test_approval_gate_e2e.py`

**Interfaces:**
- Consumes: `smart_approve(...) -> "approve"|"deny"|"escalate"|"unavailable"`(Task 2);`degraded_notice.notice_for / REASON_*`(Task 1)。
- Produces:
  - `ApprovalCheck` 新增字段 `notify_user: bool = False`、`notice: str = ""`。
  - smart `unavailable` → 返回 `ApprovalCheck(denial=ToolResult(success=False, error=...), notify_user=True, notice=notice_for(REASON_APPROVAL_UNAVAILABLE))`,**不进入 manual flow**。
  - manual flow 超时 denial → 同一 `ApprovalCheck` 带 `notify_user=True`、`notice=notice_for(REASON_APPROVAL_TIMEOUT, tool=tool_name, request_id=approval_req.id)`。

- [ ] **Step 1: 写失败测试**

在 `tests/test_approval_gate_e2e.py` 末尾追加(复用文件内已有的 `_make_gate`/`load_config` 惯例;smart 路径需 provider,故单独构造):

```python
import pytest
from unittest.mock import AsyncMock, MagicMock
from echo_agent.agent.degraded_notice import (
    REASON_APPROVAL_UNAVAILABLE,
)


@pytest.mark.asyncio
async def test_smart_unavailable_sets_notify_user():
    cfg = load_config()
    cfg.permissions.approval.mode = "smart"
    bus = MessageBus()
    appr = ApprovalManager(require_approval=cfg.permissions.approval.require_approval)
    provider = MagicMock()
    provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=""))
    gate = ApprovalGate(
        config=cfg, approval=appr, inference=_FakeInference(), bus=bus, provider=provider,
    )
    event = InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="research")],
    )
    check = await gate.check("exec", {"command": "curl https://x"}, "u1", channel="weixin", event=event)
    assert check.denial is not None
    assert check.notify_user is True
    assert "安全审批暂时不可用" in check.notice
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `pytest tests/test_approval_gate_e2e.py::test_smart_unavailable_sets_notify_user -v`
Expected: FAIL,`AttributeError: 'ApprovalCheck' object has no attribute 'notify_user'`(或 `verdict=="unavailable"` 未处理而走进 manual flow)。

- [ ] **Step 3: 改实现**

3a. `ApprovalCheck` 数据类(行 20-23)改为:
```python
@dataclass
class ApprovalCheck:
    denial: ToolResult | None = None
    approved_actions: frozenset[str] = frozenset()
    notify_user: bool = False
    notice: str = ""
```

3b. 文件顶部 import 区(约行 17 之后)加入:
```python
from echo_agent.agent.degraded_notice import (
    REASON_APPROVAL_TIMEOUT,
    REASON_APPROVAL_UNAVAILABLE,
    notice_for,
)
```

3c. Step 12 smart 分支(行 138-148)在 `if verdict == "deny":` 之后增加 `unavailable` 处理:
```python
        # Step 12: Smart approval (EXEC level only)
        if risk == RiskLevel.EXEC and approval_cfg.mode == "smart" and self._provider:
            verdict = await self._run_smart_approval(tool_name, arguments, guard)
            if verdict == "approve":
                self._allowlist.approve(session_key, pattern_key, ApprovalLevel.SESSION)
                return _approved()
            if verdict == "deny":
                return ApprovalCheck(ToolResult(
                    success=False,
                    error=f"Smart approval denied '{tool_name}': {guard.reason or 'assessed as dangerous'}",
                ))
            if verdict == "unavailable":
                # Provider outage: fail closed but tell the user, instead of
                # falling through to a blocking manual wait that times out
                # silently. See spec 2.1.
                return ApprovalCheck(
                    denial=ToolResult(
                        success=False,
                        error=f"Approval system unavailable for '{tool_name}' (provider outage).",
                    ),
                    notify_user=True,
                    notice=notice_for(REASON_APPROVAL_UNAVAILABLE),
                )
```

3d. `_manual_approval_flow` 超时分支(行 219-227)改为带通知:
```python
        return ApprovalCheck(
            denial=ToolResult(
                success=False,
                error=(
                    f"Approval timed out for '{tool_name}'. "
                    f"Request id: {approval_req.id}. "
                    f"Reply `/approve {approval_req.id}` or `/deny {approval_req.id} <reason>`."
                ),
                metadata={"approval_request_id": approval_req.id},
            ),
            notify_user=True,
            notice=notice_for(REASON_APPROVAL_TIMEOUT, tool=tool_name, request_id=approval_req.id),
        )
```

- [ ] **Step 4: 运行测试,确认通过**

Run: `pytest tests/test_approval_gate_e2e.py -v`
Expected: PASS(新用例 + 原有用例全绿)。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/agent/approval_gate.py tests/test_approval_gate_e2e.py
git commit -m "ApprovalGate:smart unavailable 快速失败并标记通知,审批超时 denial 携带中文文案"
```

---

### Task 4: degraded_notices 在工具循环内收集并冒泡到 InferenceResult

在 `_run_tool_loop` 内收集两类降级事件(审批 denial 的 notice、repeat_blocked),经 `_LoopResult` → `InferenceResult` 透传。这是纯数据管道任务,不涉及发送。

**Files:**
- Modify: `echo_agent/agent/pipeline/inference_stage.py`(`_LoopResult` 行 36-48;`run()` 合并行 170-179 与返回 224-229;`_run_tool_loop` denial 行 369-378、repeat 行 385-406、返回 558-567)
- Modify: `echo_agent/agent/pipeline/types.py`(`InferenceResult` 行 35-41)
- Test: 扩展 `tests/test_inference_stage.py`

**Interfaces:**
- Consumes: `approval_check.notify_user`/`approval_check.notice`(Task 3);`degraded_notice.notice_for / REASON_REPEAT_BLOCKED`(Task 1)。
- Produces:
  - `_LoopResult.degraded_notices: list[str]`(默认空 list)。
  - `InferenceResult.degraded_notices: list[str]`(默认空 list)。

- [ ] **Step 1: 写失败测试**

在 `tests/test_inference_stage.py` 末尾追加(若文件无统一 helper,用最小直接断言数据类字段默认值,保证管道字段存在且可累计):

```python
def test_loopresult_has_degraded_notices_default():
    from echo_agent.agent.pipeline.inference_stage import _LoopResult
    r = _LoopResult()
    assert r.degraded_notices == []


def test_inferenceresult_has_degraded_notices_default():
    from echo_agent.agent.pipeline.types import InferenceResult
    r = InferenceResult()
    assert r.degraded_notices == []
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `pytest tests/test_inference_stage.py::test_loopresult_has_degraded_notices_default tests/test_inference_stage.py::test_inferenceresult_has_degraded_notices_default -v`
Expected: FAIL,`AttributeError`(字段不存在)。

- [ ] **Step 3: 改实现**

3a. `types.py` 的 `InferenceResult`(行 35-41)加字段(`field` 已 import,见行 5):
```python
@dataclass
class InferenceResult:
    """Output of the inference stage."""

    response_text: str = ""
    total_tool_calls: int = 0
    should_review_skills: bool = False
    should_review_memory: bool = False
    degraded_notices: list[str] = field(default_factory=list)
```

3b. `inference_stage.py` 行 8 当前是 `from dataclasses import dataclass`,改为 `from dataclasses import dataclass, field`。然后 `_LoopResult`(行 36-48)末尾加字段:
```python
    skill_iters: int = 0
    memory_iters: int = 0
    degraded_notices: list[str] = field(default_factory=list)
```

3c. `_run_tool_loop` 顶部(约行 244,`response_text = ""` 附近)初始化收集器:
```python
        degraded_notices: list[str] = []
```

3d. denial 分支(行 369-378)在 `continue` 前收集 notice:
```python
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
```

3e. repeat 分支(行 385-406)在 `continue` 前收集 repeat 文案。先确认顶部 import 区有:
```python
from echo_agent.agent.degraded_notice import REASON_REPEAT_BLOCKED, notice_for
```
然后在 `total_tool_calls += 1`(行 405)之前加:
```python
                        degraded_notices.append(notice_for(REASON_REPEAT_BLOCKED))
```

3f. `_run_tool_loop` 的 `return _LoopResult(...)`(行 558-567)加上 `degraded_notices=degraded_notices`:
```python
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
```

3g. `run()` 的反思重跑合并(行 170-179)拼接两轮 notices:
```python
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
```

3h. `run()` 的 `return InferenceResult(...)`(行 224-229)透传:
```python
        return InferenceResult(
            response_text=loop_result.response_text,
            total_tool_calls=loop_result.total_tool_calls,
            should_review_skills=loop_result.should_review_skills,
            should_review_memory=should_review_memory,
            degraded_notices=loop_result.degraded_notices,
        )
```

- [ ] **Step 4: 运行测试,确认通过**

Run: `pytest tests/test_inference_stage.py -v`
Expected: PASS(新 2 用例 + 原有全绿)。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/agent/pipeline/inference_stage.py echo_agent/agent/pipeline/types.py tests/test_inference_stage.py
git commit -m "工具循环收集审批/重复降级事件,经 _LoopResult 与 InferenceResult 冒泡"
```

---

### Task 5: ProcessResult 透传 degraded_notices 到 _process_event 出口

把 `degraded_notices` 从 `InferenceResult` 经 `ProcessResult`(即 `_ProcessResult`)透传到 `_on_inbound` 可见的位置。纯管道任务。

**Files:**
- Modify: `echo_agent/agent/pipeline/response_stage.py`(`ProcessResult` 行 23-26;`finalize` 返回 行 126)
- Modify: `echo_agent/agent/loop.py`(`_process_event` 返回 行 708;早返回 行 635 保持默认空)
- Test: 扩展 `tests/test_inference_stage.py` 或新建轻量断言(数据类字段默认值)

**Interfaces:**
- Consumes: `InferenceResult.degraded_notices`(Task 4)。
- Produces: `ProcessResult.degraded_notices: list[str]`(默认空 list),`_process_event` 返回的 `_ProcessResult`(即同一类)带该字段。

- [ ] **Step 1: 写失败测试**

在 `tests/test_inference_stage.py` 末尾追加:

```python
def test_processresult_has_degraded_notices_default():
    from echo_agent.agent.pipeline.response_stage import ProcessResult
    r = ProcessResult()
    assert r.degraded_notices == []
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `pytest tests/test_inference_stage.py::test_processresult_has_degraded_notices_default -v`
Expected: FAIL,`AttributeError`。

- [ ] **Step 3: 改实现**

3a. `response_stage.py` 行 5 当前是 `from dataclasses import dataclass`,改为 `from dataclasses import dataclass, field`。`ProcessResult`(行 23-26)改为:
```python
@dataclass
class ProcessResult:
    response_text: str = ""
    outbound_sent: bool = False
    degraded_notices: list[str] = field(default_factory=list)
```

3b. `finalize` 的 `return`(行 126)改为透传 `result.degraded_notices`:
```python
        return ProcessResult(
            response_text=response_text or "",
            outbound_sent=outbound_sent,
            degraded_notices=list(result.degraded_notices),
        )
```

3c. `loop.py` 的 `_process_event` 正常返回(行 708)改为:
```python
        return _ProcessResult(
            response_text=result.response_text,
            outbound_sent=result.outbound_sent,
            degraded_notices=result.degraded_notices,
        )
```
(行 635 的早返回 `_ProcessResult(response_text=command_response)` 保持不变——审批命令路径无降级事件,默认空 list 即可。)

- [ ] **Step 4: 运行测试,确认通过**

Run: `pytest tests/test_inference_stage.py -v`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/agent/pipeline/response_stage.py echo_agent/agent/loop.py tests/test_inference_stage.py
git commit -m "ProcessResult 透传 degraded_notices 至 _process_event 出口"
```

---

### Task 6: _on_inbound 收敛点 — 保证降级通知送达用户

核心任务。回合结束后,在发送前判定:有降级事件时,合成中文通知;响应为空/泛化英文兜底时,用通知替代;正常回复 + 有通知时,通知作为补充。绝不允许"回合结束但用户什么都没收到"。

**Files:**
- Modify: `echo_agent/agent/degraded_notice.py`(新增 `is_generic_fallback` 与英文兜底常量)
- Modify: `echo_agent/agent/loop.py`(`_on_inbound` 发送逻辑 行 600-608)
- Test: `tests/test_approval_degraded_notice.py`(新建,收敛点行为)
- Test: 扩展 `tests/test_degraded_notice_copy.py`(`is_generic_fallback`)

**Interfaces:**
- Consumes: `_ProcessResult.degraded_notices`/`response_text`/`outbound_sent`(Task 5);`combine_notices`(Task 1)。
- Produces:
  - `degraded_notice.GENERIC_ENGLISH_FALLBACKS: frozenset[str]`(inference_stage 现用的英文兜底句)。
  - `degraded_notice.is_generic_fallback(text: str) -> bool` — 文本为空或属于英文兜底句集合时返回 True。
  - `_on_inbound` 出口:保证有降级事件或空回复时,送达一条 `is_final=True` 的中文消息。

- [ ] **Step 1: 写失败测试**

1a. 在 `tests/test_degraded_notice_copy.py` 末尾追加:
```python
from echo_agent.agent.degraded_notice import is_generic_fallback


def test_is_generic_fallback_true_for_empty():
    assert is_generic_fallback("") is True
    assert is_generic_fallback("   ") is True


def test_is_generic_fallback_true_for_english_filler():
    assert is_generic_fallback(
        "I encountered an issue processing your request. Please try again or rephrase your question."
    ) is True


def test_is_generic_fallback_false_for_real_answer():
    assert is_generic_fallback("调研完成,结论是 ...") is False
```

1b. 新建 `tests/test_approval_degraded_notice.py`(收敛点不变量;用最小 fake 驱动 `_on_inbound`):
```python
from __future__ import annotations

import pytest

from echo_agent.agent.degraded_notice import notice_for, REASON_APPROVAL_UNAVAILABLE
from echo_agent.agent.pipeline.response_stage import ProcessResult
from echo_agent.bus.events import InboundEvent, ContentBlock, ContentType


def _make_loop():
    """Build an AgentLoop with _process_event stubbed, capturing outbound."""
    from echo_agent.agent.loop import AgentLoop
    loop = AgentLoop.__new__(AgentLoop)  # bypass heavy __init__
    sent: list = []

    class _Bus:
        async def publish_outbound(self, out):
            sent.append(out)

    class _Sessions:
        async def acquire(self, key):
            import asyncio
            return asyncio.Lock()

    class _Tracer:
        def start_span(self, *a, **k): return None
        def end_span(self, *a, **k): pass
        def flush_trace(self, *a, **k): pass

    loop.bus = _Bus()
    loop.sessions = _Sessions()
    loop.tracer = _Tracer()
    loop._running = True
    loop.config = None
    return loop, sent


def _event():
    return InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="research")],
    )


@pytest.mark.asyncio
async def test_empty_response_with_notice_sends_chinese(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="", outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert "安全审批暂时不可用" in sent[0].text
    assert sent[0].is_final is True


@pytest.mark.asyncio
async def test_empty_response_no_notice_sends_generic_chinese(monkeypatch):
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="", outbound_sent=False, degraded_notices=[])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert sent[0].text.startswith("⚠️")


@pytest.mark.asyncio
async def test_generic_english_replaced_by_notice(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)
    english = "I encountered an issue processing your request. Please try again or rephrase your question."

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text=english, outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert english not in sent[0].text
    assert "安全审批暂时不可用" in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_already_sent_appends_notice(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="真实回答", outbound_sent=True, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    # main answer already streamed; notice delivered as a single follow-up
    assert len(sent) == 1
    assert "安全审批暂时不可用" in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_no_notice_unchanged(monkeypatch):
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="真实回答", outbound_sent=False, degraded_notices=[])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert sent[0].text == "真实回答"
```

- [ ] **Step 2: 运行测试,确认失败**

Run: `pytest tests/test_approval_degraded_notice.py tests/test_degraded_notice_copy.py -v`
Expected: FAIL(`is_generic_fallback` 不存在;收敛点未合成通知)。

- [ ] **Step 3: 改实现**

3a. `echo_agent/agent/degraded_notice.py` 追加:
```python
# The generic English fillers inference_stage falls back to when a turn
# produced no real text. The convergence point treats these as "no real
# answer" so a Chinese degraded notice replaces them.
GENERIC_ENGLISH_FALLBACKS = frozenset({
    "I encountered an issue processing your request. Please try again.",
    "I encountered an issue processing your request. Please try again or rephrase your question.",
})


def is_generic_fallback(text: str) -> bool:
    """True if text is empty/whitespace or one of the generic English fillers."""
    stripped = (text or "").strip()
    if not stripped:
        return True
    return stripped in GENERIC_ENGLISH_FALLBACKS
```

3b. `echo_agent/agent/loop.py` 顶部 import 区加入:
```python
from echo_agent.agent.degraded_notice import (
    GENERIC_FALLBACK_TEXT,
    combine_notices,
    is_generic_fallback,
)
```

3c. `_on_inbound` 的 try 块发送逻辑(行 600-609)替换为:
```python
                result = await self._process_event(event, trace_id, publish_response=True)
                response_text = result.response_text
                notice = combine_notices(result.degraded_notices)

                # Convergence point: the turn MUST deliver a meaningful message.
                # 1) degraded event + no real answer  -> send the Chinese notice
                # 2) degraded event + real answer not yet sent -> answer + notice
                # 3) degraded event + real answer already streamed -> notice only
                # 4) no degraded event, empty/generic answer -> generic Chinese
                # 5) no degraded event, real answer -> unchanged behaviour
                final_text = ""
                if notice:
                    if result.outbound_sent:
                        final_text = notice
                    elif is_generic_fallback(response_text):
                        final_text = notice
                    else:
                        final_text = f"{response_text}\n\n{notice}"
                elif not result.outbound_sent:
                    if is_generic_fallback(response_text):
                        final_text = GENERIC_FALLBACK_TEXT
                    else:
                        final_text = response_text

                if final_text:
                    out = OutboundEvent.from_text_with_media(
                        channel=event.channel, chat_id=event.chat_id, text=final_text, reply_to_id=event.reply_to_id,
                    )
                    out.metadata = dict(event.metadata)
                    out.metadata["_inbound_event_id"] = event.event_id
                    await self.bus.publish_outbound(out)
                self.tracer.end_span(span, metadata={"response_len": len(response_text or "")})
```

注:`OutboundEvent.from_text_with_media` 默认 `is_final=True`(`bus/events.py:121`),满足微信 `should_deliver` 要求。

- [ ] **Step 4: 运行测试,确认通过**

Run: `pytest tests/test_approval_degraded_notice.py tests/test_degraded_notice_copy.py -v`
Expected: PASS(全部)。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/agent/degraded_notice.py echo_agent/agent/loop.py tests/test_approval_degraded_notice.py tests/test_degraded_notice_copy.py
git commit -m "_on_inbound 收敛点:保证降级事件/空回复时送达中文通知,消除静默挂起"
```

---

### Task 7: 审批路径不变量测试 + 全量验证

补齐规格 5.1 的审批侧不变量测试(smart unavailable 走快速失败而非 manual,ESCALATE 仍走 manual 的回归保护),并跑全量 lint + 测试,确认无回归、不降覆盖率。

**Files:**
- Modify: `tests/test_approval_degraded_notice.py`(追加审批不变量)
- 验证:全仓 `ruff check .` + `pytest`

**Interfaces:**
- Consumes: `ApprovalGate.check`(Task 3 的 `notify_user`/`notice`);`smart_approve`(Task 2 的 `unavailable`)。

- [ ] **Step 1: 写失败测试(若 Task 3 已使行为可用,这些应直接通过 → 仍先写,作回归锁)**

在 `tests/test_approval_degraded_notice.py` 追加:

```python
import pytest
from unittest.mock import AsyncMock, MagicMock

from echo_agent.agent.approval_gate import ApprovalGate
from echo_agent.bus.events import InboundEvent as _IE, ContentBlock as _CB, ContentType as _CT
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.permissions.manager import ApprovalManager


class _FakeInf:
    def needs_confirmation(self, name: str) -> bool:
        return False


def _gate_with_provider(content):
    cfg = load_config()
    cfg.permissions.approval.mode = "smart"
    appr = ApprovalManager(require_approval=cfg.permissions.approval.require_approval)
    provider = MagicMock()
    provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=content))
    return ApprovalGate(config=cfg, approval=appr, inference=_FakeInf(), bus=MessageBus(), provider=provider)


def _exec_event():
    return _IE(channel="weixin", sender_id="u1", chat_id="c1",
               content=[_CB(type=_CT.TEXT, text="research")])


@pytest.mark.asyncio
async def test_smart_unavailable_does_not_block_in_manual():
    # Empty provider content -> unavailable -> immediate denial with notice,
    # NOT a blocking manual-approval wait.
    gate = _gate_with_provider("")
    check = await gate.check("exec", {"command": "curl x"}, "u1", channel="weixin", event=_exec_event())
    assert check.denial is not None
    assert check.notify_user is True
    assert "安全审批暂时不可用" in check.notice


@pytest.mark.asyncio
async def test_smart_escalate_still_enters_manual_flow(monkeypatch):
    # Explicit ESCALATE must still reach the manual flow (publishes a request),
    # i.e. 2.1 must not swallow legitimate escalations.
    gate = _gate_with_provider("ESCALATE")
    published = []
    monkeypatch.setattr(gate._bus, "publish_outbound", AsyncMock(side_effect=lambda o: published.append(o)))
    # wait_for_decision returns None (no decider) quickly to avoid real blocking
    monkeypatch.setattr(gate._approval, "wait_for_decision", AsyncMock(return_value=None))
    check = await gate.check("exec", {"command": "ls"}, "u1", channel="weixin", event=_exec_event())
    # an approval request was published (manual flow entered)
    assert any(getattr(o, "metadata", {}).get("_approval_request") for o in published)
```

- [ ] **Step 2: 运行新测试**

Run: `pytest tests/test_approval_degraded_notice.py -v`
Expected: PASS。若 `test_smart_escalate_still_enters_manual_flow` 因 `wait_for_decision` 真实阻塞而挂起,确认 monkeypatch 生效(它把决策替换为立即返回 None)。

- [ ] **Step 3: 全量 lint**

Run: `ruff check .`
Expected: 无错误(若有未用 import,清理)。

- [ ] **Step 4: 全量测试**

Run: `pytest -q`
Expected: 全绿。重点确认 `tests/test_security_new_features.py`、`tests/test_approval_gate_e2e.py`、`tests/test_inference_stage.py` 无回归。

- [ ] **Step 5: 提交**

```bash
git add tests/test_approval_degraded_notice.py
git commit -m "补齐审批不变量测试:unavailable 快速失败、ESCALATE 仍走人工审批"
```

---

## Self-Review 结果

- **Spec 覆盖**:
  - 第 1 节统一收敛点 → Task 6。
  - 2.1 smart 快速失败 → Task 2 + Task 3(unavailable 分支)。
  - 2.3 审批结果冒泡 → Task 3(denial 带通知)+ Task 4(收集冒泡)。
  - 4.4 数据流 → Task 4 + Task 5。
  - 4.5 收敛决策 → Task 6。
  - 4.6 中文文案 → Task 1。
  - 4.7 去重/防双发/is_final → Task 1(combine_notices 去重)+ Task 6(防双发逻辑、is_final)。
  - 5.1 不变量测试 → Task 2/3/6/7。
  - 5.2 收敛点测试 → Task 6。
  - 5.3 端到端契约 → 见下方补充说明。
  - 5.4 验证 → Task 7。
  - 2.2(持锁)与 provider 韧性 → 明确不做(Global Constraints)。
- **类型一致性**:`degraded_notices: list[str]` 在 `_LoopResult`/`InferenceResult`/`ProcessResult` 三处命名一致;`notify_user`/`notice` 在 `ApprovalCheck` 与消费点一致;`notice_for`/`combine_notices`/`is_generic_fallback`/`GENERIC_FALLBACK_TEXT`/`GENERIC_ENGLISH_FALLBACKS` 命名跨任务一致。
- **占位符扫描**:无 TBD/TODO,所有代码步骤含完整代码。

### 关于 5.3 端到端契约测试

规格 5.3 的"embedding 失败 + smart 解析失败 → 最终 bus 必有 is_final outbound"由 Task 6 的 `test_empty_response_with_notice_sends_chinese` 与 Task 7 的 `test_smart_unavailable_does_not_block_in_manual` 共同覆盖其核心断言(收敛点必发 + 审批不阻塞)。完整跨 `_process_event` 的真实链路 e2e 因需构造完整 AgentLoop(重 __init__),成本高于收益,本计划用收敛点单元测试 + 审批 gate 测试替代,等价锁定回归。若后续需要,可作为独立任务补充。
