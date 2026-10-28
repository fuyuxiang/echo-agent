"""Per-tool circuit breaker — prevents cascading failures from broken tools."""

from __future__ import annotations

import time
from dataclasses import dataclass
from enum import Enum


class CircuitState(str, Enum):
    CLOSED = "closed"
    OPEN = "open"
    HALF_OPEN = "half_open"


@dataclass
class ToolCircuit:
    failure_count: int = 0
    last_failure: float = 0.0
    state: CircuitState = CircuitState.CLOSED
    half_open_successes: int = 0
    half_open_probes: int = 0


class ToolCircuitBreaker:
    """Tracks per-tool failure rates and temporarily disables broken tools.

    States:
      CLOSED   — tool is healthy, all calls pass through
      OPEN     — tool hit failure threshold, calls are blocked
      HALF_OPEN — recovery window, limited calls allowed to probe health
    """

    def __init__(
        self,
        failure_threshold: int = 5,
        recovery_seconds: float = 60.0,
        half_open_max: int = 2,
    ):
        self._circuits: dict[str, ToolCircuit] = {}
        self._failure_threshold = failure_threshold
        self._recovery_seconds = recovery_seconds
        self._half_open_max = half_open_max

    # 放行判断条件须与 peek_available() 保持一致（本方法多了 acquire 副作用）。
    def is_available(self, tool_name: str) -> bool:
        circuit = self._circuits.get(tool_name)
        if not circuit or circuit.state == CircuitState.CLOSED:
            return True
        if circuit.state == CircuitState.OPEN:
            if time.monotonic() - circuit.last_failure >= self._recovery_seconds:
                circuit.state = CircuitState.HALF_OPEN
                circuit.half_open_successes = 0
                circuit.half_open_probes = 1
                return True
            return False
        # HALF_OPEN: bound concurrent probes — without this every queued call
        # rushes the possibly-still-broken tool at once.
        if circuit.half_open_probes < self._half_open_max:
            circuit.half_open_probes += 1
            return True
        return False

    def record_success(self, tool_name: str) -> None:
        circuit = self._circuits.get(tool_name)
        if not circuit:
            return
        if circuit.state == CircuitState.HALF_OPEN:
            circuit.half_open_successes += 1
            if circuit.half_open_successes >= self._half_open_max:
                circuit.state = CircuitState.CLOSED
                circuit.failure_count = 0
                circuit.half_open_probes = 0
        elif circuit.state == CircuitState.CLOSED:
            circuit.failure_count = 0

    def record_failure(self, tool_name: str) -> None:
        circuit = self._circuits.setdefault(tool_name, ToolCircuit())
        circuit.failure_count += 1
        circuit.last_failure = time.monotonic()
        if circuit.state == CircuitState.HALF_OPEN:
            # A failed probe reopens the circuit immediately.
            circuit.state = CircuitState.OPEN
            circuit.half_open_probes = 0
        elif circuit.failure_count >= self._failure_threshold:
            circuit.state = CircuitState.OPEN

    def peek_available(self, tool_name: str) -> bool:
        """只读判断工具当前是否会被放行 —— 绝不修改任何状态。

        与 is_available() 的区别：is_available 会把到期的 OPEN 翻成 HALF_OPEN
        并消耗探测预算（acquire-a-probe 语义），仅供真实调用路径使用；监控/过滤
        类查询必须用 peek，否则会偷走探测预算、甚至触发状态跃迁。
        """
        # 放行判断条件须与 is_available() 保持一致。
        circuit = self._circuits.get(tool_name)
        if not circuit or circuit.state == CircuitState.CLOSED:
            return True
        if circuit.state == CircuitState.OPEN:
            return time.monotonic() - circuit.last_failure >= self._recovery_seconds
        # HALF_OPEN: 是否还有探测名额（不占用）
        return circuit.half_open_probes < self._half_open_max

    def get_unavailable_tools(self) -> set[str]:
        return {name for name in self._circuits if not self.peek_available(name)}

    def reset(self, tool_name: str) -> None:
        self._circuits.pop(tool_name, None)

    def reset_all(self) -> None:
        self._circuits.clear()
