"""Extra tests for remote/container executors — subprocess fully mocked."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.executors.base import ExecRequest
from echo_agent.agent.executors.remote import ContainerExecutor, RemoteExecutor


def _fake_proc(returncode=0, stdout=b"out", stderr=b"err"):
    proc = MagicMock()
    proc.returncode = returncode
    proc.communicate = AsyncMock(return_value=(stdout, stderr))
    proc.wait = AsyncMock()
    proc.kill = MagicMock()
    return proc


# ===========================================================================
# RemoteExecutor.execute
# ===========================================================================


class TestRemoteExecutorExecute:
    def _make(self, **kwargs):
        defaults = {"host": "10.0.0.1", "user": "deploy", "key_path": "/k"}
        defaults.update(kwargs)
        return RemoteExecutor(**defaults)

    @pytest.mark.asyncio
    async def test_setup_requires_host(self):
        ex = RemoteExecutor(host="")
        with pytest.raises(ValueError):
            await ex.setup()

    @pytest.mark.asyncio
    async def test_setup_ok_with_host(self):
        ex = self._make()
        await ex.setup()  # no raise
        await ex.teardown()

    @pytest.mark.asyncio
    async def test_execute_success(self):
        ex = self._make()
        proc = _fake_proc(returncode=0, stdout=b"hello", stderr=b"")
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
            resp = await ex.execute(ExecRequest(command="echo hi"))
        assert resp.success is True
        assert resp.stdout == "hello"
        assert resp.executor == "remote"

    @pytest.mark.asyncio
    async def test_execute_nonzero_return(self):
        ex = self._make()
        proc = _fake_proc(returncode=2, stdout=b"", stderr=b"fail")
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
            resp = await ex.execute(ExecRequest(command="false"))
        assert resp.success is False
        assert resp.return_code == 2

    @pytest.mark.asyncio
    async def test_execute_timeout(self):
        ex = self._make()
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=_fake_proc())), \
             patch("asyncio.wait_for", AsyncMock(side_effect=__import__("asyncio").TimeoutError())):
            resp = await ex.execute(ExecRequest(command="sleep 100", timeout=1))
        assert resp.success is False
        assert "Timeout" in resp.stderr

    @pytest.mark.asyncio
    async def test_execute_generic_exception(self):
        ex = self._make()
        with patch("asyncio.create_subprocess_exec", AsyncMock(side_effect=OSError("boom"))):
            resp = await ex.execute(ExecRequest(command="x"))
        assert resp.success is False
        assert "boom" in resp.stderr

    @pytest.mark.asyncio
    async def test_network_denied(self):
        ex = self._make(network_policy="deny")
        with patch(
            "echo_agent.agent.executors.remote.command_uses_network",
            return_value=True,
        ):
            resp = await ex.execute(ExecRequest(command="curl http://x"))
        assert resp.success is False
        assert "Network access is denied" in resp.stderr

    @pytest.mark.asyncio
    async def test_execute_with_stdin(self):
        ex = self._make()
        proc = _fake_proc(returncode=0, stdout=b"ok", stderr=b"")
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
            resp = await ex.execute(ExecRequest(command="cat", stdin="data"))
        assert resp.success is True
        proc.communicate.assert_awaited_once()


# ===========================================================================
# ContainerExecutor
# ===========================================================================


class TestContainerExecutor:
    @pytest.mark.asyncio
    async def test_setup_requires_image(self):
        ex = ContainerExecutor(image="")
        with pytest.raises(ValueError):
            await ex.setup()

    @pytest.mark.asyncio
    async def test_setup_creates_and_starts(self):
        ex = ContainerExecutor(image="python:3", network_policy="deny")
        create_proc = _fake_proc(returncode=0, stdout=b"container-abc\n", stderr=b"")
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=create_proc)):
            await ex.setup()
        assert ex._container_id == "container-abc"

    @pytest.mark.asyncio
    async def test_setup_docker_not_found(self):
        ex = ContainerExecutor(image="python:3")
        with patch("asyncio.create_subprocess_exec", AsyncMock(side_effect=FileNotFoundError())):
            with pytest.raises(RuntimeError, match="Docker not found"):
                await ex.setup()

    @pytest.mark.asyncio
    async def test_setup_create_failure(self):
        ex = ContainerExecutor(image="python:3")
        fail_proc = _fake_proc(returncode=1, stdout=b"", stderr=b"no image")
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=fail_proc)):
            with pytest.raises(RuntimeError, match="docker create failed"):
                await ex.setup()

    @pytest.mark.asyncio
    async def test_execute_runs_in_container(self):
        ex = ContainerExecutor(image="python:3")
        ex._container_id = "cid123"
        proc = _fake_proc(returncode=0, stdout=b"hello", stderr=b"")
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=proc)):
            resp = await ex.execute(ExecRequest(command="echo hi", env={"A": "b"}))
        assert resp.success is True
        assert resp.stdout == "hello"
        assert resp.executor == "container"

    @pytest.mark.asyncio
    async def test_execute_timeout(self):
        ex = ContainerExecutor(image="python:3")
        ex._container_id = "cid123"
        with patch("asyncio.create_subprocess_exec", AsyncMock(return_value=_fake_proc())), \
             patch("asyncio.wait_for", AsyncMock(side_effect=__import__("asyncio").TimeoutError())):
            resp = await ex.execute(ExecRequest(command="sleep 99", timeout=1))
        assert resp.success is False
        assert "Timeout" in resp.stderr

    @pytest.mark.asyncio
    async def test_teardown_removes_container(self):
        ex = ContainerExecutor(image="python:3")
        ex._container_id = "cid123"
        mock_exec = AsyncMock(return_value=_fake_proc())
        with patch("asyncio.create_subprocess_exec", mock_exec):
            await ex.teardown()
        mock_exec.assert_awaited()

    def test_container_cwd_no_workspace(self):
        ex = ContainerExecutor(image="python:3")
        assert ex._container_cwd("/some/path") == "/some/path"

    def test_container_cwd_default_workspace(self, tmp_path):
        ex = ContainerExecutor(image="python:3", workspace=str(tmp_path))
        assert ex._container_cwd("") == "/workspace"

    def test_container_cwd_relative_to_workspace(self, tmp_path):
        sub = tmp_path / "sub"
        sub.mkdir()
        ex = ContainerExecutor(image="python:3", workspace=str(tmp_path))
        assert ex._container_cwd(str(sub)) == "/workspace/sub"

    def test_container_cwd_outside_workspace(self, tmp_path):
        ex = ContainerExecutor(image="python:3", workspace=str(tmp_path))
        # An unrelated absolute path falls back to /workspace.
        assert ex._container_cwd("/etc") == "/workspace"
