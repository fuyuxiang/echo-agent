"""LocalExecutor / SandboxExecutor 执行主流程测试。

直接驱动执行器跑真实子进程，覆盖成功/失败/超时/网络策略/凭证注入，
以及 sandbox 的隔离目录复制与路径解析。类 Unix 环境下运行。"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

from echo_agent.agent.executors.base import (
    BaseExecutor,
    ExecRequest,
    ExecResponse,
    LocalExecutor,
    SandboxExecutor,
)

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="shell 子进程语义按类 Unix 假设编写"
)


# ---------------------------------------------------------------------------
# BaseExecutor.inject_credentials
# ---------------------------------------------------------------------------

def test_inject_credentials_merges_without_mutating_source():
    class _Dummy(BaseExecutor):
        async def execute(self, request):  # pragma: no cover - 抽象占位
            ...

        async def setup(self):  # pragma: no cover
            ...

        async def teardown(self):  # pragma: no cover
            ...

    base_env = {"A": "1"}
    merged = _Dummy().inject_credentials(base_env, {"SECRET": "xyz", "A": "override"})
    assert merged == {"A": "override", "SECRET": "xyz"}
    # 源 env 不被就地修改
    assert base_env == {"A": "1"}


# ---------------------------------------------------------------------------
# LocalExecutor
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_local_executor_runs_command_and_captures_stdout(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="echo hello-echo"))
    assert isinstance(resp, ExecResponse)
    assert resp.success is True
    assert resp.return_code == 0
    assert "hello-echo" in resp.stdout
    assert resp.executor == "local"
    assert resp.duration_ms >= 0
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_nonzero_exit_marks_failure(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="exit 3"))
    assert resp.success is False
    assert resp.return_code == 3
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_default_cwd_is_workspace(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="pwd"))
    assert str(tmp_path) in resp.stdout
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_reads_stdin(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="cat", stdin="piped-input"))
    assert "piped-input" in resp.stdout
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_injects_credentials_into_env(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()
    resp = await ex.execute(
        ExecRequest(command="echo $MY_TOKEN", credentials={"MY_TOKEN": "s3cr3t"})
    )
    assert "s3cr3t" in resp.stdout
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_timeout_returns_error(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="sleep 5", timeout=1))
    assert resp.success is False
    assert resp.return_code == -1
    assert "Timeout" in resp.stderr
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_network_deny_blocks_networked_command(tmp_path: Path):
    ex = LocalExecutor(workspace=str(tmp_path), network_policy="deny")
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="curl https://example.com"))
    assert resp.success is False
    assert "Network access is denied" in resp.stderr
    assert resp.return_code == -1
    await ex.teardown()


@pytest.mark.asyncio
async def test_local_executor_exec_failure_captured_as_stderr(tmp_path: Path, monkeypatch):
    ex = LocalExecutor(workspace=str(tmp_path))
    await ex.setup()

    async def _boom(*a, **k):
        raise OSError("spawn failed")

    monkeypatch.setattr(
        "echo_agent.agent.executors.base.asyncio.create_subprocess_shell", _boom
    )
    resp = await ex.execute(ExecRequest(command="echo hi"))
    assert resp.success is False
    assert resp.return_code == -1
    assert "spawn failed" in resp.stderr
    await ex.teardown()


# ---------------------------------------------------------------------------
# SandboxExecutor
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_sandbox_setup_copies_workspace_excluding_ignored(tmp_path: Path):
    src = tmp_path / "src"
    (src / ".git").mkdir(parents=True)
    (src / ".git" / "config").write_text("should-be-ignored")
    (src / "keep.txt").write_text("copied")
    root = tmp_path / "sbroot"

    ex = SandboxExecutor(sandbox_root=str(root), workspace=str(src))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="ls -a"))
    assert "keep.txt" in resp.stdout
    assert ".git" not in resp.stdout
    await ex.teardown()


@pytest.mark.asyncio
async def test_sandbox_execute_lazy_setup_when_not_initialized(tmp_path: Path):
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"))
    # 未显式 setup，execute 应自行初始化
    resp = await ex.execute(ExecRequest(command="echo lazy"))
    assert resp.success is True
    assert "lazy" in resp.stdout
    assert resp.executor == "sandbox"
    await ex.teardown()


@pytest.mark.asyncio
async def test_sandbox_network_deny_blocks(tmp_path: Path):
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"), network_policy="deny")
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="wget http://example.com"))
    assert resp.success is False
    assert "Network access is denied" in resp.stderr
    await ex.teardown()


@pytest.mark.asyncio
async def test_sandbox_teardown_removes_dir(tmp_path: Path):
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"))
    await ex.setup()
    sandbox_dir = ex._sandbox_dir
    assert sandbox_dir is not None and sandbox_dir.exists()
    await ex.teardown()
    assert not sandbox_dir.exists()


@pytest.mark.asyncio
async def test_sandbox_timeout_returns_error(tmp_path: Path):
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"))
    await ex.setup()
    resp = await ex.execute(ExecRequest(command="sleep 5", timeout=1))
    assert resp.success is False
    assert "Timeout" in resp.stderr
    await ex.teardown()


def test_sandbox_resolve_cwd_falls_back_to_workdir_for_outside_path(tmp_path: Path):
    src = tmp_path / "src"
    src.mkdir()
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"), workspace=str(src))
    # 未 setup 时 workdir 为 None，_resolve_cwd 走 sandbox_dir 断言分支需先 setup。
    # 这里手动构造 workdir 以测试路径逃逸回退。
    ex._workdir = tmp_path / "wd"
    ex._workdir.mkdir()
    ex._source_workspace = src.resolve()
    # 请求一个在 source_workspace 之外的路径 -> ValueError -> 回退到 workdir
    outside = str(tmp_path / "elsewhere")
    resolved = ex._resolve_cwd(outside)
    assert resolved == ex._workdir


def test_sandbox_resolve_cwd_maps_relative_path_into_workdir(tmp_path: Path):
    src = tmp_path / "src"
    (src / "sub").mkdir(parents=True)
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"), workspace=str(src))
    ex._workdir = tmp_path / "wd"
    ex._workdir.mkdir()
    ex._source_workspace = src.resolve()
    resolved = ex._resolve_cwd(str(src / "sub"))
    assert resolved == (ex._workdir / "sub")
    assert resolved.exists()


def test_sandbox_resolve_cwd_no_requested_returns_workdir(tmp_path: Path):
    ex = SandboxExecutor(sandbox_root=str(tmp_path / "root"), workspace=str(tmp_path / "src"))
    ex._workdir = tmp_path / "wd"
    ex._workdir.mkdir()
    assert ex._resolve_cwd("") == ex._workdir
