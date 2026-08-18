"""Base channel interface — all platform adapters implement this."""

from __future__ import annotations

import hashlib
from abc import ABC, abstractmethod
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.bus.events import InboundEvent, OutboundEvent, ContentBlock, ContentType, PollRequest
from echo_agent.bus.queue import MessageBus


@dataclass
class SendResult:
    success: bool
    message_id: str = ""
    error: str = ""
    skipped: bool = False


class BaseChannel(ABC):
    """Abstract base for chat channel implementations.

    Each channel (Telegram, Discord, Webhook, CLI, Cron, etc.) implements this
    to integrate with the message bus.
    """

    name: str = "base"
    transcription_api_key: str = ""
    supports_edit: bool = False
    supports_reactions: bool = False
    is_realtime: bool = True          # False for async channels (email/webhook/cron)
    # Whether this channel can render a real, selectable choice control (as the
    # CLI TUI does from a clarify_request frame). IM channels can only show text,
    # so the clarify tool must not promise the model a clickable picker there.
    supports_interactive_choices: bool = False
    # Whether ``send()`` actually uploads FILE / IMAGE content blocks, rather
    # than sending only the accompanying caption text.
    #
    # Most channels consume `event.text` and ignore structured media blocks
    # entirely, so a send_file call to them delivered the caption and silently
    # dropped the attachment — while the tool told the model "File sent". A
    # channel that implements real uploads sets this True; send_file consults it
    # and tells the model the truth when it is False. Default False is the
    # conservative reading: a channel that has not been taught to upload cannot
    # claim it.
    supports_files: bool = False

    def __init__(self, config: Any, bus: MessageBus):
        self.config = config
        self.bus = bus
        self._running = False
        self.transcription_api_key = getattr(config, "transcription_api_key", "") or ""

    @abstractmethod
    async def start(self) -> None:
        """Start listening for messages."""

    @abstractmethod
    async def stop(self) -> None:
        """Stop and clean up resources."""

    @abstractmethod
    async def send(self, event: OutboundEvent) -> SendResult | None:
        """Send a message through this channel."""

    def should_deliver(self, event: OutboundEvent) -> bool:
        """Return False to silently skip non-final messages on channels that cannot edit.

        Channels that support message editing (Telegram, Discord) can show progress
        then overwrite it with the final response. Channels without edit support would
        deliver every intermediate chunk as a separate, irrevocable message — confusing
        the user with duplicates. This guard prevents that at the base layer.
        """
        if self.supports_edit:
            return True
        if event.is_final:
            return True
        # Heartbeats are level-triggered progress beats that the ChannelManager
        # has already deduped per the turn's milestone seq and filtered per the
        # channel's verbosity tier before reaching here. Approval prompts are
        # control messages: the parked turn cannot continue until the user sees
        # and answers one. Neither is a retractable progress/draft chunk, so both
        # must pass on uneditable channels such as weixin.
        if event.message_kind in {"heartbeat", "approval_prompt"}:
            return True
        return False

    async def edit_message(
        self,
        chat_id: str,
        message_id: str,
        text: str,
        *,
        metadata: dict[str, Any] | None = None,
        finalize: bool = False,
    ) -> SendResult:
        """Edit an existing platform message when the channel supports it."""
        return SendResult(success=False, error=f"channel {self.name} does not support message editing")

    async def send_typing(self, chat_id: str, metadata: dict[str, Any] | None = None) -> None:
        pass

    async def stop_typing(self, chat_id: str) -> None:
        pass

    async def send_reaction(self, chat_id: str, message_id: str, emoji: str, metadata: dict[str, Any] | None = None) -> SendResult:
        return SendResult(success=False, error=f"channel {self.name} does not support reactions")

    async def remove_reaction(self, chat_id: str, message_id: str, emoji: str, metadata: dict[str, Any] | None = None) -> SendResult:
        return SendResult(success=False, error=f"channel {self.name} does not support reactions")

    async def send_poll(self, chat_id: str, poll: PollRequest, metadata: dict[str, Any] | None = None) -> SendResult:
        return SendResult(success=False, error=f"channel {self.name} does not support polls")

    async def delete_message(self, chat_id: str, message_id: str, metadata: dict[str, Any] | None = None) -> SendResult:
        return SendResult(success=False, error=f"channel {self.name} does not support message deletion")

    async def send_read_receipt(self, chat_id: str, message_id: str, metadata: dict[str, Any] | None = None) -> None:
        pass

    async def send_voice(self, chat_id: str, audio_source: str, metadata: dict[str, Any] | None = None) -> SendResult:
        return SendResult(success=False, error=f"channel {self.name} does not support voice messages")

    def is_allowed(self, sender_id: str) -> bool:
        allow_list = getattr(self.config, "allow_from", [])
        if not allow_list:
            return True
        if "*" in allow_list:
            return True
        return str(sender_id) in allow_list

    def _build_event(
        self,
        sender_id: str,
        chat_id: str,
        text: str,
        media: list[dict[str, str]] | None = None,
        metadata: dict[str, Any] | None = None,
        session_key: str | None = None,
        reply_to_id: str | None = None,
        reply_to_text: str | None = None,
        reply_to_sender: str | None = None,
        reply_to_is_own: bool = False,
        thread_id: str | None = None,
        is_group: bool = False,
    ) -> InboundEvent:
        if not self.is_allowed(sender_id):
            logger.warning("Access denied for sender {} on channel {}", sender_id, self.name)
            raise PermissionError(f"sender {sender_id} is not allowed on channel {self.name}")

        # Inbound metadata is untrusted input (a webhook body, an IM payload,
        # etc.). Internal control keys are namespaced with a leading underscore
        # (_unattended, _cron_authorized, _drop, ...) and some of them gate the
        # approval path — so a channel that forwarded a caller-supplied key like
        # {"_cron_authorized": true} could bypass EXEC approval. Strip the "_"
        # namespace here, the single choke point every external channel routes
        # through. This mirrors the outbound _public_metadata filter, making the
        # "_ keys are internal-only" invariant symmetric on both directions.
        # Trusted internal producers (scheduler/delivery.py, cron.py,
        # gateway/server.py) construct InboundEvent directly and are unaffected.
        safe_metadata = {
            k: v for k, v in (metadata or {}).items() if not str(k).startswith("_")
        }

        content_blocks = [ContentBlock(type=ContentType.TEXT, text=text)]
        for item in (media or []):
            name = item.get("name", "")
            meta: dict[str, Any] = {}
            if name:
                meta["name"] = name
            if item.get("aes_key"):
                meta["aes_key"] = item["aes_key"]
            content_blocks.append(ContentBlock(
                type=ContentType(item.get("type", "file")),
                url=item.get("url", ""),
                mime_type=item.get("mime_type", ""),
                metadata=meta,
            ))

        return InboundEvent(
            channel=self.name,
            sender_id=str(sender_id),
            chat_id=str(chat_id),
            content=content_blocks,
            reply_to_id=reply_to_id,
            reply_to_text=reply_to_text,
            reply_to_sender=reply_to_sender,
            reply_to_is_own=reply_to_is_own,
            thread_id=thread_id,
            session_key_override=session_key,
            metadata=safe_metadata,
            is_group=is_group,
        )

    async def _handle_message(
        self,
        sender_id: str,
        chat_id: str,
        text: str,
        media: list[dict[str, str]] | None = None,
        metadata: dict[str, Any] | None = None,
        session_key: str | None = None,
        reply_to_id: str | None = None,
        reply_to_text: str | None = None,
        reply_to_sender: str | None = None,
        reply_to_is_own: bool = False,
        thread_id: str | None = None,
        is_group: bool = False,
    ) -> InboundEvent | None:
        try:
            event = self._build_event(
                sender_id=sender_id,
                chat_id=chat_id,
                text=text,
                media=media,
                metadata=metadata,
                session_key=session_key,
                reply_to_id=reply_to_id,
                reply_to_text=reply_to_text,
                reply_to_sender=reply_to_sender,
                reply_to_is_own=reply_to_is_own,
                thread_id=thread_id,
                is_group=is_group,
            )
        except PermissionError:
            return None
        accepted = await self.bus.publish_inbound(event)
        if not accepted:
            # Tell the user instead of silently dropping their message.
            try:
                await self.bus.publish_outbound(OutboundEvent.text_reply(
                    channel=event.channel,
                    chat_id=event.chat_id,
                    text="系统繁忙，消息暂时无法处理，请稍后重试。",
                    reply_to_id=event.reply_to_id,
                ))
            except Exception as e:
                logger.warning("Failed to send busy notice on {}: {}", self.name, e)
            return None
        return event

    @property
    def is_running(self) -> bool:
        return self._running

    _media_cache_root: Path | None = None

    async def _resolve_media_to_cache(
        self,
        source_id: str,
        platform: str,
        fetch: Callable[[], Awaitable[bytes]],
        suffix: str = ".jpg",
    ) -> str | None:
        """Download media via a channel-provided *fetch* callback and cache locally.

        Returns the absolute path string on success, ``None`` on any failure.
        The caller decides how to degrade (skip the image, insert a placeholder, etc.).
        """
        root = self._media_cache_root or (Path.home() / ".echo-agent" / "data" / "media_cache")
        cache_dir = root / platform
        cache_dir.mkdir(parents=True, exist_ok=True)
        url_hash = hashlib.sha256(source_id.encode()).hexdigest()[:16]
        target = cache_dir / f"{url_hash}{suffix}"
        if target.exists():
            target.touch()
            return str(target)
        try:
            data = await fetch()
            if not data:
                logger.warning("Empty media response for {} on {}", source_id[:60], platform)
                return None
            target.write_bytes(data)
            logger.debug("Cached channel media: {} → {}", source_id[:60], target.name)
            return str(target)
        except Exception as e:
            logger.warning("Channel media download failed for {} on {}: {}", source_id[:60], platform, e)
            return None

    async def transcribe_audio(self, file_path: str | Path) -> str:
        """Transcribe an audio file via the shared cloud whisper backend.

        Kept as a thin delegator; inbound transcription now flows through
        ContextBuilder + the media.understanding package, not this method.
        """
        from echo_agent.agent.media.understanding.audio import CloudWhisperBackend
        if not self.transcription_api_key:
            return ""
        return await CloudWhisperBackend(self.transcription_api_key).transcribe(Path(file_path))
