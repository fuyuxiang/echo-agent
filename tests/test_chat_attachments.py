"""Tests for chat attachment support: upload endpoint, WS content-block building,
and local-path document extraction in the context builder.

These guard the invariant that the pure-text WS path is unchanged while attachments
flow through the existing media/document-extract pipeline regardless of local vs.
remote agent placement (the bytes are uploaded, not a path the agent must reach)."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import ContentType


def _make_gateway(workspace: Path):
    from echo_agent.gateway.server import GatewayServer
    from echo_agent.config.schema import (
        GatewayConfig,
        GatewayAuthConfig,
        GatewaySessionPolicyConfig,
    )

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=19998,
        auth=GatewayAuthConfig(mode="open"),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    bus = MagicMock()
    channel_manager = MagicMock()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    agent_loop = MagicMock()

    return GatewayServer(
        config=config,
        bus=bus,
        channel_manager=channel_manager,
        session_manager=session_manager,
        workspace=workspace,
        agent_loop=agent_loop,
    )


# ── WS content-block building ────────────────────────────────────────────────


def test_ws_text_only_yields_single_text_block(tmp_path):
    """No attachments → exactly one TEXT block (behaviour identical to text_message)."""
    gw = _make_gateway(tmp_path)
    blocks = gw._build_ws_content_blocks("hello", [])
    assert len(blocks) == 1
    assert blocks[0].type == ContentType.TEXT
    assert blocks[0].text == "hello"


def test_ws_attachment_appends_typed_block(tmp_path):
    """A valid attachment id is resolved to its cached path and appended as a block."""
    from echo_agent.gateway.api.chat_attachments import attachments_dir

    gw = _make_gateway(tmp_path)
    adir = attachments_dir(gw)
    adir.mkdir(parents=True, exist_ok=True)
    (adir / "abc123.pdf").write_bytes(b"%PDF-1.4 fake")

    blocks = gw._build_ws_content_blocks(
        "summarize this",
        [{"id": "abc123.pdf", "name": "report.pdf", "mime_type": "application/pdf"}],
    )
    assert len(blocks) == 2
    assert blocks[0].text == "summarize this"
    assert blocks[1].type == ContentType.FILE
    assert blocks[1].url.endswith("abc123.pdf")
    assert blocks[1].metadata.get("name") == "report.pdf"


def test_ws_attachment_traversal_is_rejected(tmp_path):
    """A crafted id cannot escape the attachments dir; unknown ids are skipped."""
    from echo_agent.gateway.api.chat_attachments import attachments_dir

    gw = _make_gateway(tmp_path)
    attachments_dir(gw).mkdir(parents=True, exist_ok=True)
    # A secret outside the attachments dir must never be reachable.
    (tmp_path / "secret.txt").write_text("top secret")

    blocks = gw._build_ws_content_blocks(
        "read it", [{"id": "../../secret.txt", "name": "secret.txt"}]
    )
    # Only the text block survives; the traversal attempt is dropped.
    assert len(blocks) == 1
    assert blocks[0].text == "read it"


def test_ws_missing_attachment_is_skipped(tmp_path):
    gw = _make_gateway(tmp_path)
    blocks = gw._build_ws_content_blocks("hi", [{"id": "nope.pdf", "name": "nope.pdf"}])
    assert len(blocks) == 1


# ── Local-path document extraction ───────────────────────────────────────────


@pytest.mark.asyncio
async def test_resolve_inbound_media_extracts_local_file(tmp_path):
    """A FILE block with a local path is extracted in place (no download attempted)."""
    from echo_agent.agent.context import ContextBuilder
    from echo_agent.bus.events import ContentBlock

    doc = tmp_path / "note.txt"
    doc.write_text("the quick brown fox", encoding="utf-8")

    builder = ContextBuilder(workspace=tmp_path, doc_enabled=True)
    block = ContentBlock(type=ContentType.FILE, url=str(doc), metadata={"name": "note.txt"})

    resolved = await builder.resolve_inbound_media([block], channel="gateway:desktop")
    assert len(resolved) == 1
    assert resolved[0]["extracted_text"].strip() == "the quick brown fox"


@pytest.mark.asyncio
async def test_resolve_inbound_media_skips_http_in_local_branch(tmp_path):
    """An http file URL still goes through the download path, not local extraction."""
    from echo_agent.agent.context import ContextBuilder
    from echo_agent.bus.events import ContentBlock

    # No real download happens because the mock cache returns None; the point is the
    # local-extraction branch must not fire for http urls (no extracted_text added here).
    fake_cache = MagicMock()
    fake_cache.download = AsyncMock(return_value=None)
    builder = ContextBuilder(workspace=tmp_path, doc_enabled=True, media_cache=fake_cache)
    block = ContentBlock(
        type=ContentType.FILE, url="https://example.com/a.txt", metadata={"name": "a.txt"}
    )

    resolved = await builder.resolve_inbound_media([block], channel="gateway:desktop")
    assert "extracted_text" not in resolved[0]
    fake_cache.download.assert_awaited_once()
