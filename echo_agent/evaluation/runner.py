"""Evaluation runner — executes test cases against the agent."""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.evaluation.dataset import EvalCase, EvalDataset
from echo_agent.evaluation.metrics import (
    MetricResult, contains_all, tool_usage_correctness,
    iteration_efficiency, response_quality,
    not_contains, forbidden_tools_check,
)
from echo_agent.evaluation.semantic_metrics import semantic_quality

if TYPE_CHECKING:
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.models.provider import LLMProvider


@dataclass
class CaseResult:
    case_id: str = ""
    passed: bool = False
    response: str = ""
    tools_used: list[str] = field(default_factory=list)
    iterations: int = 0
    duration_ms: float = 0.0
    metrics: list[MetricResult] = field(default_factory=list)
    error: str = ""
    category: str = ""

    @property
    def score(self) -> float:
        if not self.metrics:
            return 0.0
        return sum(m.score for m in self.metrics) / len(self.metrics)

    @property
    def inconclusive(self) -> bool:
        return any(m.inconclusive for m in self.metrics)


@dataclass
class EvalReport:
    results: list[CaseResult] = field(default_factory=list)
    total_cases: int = 0
    passed_cases: int = 0
    duration_ms: float = 0.0

    @property
    def pass_rate(self) -> float:
        return self.passed_cases / max(self.total_cases, 1)

    @property
    def inconclusive_cases(self) -> int:
        return sum(1 for r in self.results if r.inconclusive)

    @property
    def avg_score(self) -> float:
        conclusive = [r for r in self.results if not r.inconclusive]
        if not conclusive:
            return 0.0
        return sum(r.score for r in conclusive) / len(conclusive)

    def summary(self) -> dict[str, Any]:
        return {
            "total": self.total_cases,
            "passed": self.passed_cases,
            "pass_rate": round(self.pass_rate, 3),
            "avg_score": round(self.avg_score, 3),
            "inconclusive": self.inconclusive_cases,
            "duration_ms": round(self.duration_ms, 1),
        }

    def regressed_categories(
        self, baseline: "EvalReport", categories: set[str]
    ) -> set[str]:
        """Categories (from ``categories``) where at least one case that
        passed in ``baseline`` now fails in this report. Cases are matched by
        ``case_id``. Used for zero-tolerance safety/refusal gating."""
        base_pass = {
            r.case_id: r.passed for r in baseline.results if r.category in categories
        }
        regressed: set[str] = set()
        for r in self.results:
            if r.category not in categories:
                continue
            if base_pass.get(r.case_id) and not r.passed:
                regressed.add(r.category)
        return regressed


class EvalRunner:
    def __init__(
        self,
        agent_loop: AgentLoop,
        parallel: int = 3,
        timeout: int = 120,
        provider: "LLMProvider | None" = None,
        judge_model: str | None = None,
    ):
        self._loop = agent_loop
        self._parallel = parallel
        self._timeout = timeout
        self._provider = provider
        self._judge_model = judge_model

    async def run_case(self, case: EvalCase) -> CaseResult:
        start = time.monotonic()
        result = CaseResult(case_id=case.id, category=case.category)
        try:
            from echo_agent.bus.events import InboundEvent
            import uuid
            event = InboundEvent.text_message(
                channel="eval", sender_id="eval-runner",
                chat_id=f"eval:{case.id}", text=case.input,
            )
            proc_result = await asyncio.wait_for(
                self._loop._process_event(event, trace_id=uuid.uuid4().hex[:12]),
                timeout=self._timeout,
            )
            result.response = proc_result.response_text or ""
            try:
                session = await self._loop.sessions.get_or_create(event.session_key)
                seen: list[str] = []
                seen_set: set[str] = set()
                for msg in session.get_history(max_messages=500):
                    if msg.get("role") != "tool":
                        continue
                    name = msg.get("name") or ""
                    if name and name not in seen_set:
                        seen_set.add(name)
                        seen.append(name)
                result.tools_used = seen
                result.iterations = sum(
                    1 for m in session.get_history(max_messages=500)
                    if m.get("role") == "assistant"
                )
            except Exception as e:
                logger.debug("Failed to extract tools_used for case {}: {}", case.id, e)
        except asyncio.TimeoutError:
            result.error = "Timeout"
        except Exception as e:
            result.error = str(e)

        result.duration_ms = (time.monotonic() - start) * 1000

        # Score
        result.metrics = await self._score_response(
            case, result.response, result.tools_used, result.iterations,
        )
        result.passed = all(m.passed for m in result.metrics) and not result.error
        return result

    async def _score_response(
        self, case: EvalCase, response: str, tools_used: list[str], iterations: int,
    ) -> list[MetricResult]:
        metrics: list[MetricResult] = [
            contains_all(case.expected_contains, response),
            tool_usage_correctness(case.expected_tools, tools_used),
            iteration_efficiency(iterations, case.max_iterations),
            not_contains(case.expected_not_contains, response),
            forbidden_tools_check(case.forbidden_tools, tools_used),
        ]
        if case.expected_output:
            metrics.append(response_quality(case.expected_output, response))
            # Semantic judging costs a real LLM call — skip it when the case
            # produced no response (error/timeout) to avoid wasted spend and a
            # meaningless score in the average.
            if self._provider is not None and response:
                metrics.append(
                    await semantic_quality(
                        case.expected_output, response,
                        self._provider, model=self._judge_model,
                    )
                )
        return metrics

    async def run_dataset(self, dataset: EvalDataset) -> EvalReport:
        start = time.monotonic()
        semaphore = asyncio.Semaphore(self._parallel)

        async def run_with_limit(case: EvalCase) -> CaseResult:
            async with semaphore:
                return await self.run_case(case)

        tasks = [run_with_limit(c) for c in dataset.cases]
        results = await asyncio.gather(*tasks, return_exceptions=True)

        report = EvalReport(total_cases=len(dataset.cases))
        for r in results:
            if isinstance(r, CaseResult):
                report.results.append(r)
                if r.passed:
                    report.passed_cases += 1
            else:
                report.results.append(CaseResult(error=str(r)))
        report.duration_ms = (time.monotonic() - start) * 1000
        logger.info("Eval complete: {}/{} passed, avg score {:.3f}", report.passed_cases, report.total_cases, report.avg_score)
        return report
