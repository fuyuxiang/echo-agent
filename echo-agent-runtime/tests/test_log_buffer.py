"""Tests for echo_agent.observability.log_buffer — the in-memory ring buffer
backing the dashboard /api/logs endpoint."""

import pytest
from loguru import logger

from echo_agent.observability import log_buffer as lb


@pytest.fixture
def fresh_buffer():
    """Install the sink and clear the buffer; remove the sink afterward so the
    process-global logger is left as we found it (no cross-test leakage)."""
    lb.clear_log_buffer()
    lb.install_log_buffer(level="DEBUG")
    try:
        yield lb.get_log_buffer()
    finally:
        if lb._sink_id is not None:
            try:
                logger.remove(lb._sink_id)
            except ValueError:
                pass
            lb._sink_id = None
        lb.clear_log_buffer()


def test_records_are_captured_with_expected_shape(fresh_buffer):
    logger.info("hello world")
    entries = [e for e in fresh_buffer if e["message"] == "hello world"]
    assert len(entries) == 1
    entry = entries[0]
    assert entry["level"] == "INFO"
    assert set(entry) == {"ts", "level", "message"}
    assert entry["ts"]  # ISO timestamp string, non-empty


def test_level_is_recorded_per_record(fresh_buffer):
    logger.warning("careful")
    logger.error("broken")
    levels = {e["message"]: e["level"] for e in fresh_buffer}
    assert levels["careful"] == "WARNING"
    assert levels["broken"] == "ERROR"


def test_install_is_idempotent_no_duplicate_records(fresh_buffer):
    # Re-installing must replace the sink, not stack a second one that would
    # double every subsequent record.
    lb.install_log_buffer(level="DEBUG")
    logger.info("once")
    assert sum(1 for e in lb.get_log_buffer() if e["message"] == "once") == 1


def test_buffer_is_bounded():
    # The deque caps at _MAX_LOG_RECORDS regardless of how much is logged.
    assert lb.get_log_buffer().maxlen == lb._MAX_LOG_RECORDS


def test_clear_empties_the_buffer(fresh_buffer):
    logger.info("transient")
    assert len(fresh_buffer) >= 1
    lb.clear_log_buffer()
    assert len(lb.get_log_buffer()) == 0
