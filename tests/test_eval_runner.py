"""Tests for EvalRunner semantic metric wiring."""
from __future__ import annotations

import pytest

from echo_agent.evaluation.runner import CaseResult, EvalReport, EvalRunner
from echo_agent.evaluation.dataset import EvalCase
from echo_agent.evaluation.metrics import MetricResult


# agent_loop=None is safe here: _score_response never touches it.
class _FakeProvider:
    def __init__(self):
        self.calls = 0

    async def chat_with_retry(self, **kwargs):
        self.calls += 1
        class _Resp:
            content = '{"score": 0.8, "reasoning": "ok"}'
        return _Resp()


def _case(expected_output: str = "") -> EvalCase:
    return EvalCase(
        id="t", input="hi", expected_tools=[], expected_contains=[],
        expected_output=expected_output, max_iterations=3, tags=[], metadata={},
    )


@pytest.mark.asyncio
async def test_score_response_no_provider_skips_semantic():
    runner = EvalRunner(agent_loop=None, provider=None)
    metrics = await runner._score_response(_case("some expected text"), "actual", [], 1)
    names = [m.name for m in metrics]
    assert "quality" in names
    assert "semantic_quality" not in names


@pytest.mark.asyncio
async def test_score_response_with_provider_adds_semantic():
    provider = _FakeProvider()
    runner = EvalRunner(agent_loop=None, provider=provider)
    metrics = await runner._score_response(_case("some expected text"), "actual", [], 1)
    names = [m.name for m in metrics]
    assert "quality" in names
    assert "semantic_quality" in names
    assert provider.calls == 1


@pytest.mark.asyncio
async def test_score_response_no_expected_output_skips_both():
    provider = _FakeProvider()
    runner = EvalRunner(agent_loop=None, provider=provider)
    metrics = await runner._score_response(_case(""), "actual", [], 1)
    names = [m.name for m in metrics]
    assert "quality" not in names
    assert "semantic_quality" not in names
    assert provider.calls == 0


@pytest.mark.asyncio
async def test_score_response_empty_response_skips_semantic():
    provider = _FakeProvider()
    runner = EvalRunner(agent_loop=None, provider=provider)
    # error/timeout 路径下 response 为空，不应发起 LLM 判分调用
    metrics = await runner._score_response(_case("some expected text"), "", [], 0)
    names = [m.name for m in metrics]
    assert "quality" in names              # 同步的 response_quality 仍计算
    assert "semantic_quality" not in names # 语义指标被守卫跳过
    assert provider.calls == 0


# ---------------------------------------------------------------------------
# run_case —— 执行单个 case 的主流程（含工具/迭代提取、超时、异常）
# ---------------------------------------------------------------------------

class _FakeSession:
    def __init__(self, history: list[dict] | None = None):
        self._history = history or []

    def get_history(self, max_messages: int = 500) -> list[dict]:
        return self._history[:max_messages]


class _FakeSessions:
    def __init__(self, session: _FakeSession):
        self._session = session

    async def get_or_create(self, key: str) -> _FakeSession:
        return self._session


class _FakeProcResult:
    def __init__(self, response_text: str):
        self.response_text = response_text


class _FakeLoop:
    """最小 AgentLoop 替身：只暴露 run_case 触达的 _process_event 与 sessions。"""

    def __init__(self, response_text: str = "hello", session: _FakeSession | None = None,
                 raises: Exception | None = None, hang: bool = False):
        self._response_text = response_text
        self.sessions = _FakeSessions(session or _FakeSession())
        self._raises = raises
        self._hang = hang
        self.process_calls = 0

    async def _process_event(self, event, trace_id: str):
        self.process_calls += 1
        if self._hang:
            import asyncio
            await asyncio.sleep(60)
        if self._raises:
            raise self._raises
        return _FakeProcResult(self._response_text)


@pytest.mark.asyncio
async def test_run_case_extracts_tools_and_iterations():
    history = [
        {"role": "assistant", "content": "let me search"},
        {"role": "tool", "name": "web_search", "content": "..."},
        {"role": "tool", "name": "web_search", "content": "dup name deduped"},
        {"role": "assistant", "content": "here it is"},
        {"role": "tool", "name": "read_file", "content": "..."},
    ]
    loop = _FakeLoop(response_text="answer", session=_FakeSession(history))
    runner = EvalRunner(agent_loop=loop, provider=None)
    result = await runner.run_case(_case())
    assert result.response == "answer"
    # 工具去重且保序
    assert result.tools_used == ["web_search", "read_file"]
    # assistant 消息计数即迭代数
    assert result.iterations == 2
    assert not result.error
    assert result.duration_ms >= 0
    assert loop.process_calls == 1


@pytest.mark.asyncio
async def test_run_case_timeout_sets_error_and_no_crash():
    loop = _FakeLoop(hang=True)
    runner = EvalRunner(agent_loop=loop, provider=None, timeout=0)
    result = await runner.run_case(_case())
    assert result.error == "Timeout"
    assert result.response == ""
    # 超时不应算通过
    assert result.passed is False


@pytest.mark.asyncio
async def test_run_case_process_exception_captured():
    loop = _FakeLoop(raises=RuntimeError("boom"))
    runner = EvalRunner(agent_loop=loop, provider=None)
    result = await runner.run_case(_case())
    assert "boom" in result.error
    assert result.passed is False


@pytest.mark.asyncio
async def test_run_case_tool_extraction_failure_is_swallowed():
    """session 提取阶段抛错不应让整个 case 崩溃——response 仍应保留。"""
    class _BadSessions:
        async def get_or_create(self, key):
            raise ValueError("session backend down")

    loop = _FakeLoop(response_text="ok")
    loop.sessions = _BadSessions()
    runner = EvalRunner(agent_loop=loop, provider=None)
    result = await runner.run_case(_case())
    assert result.response == "ok"
    assert result.tools_used == []
    assert result.iterations == 0
    # 提取失败被吞，不视为 case error
    assert result.error == ""


# ---------------------------------------------------------------------------
# run_dataset —— 并行编排与异常聚合
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_run_dataset_aggregates_pass_and_exceptions():
    from echo_agent.evaluation.dataset import EvalDataset

    loop = _FakeLoop(response_text="answer", session=_FakeSession([]))
    runner = EvalRunner(agent_loop=loop, provider=None, parallel=2)
    cases = [
        EvalCase(id="c1", input="a", expected_tools=[], expected_contains=[],
                 expected_output="", max_iterations=3, tags=[], metadata={}),
        EvalCase(id="c2", input="b", expected_tools=[], expected_contains=[],
                 expected_output="", max_iterations=3, tags=[], metadata={}),
    ]
    dataset = EvalDataset(cases=cases)
    report = await runner.run_dataset(dataset)
    assert report.total_cases == 2
    assert len(report.results) == 2
    # 无 expected_* 约束的 case 全指标通过
    assert report.passed_cases == 2
    assert report.pass_rate == 1.0


@pytest.mark.asyncio
async def test_run_dataset_gather_exception_becomes_error_result():
    """run_with_limit 抛出的异常（return_exceptions=True）应转为 error CaseResult。"""
    from echo_agent.evaluation.dataset import EvalDataset

    loop = _FakeLoop(raises=RuntimeError("kaboom"))
    # run_case 内部会捕获 _process_event 异常，所以这里直接让 run_case 本身崩：
    # 用一个 sessions 为 None 且 response 提取前就崩的路径不够，改为 monkeypatch run_case。
    runner = EvalRunner(agent_loop=loop, provider=None, parallel=1)

    async def _boom(case):
        raise RuntimeError("gather-level failure")

    runner.run_case = _boom  # type: ignore[assignment]
    cases = [EvalCase(id="c1", input="a", expected_tools=[], expected_contains=[],
                      expected_output="", max_iterations=3, tags=[], metadata={})]
    report = await runner.run_dataset(EvalDataset(cases=cases))
    assert report.total_cases == 1
    assert report.passed_cases == 0
    assert len(report.results) == 1
    assert "gather-level failure" in report.results[0].error


# ---------------------------------------------------------------------------
# EvalReport.regressed_categories —— 零容忍安全回归门禁
# ---------------------------------------------------------------------------

def _result(case_id: str, category: str, passed: bool) -> CaseResult:
    return CaseResult(case_id=case_id, category=category, passed=passed)


def test_regressed_categories_flags_new_failure():
    baseline = EvalReport(results=[
        _result("s1", "safety", True),
        _result("s2", "safety", True),
        _result("q1", "quality", True),
    ])
    current = EvalReport(results=[
        _result("s1", "safety", True),
        _result("s2", "safety", False),   # 在 baseline 通过、现在失败 -> 回归
        _result("q1", "quality", False),  # quality 不在门禁集合内，忽略
    ])
    regressed = current.regressed_categories(baseline, {"safety"})
    assert regressed == {"safety"}


def test_regressed_categories_no_regression_when_still_passing():
    baseline = EvalReport(results=[_result("s1", "safety", True)])
    current = EvalReport(results=[_result("s1", "safety", True)])
    assert current.regressed_categories(baseline, {"safety"}) == set()


def test_regressed_categories_new_failure_not_in_baseline_ignored():
    """baseline 里没通过（或不存在）的 case 现在失败，不算回归。"""
    baseline = EvalReport(results=[_result("s1", "safety", False)])
    current = EvalReport(results=[_result("s1", "safety", False)])
    assert current.regressed_categories(baseline, {"safety"}) == set()


# ---------------------------------------------------------------------------
# EvalReport 聚合属性
# ---------------------------------------------------------------------------

def test_report_summary_and_avg_score_ignores_inconclusive():
    good = CaseResult(case_id="g", passed=True,
                      metrics=[MetricResult(name="m", score=0.9, passed=True)])
    # inconclusive case 不计入 avg_score
    incon = CaseResult(case_id="i", passed=False,
                       metrics=[MetricResult(name="m", score=0.0, passed=False, inconclusive=True)])
    report = EvalReport(results=[good, incon], total_cases=2, passed_cases=1)
    assert report.inconclusive_cases == 1
    assert report.avg_score == pytest.approx(0.9)
    summary = report.summary()
    assert summary["total"] == 2
    assert summary["passed"] == 1
    assert summary["inconclusive"] == 1
    assert summary["pass_rate"] == 0.5


def test_report_avg_score_all_inconclusive_returns_zero():
    incon = CaseResult(case_id="i",
                       metrics=[MetricResult(name="m", score=0.0, passed=False, inconclusive=True)])
    report = EvalReport(results=[incon], total_cases=1)
    assert report.avg_score == 0.0


def test_case_result_score_empty_metrics_is_zero():
    assert CaseResult().score == 0.0
