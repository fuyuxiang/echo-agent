"""Unit tests for the 17 agent tools in echo_agent/agent/tools/."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext, ToolResult

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _ctx(**kwargs) -> ToolExecutionContext:
    defaults = {"session_key": "test:c1", "user_id": "u1"}
    defaults.update(kwargs)
    return ToolExecutionContext(**defaults)


# ===========================================================================
# 1. ClarifyTool
# ===========================================================================


class TestClarifyTool:
    def _make(self):
        from echo_agent.agent.tools.clarify import ClarifyTool

        bus = MagicMock()
        return ClarifyTool(bus=bus)

    @pytest.mark.asyncio
    async def test_basic_question(self):
        tool = self._make()
        result = await tool.execute({"question": "What color?"}, _ctx())
        assert result.success is True
        assert "What color?" in result.output
        assert result.metadata["type"] == "clarify"

    @pytest.mark.asyncio
    async def test_with_options(self):
        tool = self._make()
        result = await tool.execute(
            {"question": "Pick one:", "options": ["A", "B", "C"]}, _ctx()
        )
        assert result.success is True
        assert "1. A" in result.output
        assert "2. B" in result.output
        assert result.metadata["options"] == ["A", "B", "C"]

    @pytest.mark.asyncio
    async def test_empty_options_list(self):
        tool = self._make()
        result = await tool.execute({"question": "Why?", "options": []}, _ctx())
        assert result.success is True
        assert result.output == "Why?"


# ===========================================================================
# 2. Filesystem tools (ReadFileTool, WriteFileTool, EditFileTool, ListDirTool)
# ===========================================================================
class TestReadFileTool:
    def _make(self, workspace: str):
        from echo_agent.agent.tools.filesystem import ReadFileTool

        return ReadFileTool(workspace=workspace)

    @pytest.mark.asyncio
    async def test_read_existing_file(self, tmp_path):
        f = tmp_path / "hello.txt"
        f.write_text("line1\nline2\nline3\n")
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "hello.txt"}, _ctx())
        assert result.success is True
        assert "line1" in result.output
        assert result.metadata["total_lines"] == 3

    @pytest.mark.asyncio
    async def test_read_with_offset_and_limit(self, tmp_path):
        f = tmp_path / "data.txt"
        f.write_text("\n".join(f"L{i}" for i in range(10)))
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "data.txt", "offset": 2, "limit": 3}, _ctx())
        assert result.success is True
        assert "L2" in result.output
        assert "L0" not in result.output

    @pytest.mark.asyncio
    async def test_read_missing_file(self, tmp_path):
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "nope.txt"}, _ctx())
        assert result.success is False


class TestWriteFileTool:
    def _make(self, workspace: str):
        from echo_agent.agent.tools.filesystem import WriteFileTool

        return WriteFileTool(workspace=workspace)

    @pytest.mark.asyncio
    async def test_write_creates_file(self, tmp_path):
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "out.txt", "content": "hello"}, _ctx())
        assert result.success is True
        assert (tmp_path / "out.txt").read_text() == "hello"

    @pytest.mark.asyncio
    async def test_write_creates_subdirs(self, tmp_path):
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "sub/dir/f.txt", "content": "data"}, _ctx())
        assert result.success is True
        assert (tmp_path / "sub" / "dir" / "f.txt").exists()


class TestEditFileTool:
    def _make(self, workspace: str):
        from echo_agent.agent.tools.filesystem import EditFileTool

        return EditFileTool(workspace=workspace)

    @pytest.mark.asyncio
    async def test_edit_replaces_string(self, tmp_path):
        f = tmp_path / "code.py"
        f.write_text("x = 1\ny = 2\n")
        tool = self._make(str(tmp_path))
        result = await tool.execute(
            {"path": "code.py", "old_string": "x = 1", "new_string": "x = 99"}, _ctx()
        )
        assert result.success is True
        assert "x = 99" in f.read_text()

    @pytest.mark.asyncio
    async def test_edit_old_string_not_found(self, tmp_path):
        f = tmp_path / "code.py"
        f.write_text("abc")
        tool = self._make(str(tmp_path))
        result = await tool.execute(
            {"path": "code.py", "old_string": "xyz", "new_string": "new"}, _ctx()
        )
        assert result.success is False
        assert "not found" in result.error

    @pytest.mark.asyncio
    async def test_edit_ambiguous_match(self, tmp_path):
        f = tmp_path / "dup.py"
        f.write_text("a = 1\na = 1\n")
        tool = self._make(str(tmp_path))
        result = await tool.execute(
            {"path": "dup.py", "old_string": "a = 1", "new_string": "a = 2"}, _ctx()
        )
        assert result.success is False
        assert "2 times" in result.error


class TestListDirTool:
    def _make(self, workspace: str):
        from echo_agent.agent.tools.filesystem import ListDirTool

        return ListDirTool(workspace=workspace)

    @pytest.mark.asyncio
    async def test_list_dir(self, tmp_path):
        (tmp_path / "a.txt").touch()
        (tmp_path / "sub").mkdir()
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "."}, _ctx())
        assert result.success is True
        assert "file\ta.txt" in result.output
        assert "dir\tsub" in result.output

    @pytest.mark.asyncio
    async def test_list_dir_missing(self, tmp_path):
        tool = self._make(str(tmp_path))
        result = await tool.execute({"path": "nonexist"}, _ctx())
        assert result.success is False


# ===========================================================================
# 3. ImageGenTool
# ===========================================================================


class TestImageGenTool:
    def _make(self, api_key="test-key"):
        from echo_agent.agent.tools.image_gen import ImageGenTool

        return ImageGenTool(api_key=api_key)

    @pytest.mark.asyncio
    async def test_no_api_key(self):
        from echo_agent.agent.tools.image_gen import ImageGenTool

        tool = ImageGenTool(api_key="")
        result = await tool.execute({"prompt": "a cat"}, _ctx())
        assert result.success is False
        assert "not configured" in result.error

    @pytest.mark.asyncio
    async def test_successful_generation(self):
        tool = self._make()
        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.json = AsyncMock(return_value={
            "data": [{"url": "https://img.example.com/1.png", "revised_prompt": "a cute cat"}]
        })
        mock_resp.text = AsyncMock(return_value="")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.post = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"prompt": "a cat"}, _ctx())
        assert result.success is True
        assert "https://img.example.com/1.png" in result.output
        assert result.metadata["url"] == "https://img.example.com/1.png"

    @pytest.mark.asyncio
    async def test_api_error(self):
        tool = self._make()
        mock_resp = AsyncMock()
        mock_resp.status = 400
        mock_resp.text = AsyncMock(return_value="bad request")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.post = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"prompt": "a cat"}, _ctx())
        assert result.success is False
        assert "400" in result.error


# ===========================================================================
# 4. KnowledgeSearchTool / KnowledgeIndexTool
# ===========================================================================
class TestKnowledgeSearchTool:
    def _make(self):
        from echo_agent.agent.tools.knowledge import KnowledgeSearchTool

        index = MagicMock()
        return KnowledgeSearchTool(index=index), index

    @pytest.mark.asyncio
    async def test_empty_query(self):
        tool, _ = self._make()
        result = await tool.execute({"query": "  "}, _ctx())
        assert result.success is False
        assert "required" in result.error

    @pytest.mark.asyncio
    async def test_no_results(self):
        tool, index = self._make()
        index.search_async = AsyncMock(return_value=[])
        index.format_results.return_value = ""
        result = await tool.execute({"query": "anything"}, _ctx())
        assert result.success is True
        assert "No matching" in result.output

    @pytest.mark.asyncio
    async def test_with_results(self):
        tool, index = self._make()
        mock_result = SimpleNamespace(citation_id="c1", path="doc.md", chunk_id="ch1", score=0.9)
        index.search_async = AsyncMock(return_value=[mock_result])
        index.format_results.return_value = "Found: doc.md chunk"
        result = await tool.execute({"query": "test query", "max_results": 3}, _ctx())
        assert result.success is True
        assert "Found:" in result.output
        assert result.metadata["count"] == 1


class TestKnowledgeIndexTool:
    def _make(self):
        from echo_agent.agent.tools.knowledge import KnowledgeIndexTool

        index = MagicMock()
        return KnowledgeIndexTool(index=index), index

    @pytest.mark.asyncio
    async def test_status(self):
        tool, index = self._make()
        index.status.return_value = {"documents": 10, "chunks": 50}
        result = await tool.execute({"action": "status"}, _ctx())
        assert result.success is True
        assert "10" in result.output

    @pytest.mark.asyncio
    async def test_rebuild(self):
        tool, index = self._make()
        index.rebuild.return_value = {"rebuilt": True, "chunks": 100}
        result = await tool.execute({"action": "rebuild"}, _ctx())
        assert result.success is True
        assert "100" in result.output

    @pytest.mark.asyncio
    async def test_unsupported_action(self):
        tool, index = self._make()
        result = await tool.execute({"action": "drop"}, _ctx())
        assert result.success is False
        assert "Unsupported" in result.error


# ===========================================================================
# 5. MemoryTool
# ===========================================================================


class TestMemoryTool:
    def _make(self):
        from echo_agent.agent.tools.memory import MemoryTool

        store = MagicMock()
        return MemoryTool(store=store), store

    @pytest.mark.asyncio
    async def test_add_success(self):
        tool, store = self._make()
        mock_entry = SimpleNamespace(type=SimpleNamespace(value="user"), key="lang")
        store.add.return_value = mock_entry
        result = await tool.execute(
            {"action": "add", "target": "user", "key": "lang", "content": "Python"},
            _ctx(),
        )
        assert result.success is True
        assert "lang" in result.output

    @pytest.mark.asyncio
    async def test_add_missing_key(self):
        tool, store = self._make()
        result = await tool.execute(
            {"action": "add", "target": "user", "content": "something"},
            _ctx(),
        )
        assert result.success is False
        assert "required" in result.error

    @pytest.mark.asyncio
    async def test_search_no_results(self):
        tool, store = self._make()
        store.search_scored.return_value = []
        result = await tool.execute({"action": "search", "query": "foo"}, _ctx())
        assert result.success is True
        assert "No matching" in result.output

    @pytest.mark.asyncio
    async def test_unknown_action(self):
        tool, store = self._make()
        result = await tool.execute({"action": "nope"}, _ctx())
        assert result.success is False
        assert "Unknown" in result.error


# ===========================================================================
# 6. MessageTool
# ===========================================================================


class TestMessageTool:
    def _make(self, publish_fn=None):
        from echo_agent.agent.tools.message import MessageTool

        return MessageTool(publish_fn=publish_fn)

    @pytest.mark.asyncio
    async def test_no_bus(self):
        tool = self._make(publish_fn=None)
        result = await tool.execute(
            {"channel": "cli", "chat_id": "c1", "text": "hi"}, _ctx()
        )
        assert result.success is False
        assert "not connected" in result.error

    @pytest.mark.asyncio
    async def test_send_success(self):
        publish = AsyncMock()
        tool = self._make(publish_fn=publish)
        result = await tool.execute(
            {"channel": "telegram", "chat_id": "123", "text": "hello"}, _ctx()
        )
        assert result.success is True
        assert "telegram:123" in result.output
        publish.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_send_exception(self):
        publish = AsyncMock(side_effect=RuntimeError("bus down"))
        tool = self._make(publish_fn=publish)
        result = await tool.execute(
            {"channel": "cli", "chat_id": "c1", "text": "hi"}, _ctx()
        )
        assert result.success is False
        assert "bus down" in result.error


# ===========================================================================
# 7. NotifyTool
# ===========================================================================


class TestNotifyTool:
    def _make(self):
        from echo_agent.agent.tools.notify import NotifyTool

        bus = MagicMock()
        bus.publish_outbound = AsyncMock()
        return NotifyTool(bus=bus), bus

    @pytest.mark.asyncio
    async def test_notify_success(self):
        tool, bus = self._make()
        result = await tool.execute({"message": "Alert!"}, _ctx())
        assert result.success is True
        assert "cli:default" in result.output
        bus.publish_outbound.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_notify_custom_channel(self):
        tool, bus = self._make()
        result = await tool.execute(
            {"message": "test", "channel": "telegram", "chat_id": "g42"}, _ctx()
        )
        assert result.success is True
        assert "telegram:g42" in result.output


# ===========================================================================
# 8. ProcessTool
# ===========================================================================
class TestProcessTool:
    def _make(self, workspace="/tmp"):
        from echo_agent.agent.tools.process import ProcessTool

        policy = SimpleNamespace(security="full", ask="off", blocked_commands=[])
        return ProcessTool(workspace=workspace, exec_policy=policy)

    @pytest.mark.asyncio
    async def test_start_no_command(self):
        tool = self._make()
        result = await tool.execute({"action": "start", "command": ""}, _ctx())
        assert result.success is False
        assert "No command" in result.error

    @pytest.mark.asyncio
    async def test_start_success(self):
        tool = self._make()
        mock_proc = AsyncMock()
        mock_proc.pid = 12345
        mock_proc.stdout = AsyncMock()
        mock_proc.stdout.read = AsyncMock(return_value=b"")
        mock_proc.stderr = AsyncMock()

        with patch("asyncio.create_subprocess_shell", return_value=mock_proc):
            with patch("echo_agent.agent.tools.process.evaluate_shell_command") as mock_eval:
                mock_eval.return_value = SimpleNamespace(action="allow", reason="ok", pattern_key="")
                result = await tool.execute({"action": "start", "command": "echo hi"}, _ctx())
        assert result.success is True
        assert "proc_12345" in result.output

    @pytest.mark.asyncio
    async def test_list_empty(self):
        tool = self._make()
        # Clear global state
        import echo_agent.agent.tools.process as proc_mod
        orig = proc_mod._PROCESSES.copy()
        proc_mod._PROCESSES.clear()
        try:
            result = await tool.execute({"action": "list"}, _ctx())
            assert result.success is True
            assert "No background" in result.output
        finally:
            proc_mod._PROCESSES.update(orig)

    @pytest.mark.asyncio
    async def test_poll_not_found(self):
        tool = self._make()
        import echo_agent.agent.tools.process as proc_mod
        orig = proc_mod._PROCESSES.copy()
        proc_mod._PROCESSES.clear()
        try:
            result = await tool.execute({"action": "poll", "process_id": "proc_999"}, _ctx())
            assert result.success is False
            assert "not found" in result.error
        finally:
            proc_mod._PROCESSES.update(orig)

    @pytest.mark.asyncio
    async def test_stop_not_found(self):
        tool = self._make()
        import echo_agent.agent.tools.process as proc_mod
        orig = proc_mod._PROCESSES.copy()
        proc_mod._PROCESSES.clear()
        try:
            result = await tool.execute({"action": "stop", "process_id": "proc_999"}, _ctx())
            assert result.success is False
            assert "not found" in result.error
        finally:
            proc_mod._PROCESSES.update(orig)


# ===========================================================================
# 9. SearchFilesTool
# ===========================================================================


class TestSearchFilesTool:
    def _make(self, workspace: str):
        from echo_agent.agent.tools.search import SearchFilesTool

        return SearchFilesTool(workspace=workspace)

    @pytest.mark.asyncio
    async def test_content_search(self, tmp_path):
        (tmp_path / "app.py").write_text("def hello():\n    return 'world'\n")
        tool = self._make(str(tmp_path))
        result = await tool.execute({"pattern": "hello", "mode": "content"}, _ctx())
        assert result.success is True
        assert "hello" in result.output
        assert result.metadata["count"] >= 1

    @pytest.mark.asyncio
    async def test_glob_search(self, tmp_path):
        (tmp_path / "a.py").touch()
        (tmp_path / "b.txt").touch()
        tool = self._make(str(tmp_path))
        result = await tool.execute({"pattern": "*.py", "mode": "glob"}, _ctx())
        assert result.success is True
        assert "a.py" in result.output

    @pytest.mark.asyncio
    async def test_invalid_regex(self, tmp_path):
        tool = self._make(str(tmp_path))
        result = await tool.execute({"pattern": "[invalid", "mode": "content"}, _ctx())
        assert result.success is False
        assert "Invalid regex" in result.error

    @pytest.mark.asyncio
    async def test_directory_not_found(self, tmp_path):
        tool = self._make(str(tmp_path))
        result = await tool.execute({"pattern": "x", "path": "nonexist"}, _ctx())
        assert result.success is False


# ===========================================================================
# 10. SessionSearchTool
# ===========================================================================


class TestSessionSearchTool:
    def _make(self):
        from echo_agent.agent.tools.session_search import SessionSearchTool

        manager = MagicMock()
        return SessionSearchTool(session_manager=manager), manager

    @pytest.mark.asyncio
    async def test_search_specific_session(self):
        tool, manager = self._make()
        mock_session = SimpleNamespace(
            key="s1",
            messages=[
                {"role": "user", "content": "hello world"},
                {"role": "assistant", "content": "hi there"},
            ],
        )
        manager.get_or_create = AsyncMock(return_value=mock_session)
        result = await tool.execute({"query": "hello", "session_key": "s1"}, _ctx())
        assert result.success is True
        assert "hello" in result.output

    @pytest.mark.asyncio
    async def test_search_no_match(self):
        tool, manager = self._make()
        mock_session = SimpleNamespace(
            key="s1",
            messages=[{"role": "user", "content": "goodbye"}],
        )
        manager.get_or_create = AsyncMock(return_value=mock_session)
        result = await tool.execute({"query": "zzzzz", "session_key": "s1"}, _ctx())
        assert result.success is True
        assert "No matches" in result.output

    @pytest.mark.asyncio
    async def test_role_filter(self):
        tool, manager = self._make()
        mock_session = SimpleNamespace(
            key="s1",
            messages=[
                {"role": "user", "content": "keyword here"},
                {"role": "assistant", "content": "keyword too"},
            ],
        )
        manager.get_or_create = AsyncMock(return_value=mock_session)
        result = await tool.execute(
            {"query": "keyword", "session_key": "s1", "role_filter": "user"}, _ctx()
        )
        assert result.success is True
        assert "user:" in result.output
        assert "assistant:" not in result.output


# ===========================================================================
# 11. SkillInstallTool
# ===========================================================================
class TestSkillInstallTool:
    def _make(self):
        from echo_agent.agent.tools.skill_install import SkillInstallTool

        store = MagicMock()
        return SkillInstallTool(store=store), store

    @pytest.mark.asyncio
    async def test_missing_location(self):
        tool, _ = self._make()
        result = await tool.execute({"source": "git", "location": ""}, _ctx())
        assert result.success is False
        assert result.error  # Some error is returned for empty location

    @pytest.mark.asyncio
    async def test_invalid_source(self):
        tool, _ = self._make()
        result = await tool.execute({"source": "ftp", "location": "ftp://x"}, _ctx())
        assert result.success is False

    @pytest.mark.asyncio
    async def test_local_path_not_found(self):
        tool, _ = self._make()
        result = await tool.execute(
            {"source": "local", "location": "/nonexistent/path/xyz"}, _ctx()
        )
        assert result.success is False
        assert "not found" in result.error.lower() or "SKILL.md" in result.error


# ===========================================================================
# 12. SkillsListTool / SkillViewTool / SkillManageTool
# ===========================================================================


class TestSkillsListTool:
    def _make(self):
        from echo_agent.agent.tools.skills import SkillsListTool

        store = MagicMock()
        return SkillsListTool(store=store), store

    @pytest.mark.asyncio
    async def test_no_skills(self):
        tool, store = self._make()
        store.list_all.return_value = []
        result = await tool.execute({}, _ctx())
        assert result.success is True
        assert "No skills" in result.output

    @pytest.mark.asyncio
    async def test_with_skills(self):
        tool, store = self._make()
        mock_skill = SimpleNamespace(to_dict=lambda: {"name": "git-commit", "category": "dev"})
        store.list_all.return_value = [mock_skill]
        result = await tool.execute({}, _ctx())
        assert result.success is True
        assert "git-commit" in result.output


class TestSkillViewTool:
    def _make(self):
        from echo_agent.agent.tools.skills import SkillViewTool

        store = MagicMock()
        return SkillViewTool(store=store), store

    @pytest.mark.asyncio
    async def test_view_skill_not_found(self):
        tool, store = self._make()
        store.read_skill.return_value = None
        result = await tool.execute({"name": "nope"}, _ctx())
        assert result.success is False
        assert "not found" in result.error

    @pytest.mark.asyncio
    async def test_view_skill_success(self):
        tool, store = self._make()
        store.read_skill.return_value = "# My Skill\nDoes things."
        store.list_files.return_value = ["references/api.md"]
        result = await tool.execute({"name": "my-skill"}, _ctx())
        assert result.success is True
        assert "My Skill" in result.output
        assert "references/api.md" in result.output

    @pytest.mark.asyncio
    async def test_view_file(self):
        tool, store = self._make()
        store.read_file.return_value = "file content here"
        result = await tool.execute({"name": "s1", "file_path": "references/x.md"}, _ctx())
        assert result.success is True
        assert "file content here" in result.output


class TestSkillManageTool:
    def _make(self):
        from echo_agent.agent.tools.skills import SkillManageTool

        store = MagicMock()
        return SkillManageTool(store=store), store

    @pytest.mark.asyncio
    async def test_create_success(self):
        tool, store = self._make()
        store.create_skill.return_value = None  # no error
        result = await tool.execute(
            {"action": "create", "name": "my-skill", "content": "---\nname: my-skill\n---\n# Skill"},
            _ctx(),
        )
        assert result.success is True
        assert "created" in result.output

    @pytest.mark.asyncio
    async def test_create_no_content(self):
        tool, store = self._make()
        result = await tool.execute({"action": "create", "name": "x"}, _ctx())
        assert result.success is False
        assert "content" in result.error

    @pytest.mark.asyncio
    async def test_delete(self):
        tool, store = self._make()
        store.delete_skill.return_value = None
        result = await tool.execute({"action": "delete", "name": "old-skill"}, _ctx())
        assert result.success is True
        assert "deleted" in result.output

    @pytest.mark.asyncio
    async def test_unknown_action(self):
        tool, store = self._make()
        result = await tool.execute({"action": "explode", "name": "x"}, _ctx())
        assert result.success is False


# ===========================================================================
# 13. TaskTool
# ===========================================================================


class TestTaskTool:
    def _make(self):
        from echo_agent.agent.tools.task import TaskTool

        manager = AsyncMock()
        return TaskTool(manager=manager), manager

    @pytest.mark.asyncio
    async def test_create_task(self):
        tool, mgr = self._make()
        mock_task = SimpleNamespace(id="t1", title="Do thing")
        mgr.create.return_value = mock_task
        result = await tool.execute(
            {"action": "create", "title": "Do thing", "description": "desc"}, _ctx()
        )
        assert result.success is True
        assert "t1" in result.output

    @pytest.mark.asyncio
    async def test_get_not_found(self):
        tool, mgr = self._make()
        mgr.get.return_value = None
        result = await tool.execute({"action": "get", "task_id": "bad"}, _ctx())
        assert result.success is False
        assert "not found" in result.error.lower()

    @pytest.mark.asyncio
    async def test_list_empty(self):
        tool, mgr = self._make()
        mgr.list_by_status.return_value = []
        result = await tool.execute({"action": "list"}, _ctx())
        assert result.success is True
        assert "No tasks" in result.output

    @pytest.mark.asyncio
    async def test_unknown_action(self):
        tool, mgr = self._make()
        result = await tool.execute({"action": "nope"}, _ctx())
        assert result.success is False


# ===========================================================================
# 14. TTSTool
# ===========================================================================
class TestTTSTool:
    def _make(self, workspace="/tmp", openai_key=""):
        from echo_agent.agent.tools.tts import TTSTool

        return TTSTool(workspace=workspace, openai_api_key=openai_key)

    @pytest.mark.asyncio
    async def test_edge_tts_not_installed(self):
        tool = self._make()
        with patch.dict("sys.modules", {"edge_tts": None}):
            # Simulate ImportError by patching the import inside execute
            with patch("echo_agent.agent.tools.tts.TTSTool._edge_tts") as mock_edge:
                mock_edge.return_value = ToolResult(success=False, error="edge-tts not installed: pip install edge-tts")
                result = await tool.execute({"text": "hello"}, _ctx())
        assert result.success is False
        assert "edge-tts" in result.error

    @pytest.mark.asyncio
    async def test_edge_tts_success(self, tmp_path):
        tool = self._make(workspace=str(tmp_path))
        mock_communicate = MagicMock()
        mock_communicate.save = AsyncMock()

        with patch("echo_agent.agent.tools.tts.TTSTool._edge_tts") as mock_method:
            mock_method.return_value = ToolResult(
                output="Audio saved to output.mp3",
                metadata={"path": str(tmp_path / "output.mp3"), "voice": "en-US-AriaNeural"},
            )
            result = await tool.execute({"text": "hello", "backend": "edge"}, _ctx())
        assert result.success is True
        assert "Audio saved" in result.output

    @pytest.mark.asyncio
    async def test_openai_tts_no_key(self):
        tool = self._make(openai_key="")
        # Force openai backend
        result = await tool.execute({"text": "hello", "backend": "openai"}, _ctx())
        assert result.success is False
        assert "not configured" in result.error

    @pytest.mark.asyncio
    async def test_openai_tts_success(self, tmp_path):
        tool = self._make(workspace=str(tmp_path), openai_key="sk-test")
        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.read = AsyncMock(return_value=b"\x00\x01\x02audio-bytes")
        mock_resp.text = AsyncMock(return_value="")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.post = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"text": "hello", "backend": "openai"}, _ctx())
        assert result.success is True
        assert "Audio saved" in result.output


# ===========================================================================
# 15. VisionTool
# ===========================================================================


class TestVisionTool:
    def _make(self):
        from echo_agent.agent.tools.vision import VisionTool

        provider = AsyncMock()
        return VisionTool(provider=provider, workspace="/tmp"), provider

    @pytest.mark.asyncio
    async def test_url_image(self):
        tool, provider = self._make()
        provider.chat_with_retry.return_value = SimpleNamespace(
            finish_reason="stop", content="A cat sitting on a table"
        )
        result = await tool.execute(
            {"image": "https://example.com/cat.jpg", "prompt": "What is this?"}, _ctx()
        )
        assert result.success is True
        assert "cat" in result.output

    @pytest.mark.asyncio
    async def test_file_not_found(self):
        tool, provider = self._make()
        result = await tool.execute(
            {"image": "/nonexistent/img.png", "prompt": "describe"}, _ctx()
        )
        assert result.success is False
        assert "not found" in result.error

    @pytest.mark.asyncio
    async def test_provider_error(self):
        tool, provider = self._make()
        provider.chat_with_retry.return_value = SimpleNamespace(
            finish_reason="error", content="model overloaded"
        )
        result = await tool.execute(
            {"image": "https://example.com/x.jpg", "prompt": "what"}, _ctx()
        )
        assert result.success is False
        assert "overloaded" in result.error


# ===========================================================================
# 16. WebFetchTool / WebSearchTool
# ===========================================================================


class TestWebFetchTool:
    def _make(self):
        from echo_agent.agent.tools.web import WebFetchTool

        return WebFetchTool(allow_private=True)

    @pytest.mark.asyncio
    async def test_fetch_success(self):
        tool = self._make()
        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.reason = "OK"
        mock_resp.url = "https://example.com"
        mock_resp.headers = {"content-type": "text/html"}
        mock_resp.text = AsyncMock(return_value="<html>Hello</html>")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.get = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"url": "https://example.com"}, _ctx())
        assert result.success is True
        assert "Hello" in result.output
        assert result.metadata["status"] == 200

    @pytest.mark.asyncio
    async def test_fetch_4xx(self):
        tool = self._make()
        mock_resp = AsyncMock()
        mock_resp.status = 404
        mock_resp.reason = "Not Found"
        mock_resp.url = "https://example.com/missing"
        mock_resp.headers = {"content-type": "text/html"}
        mock_resp.text = AsyncMock(return_value="Not found")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.get = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"url": "https://example.com/missing"}, _ctx())
        assert result.success is False
        assert result.metadata["status"] == 404

    @pytest.mark.asyncio
    async def test_fetch_exception(self):
        tool = self._make()
        mock_session = AsyncMock()
        mock_session.get = MagicMock(side_effect=Exception("connection refused"))
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"url": "https://example.com"}, _ctx())
        assert result.success is False
        assert "connection refused" in result.error


class TestWebSearchTool:
    def _make(self, api_key="test-key", provider="brave"):
        from echo_agent.agent.tools.web import WebSearchTool

        return WebSearchTool(api_key=api_key, provider=provider)

    @pytest.mark.asyncio
    async def test_no_api_key(self):
        tool = self._make(api_key="")
        result = await tool.execute({"query": "test"}, _ctx())
        assert result.success is False
        assert "not configured" in result.error

    @pytest.mark.asyncio
    async def test_empty_query(self):
        tool = self._make()
        result = await tool.execute({"query": "  "}, _ctx())
        assert result.success is False
        assert "required" in result.error

    @pytest.mark.asyncio
    async def test_search_success_brave(self):
        tool = self._make()
        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.text = AsyncMock(return_value='{}')
        mock_resp.json = AsyncMock(return_value={
            "web": {"results": [
                {"title": "Result 1", "url": "https://r1.com", "description": "snippet 1"},
            ]}
        })
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.get = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"query": "python docs"}, _ctx())
        assert result.success is True
        assert "Result 1" in result.output
        assert result.metadata["count"] == 1

    @pytest.mark.asyncio
    async def test_search_no_results(self):
        tool = self._make()
        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.text = AsyncMock(return_value='{}')
        mock_resp.json = AsyncMock(return_value={"web": {"results": []}})
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)

        mock_session = AsyncMock()
        mock_session.get = MagicMock(return_value=mock_resp)
        mock_session.__aenter__ = AsyncMock(return_value=mock_session)
        mock_session.__aexit__ = AsyncMock(return_value=False)

        with patch("aiohttp.ClientSession", return_value=mock_session):
            result = await tool.execute({"query": "xyzzy"}, _ctx())
        assert result.success is True
        assert "No search results" in result.output


# ===========================================================================
# 17. WorkflowTool
# ===========================================================================


class TestWorkflowTool:
    def _make(self):
        from echo_agent.agent.tools.workflow import WorkflowTool

        engine = AsyncMock()
        return WorkflowTool(engine=engine), engine

    @pytest.mark.asyncio
    async def test_create_no_steps(self):
        tool, engine = self._make()
        result = await tool.execute({"action": "create", "name": "wf1"}, _ctx())
        assert result.success is False
        assert "steps" in result.error

    @pytest.mark.asyncio
    async def test_create_success(self):
        tool, engine = self._make()
        mock_step = SimpleNamespace(name="step1")
        mock_wf = SimpleNamespace(id="wf-1", name="My WF", steps=[mock_step])
        engine.create.return_value = mock_wf
        result = await tool.execute(
            {"action": "create", "name": "My WF", "steps": [{"id": "s1", "name": "step1", "tool_name": "echo"}]},
            _ctx(),
        )
        assert result.success is True
        assert "wf-1" in result.output
        assert "step1" in result.output

    @pytest.mark.asyncio
    async def test_status_not_found(self):
        tool, engine = self._make()
        engine.get.return_value = None
        result = await tool.execute({"action": "status", "workflow_id": "bad"}, _ctx())
        assert result.success is False
        assert "not found" in result.error.lower()

    @pytest.mark.asyncio
    async def test_list_empty(self):
        tool, engine = self._make()
        engine.list_all.return_value = []
        result = await tool.execute({"action": "list"}, _ctx())
        assert result.success is True
        assert "No workflows" in result.output

    @pytest.mark.asyncio
    async def test_unknown_action(self):
        tool, engine = self._make()
        result = await tool.execute({"action": "explode"}, _ctx())
        assert result.success is False
