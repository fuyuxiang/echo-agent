from echo_agent.cli.render.status import (
    context_gauge,
    context_percent,
    fmt_duration,
    fmt_tokens,
)


def test_status_measurements_are_compact_and_safe():
    assert fmt_tokens(57_300) == "57.3K"
    assert fmt_tokens(1_000_000) == "1.0M"
    assert fmt_tokens(None) == "0"
    assert fmt_duration(51) == "51s"
    assert fmt_duration(111) == "1m 51s"
    assert fmt_duration(3_600) == "1h"


def test_context_gauge_clamps_occupancy():
    assert context_percent(57_300, 1_000_000) == 6
    assert context_percent(10, 0) == 0
    assert len(context_gauge(50)) == 10
    assert context_gauge(200, 4) == "████"
    assert context_gauge(-1, 4) == "░░░░"
