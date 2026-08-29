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


TERMINAL_TURN_OUTCOMES = frozenset(
    {"completed", "incomplete", "failed", "interrupted"}
)

# Outcomes that produced no usable answer. Only these are *errors*: a turn that
# hit the iteration ceiling or a length cap still carries the model's
# conclusion, so presenting it as a failure would be untrue to the user.
FAULTED_TURN_OUTCOMES = frozenset({"failed"})

# HTTP status per outcome. `incomplete`/`interrupted` deliberately stay 200: the
# body's `status` field carries the nuance, and 409 is already spoken for by
# idempotency-key conflicts — overloading it would leave a client unable to tell
# "your key was reused with different content" (never retry) from "here is the
# answer, the task just did not finish" (retryable).
_TURN_OUTCOME_HTTP_STATUS: dict[str, int] = {
    "completed": 200,
    "incomplete": 200,
    "interrupted": 200,
    "failed": 500,
}


def turn_outcome_http_status(status: str) -> int:
    """HTTP status for a terminal turn outcome, defaulting to 500 if unknown."""
    return _TURN_OUTCOME_HTTP_STATUS.get(status, 500)


def final_frame_http_status(metadata: dict[str, Any], turn_status: str) -> int:
    """Resolve a final frame's HTTP status, honouring an explicit override.

    Producers that reject a turn before it runs (bus rate limiting) set a
    truthful ``_http_status`` such as 429 that the outcome table cannot express.
    An out-of-range or non-numeric value is ignored rather than trusted, so a
    malformed override cannot produce an invalid response status.
    """
    raw = metadata.get("_http_status")
    if raw is not None:
        try:
            explicit = int(raw)
        except (TypeError, ValueError):
            explicit = 0
        if 200 <= explicit <= 599:
            return explicit
    return turn_outcome_http_status(turn_status)


def stamp_turn_outcome(
    metadata: dict[str, Any], status: str, *, error: str = "",
) -> None:
    """Attach a transport-neutral final turn outcome to outbound metadata.

    Synchronous Gateway/Webhook transports see the final frame before the loop
    writes its terminal ledger row. These fields let them cache and return the
    same status atomically with delivery instead of reporting every final frame
    as a successful HTTP 200/completed result.

    ``_error`` means "this turn produced no answer" — it drives user-visible
    failure signals such as the channel reaction emoji. An unfinished turn that
    still answered (forced convergence, truncated output, an interrupt after
    partial work) is NOT flagged: ``_turn_status`` carries that nuance for
    callers who need it, without telling the user their answer failed.
    """
    if status not in TERMINAL_TURN_OUTCOMES:
        raise ValueError(f"invalid turn outcome: {status}")
    metadata["_turn_status"] = status
    metadata["_http_status"] = turn_outcome_http_status(status)
    if status not in FAULTED_TURN_OUTCOMES:
        metadata.pop("_error", None)
        if status == "completed":
            metadata.pop("_error_reason", None)
        elif error:
            # Retained for diagnostics/observability; not an error signal.
            metadata["_error_reason"] = error
        return
    metadata["_error"] = True
    metadata["_error_reason"] = error or f"turn {status}"


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
    # 记忆作用域键(owner-aware),由 AgentLoop._on_inbound 冻结。刻意独立于
    # session_key:后者承载会话锁/历史/投递路由(delivery 反解 channel:chat_id),
    # 不能按人归一;记忆作用域可以。见 memory_scope_key。
    memory_scope: str = ""
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
    # Internal control command (e.g. the synthesized clarify-cancel on ws
    # disconnect). Like the trust signals above, this is a FIRST-CLASS typed
    # field — NOT a metadata key — precisely because it bypasses the session
    # rate limiter: a forgeable metadata flag would let an external payload
    # skip throttling at will. Only trusted internal producers set it. Control
    # events must still be handled BEFORE the session lock (see AgentLoop),
    # since they exist to wake a turn that is holding that lock.
    is_control: bool = False

    @property
    def session_key(self) -> str:
        if self.session_key_override:
            return self.session_key_override
        return f"{self.channel}:{self.chat_id}"

    def scoped_session_key(self, scope: str) -> str:
        """会话作用域键。私聊及 shared 策略下等同 session_key;
        群聊 + per_user 策略时把 sender_id 纳入键,实现群内每人隔离。

        群聊 per_user 隔离优先于 session_key_override:override 承载的是
        群/会话本身的键,不含成员维度;若直接返回它会让整群共用一个键,
        丢失群内按人隔离。故群聊 per_user 场景在 override 基础上仍拼 sender。
        拼接幂等:_on_inbound 可能已把含 sender 的 scoped 键写回 override,
        此时不重复拼,避免 memory_scope 出现 :sender:sender 双拼、与 session_key 背离。"""
        base = self.session_key_override or f"{self.channel}:{self.chat_id}"
        if scope == "per_user" and self.is_group and self.sender_id:
            suffix = f":{self.sender_id}"
            if not base.endswith(suffix):
                return f"{base}{suffix}"
        return base

    def memory_scope_key(
        self, group_scope: str, owner_key: str, bindings: object = frozenset()
    ) -> str:
        """记忆作用域键(独立于 session_key,不参与投递路由)。

        三分支:
        - 群聊 → scoped_session_key(group_scope),群成员按会话隔离,永不进 owner;
        - 1:1 私聊且 "{channel}:{sender_id}" 命中绑定表 → owner_key,跨通道互通;
        - 1:1 私聊未命中 → session_key,fail-closed 按会话隔离。
        """
        if self.is_group:
            return self.scoped_session_key(group_scope)
        if self.sender_id and f"{self.channel}:{self.sender_id}" in bindings:
            return owner_key
        return self.session_key

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

    def mark_tool_delivery(self, ctx: Any) -> OutboundEvent:
        """Attribute this event to the turn whose tool call produced it.

        Tools that publish their own OutboundEvent (message / notify / send_file
        / tts) inherit ``is_final=True`` and ``message_kind="final"`` from the
        field defaults, so the delivery layer treats them exactly like a turn's
        final answer — but without ``_inbound_event_id`` they belonged to no turn,
        so nothing could relate them to the final reply that follows. A cron job
        whose instruction said "send the report" therefore delivered twice: once
        from the tool, once from the turn's own reply.

        ``_tool_delivery`` distinguishes the two for the guard below: a tool
        delivery claims the target, but must not be *suppressed* by an earlier
        claim — two message calls to different chats are both legitimate.

        Returns self so call sites read as one expression. A None/partial ctx
        leaves the event unstamped, which is the old behaviour: unattributed, and
        therefore never suppressed.
        """
        inbound_event_id = str(getattr(ctx, "inbound_event_id", "") or "")
        if inbound_event_id:
            self.metadata["_inbound_event_id"] = inbound_event_id
            self.metadata["_tool_delivery"] = True
        return self

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
