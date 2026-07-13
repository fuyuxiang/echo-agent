# tests/cli/tui/test_protocol.py
from echo_agent.cli.tui.protocol import (
    parse_cog_frame, approve_command, deny_command, CogDedup, COG_TYPES,
)


def test_parse_cog_frame_extracts_fields():
    payload = {
        "type": "message", "message_kind": "cognitive", "text": "召回 3 条记忆",
        "metadata": {
            "cog_type": "memory_recalled", "cog_event_id": "evt_1",
            "_inbound_event_id": "in_9",
            "data": {"items": [{"content": "喜欢深色", "source": "user_stated", "score": 0.9}]},
        },
    }
    ev = parse_cog_frame(payload)
    assert ev.cog_type == "memory_recalled"
    assert ev.cog_event_id == "evt_1"
    assert ev.inbound_event_id == "in_9"
    assert ev.summary == "召回 3 条记忆"
    assert ev.data["items"][0]["source"] == "user_stated"


def test_parse_non_cognitive_returns_none():
    assert parse_cog_frame({"type": "message", "message_kind": "final", "text": "hi"}) is None
    assert parse_cog_frame({"type": "message", "text": "hi"}) is None


def test_approve_and_deny_commands():
    assert approve_command("abc") == "/approve abc"
    assert approve_command("abc", "session") == "/approve abc session"
    assert deny_command("abc") == "/deny abc"
    assert deny_command("abc", "太危险") == "/deny abc 太危险"


def test_dedup_reports_repeats():
    d = CogDedup()
    assert d.seen("evt_1") is False
    assert d.seen("evt_1") is True
    assert d.seen("evt_2") is False


def test_cog_types_exact_membership():
    assert COG_TYPES == frozenset({
        "memory_recalled", "memory_written", "thinking", "tool_call",
        "approval_request", "cost_update", "heartbeat", "evolution",
        "clarify_request",
    })


def test_clarify_request_frame_parses():
    from echo_agent.cli.tui.protocol import parse_cog_frame
    payload = {
        "message_kind": "cognitive",
        "text": "选哪个?",
        "metadata": {
            "cog_type": "clarify_request",
            "cog_event_id": "evt_abc",
            "_inbound_event_id": "in_1",
            "data": {"clarify_id": "c123", "question": "选哪个?", "options": ["A", "B"]},
        },
    }
    ev = parse_cog_frame(payload)
    assert ev is not None
    assert ev.cog_type == "clarify_request"
    assert ev.data["clarify_id"] == "c123"
    assert ev.data["options"] == ["A", "B"]


def test_clarify_command_encoding():
    from echo_agent.cli.tui.protocol import clarify_command
    assert clarify_command("c123", "A") == "/clarify c123 A"
    # 自由文本原样保留(含空格)
    assert clarify_command("c123", "都不要,用 C") == "/clarify c123 都不要,用 C"
