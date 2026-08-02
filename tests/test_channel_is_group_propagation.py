from __future__ import annotations

import inspect
from unittest.mock import AsyncMock

import pytest

from echo_agent.channels import matrix as matrix_mod
from echo_agent.channels import slack as slack_mod


def test_slack_on_event_passes_is_group_from_channel_type():
    # _on_event 必须把 channel_type != "im" 判定为群聊并传 is_group。
    src = inspect.getsource(slack_mod.SlackChannel._on_event)
    assert "channel_type" in src
    assert "is_group" in src


@pytest.mark.asyncio
async def test_matrix_handle_message_passes_is_group_true():
    # Matrix fail-closed:room 消息一律按群处理,不进 owner。
    channel = object.__new__(matrix_mod.MatrixChannel)
    channel._user_id = "@bot:example.org"
    channel._allow_rooms = None
    channel._handle_message = AsyncMock()

    await channel._on_event(
        "!room:example.org",
        {
            "type": "m.room.message",
            "event_id": "$event",
            "sender": "@alice:example.org",
            "content": {"msgtype": "m.text", "body": "hello"},
        },
    )

    channel._handle_message.assert_awaited_once_with(
        sender_id="@alice:example.org",
        chat_id="!room:example.org",
        text="hello",
        media=None,
        reply_to_id="$event",
        metadata={"msgtype": "m.text"},
        is_group=True,
    )
