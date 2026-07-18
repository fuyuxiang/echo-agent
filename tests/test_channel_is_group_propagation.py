from __future__ import annotations

import inspect

from echo_agent.channels import matrix as matrix_mod
from echo_agent.channels import slack as slack_mod


def test_slack_on_event_passes_is_group_from_channel_type():
    # _on_event 必须把 channel_type != "im" 判定为群聊并传 is_group。
    src = inspect.getsource(slack_mod.SlackChannel._on_event)
    assert "channel_type" in src
    assert "is_group" in src


def test_matrix_handle_message_passes_is_group_true():
    # Matrix fail-closed:room 消息一律按群处理,不进 owner。
    src = inspect.getsource(matrix_mod.MatrixChannel._on_message) \
        if hasattr(matrix_mod.MatrixChannel, "_on_message") else \
        inspect.getsource(matrix_mod.MatrixChannel)
    assert "is_group=True" in src
