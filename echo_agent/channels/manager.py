"""Channel manager — lifecycle management for all channel adapters."""

from __future__ import annotations

import asyncio
import re
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from loguru import logger

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.channels.cli import CLIChannel
from echo_agent.channels.cron import CronChannel
from echo_agent.channels.dingtalk import DingTalkChannel
from echo_agent.channels.discord import DiscordChannel
from echo_agent.channels.email import EmailChannel
from echo_agent.channels.feishu import FeishuChannel
from echo_agent.channels.matrix import MatrixChannel
from echo_agent.channels.qqbot import QQBotChannel
from echo_agent.channels.slack import SlackChannel
from echo_agent.channels.telegram import TelegramChannel
from echo_agent.channels.wecom import WeComChannel
from echo_agent.channels.webhook import WebhookChannel
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.channels.whatsapp import WhatsAppChannel
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig

_CHANNEL_REGISTRY: dict[str, type[BaseChannel]] = {
    "cli": CLIChannel,
    "webhook": WebhookChannel,
    "cron": CronChannel,
    "telegram": TelegramChannel,
    "discord": DiscordChannel,
    "slack": SlackChannel,
    "whatsapp": WhatsAppChannel,
    "weixin": WeixinChannel,
    "qqbot": QQBotChannel,
    "feishu": FeishuChannel,
    "dingtalk": DingTalkChannel,
    "email": EmailChannel,
    "wecom": WeComChannel,
    "matrix": MatrixChannel,
}


_STREAM_CURSOR = " ..."
_THINK_BLOCK_RE = re.compile(r"<think>.*?</think>", re.DOTALL | re.IGNORECASE)
_THINK_OPEN_RE = re.compile(r"<think>.*$", re.DOTALL | re.IGNORECASE)
_THINK_CLOSE_RE = re.compile(r"^.*?</think>", re.DOTALL | re.IGNORECASE)


@dataclass
class _StreamState:
    raw_text: str = ""
    message_id: str = ""
    rendered_text: str = ""
    failed: bool = False
    created_at: float = field(default_factory=time.monotonic)


def register_channel_type(name: str, cls: type[BaseChannel]) -> None:
    _CHANNEL_REGISTRY[name] = cls


_EMOJI_MAP: dict[str, dict[str, str]] = {
    "processing": {"telegram": "\U0001f440", "discord": "\U0001f440", "slack": "eyes", "matrix": "\U0001f440"},
    "success": {"telegram": "\U0001f44d", "discord": "✅", "slack": "white_check_mark", "matrix": "✅"},
    "failure": {"telegram": "❌", "discord": "❌", "slack": "x", "matrix": "❌"},
}


def _emoji(channel_name: str, kind: str) -> str:
    return _EMOJI_MAP.get(kind, {}).get(channel_name, "")


class ChannelManager:
    """Manages the lifecycle of all enabled channel adapters."""

    def __init__(self, config: ChannelsConfig, bus: MessageBus, on_cli_exit: Callable[[], None] | None = None):
        self.config = config
        self.bus = bus
        self._channels: dict[str, BaseChannel] = {}
        self._send_progress = config.send_progress
        self._send_tool_hints = config.send_tool_hints
        # Heartbeat verbosity tier. ChannelsConfig does not carry the heartbeat
        # block (it lives at config.agent.heartbeat), so default here and let the
        # composition root override _heartbeat_cfg when it wires the real value.
        self._heartbeat_cfg = HeartbeatConfig()
        self._on_cli_exit = on_cli_exit
        self._stream_states: dict[str, _StreamState] = {}
        self._heartbeat_msg_ids: dict[str, str] = {}  # inbound_event_id -> platform msg id
        self._delivered_milestone: dict[str, int] = {}  # inbound_event_id -> max delivered seq
        self._finalized_keys: dict[str, float] = {}  # inbound_event_id -> finalize time (bounded set)
        # Which targets a turn has already delivered a final to, so the turn's own
        # reply is not sent on top of a message the tools already delivered there.
        # Keyed per turn, valued by "channel:chat_id" — NOT a bare per-turn flag:
        # a turn that notifies another chat must still answer the one it is in.
        self._finalized_targets: dict[str, set[str]] = {}
        self._inbound_msg_ids: dict[str, tuple[str, str, float]] = {}
        self._max_inbound_ids = 1000
        self._max_stream_states = 500
        self._stream_ttl_seconds = 300.0
        self._inbound_ttl_seconds = 600.0
        self._state_lock = asyncio.Lock()
        self._cleanup_task: asyncio.Task | None = None
        self.bus.subscribe_outbound_global(self._filter_and_dispatch)
        self.bus.subscribe_inbound(self._on_inbound_lifecycle)

    @property
    def active_channels(self) -> list[str]:
        return [name for name, ch in self._channels.items() if ch.is_running]

    def get_channel(self, name: str) -> BaseChannel | None:
        """Look up a running adapter by name, or None.

        Exposed so tools can ask a channel about its own capabilities (e.g.
        send_file checking ``supports_files``) instead of assuming every channel
        can do everything. Returns the adapter even if not yet started — the
        capability flags are static and readable before start_all().
        """
        return self._channels.get(name)

    async def _on_inbound_lifecycle(self, event: InboundEvent) -> None:
        channel = self._channels.get(event.channel)
        if not channel:
            return
        if event.reply_to_id:
            async with self._state_lock:
                self._inbound_msg_ids[event.event_id] = (event.channel, event.reply_to_id, time.monotonic())
                while len(self._inbound_msg_ids) > self._max_inbound_ids:
                    oldest = next(iter(self._inbound_msg_ids))
                    del self._inbound_msg_ids[oldest]
        try:
            await channel.send_typing(event.chat_id, metadata=event.metadata)
        except Exception as e:
            logger.debug("send_typing failed on {}: {}", event.channel, e)
        if event.reply_to_id:
            try:
                await channel.send_read_receipt(event.chat_id, event.reply_to_id, metadata=event.metadata)
            except Exception as e:
                logger.debug("send_read_receipt failed on {}: {}", event.channel, e)
            emoji = _emoji(event.channel, "processing")
            if emoji and getattr(getattr(channel, "config", None), "reactions_enabled", False):
                try:
                    await channel.send_reaction(event.chat_id, event.reply_to_id, emoji, metadata=event.metadata)
                except Exception as e:
                    logger.debug("send_reaction failed on {}: {}", event.channel, e)

    async def _filter_and_dispatch(self, event: OutboundEvent) -> SendResult | None:
        if event.metadata.get("_token_stream"):
            await self._handle_token_stream(event)
            if event.metadata.get("_drop"):
                return

        if event.metadata.get("_heartbeat"):
            await self._handle_heartbeat(event)
            event.metadata["_drop"] = True
            return

        if event.metadata.get("_progress"):
            is_tool_hint = event.metadata.get("_tool_hint", False)
            if is_tool_hint and not self._send_tool_hints:
                event.metadata["_drop"] = True
                return
            if not is_tool_hint and not self._send_progress:
                event.metadata["_drop"] = True
                return
            channel = self._channels.get(event.channel)
            if channel and not channel.supports_edit:
                event.metadata["_drop"] = True
                return

        if event.is_final and event.message_kind == "final":
            result = None
            if not event.metadata.get("_token_stream"):
                result = await self._deliver_final(event)
            await self._on_outbound_final(event)
            return result
        return None

    async def _on_outbound_final(self, event: OutboundEvent) -> None:
        channel = self._channels.get(event.channel)
        if not channel:
            return
        try:
            await channel.stop_typing(event.chat_id)
        except Exception as e:
            logger.debug("stop_typing failed on {}: {}", event.channel, e)
        inbound_event_id = str(event.metadata.get("_inbound_event_id", ""))
        async with self._state_lock:
            mapping = self._inbound_msg_ids.pop(inbound_event_id, None)
            self._heartbeat_msg_ids.pop(inbound_event_id, None)
            self._delivered_milestone.pop(inbound_event_id, None)
        if not mapping:
            return
        _, platform_msg_id, _ = mapping
        if not getattr(getattr(channel, "config", None), "reactions_enabled", False):
            return
        is_error = event.metadata.get("_error", False)
        processing_emoji = _emoji(event.channel, "processing")
        outcome_emoji = _emoji(event.channel, "failure" if is_error else "success")
        if processing_emoji:
            try:
                await channel.remove_reaction(event.chat_id, platform_msg_id, processing_emoji)
            except Exception as e:
                logger.debug("remove_reaction failed on {}: {}", event.channel, e)
        if outcome_emoji:
            try:
                await channel.send_reaction(event.chat_id, platform_msg_id, outcome_emoji)
            except Exception as e:
                logger.debug("send_reaction failed on {}: {}", event.channel, e)

    async def _deliver_final(self, event: OutboundEvent) -> SendResult | None:
        # Delivery ledger for terminal answers to a turn's primary target.
        #
        # States a (turn_id, channel, chat_id, purpose) tuple can be in:
        #
        #   PENDING     — outbound is in flight; nothing committed yet
        #   DELIVERED   — channel.send returned success, target claimed
        #   FAILED      — terminal failure (channel refused), target NOT
        #                 claimed so a retry has room to try again
        #   RELEASED    — cleared by caller (e.g. caller changed its mind)
        #
        # Two distinct bugs the old shape had, both pinned here:
        #
        #   (a) An approval prompt — which inherits OutboundEvent's
        #       ``is_final=True`` / ``message_kind="final"`` defaults — claimed
        #       the target before the actual answer had run, so the user's
        #       post-/approve result was suppressed as a duplicate. Fixed by
        #       the gate marking those prompts as message_kind="approval_prompt"
        #       so they never reach this ledger at all (see _filter_and_dispatch).
        #
        #   (b) The old code wrote the target BEFORE awaiting channel.send and
        #       never unwrote it on failure, so a transient transport error
        #       suppressed every later retry of the same turn. Fixed by only
        #       committing the target on a successful receipt.
        #
        # A tool delivery (``_tool_delivery``) is an explicit user instruction
        # to send to a target and may legitimately repeat, so it claims the
        # target but is NOT itself suppressed by an earlier claim — two message
        # calls to different chats are two messages the model asked for.
        key = str(event.metadata.get("_inbound_event_id", ""))
        target = f"{event.channel}:{event.chat_id}"
        is_tool_delivery = bool(event.metadata.get("_tool_delivery"))

        if key:
            async with self._state_lock:
                claimed = self._finalized_targets.setdefault(key, set())
                already_delivered = target in claimed
            if already_delivered and not is_tool_delivery:
                logger.debug(
                    "Suppressing duplicate final for {} (already delivered by a tool this turn)",
                    target,
                )
                event.metadata["_drop"] = True
                return
            # Record finalization so a late heartbeat that fires during the
            # in-flight channel.send is discarded rather than overwriting the
            # answer. This is the only state written before transport: a
            # "this turn has *begun* its terminal write" marker, not a
            # delivery claim. The claim itself happens only after a successful
            # channel.send below.
            async with self._state_lock:
                self._finalized_keys[key] = time.monotonic()
                while len(self._finalized_keys) > self._max_inbound_ids:
                    oldest = next(iter(self._finalized_keys))
                    del self._finalized_keys[oldest]

        channel = self._channels.get(event.channel)
        has_content = any(b.text or b.url for b in event.content)
        if not has_content:
            event.metadata["_drop"] = True
            return
        if not channel:
            logger.warning(
                "Outbound dropped: no channel registered for channel={!r} chat_id={} event_id={}",
                event.channel, str(event.chat_id)[:16], event.metadata.get("_inbound_event_id", ""),
            )
            return

        # If a heartbeat already occupies a message on an editable channel, seal
        # the final answer into that same message so the turn uses one slot total.
        if key and getattr(channel, "supports_edit", False):
            async with self._state_lock:
                hb_msg_id = self._heartbeat_msg_ids.get(key)
            if hb_msg_id:
                try:
                    result = await channel.edit_message(
                        event.chat_id, hb_msg_id, event.text,
                        metadata=self._public_metadata(event.metadata),
                        finalize=True,
                    )
                    if result and not result.success:
                        logger.warning("Final seal-edit failed on {}: {}", event.channel, result.error)
                        await self._delete_stale_heartbeat(channel, event, hb_msg_id)
                        # fall through to a fresh send on edit failure
                    else:
                        # In-place edit succeeded → the heartbeat slot IS the
                        # delivery. Commit the claim now; do NOT leave the
                        # target unclaimed, otherwise a later fallback send
                        # would duplicate it.
                        if key:
                            async with self._state_lock:
                                claimed = self._finalized_targets.setdefault(key, set())
                                claimed.add(target)
                        event.metadata["_drop"] = True
                        return result
                except Exception as e:
                    logger.error("Final seal-edit exception on {}: {}", event.channel, e)
                    await self._delete_stale_heartbeat(channel, event, hb_msg_id)
                    # fall through to a fresh send on edit exception

        send_event = OutboundEvent(
            channel=event.channel,
            chat_id=event.chat_id,
            content=list(event.content),
            reply_to_id=event.reply_to_id,
        )
        send_event.is_final = True
        send_event.message_kind = "final"
        send_event.metadata = self._public_metadata(event.metadata)
        try:
            result = await channel.send(send_event)
        except Exception as e:
            logger.error("Final delivery exception on {}: {}", event.channel, e)
            # Transport threw — leave the target unclaimed so a retry can try
            # again. Do NOT mark _drop; the caller's retry path needs this event.
            return SendResult(success=False, error=str(e))
        if result and not result.success:
            logger.warning("Final delivery failed on {}: {}", event.channel, result.error)
            # Channel refused (transport returned a non-success receipt).
            # Leave the target unclaimed — a later retry of this same turn, if
            # the framework invokes one, has room to try again.
            return result
        # Success receipt. Commit the claim so subsequent finals to the same
        # target within this turn are deduped (the duplicate-final bug the
        # ledger was originally built for).
        if key:
            async with self._state_lock:
                claimed = self._finalized_targets.setdefault(key, set())
                claimed.add(target)
                while len(self._finalized_targets) > self._max_inbound_ids:
                    oldest = next(iter(self._finalized_targets))
                    del self._finalized_targets[oldest]
        event.metadata["_drop"] = True
        return result

    async def _delete_stale_heartbeat(self, channel, event: OutboundEvent, msg_id: str) -> None:
        """Best-effort removal of a lingering heartbeat message after a failed
        seal-edit. Isolated from the caller: failures only log at debug level."""
        try:
            await channel.delete_message(
                event.chat_id, msg_id,
                metadata=self._public_metadata(event.metadata),
            )
        except Exception as e:  # noqa: BLE001
            logger.debug("stale heartbeat delete failed on {}: {}", event.channel, e)

    async def _handle_heartbeat(self, event: OutboundEvent) -> None:
        channel = self._channels.get(event.channel)
        if not channel:
            return
        # Authoritative finalize guard: if the turn is already finalized, drop
        # this late beat entirely — no typing refresh, no text, no edit — so it
        # cannot overwrite the answer or post a stray "正在处理中…" after it.
        key = str(event.metadata.get("_inbound_event_id", ""))
        if key:
            async with self._state_lock:
                if key in self._finalized_keys:
                    return
        # Milestone dedup: each turn delivers a given milestone seq at
        # most once. The seq is set by ProgressHeartbeat and always present on
        # real beats; _delivered_milestone keys off the turn's milestone, not a
        # msg id. This is the structural fix for weixin spam.
        has_milestone = "_hb_milestone" in event.metadata
        milestone = int(event.metadata.get("_hb_milestone", 0))
        pass_only_typing = False
        if key and has_milestone:
            async with self._state_lock:
                pass_only_typing = milestone <= self._delivered_milestone.get(key, 0)
        # Keep typing alive on every beat, regardless of text display.
        try:
            await channel.send_typing(event.chat_id, metadata=self._public_metadata(event.metadata))
        except Exception as e:  # noqa: BLE001
            logger.debug("heartbeat typing failed on {}: {}", event.channel, e)
        if pass_only_typing or not event.text:
            return
        try:
            await self._dispatch_heartbeat_text(channel, event, key, milestone, has_milestone)
        except Exception as e:  # noqa: BLE001 — heartbeat must never crash delivery
            logger.debug("heartbeat delivery failed on {}: {}", event.channel, e)

    async def _dispatch_heartbeat_text(
        self, channel, event: OutboundEvent, key: str, milestone: int, has_milestone: bool
    ) -> None:
        verbosity = getattr(getattr(self, "_heartbeat_cfg", None), "verbosity", "key_milestones")
        if verbosity == "silent":
            return
        if not getattr(channel, "is_realtime", True):
            return  # async tier (e.g. email): zero heartbeat
        is_key = bool(event.metadata.get("_hb_key", False))
        if (
            verbosity == "key_milestones"
            and has_milestone
            and not getattr(channel, "supports_edit", False)
            and not is_key
        ):
            return  # plain-text tier in key-only mode: skip non-key milestones
        if getattr(channel, "supports_edit", False):
            # Editable channel: reuse the turn's single heartbeat slot via edit.
            await self._heartbeat_edit_or_send(channel, event, key)
        elif has_milestone:
            # Uneditable plain-text tier: milestone dedup upstream already
            # guaranteed this seq is new, so just send (no msg-id bookkeeping).
            await self._heartbeat_send(channel, event, key)
        # Record the delivered milestone so repeat beats for the same seq are
        # suppressed at the top of _handle_heartbeat.
        if key and has_milestone:
            async with self._state_lock:
                self._delivered_milestone[key] = milestone
                while len(self._delivered_milestone) > self._max_inbound_ids:
                    oldest = next(iter(self._delivered_milestone))
                    del self._delivered_milestone[oldest]

    async def _heartbeat_edit_or_send(self, channel, event: OutboundEvent, key: str) -> None:
        """Editable tier: edit the turn's draft if one exists, else first-send it."""
        async with self._state_lock:
            msg_id = self._heartbeat_msg_ids.get(key)
        if msg_id:
            await channel.edit_message(
                event.chat_id, msg_id, event.text,
                metadata=self._public_metadata(event.metadata),
            )
            return
        result = await channel.send(self._heartbeat_send_event(event))
        if result and result.success and result.message_id and key:
            async with self._state_lock:
                self._heartbeat_msg_ids[key] = result.message_id
                while len(self._heartbeat_msg_ids) > self._max_inbound_ids:
                    oldest = next(iter(self._heartbeat_msg_ids))
                    del self._heartbeat_msg_ids[oldest]

    def _heartbeat_send_event(self, event: OutboundEvent) -> OutboundEvent:
        """Build the OutboundEvent used to deliver a heartbeat beat (DRY)."""
        send_event = OutboundEvent.text_reply(
            channel=event.channel, chat_id=event.chat_id,
            text=event.text, reply_to_id=event.reply_to_id,
        )
        send_event.is_final = False
        send_event.message_kind = "heartbeat"
        send_event.metadata = self._public_metadata(event.metadata)
        return send_event

    async def _heartbeat_send(self, channel, event: OutboundEvent, key: str) -> None:
        send_event = self._heartbeat_send_event(event)
        result = await channel.send(send_event)
        if result and result.success and not getattr(result, "skipped", False):
            logger.info("heartbeat delivered: channel={} chat={}", event.channel, str(event.chat_id)[:8])
        elif result and not result.success:
            logger.info("heartbeat send failed: channel={} error={}", event.channel, getattr(result, "error", ""))

    async def _handle_token_stream(self, event: OutboundEvent) -> None:
        channel = self._channels.get(event.channel)
        if channel is None:
            return

        if not channel.supports_edit:
            if not event.is_final:
                event.metadata["_drop"] = True
            return

        event.metadata["_drop"] = True
        stream_key = self._stream_key(event)
        async with self._state_lock:
            state = self._stream_states.setdefault(stream_key, _StreamState())
            while len(self._stream_states) > self._max_stream_states:
                oldest = next(iter(self._stream_states))
                del self._stream_states[oldest]

        if event.metadata.get("_stream_full_text"):
            state.raw_text = event.text or ""
        else:
            state.raw_text += event.text or ""

        visible_text = self._visible_stream_text(state.raw_text, final=event.is_final)

        if state.failed:
            if event.is_final:
                await self._send_stream_fallback(channel, event, visible_text)
                async with self._state_lock:
                    self._stream_states.pop(stream_key, None)
            return

        rendered_text = self._render_stream_text(visible_text, final=event.is_final)
        if not rendered_text:
            if event.is_final:
                async with self._state_lock:
                    self._stream_states.pop(stream_key, None)
            return

        if rendered_text == state.rendered_text:
            if event.is_final:
                async with self._state_lock:
                    self._stream_states.pop(stream_key, None)
            return

        if state.message_id:
            result = await channel.edit_message(
                event.chat_id,
                state.message_id,
                rendered_text,
                metadata=self._public_metadata(event.metadata),
                finalize=event.is_final,
            )
        else:
            send_event = OutboundEvent.text_reply(
                channel=event.channel,
                chat_id=event.chat_id,
                text=rendered_text,
                reply_to_id=event.reply_to_id,
            )
            send_event.is_final = event.is_final
            send_event.message_kind = event.message_kind
            send_event.metadata = self._public_metadata(event.metadata)
            result = await channel.send(send_event)

        if not result or not result.success:
            error = result.error if result else "channel returned no send result"
            logger.warning("Token stream delivery failed on {}: {}", event.channel, error)
            state.failed = True
            if event.is_final:
                await self._send_stream_fallback(channel, event, visible_text)
                async with self._state_lock:
                    self._stream_states.pop(stream_key, None)
            return

        if result.message_id:
            state.message_id = result.message_id
        elif not event.is_final:
            logger.warning("Token stream send on {} did not return a message id", event.channel)
            state.failed = True
            return

        state.rendered_text = rendered_text
        if event.is_final:
            async with self._state_lock:
                self._stream_states.pop(stream_key, None)

    async def _send_stream_fallback(self, channel: BaseChannel, event: OutboundEvent, text: str) -> None:
        final_text = self._render_stream_text(text, final=True)
        if not final_text:
            return
        fallback = OutboundEvent.text_reply(
            channel=event.channel,
            chat_id=event.chat_id,
            text=final_text,
            reply_to_id=event.reply_to_id,
        )
        fallback.is_final = True
        fallback.message_kind = "final"
        fallback.metadata = self._public_metadata(event.metadata)
        result = await channel.send(fallback)
        if result and not result.success:
            logger.warning("Token stream fallback send failed on {}: {}", event.channel, result.error)

    @staticmethod
    def _stream_key(event: OutboundEvent) -> str:
        stream_id = str(event.metadata.get("_inbound_event_id") or event.event_id)
        return f"{event.channel}:{event.chat_id}:{stream_id}"

    @staticmethod
    def _public_metadata(metadata: dict[str, Any]) -> dict[str, Any]:
        return {key: value for key, value in metadata.items() if not key.startswith("_")}

    @staticmethod
    def _visible_stream_text(text: str, *, final: bool) -> str:
        if not text:
            return ""
        visible = _THINK_BLOCK_RE.sub("", text)
        visible = _THINK_CLOSE_RE.sub("", visible)
        visible = _THINK_OPEN_RE.sub("", visible)
        if not final:
            visible = ChannelManager._trim_partial_think_marker(visible)
            return visible
        return visible.strip()

    @staticmethod
    def _trim_partial_think_marker(text: str) -> str:
        marker = "<think>"
        lower = text.lower()
        for length in range(len(marker) - 1, 0, -1):
            if lower.endswith(marker[:length]):
                return text[:-length]
        return text

    @staticmethod
    def _render_stream_text(text: str, *, final: bool) -> str:
        rendered = text.strip() if final else text.rstrip()
        if final:
            return rendered
        if not rendered:
            return ""
        return f"{rendered}{_STREAM_CURSOR}"

    async def start_all(self) -> None:
        for name, cls in _CHANNEL_REGISTRY.items():
            channel_config = getattr(self.config, name, None)
            if channel_config is None:
                continue
            if not getattr(channel_config, "enabled", False):
                continue
            try:
                if cls is CLIChannel:
                    channel = cls(channel_config, self.bus, on_exit=self._on_cli_exit)
                else:
                    channel = cls(channel_config, self.bus)
                if self.config.transcription_api_key:
                    channel.transcription_api_key = self.config.transcription_api_key
                await channel.start()
                self._channels[name] = channel
                if channel.is_running:
                    logger.info("Channel {} started", name)
                else:
                    logger.info("Channel {} inactive", name)
            except Exception as e:
                logger.error("Failed to start channel {}: {}", name, e)
        self._cleanup_task = asyncio.create_task(self._ttl_cleanup_loop())

    async def stop_all(self) -> None:
        if self._cleanup_task:
            self._cleanup_task.cancel()
            try:
                await self._cleanup_task
            except asyncio.CancelledError:
                pass
            self._cleanup_task = None
        for name, channel in self._channels.items():
            try:
                await asyncio.wait_for(channel.stop(), timeout=10)
                logger.info("Channel {} stopped", name)
            except asyncio.TimeoutError:
                logger.warning("Channel {} stop timed out", name)
            except Exception as e:
                logger.error("Failed to stop channel {}: {}", name, e)
        self._channels.clear()

    async def _ttl_cleanup_loop(self) -> None:
        """Periodically evict stale stream states and inbound msg IDs."""
        while True:
            await asyncio.sleep(60.0)
            now = time.monotonic()
            async with self._state_lock:
                stale_streams = [
                    k for k, v in self._stream_states.items()
                    if (now - v.created_at) > self._stream_ttl_seconds
                ]
                for k in stale_streams:
                    del self._stream_states[k]

                stale_inbound = [
                    k for k, v in self._inbound_msg_ids.items()
                    if (now - v[2]) > self._inbound_ttl_seconds
                ]
                for k in stale_inbound:
                    del self._inbound_msg_ids[k]

                stale_finalized = [
                    k for k, ts in self._finalized_keys.items()
                    if (now - ts) > self._inbound_ttl_seconds
                ]
                for k in stale_finalized:
                    del self._finalized_keys[k]
                    # Same lifetime as the timestamp it is keyed alongside;
                    # expiring one without the other would leak the target sets.
                    self._finalized_targets.pop(k, None)
