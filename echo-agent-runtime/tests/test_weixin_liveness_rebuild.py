"""weixin 活性重建的回归测试。

30 分钟没有消息时轮询循环会重建 ClientSession。start() 里建会话带
trust_env=True,重建那一处过去漏了,于是靠 HTTPS_PROXY 出网的部署在第一次
重建之后就再也不走代理 —— 轮询静默收不到任何消息,而 "no messages for 1800s,
rebuilding session" 这行日志恰好长得像"一切正常"。
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.channels import weixin as wx
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.config.schema import WeixinChannelConfig


def _make_weixin(tmp_path: Path) -> WeixinChannel:
    cfg = WeixinChannelConfig(
        account_id="acct@im.bot",
        token="acct@im.bot:tok",
        data_dir=str(tmp_path / "weixin"),
    )
    ch = WeixinChannel(cfg, MessageBus())
    ch._send_session = MagicMock()
    return ch


class TestLivenessRebuildKeepsProxySupport:
    @pytest.mark.asyncio
    async def test_rebuilt_poll_session_keeps_trust_env(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        ch._poll_session = AsyncMock()
        ch._running = True
        built: list[dict] = []

        class _RecordingSession:
            def __init__(self, *args, **kwargs):
                built.append(kwargs)

        monkeypatch.setattr(wx.aiohttp, "ClientSession", _RecordingSession)

        async def fake_get_updates(session, **kwargs):
            ch._running = False  # 一轮即停
            return {"errcode": 0, "msgs": []}

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        # 让活性判定立刻超时。这里调阈值而不是调时钟:last_message_time 也是用
        # time.monotonic() 取的,平移时钟会把它一起平移,判定永远不触发;而事件
        # 循环自己也在调 monotonic,替换掉会引入无关的不确定性。
        monkeypatch.setattr(wx, "_LIVENESS_TIMEOUT", -1)

        await ch._poll_loop()
        assert built == [{"trust_env": True}]

    @pytest.mark.asyncio
    async def test_no_rebuild_while_messages_keep_arriving(self, tmp_path, monkeypatch):
        """有消息进来就不该重建 —— 否则每一轮都在丢弃可用连接。"""
        ch = _make_weixin(tmp_path)
        ch._poll_session = AsyncMock()
        ch._running = True
        ch._spawn_msg_task = MagicMock(side_effect=lambda c: c.close())
        built: list[dict] = []

        class _RecordingSession:
            def __init__(self, *args, **kwargs):
                built.append(kwargs)

        monkeypatch.setattr(wx.aiohttp, "ClientSession", _RecordingSession)

        async def fake_get_updates(session, **kwargs):
            ch._running = False
            return {"errcode": 0, "msgs": [{"from_user_id": "u@x", "item_list": []}]}

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        await ch._poll_loop()
        assert built == []
