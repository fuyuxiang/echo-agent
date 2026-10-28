"""Typed server controls obey the same turn contract as approval keybindings."""

from unittest.mock import AsyncMock

import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.blocks import ToolCallBlock
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.protocol import CogEvent


@pytest.mark.asyncio
@pytest.mark.parametrize("command", ["/approvals", "/approve req1", "/deny req1", "/clarify q1 answer"])
async def test_typed_control_does_not_become_primary(command):
    app = EchoTUI(send_coro=AsyncMock())
    async with app.run_test() as pilot:
        await app.on_prompt_input_submitted(PromptInput.Submitted("do the work"))
        app.on_turn_accepted("primary")
        await app.on_prompt_input_submitted(PromptInput.Submitted(command))
        app.on_turn_accepted("control")
        app.on_user_reply_final("primary", "work complete")
        await pilot.pause()
        assert not app._turns.has_active_primary
        assert app._turns.active_turn_id == ""
        app.on_user_reply_final("control", "control result")
        await pilot.pause()
        exported = app._tv.export_text()
        assert "control result" in exported


@pytest.mark.asyncio
@pytest.mark.parametrize("reply", ["Usage: `/approve <request_id>`", "Approval request not found", "Permission denied"])
async def test_typed_control_errors_remain_visible_while_primary_keeps_running(reply):
    app = EchoTUI(send_coro=AsyncMock())
    async with app.run_test() as pilot:
        await app.on_prompt_input_submitted(PromptInput.Submitted("work"))
        app.on_turn_accepted("primary")
        await app.on_prompt_input_submitted(PromptInput.Submitted("/approve missing"))
        app.on_turn_accepted("control")
        app.on_user_reply_final("control", reply)
        await pilot.pause()
        assert reply in app._tv.export_text()
        assert app._turns.has_active_primary
        assert app._turns.active_turn_id == "primary"


@pytest.mark.asyncio
async def test_approval_key_and_typed_query_keep_distinct_response_visibility():
    app = EchoTUI(send_coro=AsyncMock())
    async with app.run_test() as pilot:
        app.on_cognitive(CogEvent("approval_request", "cog1", "primary", {
            "request_id": "req1", "action": "write_file", "params": {}, "risk": "write",
        }, ""))
        await app.action_approve()
        await app.on_prompt_input_submitted(PromptInput.Submitted("/approvals"))
        app.on_turn_accepted("approve-control")
        app.on_turn_accepted("list-control")
        app.on_user_reply_final("approve-control", "hidden ack")
        app.on_user_reply_final("list-control", "visible approval list")
        await pilot.pause()
        exported = app._tv.export_text()
        assert "hidden ack" not in exported
        assert "visible approval list" in exported


@pytest.mark.parametrize("tool,result", [
    ("web_fetch", "Authorization: Bearer result-secret-123"),
    ("edit_file", "+ Authorization: Bearer result-secret-123\n- old"),
])
def test_expanded_tool_result_redacts_known_credentials(tool, result):
    block = ToolCallBlock("call1", tool, {}, status="ok", result_text=result, expanded=True)
    assert "result-secret-123" not in block.render_detail()
