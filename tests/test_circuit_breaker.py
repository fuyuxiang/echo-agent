"""Tests for the per-tool circuit breaker."""

import time


from echo_agent.agent.tools.circuit_breaker import CircuitState, ToolCircuitBreaker


class TestToolCircuitBreaker:
    def test_new_tool_is_available(self):
        cb = ToolCircuitBreaker(failure_threshold=3)
        assert cb.is_available("some_tool")

    def test_stays_closed_below_threshold(self):
        cb = ToolCircuitBreaker(failure_threshold=3)
        cb.record_failure("tool_a")
        cb.record_failure("tool_a")
        assert cb.is_available("tool_a")

    def test_opens_at_threshold(self):
        cb = ToolCircuitBreaker(failure_threshold=3)
        cb.record_failure("tool_a")
        cb.record_failure("tool_a")
        cb.record_failure("tool_a")
        assert not cb.is_available("tool_a")

    def test_other_tools_unaffected(self):
        cb = ToolCircuitBreaker(failure_threshold=2)
        cb.record_failure("tool_a")
        cb.record_failure("tool_a")
        assert not cb.is_available("tool_a")
        assert cb.is_available("tool_b")

    def test_recovery_after_timeout(self):
        cb = ToolCircuitBreaker(failure_threshold=2, recovery_seconds=0.1)
        cb.record_failure("tool_a")
        cb.record_failure("tool_a")
        assert not cb.is_available("tool_a")
        time.sleep(0.15)
        assert cb.is_available("tool_a")

    def test_half_open_success_closes_circuit(self):
        cb = ToolCircuitBreaker(failure_threshold=2, recovery_seconds=0.01, half_open_max=2)
        cb.record_failure("t")
        cb.record_failure("t")
        time.sleep(0.02)
        assert cb.is_available("t")  # transitions to HALF_OPEN
        cb.record_success("t")
        cb.record_success("t")
        circuit = cb._circuits["t"]
        assert circuit.state == CircuitState.CLOSED

    def test_half_open_failure_reopens(self):
        cb = ToolCircuitBreaker(failure_threshold=2, recovery_seconds=0.01)
        cb.record_failure("t")
        cb.record_failure("t")
        time.sleep(0.02)
        cb.is_available("t")  # transitions to HALF_OPEN
        cb.record_failure("t")
        assert not cb.is_available("t")

    def test_success_resets_failure_count(self):
        cb = ToolCircuitBreaker(failure_threshold=3)
        cb.record_failure("t")
        cb.record_failure("t")
        cb.record_success("t")
        cb.record_failure("t")
        cb.record_failure("t")
        assert cb.is_available("t")

    def test_get_unavailable_tools(self):
        cb = ToolCircuitBreaker(failure_threshold=2)
        cb.record_failure("a")
        cb.record_failure("a")
        cb.record_failure("b")
        cb.record_failure("b")
        assert cb.get_unavailable_tools() == {"a", "b"}

    def test_reset_single_tool(self):
        cb = ToolCircuitBreaker(failure_threshold=2)
        cb.record_failure("t")
        cb.record_failure("t")
        assert not cb.is_available("t")
        cb.reset("t")
        assert cb.is_available("t")

    def test_reset_all(self):
        cb = ToolCircuitBreaker(failure_threshold=1)
        cb.record_failure("a")
        cb.record_failure("b")
        cb.reset_all()
        assert cb.get_unavailable_tools() == set()
