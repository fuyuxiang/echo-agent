"""Narrow user-artifact tools safe enough for public-gateway deployments."""

from __future__ import annotations

import json
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
        for index, chunk in enumerate(chunks, 1):
            prefix = f"{caption}\n\n" if index == 1 and caption else ""
            text = f"{prefix}[{manifest['filename']} {index}/{total}]\n{chunk}"
            event = OutboundEvent.text_reply(
                channel=ctx.channel,
                chat_id=ctx.chat_id,
                text=text,
                reply_to_id=ctx.reply_to_id or None,
            ).mark_tool_delivery(ctx)
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
        await self._store.record_delivery(
            ctx.session_key, params["artifact_id"], channel=ctx.channel, chat_id=ctx.chat_id,
        )
        return _ok({
            "artifact_id": params["artifact_id"],
            "filename": manifest["filename"],
            "delivered": True,
            "channel": ctx.channel,
            "delivery_mode": "text_chunks",
            "chunks": total,
        })

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            session_key = _session(ctx)
            if ctx is None or not ctx.channel or not ctx.chat_id:
                raise ArtifactError("artifact delivery requires the current channel and chat")
            if self._publish is None:
                raise ArtifactError("message bus is not connected")
            path, manifest = await self._store.finalized_path(session_key, params["artifact_id"])
            if self._channel_lookup is not None:
                adapter = self._channel_lookup(ctx.channel)
                if adapter is not None and not getattr(adapter, "supports_files", False):
                    if params.get("fallback_to_text", True):
                        return await self._deliver_as_text(path, manifest, params, ctx)
                    raise ArtifactError(f"channel '{ctx.channel}' cannot send file attachments")
            blocks: list[ContentBlock] = []
            caption = str(params.get("caption") or "").strip()[:160]
            if caption:
                blocks.append(ContentBlock(type=ContentType.TEXT, text=caption))
            blocks.append(ContentBlock(
                type=ContentType.FILE, url=str(path), metadata={"name": manifest["filename"]},
            ))
            event = OutboundEvent(channel=ctx.channel, chat_id=ctx.chat_id, content=blocks).mark_tool_delivery(ctx)
            receipt = await self._publish(event)
            if receipt is not None and not getattr(receipt, "ok", True):
                detail = getattr(receipt, "error", "") or getattr(getattr(receipt, "stage", None), "value", "failed")
                return ToolResult(success=False, error=f"artifact delivery failed: {detail}", error_kind="dependency")
            await self._store.record_delivery(
                session_key, params["artifact_id"], channel=ctx.channel, chat_id=ctx.chat_id,
            )
            return _ok({
                "artifact_id": params["artifact_id"], "filename": manifest["filename"],
                "delivered": True, "channel": ctx.channel, "delivery_mode": "attachment",
            })
        except (ArtifactError, OSError, KeyError, TypeError, ValueError) as exc:
            return _failure(exc)
