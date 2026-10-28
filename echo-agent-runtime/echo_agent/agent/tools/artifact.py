"""Narrow user-artifact tools safe enough for public-gateway deployments."""

from __future__ import annotations

import asyncio
import hashlib
import json
import weakref
from typing import Any

from loguru import logger

from echo_agent.artifacts import ArtifactError, ArtifactStore
from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult


def _session(ctx: ToolExecutionContext | None) -> str:
    if ctx is None or not ctx.session_key:
        raise ArtifactError("artifact operation requires the current session")
    return ctx.session_key


def _ok(data: dict[str, Any]) -> ToolResult:
    return ToolResult(output=json.dumps(data, ensure_ascii=False), metadata=data)


def _failure(exc: Exception) -> ToolResult:
    if isinstance(exc, OSError):
        # Storage errors often embed the absolute server path in their message.
        # Keep it in operator logs but never return that path to the model.
        logger.warning("Artifact storage operation failed: {}", exc)
        return ToolResult(
            success=False,
            error="artifact storage operation failed; check server logs",
            error_kind="dependency",
        )
    return ToolResult(success=False, error=str(exc), error_kind="business")


def _receipt_is_deferred(receipt: object) -> bool:
    """True when transport accepted data for later terminal delivery.

    A synchronous gateway HTTP waiter buffers tool-delivery frames until the
    turn's authoritative final response.  That is enough to continue this
    in-process turn, but not enough to write a durable delivery checkpoint: a
    crash before the final response would lose the volatile buffer.
    """
    detail = getattr(receipt, "detail", None)
    return isinstance(detail, dict) and detail.get("deferred") is True


class ArtifactCreateTool(Tool):
    name = "artifact_create"
    description = (
        "Create a safe session-scoped user document. Use this before generating a long report; "
        "then append small ordered chunks, validate, finalize, and deliver it."
    )
    capabilities = ("artifact.write",)
    risk_level = "write"

    def __init__(self, store: ArtifactStore):
        self._store = store
        extensions = sorted(store.allowed_extensions)
        self.parameters = {
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": f"User-facing filename. Allowed extensions: {', '.join(extensions)}.",
                },
                "title": {"type": "string", "description": "Optional document title."},
            },
            "required": ["filename"],
        }

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            return _ok(await self._store.create(
                _session(ctx), filename=params["filename"], title=params.get("title", ""),
            ))
        except (ArtifactError, OSError, KeyError, TypeError, ValueError) as exc:
            return _failure(exc)


class ArtifactAppendTool(Tool):
    name = "artifact_append"
    description = (
        "Append one UTF-8 text chunk to a draft artifact. Chunks must use consecutive sequence "
        "numbers starting at 0. Call exactly one artifact_append per assistant turn, then wait "
        "for its result before generating the next chunk. Identical retries are idempotent."
    )
    capabilities = ("artifact.write",)
    risk_level = "write"

    def __init__(self, store: ArtifactStore):
        self._store = store
        self.parameters = {
            "type": "object",
            "properties": {
                "artifact_id": {"type": "string", "description": "ID returned by artifact_create."},
                "sequence": {"type": "integer", "minimum": 0, "description": "Next consecutive chunk number."},
                "content": {
                    "type": "string", "maxLength": store.max_chunk_chars,
                    "description": f"Next document chunk, at most {store.max_chunk_chars} characters.",
                },
                "expected_bytes": {
                    "type": "integer", "minimum": 0,
                    "description": "Optional optimistic-concurrency byte offset from the previous result.",
                },
            },
            "required": ["artifact_id", "sequence", "content"],
        }

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            expected = params.get("expected_bytes")
            return _ok(await self._store.append(
                _session(ctx), params["artifact_id"], sequence=int(params["sequence"]),
                content=params["content"], expected_bytes=int(expected) if expected is not None else None,
            ))
        except (ArtifactError, OSError, KeyError, TypeError, ValueError) as exc:
            return _failure(exc)


class ArtifactValidateTool(Tool):
    name = "artifact_validate"
    description = (
        "Deterministically inspect a draft or finalized artifact: UTF-8 bytes, characters, Chinese "
        "characters, English words, lines, paragraphs, headings, and format errors. No shell required."
    )
    capabilities = ("artifact.read",)
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {"artifact_id": {"type": "string"}},
        "required": ["artifact_id"],
    }

    def __init__(self, store: ArtifactStore):
        self._store = store

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            return _ok(await self._store.validate(_session(ctx), params["artifact_id"]))
        except (ArtifactError, OSError, UnicodeError, KeyError, TypeError, ValueError) as exc:
            return _failure(exc)


class ArtifactFinalizeTool(Tool):
    name = "artifact_finalize"
    description = (
        "Verify all chunks, validate the document, atomically materialize it, and make it read-only "
        "for delivery. A finalized artifact cannot be appended to."
    )
    capabilities = ("artifact.write",)
    risk_level = "write"
    parameters = {
        "type": "object",
        "properties": {"artifact_id": {"type": "string"}},
        "required": ["artifact_id"],
    }

    def __init__(self, store: ArtifactStore):
        self._store = store

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            return _ok(await self._store.finalize(_session(ctx), params["artifact_id"]))
        except (ArtifactError, OSError, UnicodeError, KeyError, TypeError, ValueError) as exc:
            return _failure(exc)


class ArtifactDeliverTool(Tool):
    name = "artifact_deliver"
    description = (
        "Deliver a finalized artifact to the current conversation as an attachment. It cannot send "
        "to another chat. If the channel has no file support, it can deliver numbered text chunks."
    )
    capabilities = ("artifact.read", "message.send")
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "artifact_id": {"type": "string"},
            "caption": {
                "type": "string", "maxLength": 160,
                "description": "Optional short delivery caption (maximum 160 characters).",
            },
            "fallback_to_text": {
                "type": "boolean",
                "description": "Deliver numbered text chunks when attachments are unsupported (default true).",
            },
        },
        "required": ["artifact_id"],
    }

    def __init__(
        self, store: ArtifactStore, publish_fn=None, channel_lookup=None,
        *, text_fallback_max_chars: int = 100000, text_fallback_chunk_chars: int = 1800,
    ):
        self._store = store
        self._publish = publish_fn
        self._channel_lookup = channel_lookup
        self._text_fallback_max_chars = text_fallback_max_chars
        self._text_fallback_chunk_chars = text_fallback_chunk_chars
        # Serialize equivalent deliveries in this process.  Durable per-part
        # checkpoints below handle retries/restarts; this lock prevents two
        # concurrent calls from both observing the same checkpoint and sending
        # the same next part.
        self._delivery_locks: weakref.WeakValueDictionary[str, asyncio.Lock] = (
            weakref.WeakValueDictionary()
        )

    @staticmethod
    def _delivery_id(
        manifest: dict[str, Any], ctx: ToolExecutionContext, *, mode: str, caption: str,
        part_size: int = 0,
    ) -> str:
        # Delivery checkpoints belong to one logical user turn, not forever to
        # artifact+destination.  Otherwise asking to send the same finalized
        # report again on a later turn is mistaken for an idempotent replay and
        # nothing is emitted.  The inbound id is stable across model/tool retry
        # attempts and across delegated child contexts; narrower execution keys
        # are conservative fallbacks for embedders without an inbound event.
        intent_kind, turn_intent = next(
            (
                (kind, value)
                for kind, value in (
                    ("artifact_intent", ctx.artifact_intent_id),
                    ("inbound", ctx.inbound_event_id),
                    ("idempotency", ctx.idempotency_key),
                    ("execution", ctx.execution_id),
                    ("trace", ctx.trace_id),
                    ("session", ctx.session_key),
                )
                if value
            ),
            ("anonymous", ""),
        )
        material = "\0".join((
            str(manifest.get("artifact_id") or ""),
            str(manifest.get("sha256") or ""),
            ctx.channel,
            ctx.chat_id,
            intent_kind,
            turn_intent,
            mode,
            caption,
            str(part_size),
        ))
        return hashlib.sha256(material.encode("utf-8")).hexdigest()

    def _delivery_lock(self, delivery_id: str) -> asyncio.Lock:
        return self._delivery_locks.setdefault(delivery_id, asyncio.Lock())

    @staticmethod
    def _delivered_payload(
        params: dict[str, Any], manifest: dict[str, Any], ctx: ToolExecutionContext,
        *, mode: str, chunks: int | None = None, idempotent_replay: bool = False,
    ) -> ToolResult:
        payload: dict[str, Any] = {
            "artifact_id": params["artifact_id"],
            "filename": manifest["filename"],
            "delivered": True,
            "channel": ctx.channel,
            "delivery_mode": mode,
        }
        if chunks is not None:
            payload["chunks"] = chunks
        if idempotent_replay:
            payload["idempotent_replay"] = True
        return _ok(payload)

    async def _deliver_as_text(
        self, path, manifest: dict[str, Any], params: dict[str, Any], ctx: ToolExecutionContext,
    ) -> ToolResult:
        content = path.read_text(encoding="utf-8")
        if len(content) > self._text_fallback_max_chars:
            raise ArtifactError(
                f"channel '{ctx.channel}' has no attachment support and artifact text fallback "
                f"is limited to {self._text_fallback_max_chars} characters"
            )
        size = self._text_fallback_chunk_chars
        chunks = [content[index:index + size] for index in range(0, len(content), size)] or [""]
        caption = str(params.get("caption") or "").strip()[:160]
        total = len(chunks)
        delivery_id = self._delivery_id(
            manifest, ctx, mode="text_chunks", caption=caption, part_size=size,
        )
        progress = await self._store.delivery_progress(
            ctx.session_key, params["artifact_id"], delivery_id=delivery_id,
        )
        completed_parts = int((progress or {}).get("completed_parts", 0))
        if completed_parts >= total:
            return self._delivered_payload(
                params, manifest, ctx, mode="text_chunks", chunks=total,
                idempotent_replay=True,
            )
        for index in range(completed_parts + 1, total + 1):
            chunk = chunks[index - 1]
            prefix = f"{caption}\n\n" if index == 1 and caption else ""
            text = f"{prefix}[{manifest['filename']} {index}/{total}]\n{chunk}"
            event = OutboundEvent.text_reply(
                channel=ctx.channel,
                chat_id=ctx.chat_id,
                text=text,
                reply_to_id=ctx.reply_to_id or None,
            ).mark_tool_delivery(ctx)
            # Renderers normally dedupe/replace repeated final frames for one
            # inbound turn.  Give each artifact part a transport-neutral display
            # identity so append-only CLI and TUI clients retain every part;
            # ``_inbound_event_id`` itself stays unchanged for turn accounting.
            event.metadata.update({
                "_artifact_delivery_id": delivery_id[:16],
                "_artifact_part": index,
                "_artifact_parts": total,
            })
            receipt = await self._publish(event)
            if receipt is not None and not getattr(receipt, "ok", True):
                detail = getattr(receipt, "error", "") or getattr(
                    getattr(receipt, "stage", None), "value", "failed",
                )
                return ToolResult(
                    success=False,
                    error=f"artifact text fallback failed at part {index}/{total}: {detail}",
                    error_kind="dependency",
                )
            if not _receipt_is_deferred(receipt):
                await self._store.record_delivery(
                    ctx.session_key,
                    params["artifact_id"],
                    channel=ctx.channel,
                    chat_id=ctx.chat_id,
                    delivery_id=delivery_id,
                    mode="text_chunks",
                    completed_parts=index,
                    total_parts=total,
                )
        return self._delivered_payload(
            params, manifest, ctx, mode="text_chunks", chunks=total,
        )

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            session_key = _session(ctx)
            if ctx is None or not ctx.channel or not ctx.chat_id:
                raise ArtifactError("artifact delivery requires the current channel and chat")
            if self._publish is None:
                raise ArtifactError("message bus is not connected")
            path, manifest = await self._store.finalized_path(session_key, params["artifact_id"])
            adapter = self._channel_lookup(ctx.channel) if self._channel_lookup is not None else None
            # File support is an affirmative capability.  Gateway pseudo-
            # channels (notably gateway:cli) have no BaseChannel adapter and the
            # WS payload is text-only; treating a missing adapter as support sent
            # an empty frame and then falsely reported delivery.  Fail closed to
            # the existing text transport instead.
            if adapter is None or not getattr(adapter, "supports_files", False):
                delivery_id = self._delivery_id(
                    manifest,
                    ctx,
                    mode="text_chunks",
                    caption=str(params.get("caption") or "").strip()[:160],
                    part_size=self._text_fallback_chunk_chars,
                )
                async with self._delivery_lock(delivery_id):
                    if params.get("fallback_to_text", True):
                        return await self._deliver_as_text(path, manifest, params, ctx)
                    raise ArtifactError(f"channel '{ctx.channel}' cannot send file attachments")
            blocks: list[ContentBlock] = []
            caption = str(params.get("caption") or "").strip()[:160]
            delivery_id = self._delivery_id(
                manifest, ctx, mode="attachment", caption=caption,
            )
            async with self._delivery_lock(delivery_id):
                progress = await self._store.delivery_progress(
                    session_key, params["artifact_id"], delivery_id=delivery_id,
                )
                if int((progress or {}).get("completed_parts", 0)) >= 1:
                    return self._delivered_payload(
                        params, manifest, ctx, mode="attachment", idempotent_replay=True,
                    )
                if caption:
                    blocks.append(ContentBlock(type=ContentType.TEXT, text=caption))
                blocks.append(ContentBlock(
                    type=ContentType.FILE, url=str(path), metadata={"name": manifest["filename"]},
                ))
                event = OutboundEvent(
                    channel=ctx.channel, chat_id=ctx.chat_id, content=blocks,
                ).mark_tool_delivery(ctx)
                receipt = await self._publish(event)
                if receipt is not None and not getattr(receipt, "ok", True):
                    detail = getattr(receipt, "error", "") or getattr(
                        getattr(receipt, "stage", None), "value", "failed",
                    )
                    return ToolResult(
                        success=False,
                        error=f"artifact delivery failed: {detail}",
                        error_kind="dependency",
                    )
                if not _receipt_is_deferred(receipt):
                    await self._store.record_delivery(
                        session_key,
                        params["artifact_id"],
                        channel=ctx.channel,
                        chat_id=ctx.chat_id,
                        delivery_id=delivery_id,
                        mode="attachment",
                    )
                return self._delivered_payload(
                    params, manifest, ctx, mode="attachment",
                )
        except (ArtifactError, OSError, KeyError, TypeError, ValueError) as exc:
            return _failure(exc)
