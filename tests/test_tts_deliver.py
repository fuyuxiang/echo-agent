"""TTS 确定性投递测试：deliver=true 时生成后自动 publish 音频文件。"""
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest

from echo_agent.agent.tools.tts import TTSTool
from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
from echo_agent.tools.base import ToolExecutionContext


async def _fake_edge(self, text, voice, output: Path):
    # 模拟合成成功并落盘
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(b"fake-audio")
    from echo_agent.agent.tools.base import ToolResult
    return ToolResult(output=f"Audio saved to {output.name}", metadata={"path": str(output)})


@pytest.mark.asyncio
async def test_deliver_publishes_audio(tmp_path):
    publish = AsyncMock()
    tool = TTSTool(workspace=str(tmp_path), publish_fn=publish)
    ctx = ToolExecutionContext(channel="weixin", chat_id="userX")
    with patch.object(TTSTool, "_edge_tts", _fake_edge):
        result = await tool.execute(
            {"text": "北京今天多云", "deliver": True, "output_path": "w.mp3"}, ctx
        )
    assert result.success
    publish.assert_awaited_once()
    event = publish.await_args.args[0]
    assert event.channel == "weixin"
    assert event.chat_id == "userX"
    # 音频作为 FILE 块投递
    assert any(getattr(b, "url", "").endswith("w.mp3") for b in event.content)
    assert "delivered to weixin:userX" in result.output


@pytest.mark.asyncio
async def test_no_deliver_when_flag_absent(tmp_path):
    publish = AsyncMock()
    tool = TTSTool(workspace=str(tmp_path), publish_fn=publish)
    ctx = ToolExecutionContext(channel="weixin", chat_id="userX")
    with patch.object(TTSTool, "_edge_tts", _fake_edge):
        result = await tool.execute({"text": "hi", "output_path": "n.mp3"}, ctx)
    assert result.success
    publish.assert_not_awaited()


@pytest.mark.asyncio
async def test_deliver_falls_back_to_session_key(tmp_path):
    publish = AsyncMock()
    tool = TTSTool(workspace=str(tmp_path), publish_fn=publish)
    # 无 channel/chat_id，仅有 session_key，应能推导目标
    ctx = ToolExecutionContext(session_key="weixin:groupY")
    with patch.object(TTSTool, "_edge_tts", _fake_edge):
        result = await tool.execute(
            {"text": "hi", "deliver": True, "output_path": "s.mp3"}, ctx
        )
    assert result.success
    publish.assert_awaited_once()
    event = publish.await_args.args[0]
    assert event.channel == "weixin"
    assert event.chat_id == "groupY"


@pytest.mark.asyncio
async def test_deliver_reports_when_no_target(tmp_path):
    publish = AsyncMock()
    tool = TTSTool(workspace=str(tmp_path), publish_fn=publish)
    ctx = ToolExecutionContext()  # 无任何目标信息
    with patch.object(TTSTool, "_edge_tts", _fake_edge):
        result = await tool.execute(
            {"text": "hi", "deliver": True, "output_path": "x.mp3"}, ctx
        )
    # 音频文件仍保留供人工取用，但 deliver=true 的整体请求没有
    # 完成，必须以失败告知调用者，不能让调度任务误报成功。
    assert not result.success
    publish.assert_not_awaited()
    assert "not delivered" in result.output
    assert "not delivered" in result.error
    assert (tmp_path / "x.mp3").is_file()
    assert result.metadata["path"].endswith("x.mp3")


@pytest.mark.asyncio
async def test_deliver_propagates_publish_failure(tmp_path):
    publish = AsyncMock(return_value=DeliveryResult(
        DeliveryStage.FAILED, "weixin", error="platform down",
    ))
    tool = TTSTool(workspace=str(tmp_path), publish_fn=publish)
    ctx = ToolExecutionContext(channel="weixin", chat_id="userX")
    with patch.object(TTSTool, "_edge_tts", _fake_edge):
        result = await tool.execute(
            {"text": "hi", "deliver": True, "output_path": "failed.mp3"}, ctx
        )

    assert not result.success
    publish.assert_awaited_once()
    assert "delivery failed: platform down" in result.error
    assert result.metadata["path"].endswith("failed.mp3")
