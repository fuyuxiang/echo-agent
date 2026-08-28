"""Local and remote document resolution in the shared context builder."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import ContentType
@pytest.mark.asyncio
async def test_resolve_inbound_media_extracts_local_file(tmp_path):
    """A FILE block with a local path is extracted in place (no download attempted)."""
    from echo_agent.agent.context import ContextBuilder
    from echo_agent.bus.events import ContentBlock

    doc = tmp_path / "note.txt"
    doc.write_text("the quick brown fox", encoding="utf-8")

    builder = ContextBuilder(workspace=tmp_path, doc_enabled=True)
    block = ContentBlock(type=ContentType.FILE, url=str(doc), metadata={"name": "note.txt"})

    resolved = await builder.resolve_inbound_media([block], channel="gateway:api")
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

    resolved = await builder.resolve_inbound_media([block], channel="gateway:api")
    assert "extracted_text" not in resolved[0]
    fake_cache.download.assert_awaited_once()
