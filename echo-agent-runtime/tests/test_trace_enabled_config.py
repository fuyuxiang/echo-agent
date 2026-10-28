from echo_agent.observability.monitor import TraceLogger


def test_disabled_tracelogger_does_not_write(tmp_path):
    logger = TraceLogger(logs_dir=tmp_path, enabled=False)
    span = logger.start_span("t1", "s1", name="x", kind="tool_call")
    logger.end_span(span)
    logger.flush_trace("t1")
    assert not any(tmp_path.glob("trace_*.json"))


def test_enabled_tracelogger_writes(tmp_path):
    logger = TraceLogger(logs_dir=tmp_path, enabled=True)
    span = logger.start_span("t2", "s2", name="x", kind="tool_call")
    logger.end_span(span)
    logger.flush_trace("t2")
    assert any(tmp_path.glob("trace_*.json"))
