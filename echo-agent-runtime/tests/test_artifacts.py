"""Security and lifecycle tests for session-scoped user artifacts."""

from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import pytest

from echo_agent.agent.tools.artifact import ArtifactDeliverTool
from echo_agent.agent.tools import discover_tools
from echo_agent.artifacts import ArtifactError, ArtifactStore
from echo_agent.artifacts.sweeper import sweep
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.tools import ToolExecutionContext


def _store(tmp_path, *, max_chunk_chars=12000, max_artifact_mb=1):
    return ArtifactStore(
        tmp_path, "data/artifacts", max_chunk_chars=max_chunk_chars,
        max_artifact_mb=max_artifact_mb, allowed_extensions=[".md", ".txt", ".json", ".csv"],
    )


@pytest.mark.asyncio
async def test_artifact_chunk_lifecycle_is_idempotent_and_validated(tmp_path):
    store = _store(tmp_path)
    created = await store.create("weixin:chat-a", filename="审校 报告.md", title="审校报告")
    artifact_id = created["artifact_id"]
    assert created["filename"] == "审校_报告.md"

    first = await store.append(
        "weixin:chat-a", artifact_id, sequence=0, content="# 结论\n\n第一段。",
        expected_bytes=0,
    )
    replay = await store.append(
        "weixin:chat-a", artifact_id, sequence=0, content="# 结论\n\n第一段。",
    )
    assert replay["idempotent_replay"] is True
    assert replay["total_bytes"] == first["total_bytes"]

    await store.append(
        "weixin:chat-a", artifact_id, sequence=1, content="\n\n## 建议\n\nSecond section.",
        expected_bytes=first["total_bytes"],
    )
    metrics = await store.validate("weixin:chat-a", artifact_id)
    assert metrics["valid"] is True
    assert metrics["cjk_characters"] > 0
    assert metrics["latin_words"] == 2
    assert metrics["paragraphs"] == 4

    finalized = await store.finalize("weixin:chat-a", artifact_id)
    assert finalized["state"] == "finalized"
    assert finalized["sha256"] == finalized["validation"]["sha256"]
    replay_final = await store.finalize("weixin:chat-a", artifact_id)
    assert replay_final["idempotent_replay"] is True
    with pytest.raises(ArtifactError, match="finalized"):
        await store.append("weixin:chat-a", artifact_id, sequence=2, content="late")


@pytest.mark.asyncio
async def test_artifact_rejects_cross_session_order_conflicts_and_bad_format(tmp_path):
    store = _store(tmp_path, max_chunk_chars=5)
    created = await store.create("cli:one", filename="report.json")
    artifact_id = created["artifact_id"]

    with pytest.raises(ArtifactError, match="this session"):
        await store.validate("cli:two", artifact_id)
    with pytest.raises(ArtifactError, match="maximum"):
        await store.append("cli:one", artifact_id, sequence=0, content="123456")
    with pytest.raises(ArtifactError, match="out-of-order"):
        await store.append("cli:one", artifact_id, sequence=1, content="{}")

    await store.append("cli:one", artifact_id, sequence=0, content="{bad")
    validation = await store.validate("cli:one", artifact_id)
    assert validation["valid"] is False
    assert validation["errors"][0]["code"] == "JSON_INVALID"
    with pytest.raises(ArtifactError, match="validation failed"):
        await store.finalize("cli:one", artifact_id)


@pytest.mark.asyncio
async def test_artifact_delivery_is_current_session_only_and_truthful(tmp_path):
    store = _store(tmp_path)
    created = await store.create("weixin:chat-a", filename="report.md")
    artifact_id = created["artifact_id"]
    await store.append("weixin:chat-a", artifact_id, sequence=0, content="# Report")
    await store.finalize("weixin:chat-a", artifact_id)

    published = []

    async def _publish(event):
        published.append(event)
        return None

    ctx = ToolExecutionContext(session_key="weixin:chat-a", channel="weixin", chat_id="chat-a")
    supported = ArtifactDeliverTool(
        store, publish_fn=_publish,
        channel_lookup=lambda _name: SimpleNamespace(supports_files=True),
    )
    result = await supported.execute({"artifact_id": artifact_id, "caption": "完成"}, ctx)
    assert result.success is True
    assert json.loads(result.output)["delivery_mode"] == "attachment"
    assert len(published) == 1
    assert published[0].chat_id == "chat-a"
    assert "data/artifacts" not in result.output
    replay = await supported.execute(
        {"artifact_id": artifact_id, "caption": "完成"}, ctx,
    )
    assert json.loads(replay.output)["idempotent_replay"] is True
    assert len(published) == 1

    unsupported = ArtifactDeliverTool(
        store, publish_fn=_publish,
        channel_lookup=lambda _name: SimpleNamespace(supports_files=False),
    )
    result = await unsupported.execute({"artifact_id": artifact_id}, ctx)
    assert result.success is True
    assert json.loads(result.output)["delivery_mode"] == "text_chunks"

    result = await unsupported.execute(
        {"artifact_id": artifact_id, "fallback_to_text": False}, ctx,
    )
    assert result.success is False
    assert "cannot send file attachments" in result.error


@pytest.mark.asyncio
async def test_gateway_cli_uses_real_text_transport_instead_of_false_attachment_success(tmp_path):
    store = _store(tmp_path)
    created = await store.create("gateway:cli:alice", filename="report.md")
    artifact_id = created["artifact_id"]
    await store.append("gateway:cli:alice", artifact_id, sequence=0, content="# Report")
    await store.finalize("gateway:cli:alice", artifact_id)
    published = []

    async def _publish(event):
        published.append(event)
        return SimpleNamespace(ok=True)

    tool = ArtifactDeliverTool(
        store,
        publish_fn=_publish,
        # gateway:cli is a gateway pseudo-channel, not a ChannelManager adapter.
        channel_lookup=lambda _name: None,
    )
    ctx = ToolExecutionContext(
        session_key="gateway:cli:alice", channel="gateway:cli", chat_id="alice",
    )

    result = await tool.execute({"artifact_id": artifact_id, "caption": "已完成"}, ctx)

    payload = json.loads(result.output)
    assert result.success is True
    assert payload["delivery_mode"] == "text_chunks"
    assert len(published) == 1
    assert published[0].text.startswith("已完成\n\n[report.md 1/1]\n# Report")
    assert all(block.type.value == "text" for block in published[0].content)
    assert "data/artifacts" not in published[0].text


@pytest.mark.asyncio
async def test_text_fallback_retry_resumes_after_last_acknowledged_part(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:retry", filename="report.txt")
    artifact_id = created["artifact_id"]
    await store.append("cli:retry", artifact_id, sequence=0, content="ABCDEFGHI")
    await store.finalize("cli:retry", artifact_id)
    attempts = []
    fail_second_once = True

    async def _publish(event):
        nonlocal fail_second_once
        attempts.append(event.text)
        if "2/3" in event.text and fail_second_once:
            fail_second_once = False
            return SimpleNamespace(ok=False, error="offline")
        return SimpleNamespace(ok=True)

    tool = ArtifactDeliverTool(
        store,
        publish_fn=_publish,
        channel_lookup=lambda _name: SimpleNamespace(supports_files=False),
        text_fallback_chunk_chars=3,
    )
    ctx = ToolExecutionContext(
        session_key="cli:retry",
        channel="cli",
        chat_id="retry",
        inbound_event_id="turn-retry",
        idempotency_key="attempt-one",
        execution_id="exec-one",
    )
    retry_ctx = ToolExecutionContext(
        session_key="cli:retry",
        channel="cli",
        chat_id="retry",
        # Model retries are new executions/idempotency keys but remain the same
        # logical delivery because the originating inbound turn is stable.
        inbound_event_id="turn-retry",
        idempotency_key="attempt-two",
        execution_id="exec-two",
    )

    first = await tool.execute({"artifact_id": artifact_id}, ctx)
    second = await tool.execute({"artifact_id": artifact_id}, retry_ctx)
    before_replay = len(attempts)
    third = await tool.execute({"artifact_id": artifact_id}, retry_ctx)

    assert first.success is False
    assert "part 2/3" in first.error
    assert second.success is True
    assert json.loads(third.output)["idempotent_replay"] is True
    assert len(attempts) == before_replay
    assert [text.splitlines()[0] for text in attempts] == [
        "[report.txt 1/3]",
        "[report.txt 2/3]",
        "[report.txt 2/3]",
        "[report.txt 3/3]",
    ]


@pytest.mark.asyncio
async def test_artifact_delivery_checkpoint_is_scoped_to_originating_turn(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:resend", filename="report.txt")
    artifact_id = created["artifact_id"]
    await store.append("cli:resend", artifact_id, sequence=0, content="report body")
    await store.finalize("cli:resend", artifact_id)
    published = []

    async def _publish(event):
        published.append(event)
        return SimpleNamespace(ok=True)

    tool = ArtifactDeliverTool(
        store,
        publish_fn=_publish,
        channel_lookup=lambda _name: SimpleNamespace(supports_files=False),
    )

    def _turn(event_id: str, execution_id: str) -> ToolExecutionContext:
        return ToolExecutionContext(
            session_key="cli:resend",
            channel="cli",
            chat_id="resend",
            inbound_event_id=event_id,
            idempotency_key=f"idem-{execution_id}",
            execution_id=execution_id,
        )

    first = await tool.execute({"artifact_id": artifact_id}, _turn("turn-1", "exec-1"))
    replay = await tool.execute({"artifact_id": artifact_id}, _turn("turn-1", "exec-2"))
    resend = await tool.execute({"artifact_id": artifact_id}, _turn("turn-2", "exec-3"))

    assert first.success is True
    assert json.loads(replay.output)["idempotent_replay"] is True
    assert resend.success is True
    assert "idempotent_replay" not in json.loads(resend.output)
    assert len(published) == 2
    assert published[0].metadata["_inbound_event_id"] == "turn-1"
    assert published[1].metadata["_inbound_event_id"] == "turn-2"
    assert published[0].metadata["_artifact_delivery_id"] != (
        published[1].metadata["_artifact_delivery_id"]
    )


@pytest.mark.asyncio
async def test_continuation_turn_resumes_original_delivery_checkpoint(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:continue", filename="report.txt")
    artifact_id = created["artifact_id"]
    await store.append("cli:continue", artifact_id, sequence=0, content="abcdefghi")
    await store.finalize("cli:continue", artifact_id)
    attempts = []
    fail_second_once = True

    async def _publish(event):
        nonlocal fail_second_once
        attempts.append(event.text.splitlines()[0])
        if "2/3" in event.text and fail_second_once:
            fail_second_once = False
            return SimpleNamespace(ok=False, error="offline")
        return SimpleNamespace(ok=True)

    tool = ArtifactDeliverTool(
        store,
        publish_fn=_publish,
        channel_lookup=lambda _name: SimpleNamespace(supports_files=False),
        text_fallback_chunk_chars=3,
    )
    first_ctx = ToolExecutionContext(
        session_key="cli:continue",
        channel="cli",
        chat_id="continue",
        inbound_event_id="turn-origin",
        artifact_intent_id="turn-origin",
    )
    continuation_ctx = ToolExecutionContext(
        session_key="cli:continue",
        channel="cli",
        chat_id="continue",
        inbound_event_id="turn-continue",
        # ContextStage copies the validated continuation's source event here.
        artifact_intent_id="turn-origin",
    )

    first = await tool.execute({"artifact_id": artifact_id}, first_ctx)
    resumed = await tool.execute({"artifact_id": artifact_id}, continuation_ctx)

    assert first.success is False
    assert resumed.success is True
    assert attempts == [
        "[report.txt 1/3]",
        "[report.txt 2/3]",
        "[report.txt 2/3]",
        "[report.txt 3/3]",
    ]


@pytest.mark.asyncio
async def test_deferred_delivery_is_not_durably_checkpointed(tmp_path):
    """Volatile HTTP buffering may advance the turn, never crash recovery."""
    store = _store(tmp_path)
    created = await store.create("gateway:http:u1", filename="report.txt")
    artifact_id = created["artifact_id"]
    await store.append("gateway:http:u1", artifact_id, sequence=0, content="body")
    await store.finalize("gateway:http:u1", artifact_id)
    published = []

    async def _publish(event):
        published.append(event)
        return SimpleNamespace(ok=True, detail={"deferred": True})

    tool = ArtifactDeliverTool(
        store,
        publish_fn=_publish,
        channel_lookup=lambda _name: None,
    )
    ctx = ToolExecutionContext(
        session_key="gateway:http:u1",
        channel="gateway:http",
        chat_id="u1",
        inbound_event_id="turn-http-1",
    )

    first = await tool.execute({"artifact_id": artifact_id}, ctx)
    retry = await tool.execute({"artifact_id": artifact_id}, ctx)

    assert first.success is True
    assert retry.success is True
    assert "idempotent_replay" not in json.loads(retry.output)
    assert len(published) == 2
    assert published[0].metadata["_artifact_delivery_id"] == (
        published[1].metadata["_artifact_delivery_id"]
    )


def test_public_gateway_gets_artifacts_but_not_general_write_or_exec(tmp_path):
    config = Config(
        security={"profile": "public_gateway"},
        tools={"profile": "full"},
    )
    tools = discover_tools(config, tmp_path, MessageBus())
    names = {tool.name for tool in tools}
    assert {
        "artifact_create", "artifact_append", "artifact_validate",
        "artifact_finalize", "artifact_deliver",
    }.issubset(names)
    assert "write_file" not in names
    assert "exec" not in names


def test_artifact_config_rejects_escaping_root():
    with pytest.raises(ValueError, match="rootDir"):
        Config(artifacts={"rootDir": "../outside"})
    with pytest.raises(ValueError, match="maxTotalMb"):
        Config(artifacts={"maxArtifactMb": 2, "maxTotalMb": 1})


def test_artifact_store_rejects_root_symlinked_outside_workspace(tmp_path):
    workspace = tmp_path / "workspace"
    outside = tmp_path / "outside"
    workspace.mkdir()
    outside.mkdir()
    (workspace / "artifacts-link").symlink_to(outside, target_is_directory=True)

    with pytest.raises(ValueError, match="inside the workspace"):
        ArtifactStore(
            workspace, "artifacts-link", max_chunk_chars=3000,
            max_artifact_mb=1, allowed_extensions=[".md"],
        )


@pytest.mark.asyncio
async def test_empty_artifact_cannot_be_finalized(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:empty", filename="empty.md")
    metrics = await store.validate("cli:empty", created["artifact_id"])
    assert metrics["errors"][0]["code"] == "EMPTY_CONTENT"
    with pytest.raises(ArtifactError, match="validation failed"):
        await store.finalize("cli:empty", created["artifact_id"])


@pytest.mark.asyncio
async def test_manifest_totals_are_verified_before_finalize(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:tampered", filename="report.md")
    artifact_id = created["artifact_id"]
    await store.append("cli:tampered", artifact_id, sequence=0, content="content")
    directory = store.root / store.session_hash("cli:tampered") / artifact_id
    manifest_path = directory / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["total_bytes"] += 1
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    with pytest.raises(ArtifactError, match="totals are inconsistent"):
        await store.finalize("cli:tampered", artifact_id)


def test_artifact_tool_outputs_structured_json(tmp_path):
    # Guard the contract consumed by the model: it receives an opaque id and
    # user metadata, never an internal filesystem locator.
    store = _store(tmp_path)
    public = store.public_manifest({"artifact_id": "a" * 32, "filename": "x.md", "state": "draft"})
    payload = json.loads(json.dumps(public))
    assert payload == {"artifact_id": "a" * 32, "filename": "x.md", "state": "draft"}
    assert all("path" not in key for key in payload)


@pytest.mark.asyncio
async def test_artifact_sweeper_removes_only_expired_owned_shape(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:old", filename="old.md")
    artifact_id = created["artifact_id"]
    await store.append("cli:old", artifact_id, sequence=0, content="expired")
    await store.finalize("cli:old", artifact_id)

    directory = store.root / store.session_hash("cli:old") / artifact_id
    manifest_path = directory / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["updated_at"] = (datetime.now(timezone.utc) - timedelta(days=40)).isoformat()
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    unrelated = store.root / "not-an-owned-session"
    unrelated.mkdir()
    (unrelated / "keep.txt").write_text("keep", encoding="utf-8")

    assert sweep(store.root, retention_days=30, max_total_mb=100) == 1
    assert not directory.exists()
    assert (unrelated / "keep.txt").read_text(encoding="utf-8") == "keep"


@pytest.mark.asyncio
async def test_artifact_sweeper_preflights_unknown_nested_directories(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:foreign", filename="report.md")
    artifact_id = created["artifact_id"]
    await store.append("cli:foreign", artifact_id, sequence=0, content="keep")
    directory = store.root / store.session_hash("cli:foreign") / artifact_id
    manifest_path = directory / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["updated_at"] = (datetime.now(timezone.utc) - timedelta(days=40)).isoformat()
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    foreign = directory / "foreign"
    foreign.mkdir()
    (foreign / "keep.txt").write_text("keep", encoding="utf-8")

    assert sweep(store.root, retention_days=30, max_total_mb=100) == 0
    assert (directory / "chunks" / "00000000.part").read_text(encoding="utf-8") == "keep"
    assert (foreign / "keep.txt").read_text(encoding="utf-8") == "keep"


@pytest.mark.asyncio
async def test_artifact_sweeper_does_not_quota_delete_active_draft(tmp_path):
    store = _store(tmp_path)
    created = await store.create("cli:active", filename="report.md")
    artifact_id = created["artifact_id"]
    await store.append("cli:active", artifact_id, sequence=0, content="active")
    directory = store.root / store.session_hash("cli:active") / artifact_id
    # Simulate quota pressure without requiring a huge model-generated chunk.
    (directory / "reserved.bin").write_bytes(b"x" * (1024 * 1024 + 1))

    assert sweep(store.root, retention_days=30, max_total_mb=1) == 0
    assert (directory / "chunks" / "00000000.part").is_file()


@pytest.mark.asyncio
async def test_artifact_create_rejects_precreated_session_symlink(tmp_path):
    store = _store(tmp_path)
    store.root.mkdir(parents=True)
    outside = tmp_path / "outside"
    outside.mkdir()
    session_link = store.root / store.session_hash("cli:symlink")
    session_link.symlink_to(outside, target_is_directory=True)

    with pytest.raises(ArtifactError, match="session directory.*symbolic link"):
        await store.create("cli:symlink", filename="report.md")

    assert list(outside.iterdir()) == []
