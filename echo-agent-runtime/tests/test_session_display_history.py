"""``Session.get_display_history`` — the human-readable view of a session.

``Session.messages`` is the LLM's working record, not an immutable transcript.
Two kinds of entry in it misrepresent the conversation to a person:

* the compressor injects a summary as ``role: user`` and an acknowledgement as
  ``role: assistant`` (agent/compression/assembler.py) so the model treats the
  summary as reference material. Compression rewrites ``session.messages`` and
  the result is persisted (agent/pipeline/context_stage.py), so both land in the
  stored history — and a viewer that trusts ``role`` shows a machine-written
  summary as something the user typed.
* tool calls and tool results are interleaved with the conversation. They are
  real and worth reading, but they are not chat turns.

The LLM path (``get_history``) must keep seeing the raw list, so this view is
built by copying and filtering, never by mutating storage.
"""

from __future__ import annotations

from echo_agent.agent.compression.assembler import SUMMARY_ACK, SUMMARY_PREFIX
from echo_agent.session.manager import Session


def _session_with_tools() -> Session:
    session = Session(key="cli:local")
    session.add_message("user", "北京天气")
    session.add_message("assistant", "", tool_calls=[
        {"id": "c1", "function": {"name": "web_search", "arguments": "{}"}},
    ])
    session.add_message("tool", "晴 28C", tool_call_id="c1", name="web_search")
    session.add_message("assistant", "北京今天晴。")
    return session


class TestConsolidatedSessions:
    """The original bug: a fully consolidated session displayed nothing."""

    def test_fully_consolidated_session_still_shows_its_history(self):
        session = Session(key="cli:local")
        for i in range(5):
            session.add_message("user", f"q{i}")
            session.add_message("assistant", f"a{i}")
        session.last_consolidated = len(session.messages)

        # The LLM view is legitimately empty here — that is what it is for.
        assert session.get_history() == []
        assert len(session.get_display_history()) == 10

    def test_llm_view_is_unaffected_by_the_display_view(self):
        """Two views over one list; reading one must not change the other."""
        session = _session_with_tools()
        before = [dict(m) for m in session.messages]

        session.get_display_history()

        assert session.messages == before


class TestInjectedSummaryPair:
    def test_summary_and_ack_are_hidden(self):
        session = Session(key="cli:local")
        session.add_message("user", "帮我算个数")
        session.add_message("assistant", "好的")
        session.add_message("user", SUMMARY_PREFIX + "此前用户要求计算。")
        session.add_message("assistant", SUMMARY_ACK)
        session.add_message("user", "继续")

        contents = [m["content"] for m in session.get_display_history()]

        assert contents == ["帮我算个数", "好的", "继续"]

    def test_a_user_message_that_merely_mentions_the_summary_is_kept(self):
        """Only the injected shape is dropped, not any message about summaries.

        The filter matches the assembler's prefix at the *start* of the content,
        so a user genuinely discussing a summary keeps their message.
        """
        session = Session(key="cli:local")
        session.add_message("user", f"这段是什么意思：{SUMMARY_PREFIX}")
        session.add_message("assistant", f"这是压缩摘要的开头标记：{SUMMARY_ACK}")

        assert len(session.get_display_history()) == 2

    def test_filter_tracks_the_assembler_rather_than_copying_its_text(self):
        """Pin the shared-constant contract.

        If the assembler's wording changes and the display filter carried its own
        copy of the literals, the injected pair would silently reappear in the UI
        as user speech. Importing the constants is what prevents that; this test
        fails if someone re-inlines them.
        """
        import inspect

        from echo_agent.session import manager

        source = inspect.getsource(manager.Session.display_messages)
        assert "SUMMARY_PREFIX" in source and "SUMMARY_ACK" in source
        assert "Conversation Summary" not in source, "literal copied instead of imported"


class TestToolTraffic:
    def test_tool_entries_are_tagged_not_dropped(self):
        """Visible but marked: the client renders them as collapsed detail.

        Dropping them would lose the very content that makes a session
        debuggable; leaving them untagged is what made tool output render as an
        ordinary agent reply.
        """
        history = _session_with_tools().get_display_history()

        assert [(m["role"], m.get("internal", False)) for m in history] == [
            ("user", False),
            ("assistant", True),   # only requested tools, no user-facing content
            ("tool", True),
            ("assistant", False),  # the actual reply
        ]

    def test_the_tag_never_leaks_into_stored_messages(self):
        session = _session_with_tools()
        session.get_display_history()

        assert all("internal" not in m for m in session.messages)

    def test_tool_name_survives_for_the_client_to_label(self):
        history = _session_with_tools().get_display_history()
        tool_entry = next(m for m in history if m["role"] == "tool")

        assert tool_entry["name"] == "web_search"


class TestLimit:
    def test_filtering_happens_before_slicing(self):
        """``max_messages`` counts what the user will see.

        Slicing first would let a tool-heavy or summary-heavy tail consume the
        whole window, so a request for 2 messages could return a page the viewer
        renders as almost empty.
        """
        session = Session(key="cli:local")
        session.add_message("user", "真实提问 1")
        session.add_message("user", SUMMARY_PREFIX + "摘要")
        session.add_message("assistant", SUMMARY_ACK)
        session.add_message("assistant", "真实回复 1")

        history = session.get_display_history(max_messages=2)

        assert [m["content"] for m in history] == ["真实提问 1", "真实回复 1"]

    def test_non_positive_and_oversized_limits_are_clamped(self):
        """A slice is not a limit.

        ``messages[-0:]`` returns everything and ``messages[-(-1):]`` all but the
        first — so the unclamped parameter inverted its own meaning. The endpoint
        rejects out-of-range values outright (see test_api_sessions.py); the model
        clamps as defense in depth for every other caller.
        """
        session = Session(key="cli:local")
        for i in range(10):
            session.add_message("user", f"m{i}")

        assert len(session.get_display_history(max_messages=0)) == 1
        assert len(session.get_display_history(max_messages=-5)) == 1
        assert len(session.get_display_history(max_messages=10**9)) == 10

    def test_ceiling_is_the_documented_constant(self):
        session = Session(key="cli:local")
        for i in range(Session.MAX_DISPLAY_MESSAGES + 50):
            session.add_message("user", f"m{i}")

        history = session.get_display_history(max_messages=10**6)

        assert len(history) == Session.MAX_DISPLAY_MESSAGES
        # The newest messages are the ones kept.
        assert history[-1]["content"] == f"m{Session.MAX_DISPLAY_MESSAGES + 49}"


class TestDisplayMessages:
    def test_unsliced_view_backs_the_endpoint_total(self):
        """``display_messages`` exists so ``total`` needs no second filter pass.

        Reimplementing the filter at the endpoint is how the two would drift.
        """
        session = _session_with_tools()

        assert len(session.display_messages()) == 4
        assert session.display_messages() == session.get_display_history(max_messages=500)

    def test_non_string_content_does_not_break_the_filter(self):
        """Multimodal content arrives as a list of blocks, not a string."""
        session = Session(key="cli:local")
        session.add_message("user", [{"type": "text", "text": "看这张图"}])
        session.add_message("assistant", "好的")

        assert len(session.display_messages()) == 2
