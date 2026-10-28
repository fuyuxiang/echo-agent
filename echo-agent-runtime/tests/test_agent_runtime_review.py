"""Regressions for control answers and owned browser setup resources."""

import asyncio
import os
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.browser.session import BrowserSessionManager
from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.tools.clarify import ClarifyTool
from echo_agent.tools import ToolExecutionContext


@pytest.mark.asyncio
@pytest.mark.parametrize("answer", ["B", ""])
async def test_clarify_answer_during_prompt_delivery_reaches_the_real_tool(answer):
    manager = ClarifyManager()

    class ReplyDuringDelivery:
        async def emit(self, event, kind, data, summary):
            assert manager.resolve(data["clarify_id"], answer)
            assert not manager.resolve(data["clarify_id"], "duplicate")

    stage = InferenceStage.__new__(InferenceStage)
    stage._clarify = manager
    stage._cog = ReplyDuringDelivery()
    call = SimpleNamespace(name="clarify", arguments={"question": "Which one?", "options": ["A", "B"]})
    event = SimpleNamespace(channel="gateway:cli", session_key="gateway:cli:owner", sender_id="owner")
    await stage._prepare_clarify(call, event)
    result = await asyncio.wait_for(ClarifyTool(manager).execute(
        call.arguments, ToolExecutionContext(channel="gateway:cli", user_id="owner"),
    ), timeout=1)
    assert result.success
    assert result.output == answer
    assert manager.get(call.arguments["_clarify_id"]) is None


@pytest.mark.asyncio
@pytest.mark.parametrize("cancelled", [False, True])
async def test_browser_open_reclaims_context_when_page_setup_does_not_finish(cancelled):
    manager = BrowserSessionManager()
    context = MagicMock()
    context.close = AsyncMock()
    entered = asyncio.Event()

    async def new_page():
        entered.set()
        if cancelled:
            await asyncio.Event().wait()
        raise RuntimeError("page setup failed")

    context.new_page = new_page
    manager._browser = MagicMock()
    manager._browser.new_context = AsyncMock(return_value=context)
    manager._ensure_browser = AsyncMock()
    opening = asyncio.create_task(manager.open(owner="owner"))
    await entered.wait()
    if cancelled:
        opening.cancel()
    with pytest.raises(asyncio.CancelledError if cancelled else RuntimeError):
        await opening
    context.close.assert_awaited_once()
    assert manager.get_count() == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("finish_reason", ["error", "length", "content_filter"])
async def test_failed_compression_preserves_history_and_media_for_retry(finish_reason):
    from echo_agent.agent.compression import ConversationCompressor
    from echo_agent.config.schema import CompressionConfig
    from echo_agent.models.provider import LLMResponse

    provider = SimpleNamespace(chat_with_retry=AsyncMock(return_value=LLMResponse(
        content="unusable partial summary", finish_reason=finish_reason,
    )))
    compressor = ConversationCompressor(
        CompressionConfig(head_protect_count=1, tail_budget_ratio=0.05),
        context_window_tokens=1000, provider=provider, default_model="controlled",
    )
    messages = ([{"role": "user", "content": "start", "media_refs": [{"cache_path": "photo.png"}]}]
                + [{"role": role, "content": "must survive " + str(i) + "x" * 200}
                   for i in range(10) for role in ("assistant", "user")]
                + [{"role": "assistant", "content": "latest"}])
    result = await compressor.compress(messages)
    assert not result.was_compressed
    assert result.messages == messages
    assert messages[0]["media_refs"] == [{"cache_path": "photo.png"}]
    assert result.stats.compression_count == 0
    # Cooldown must also preserve the middle instead of turning a model outage
    # into a persisted head/tail-only transcript on the user's next attempt.
    again = await compressor.compress(messages)
    assert again.messages == messages
    assert provider.chat_with_retry.await_count == 1


@pytest.mark.asyncio
async def test_inspection_delivery_channel_does_not_turn_automation_into_interactive_work(tmp_path):
    from echo_agent.agent.approval_gate import ApprovalGate
    from echo_agent.agent.inspection.store import InspectStore
    from echo_agent.agent.inspection.tick import run_inspection_tick
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.permissions.manager import ApprovalManager

    source = tmp_path / "INSPECT.md"
    source.write_text("## Check\n- interval: 1m\n- check: inspect workspace\n")
    bus = MessageBus()
    store = InspectStore(source, tmp_path / "state.json")
    cfg = SimpleNamespace(max_items_per_tick=1, deliver_channel="telegram", deliver_chat_id="owner")
    assert await run_inspection_tick(store, cfg, bus, now_sec=60) == 1
    event = await bus._inbound_queue.get()
    config = Config()
    config.permissions.approval.unattended_policy = "deny"
    config.permissions.approval.trusted_channels = ["telegram"]
    gate = ApprovalGate(config=config, approval=ApprovalManager(), inference=MagicMock(), bus=bus)
    result = await gate.check("write_file", {"path": "note.txt", "content": "change"},
                              event.sender_id, channel=event.channel, event=event)
    assert result.denial is not None
    assert "unattended" in result.denial.error


@pytest.mark.asyncio
@pytest.mark.parametrize("tool_name", ["write_file", "exec"])
@pytest.mark.parametrize("channel", ["gateway:cli", "telegram", "cron"])
async def test_explicit_tool_denial_precedes_all_approval_conveniences(tool_name, channel):
    from echo_agent.agent.approval_gate import ApprovalGate
    from echo_agent.bus.events import InboundEvent
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.permissions.manager import ApprovalManager

    config = Config()
    config.permissions.approval.auto_deny = [tool_name]
    config.permissions.approval.auto_approve = [tool_name]
    config.permissions.approval.mode = "off"
    config.permissions.approval.trusted_channels = [channel]
    event = InboundEvent(channel=channel, sender_id="owner", chat_id="chat")
    event.cron_authorized = True
    gate = ApprovalGate(config=config, approval=ApprovalManager(), inference=MagicMock(), bus=MessageBus())
    result = await gate.check(tool_name, {"command": "echo safe", "path": "note.txt"},
                              "owner", channel=channel, event=event)
    assert result.denial is not None
    assert "auto-denied" in result.denial.error


@pytest.mark.parametrize("prior_grant", ["auto_approve", "signature"])
def test_approval_manager_explicit_deny_overrides_existing_grants(prior_grant):
    from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus

    manager = ApprovalManager(auto_deny=["exec"],
                              auto_approve=["exec"] if prior_grant == "auto_approve" else [])
    params = {"command": "echo safe"}
    manager._approved_signatures.add(manager._signature("exec", "exec", params, "owner"))
    assert manager.needs_approval("exec")
    request = manager.request_approval("exec", tool_name="exec", params=params, user_id="owner")
    assert request.status == ApprovalStatus.DENIED


@pytest.mark.asyncio
@pytest.mark.parametrize("finish_reason", ["error", "length", "content_filter"])
async def test_incomplete_smart_approval_cannot_approve_even_an_approve_prefix(finish_reason):
    from echo_agent.agent.approval_gate import ApprovalGate
    from echo_agent.bus.events import InboundEvent
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.models.provider import LLMResponse
    from echo_agent.permissions.manager import ApprovalManager
    from echo_agent.security.smart_approval import smart_approve

    provider = SimpleNamespace(chat_with_retry=AsyncMock(return_value=LLMResponse(
        content="APPROVE", finish_reason=finish_reason,
    )))
    assert await smart_approve("exec", "echo safe", "shell", provider) == "unavailable"
    config = Config()
    config.permissions.approval.mode = "smart"
    config.permissions.approval.trusted_channels = []
    config.permissions.elevated.enabled = True
    config.permissions.elevated.allow_from = {"telegram": ["owner"]}
    approvals = ApprovalManager()
    gate = ApprovalGate(config=config, approval=approvals, inference=MagicMock(),
                        bus=MessageBus(), provider=provider)
    event = InboundEvent(channel="telegram", sender_id="owner", chat_id="chat")
    result = await gate.check("exec", {"command": "echo safe"}, "owner", channel=event.channel, event=event)
    assert result.denial is not None and result.notify_user
    assert not approvals.get_pending()


def test_channel_import_without_crypto_is_safe_and_enabled_wecom_reports_dependency(tmp_path):
    # A fresh process avoids already-cached optional modules from the test
    # runner. The finder models the actual minimal wheel failure without
    # uninstalling or weakening the production credential encryption backend.
    code = """
import asyncio
import importlib.abc
import sys

class NoCrypto(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == 'cryptography' or fullname.startswith('cryptography.'):
            raise ModuleNotFoundError('cryptography unavailable in minimal environment', name=fullname)

sys.meta_path.insert(0, NoCrypto())
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.wecom import WeComChannel
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import ChannelsConfig, WeComChannelConfig
channel = WeComChannel(WeComChannelConfig(enabled=True), MessageBus())
async def check():
    try:
        await channel.start()
    except RuntimeError as exc:
        assert 'cryptography' in str(exc) and 'install' in str(exc), str(exc)
        assert channel._session is None
    else:
        await channel.stop()
        raise AssertionError('enabled channel did not diagnose missing crypto')
    config = ChannelsConfig()
    config.cli.enabled = False
    config.wecom.enabled = True
    config.cron.enabled = True
    manager = ChannelManager(config, MessageBus())
    try:
        await manager.start_all()
        assert 'cron' in manager.active_channels
        assert 'wecom' not in manager.active_channels
    finally:
        await manager.stop_all()
asyncio.run(check())
print('disabled imports safe; enabled dependency diagnosed')
"""
    env = dict(os.environ)
    env["PYTHONPATH"] = str(Path(__file__).resolve().parents[1])
    result = subprocess.run([sys.executable, "-c", code], cwd=tmp_path, env=env,
                            text=True, capture_output=True, timeout=15)
    assert result.returncode == 0, result.stderr
    assert "disabled imports safe" in result.stdout
