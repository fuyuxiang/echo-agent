"""Unified event types for the message bus.

All channel sources normalize into these types before entering the agent loop.
"""

from __future__ import annotations

import re
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any, ClassVar


class EventType(str, Enum):
    MESSAGE = "message"
    WEBHOOK = "webhook"
    CRON = "cron"
    CLI = "cli"
    SYSTEM = "system"


class ContentType(str, Enum):
    TEXT = "text"
    IMAGE = "image"
    FILE = "file"
    AUDIO = "audio"
    VIDEO = "video"
    VOICE = "voice"
    MIXED = "mixed"


class ProcessingOutcome(str, Enum):
    SUCCESS = "success"
    FAILURE = "failure"


@dataclass
class PollRequest:
    question: str
    options: list[str]
    allow_multiple: bool = False
    duration_seconds: int | None = None


@dataclass
class ContentBlock:
    """A single piece of content within a message."""
    type: ContentType = ContentType.TEXT
    text: str = ""
    url: str = ""
    mime_type: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class InboundEvent:
    """Unified inbound event from any channel source."""
    event_id: str = field(default_factory=lambda: uuid.uuid4().hex[:16])
    event_type: EventType = EventType.MESSAGE
    channel: str = ""
    sender_id: str = ""
    chat_id: str = ""
    content: list[ContentBlock] = field(default_factory=list)
    timestamp: datetime = field(default_factory=datetime.now)
    reply_to_id: str | None = None
    # 引用上下文：用户引用某条历史消息发问时，承载被引用消息的原文与作者，
    # 供 pipeline 统一注入到喂给模型的文本里做消歧（agent 才知道用户在追问哪条）。
    # 通道入站自带原文则同步填充；拿不到则留空，注入侧降级为不注入。
    reply_to_text: str | None = None        # 被引用消息的文本（可截断）
    reply_to_sender: str | None = None      # 被引用消息的作者名/id
    reply_to_is_own: bool = False           # 用户引用的是不是机器人自己发的消息
    thread_id: str | None = None
    session_key_override: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)
    gateway_metadata: dict[str, Any] = field(default_factory=dict)
    is_group: bool = False
    # Trust signals that gate the approval path. These are FIRST-CLASS typed
    # fields, deliberately NOT metadata keys: metadata is populated by external
    # channels from untrusted caller input, so a signal living there can be
    # forged (a webhook body carrying {"_cron_authorized": true}) to bypass EXEC
    # approval. Only trusted internal producers (scheduler/delivery.py) construct
    # an InboundEvent with these set; the shared channel builder (base._build_event)
    # never assigns them, so a caller cannot reach them by any payload. See the
    # approval gate's _is_unattended / _resolve_unattended for the consumers.
    unattended: bool = False       # no human at the keyboard (scheduled/cron run)
    cron_authorized: bool = False  # this specific job passed the up-front cronjob approval

    @property
    def session_key(self) -> str:
        if self.session_key_override:
            return self.session_key_override
        return f"{self.channel}:{self.chat_id}"

    def scoped_session_key(self, scope: str) -> str:
        """会话作用域键。私聊及 shared 策略下等同 session_key；
        群聊 + per_user 策略时把 sender_id 纳入键，实现群内每人隔离。"""
        if self.session_key_override:
            return self.session_key_override
        base = f"{self.channel}:{self.chat_id}"
        if scope == "per_user" and self.is_group and self.sender_id:
            return f"{base}:{self.sender_id}"
        return base

    @property
    def text(self) -> str:
        return "\n".join(b.text for b in self.content if b.text)

    @property
    def media_urls(self) -> list[str]:
        return [b.url for b in self.content if b.url and b.type != ContentType.TEXT]

    @property
    def media_items(self) -> list[ContentBlock]:
        """Non-text content blocks, preserving type/mime so downstream can route
        images vs. files correctly (instead of flattening to bare URLs)."""
        return [b for b in self.content if b.url and b.type != ContentType.TEXT]

    @classmethod
    def text_message(
        cls,
        channel: str,
        sender_id: str,
        chat_id: str,
        text: str,
        **kwargs: Any,
    ) -> InboundEvent:
        return cls(
            channel=channel,
            sender_id=sender_id,
            chat_id=chat_id,
            content=[ContentBlock(type=ContentType.TEXT, text=text)],
            **kwargs,
        )


@dataclass
class OutboundEvent:
    """Unified outbound event to any channel."""
    event_id: str = field(default_factory=lambda: uuid.uuid4().hex[:16])
    channel: str = ""
    chat_id: str = ""
    content: list[ContentBlock] = field(default_factory=list)
    reply_to_id: str | None = None
    edit_message_id: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)
    is_final: bool = True
    task_id: str | None = None
    workflow_id: str | None = None
    message_kind: str = "final"

    @property
    def text(self) -> str:
        return "\n".join(b.text for b in self.content if b.text)

    @classmethod
    def text_reply(
        cls,
        channel: str,
        chat_id: str,
        text: str,
        reply_to_id: str | None = None,
        **kwargs: Any,
    ) -> OutboundEvent:
        return cls(
            channel=channel,
            chat_id=chat_id,
            content=[ContentBlock(type=ContentType.TEXT, text=text)],
            reply_to_id=reply_to_id,
            **kwargs,
        )

    _MEDIA_TAG_RE: ClassVar[re.Pattern] = re.compile(
        r"<(qqimg|qqvoice|qqvideo|qqfile|qqmedia)>"
        r"([^<>]+)"
        r"</(?:qqimg|qqvoice|qqvideo|qqfile|qqmedia|img)>",
        re.IGNORECASE,
    )
    _TAG_TO_CONTENT_TYPE: ClassVar[dict[str, ContentType]] = {
        "qqimg": ContentType.IMAGE,
        "qqvoice": ContentType.AUDIO,
        "qqvideo": ContentType.VIDEO,
        "qqfile": ContentType.FILE,
        "qqmedia": ContentType.FILE,
    }

    @classmethod
    def from_text_with_media(
        cls,
        channel: str,
        chat_id: str,
        text: str,
        reply_to_id: str | None = None,
        **kwargs: Any,
    ) -> OutboundEvent:
        """Parse media tags in text and create structured content blocks."""
        blocks: list[ContentBlock] = []
        last_end = 0
        for m in cls._MEDIA_TAG_RE.finditer(text):
            before = text[last_end:m.start()].strip()
            if before:
                blocks.append(ContentBlock(type=ContentType.TEXT, text=before))
            tag = m.group(1).lower()
            source = m.group(2).strip()
            ct = cls._TAG_TO_CONTENT_TYPE.get(tag, ContentType.FILE)
            blocks.append(ContentBlock(type=ct, url=source))
            last_end = m.end()
        after = text[last_end:].strip()
        if after:
            blocks.append(ContentBlock(type=ContentType.TEXT, text=after))
        if not blocks:
            blocks.append(ContentBlock(type=ContentType.TEXT, text=text))
        return cls(
            channel=channel,
            chat_id=chat_id,
            content=blocks,
            reply_to_id=reply_to_id,
            **kwargs,
        )
