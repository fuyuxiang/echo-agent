"""Every file reached by a search must obey the direct-read policy."""
import pytest

from echo_agent.agent.tools.filesystem import ReadFileTool
from echo_agent.agent.tools.search import SearchFilesTool


@pytest.mark.asyncio
@pytest.mark.parametrize("filename", [".env", "secrets.yaml", "credentials.json"])
async def test_search_cannot_read_protected_files_through_parent_directory(tmp_path, filename):
    (tmp_path / filename).write_text("PRIVATE_CREDENTIAL_CONTENT")
    (tmp_path / "allowed.txt").write_text("PUBLIC_CONTENT")
    direct = await ReadFileTool(str(tmp_path), restrict=True).execute({"path": filename})
    assert direct.success is False
    result = await SearchFilesTool(str(tmp_path), restrict=True).execute({"pattern": "CONTENT"})
    assert result.success
    assert "PUBLIC_CONTENT" in result.output
    assert "PRIVATE_CREDENTIAL_CONTENT" not in result.output


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["content", "glob"])
async def test_search_does_not_follow_file_symlinks_outside_workspace(tmp_path, mode):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    outside = tmp_path / "outside.txt"
    outside.write_text("PRIVATE_OUTSIDE_CONTENT")
    (workspace / "alias.txt").symlink_to(outside)
    result = await SearchFilesTool(str(workspace), restrict=True).execute(
        {"mode": mode, "pattern": "CONTENT" if mode == "content" else "*.txt"}
    )
    assert result.success
    assert "PRIVATE_OUTSIDE_CONTENT" not in result.output
    assert "alias.txt" not in result.output


@pytest.mark.asyncio
async def test_search_does_not_follow_file_symlink_into_other_session_spill(tmp_path):
    spill = tmp_path / "spill"
    private = spill / "other-session"
    private.mkdir(parents=True)
    (private / "artifact.txt").write_text("PRIVATE_SPILL_CONTENT")
    (tmp_path / "alias.txt").symlink_to(private / "artifact.txt")
    result = await SearchFilesTool(str(tmp_path), spill_root=spill).execute({"pattern": "CONTENT"})
    assert "PRIVATE_SPILL_CONTENT" not in result.output


@pytest.mark.asyncio
async def test_skill_script_replay_key_does_not_repeat_side_effect(tmp_path):
    from types import SimpleNamespace
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.agent.tools.skill_run import SkillRunTool
    from echo_agent.tools import ToolExecutionContext

    script = tmp_path / "increment.py"
    script.write_text(
        "from pathlib import Path\np = Path('count.txt')\n"
        "p.write_text(str(int(p.read_text()) + 1 if p.exists() else 1))\nprint('done')\n"
    )
    store = SimpleNamespace(find_skill_dir=lambda name: tmp_path, read_skill=lambda name: "---\nname: increment\n---\n")
    registry = ToolRegistry()
    registry.register(SkillRunTool(store))
    ctx = ToolExecutionContext(session_key="test", idempotency_key="one-script-call")
    assert (await registry.execute("skill_run", {"name": "increment", "script": "increment.py"}, ctx)).success
    replay = await registry.execute("skill_run", {"name": "increment", "script": "increment.py"}, ctx)
    assert replay.success is False
    assert "Replay prevented" in replay.error
    assert (tmp_path / "count.txt").read_text() == "1"


@pytest.mark.asyncio
async def test_vision_applies_read_policy_before_sending_local_bytes(tmp_path):
    from types import SimpleNamespace
    from unittest.mock import AsyncMock
    from echo_agent.agent.tools.vision import VisionTool

    secret = tmp_path / "secrets.yaml"
    secret.write_text("private-token")
    provider = SimpleNamespace(chat_with_retry=AsyncMock(return_value=SimpleNamespace(finish_reason="stop", content="ok")))
    result = await VisionTool(provider, workspace=str(tmp_path)).execute({"image": str(secret), "prompt": "read this"})
    assert result.success is False
    provider.chat_with_retry.assert_not_awaited()


@pytest.mark.asyncio
async def test_tts_cannot_overwrite_protected_path(tmp_path, monkeypatch):
    from echo_agent.agent.tools.tts import TTSTool
    from echo_agent.security import path_policy
    from echo_agent.tools import ToolResult

    fake_home = tmp_path / "home"
    target = fake_home / ".ssh" / "authorized_keys"
    target.parent.mkdir(parents=True)
    target.write_text("keep-existing-credentials")
    monkeypatch.setattr(path_policy, "_home", lambda: str(fake_home))
    tool = TTSTool(workspace=str(tmp_path))
    async def synthesize(text, voice, output):
        output.write_bytes(b"audio bytes")
        return ToolResult(output="written")
    monkeypatch.setattr(tool, "_edge_tts", synthesize)
    result = await tool.execute({"text": "hello", "output_path": str(target)})
    assert result.success is False
    assert target.read_text() == "keep-existing-credentials"


@pytest.mark.asyncio
async def test_document_read_uses_the_same_resolved_path_that_was_authorized(tmp_path, monkeypatch):
    from echo_agent.agent.tools.document import ReadDocumentTool

    workspace = tmp_path / "workspace"
    workspace.mkdir()
    other = tmp_path / "cwd"
    other.mkdir()
    (workspace / "document.txt").write_text("WORKSPACE_DOCUMENT")
    (other / "document.txt").write_text("WRONG_CWD_DOCUMENT")
    monkeypatch.chdir(other)
    result = await ReadDocumentTool(str(workspace), restrict=True).execute({"path": "document.txt"})
    assert result.success
    assert "WORKSPACE_DOCUMENT" in result.output
    assert "WRONG_CWD_DOCUMENT" not in result.output


@pytest.mark.asyncio
async def test_web_fetch_bounds_response_acquisition_before_decoding(monkeypatch):
    from aiohttp import web
    from aiohttp.test_utils import TestServer
    from echo_agent.agent.tools.web import WebFetchTool

    async def large_response(request):
        return web.Response(body=b"x" * 100_000, content_type="text/plain")
    app = web.Application()
    app.router.add_get("/large", large_response)
    monkeypatch.setattr(WebFetchTool, "_MAX_RESPONSE_BYTES", 1024, raising=False)
    async with TestServer(app) as server:
        result = await WebFetchTool(allow_private=True).execute({"url": str(server.make_url("/large"))})
    assert result.success
    assert len(result.output) < 2000
    assert result.metadata["acquisition_truncated"] is True
    assert result.metadata["bytes_read"] <= 1024


@pytest.mark.asyncio
async def test_task_start_queues_work_for_dispatcher_instead_of_creating_orphan_running(tmp_path):
    from echo_agent.agent.tools.task import TaskTool
    from echo_agent.storage.sqlite import SQLiteBackend
    from echo_agent.tasks.manager import TaskManager
    from echo_agent.tasks.models import TaskStatus

    storage = SQLiteBackend(tmp_path / "tasks.db")
    await storage.initialize()
    try:
        manager = TaskManager(storage)
        task = await manager.create(title="do harmless work")
        tool = TaskTool(manager)
        result = await tool.execute({"action": "start", "task_id": task.id})
        assert result.success
        assert (await manager.get(task.id)).status == TaskStatus.QUEUED
        assert (await tool.execute({"action": "start", "task_id": task.id})).success
        assert (await manager.get(task.id)).status == TaskStatus.QUEUED
    finally:
        await storage.close()
