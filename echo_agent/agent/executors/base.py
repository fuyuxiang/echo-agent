"""Execution environments — isolated runtimes for agent task execution.

Supports local, sandbox, container, and remote execution with
command isolation, filesystem boundaries, network control, credential injection, and audit.
"""

from __future__ import annotations

import asyncio
import os
import shutil
import sys
import tempfile
import uuid
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

from loguru import logger

from echo_agent.agent.proc_lifecycle import (
    record_process_group,
    subprocess_kwargs,
    terminate_tree,
)
from echo_agent.security.guards import command_uses_network


def prepend_interpreter_bin(env: dict[str, str]) -> dict[str, str]:
    """Put the directory of ``sys.executable`` ahead on PATH.

    A skill script written as ``python3 scripts/foo.py`` inherits the shell's
    PATH, not ours. When the service is started by launchd, systemd or a
    desktop launcher, that PATH usually does NOT contain the venv ``bin`` the
    script's deps were installed into — and ``python3`` resolves to the
    system interpreter, which has none of them. The skill crashes on the
    first import.

    Putting ``sys.executable``'s directory ahead makes ``python3`` resolve to
    the *same* interpreter the agent itself runs under, and therefore to the
    same venv. We never replace PATH outright — any project-specific dirs the
    operator arranged survive, just with the venv bin in front so it wins the
    first match.
    """
    exe_dir = os.path.dirname(sys.executable) if sys.executable else ""
    if not exe_dir:
        return env
    existing = env.get("PATH", "/usr/bin:/bin")
    parts = [p for p in existing.split(os.pathsep) if p]
    if exe_dir not in parts:
        env["PATH"] = os.pathsep.join([exe_dir, *parts])
    return env


@dataclass
class ExecRequest:
    command: str
    cwd: str = ""
    env: dict[str, str] = field(default_factory=dict)
    timeout: int = 30
    stdin: str = ""
    credentials: dict[str, str] = field(default_factory=dict)


@dataclass
class ExecResponse:
    success: bool = True
    stdout: str = ""
    stderr: str = ""
    return_code: int = 0
    duration_ms: int = 0
    executor: str = ""
    audit_id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])


class BaseExecutor(ABC):
    """Abstract execution environment."""

    name: str = "base"

    @abstractmethod
    async def execute(self, request: ExecRequest) -> ExecResponse:
        """Execute a command in this environment."""

    @abstractmethod
    async def setup(self) -> None:
        """Initialize the execution environment."""

    @abstractmethod
    async def teardown(self) -> None:
        """Clean up the execution environment."""

    def inject_credentials(self, env: dict[str, str], credentials: dict[str, str]) -> dict[str, str]:
        merged = dict(env)
        for key, value in credentials.items():
            merged[key] = value
        return merged


class LocalExecutor(BaseExecutor):
    """Execute commands directly on the host."""

    name = "local"

    def __init__(self, workspace: str, network_policy: str = "allow"):
        self._workspace = workspace
        self._network_policy = network_policy

    async def setup(self) -> None:
        Path(self._workspace).mkdir(parents=True, exist_ok=True)

    async def teardown(self) -> None:
        pass

    async def execute(self, request: ExecRequest) -> ExecResponse:
        if self._network_policy == "deny" and command_uses_network(request.command):
            return ExecResponse(success=False, stderr="Network access is denied by execution policy", return_code=-1, executor=self.name)
        cwd = request.cwd or self._workspace
        env = prepend_interpreter_bin(dict(os.environ))
        env = self.inject_credentials(env, request.credentials)
        env.update(request.env)
        start = datetime.now()

        proc = None
        try:
            proc = await asyncio.create_subprocess_shell(
                request.command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                stdin=asyncio.subprocess.PIPE if request.stdin else None,
                cwd=cwd,
                env=env,
                **subprocess_kwargs(),
            )
            # Record the PGID while the leader is alive: a command that
            # backgrounds work outlives its shell, and after the leader is
            # reaped its group is no longer discoverable.
            record_process_group(proc, own_session=True)
            stdout, stderr = await asyncio.wait_for(
                proc.communicate(request.stdin.encode() if request.stdin else None),
                timeout=request.timeout,
            )
            duration = int((datetime.now() - start).total_seconds() * 1000)
            return ExecResponse(
                success=proc.returncode == 0,
                stdout=stdout.decode(errors="replace"),
                stderr=stderr.decode(errors="replace"),
                return_code=proc.returncode or 0,
                duration_ms=duration,
                executor=self.name,
            )
        except asyncio.TimeoutError:
            if proc is not None:
                await terminate_tree(proc)
            return ExecResponse(success=False, stderr=f"Timeout after {request.timeout}s", return_code=-1, executor=self.name)
        except Exception as e:
            if proc is not None:
                # Sweep unconditionally: an exited leader can still have left
                # backgrounded grandchildren in the group.
                await terminate_tree(proc)
            return ExecResponse(success=False, stderr=str(e), return_code=-1, executor=self.name)


class SandboxExecutor(BaseExecutor):
    """Execute commands in an isolated temp directory with restricted filesystem access."""

    name = "sandbox"

    def __init__(
        self,
        sandbox_root: str = "/tmp/echo-agent-sandbox",
        network_policy: str = "deny",
        workspace: str = "",
    ):
        self._root = Path(sandbox_root)
        self._network_policy = network_policy
        self._source_workspace = Path(workspace).resolve() if workspace else None
        self._sandbox_dir: Path | None = None
        self._workdir: Path | None = None

    async def setup(self) -> None:
        self._root.mkdir(parents=True, exist_ok=True)
        self._sandbox_dir = Path(tempfile.mkdtemp(dir=self._root, prefix="sandbox_"))
        self._workdir = self._sandbox_dir / "workspace"
        if self._source_workspace and self._source_workspace.exists():
            ignore = shutil.ignore_patterns(
                ".git",
                "__pycache__",
                ".pytest_cache",
                ".ruff_cache",
                ".venv",
                "node_modules",
                "data/logs",
                # Bundled runtime dirs are hundreds of MB and never needed inside
                # the sandbox; copying them synchronously froze the event loop.
                "runtime",
                "python",
            )
            # copytree walks the whole workspace synchronously; even with the
            # ignore list a large tree can block the loop thread for seconds, so
            # run it off-loop.
            await asyncio.to_thread(
                shutil.copytree,
                self._source_workspace,
                self._workdir,
                dirs_exist_ok=True,
                ignore=ignore,
            )
        else:
            self._workdir.mkdir(parents=True, exist_ok=True)
        logger.info("Sandbox created at {}", self._sandbox_dir)

    async def teardown(self) -> None:
        if self._sandbox_dir and self._sandbox_dir.exists():
            await asyncio.to_thread(shutil.rmtree, self._sandbox_dir, ignore_errors=True)

    async def execute(self, request: ExecRequest) -> ExecResponse:
        if not self._sandbox_dir:
            await self.setup()
        if self._network_policy == "deny" and command_uses_network(request.command):
            return ExecResponse(success=False, stderr="Network access is denied by execution policy", return_code=-1, executor=self.name)
        cwd = str(self._resolve_cwd(request.cwd))
        env = self.inject_credentials({"HOME": cwd, "TMPDIR": cwd}, request.credentials)
        env.update(request.env)
        env["PATH"] = prepend_interpreter_bin(dict(os.environ))["PATH"]

        start = datetime.now()
        proc = None
        try:
            proc = await asyncio.create_subprocess_shell(
                request.command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                stdin=asyncio.subprocess.PIPE if request.stdin else None,
                cwd=cwd,
                env=env,
                **subprocess_kwargs(),
            )
            record_process_group(proc, own_session=True)
            stdout, stderr = await asyncio.wait_for(
                proc.communicate(request.stdin.encode() if request.stdin else None),
                timeout=request.timeout,
            )
            duration = int((datetime.now() - start).total_seconds() * 1000)
            return ExecResponse(
                success=proc.returncode == 0,
                stdout=stdout.decode(errors="replace"),
                stderr=stderr.decode(errors="replace"),
                return_code=proc.returncode or 0,
                duration_ms=duration,
                executor=self.name,
            )
        except asyncio.TimeoutError:
            if proc is not None:
                await terminate_tree(proc)
            return ExecResponse(success=False, stderr=f"Timeout after {request.timeout}s", return_code=-1, executor=self.name)
        except Exception as e:
            if proc is not None:
                await terminate_tree(proc)
            return ExecResponse(success=False, stderr=str(e), return_code=-1, executor=self.name)

    def _resolve_cwd(self, requested_cwd: str) -> Path:
        if not self._workdir:
            assert self._sandbox_dir
            return self._sandbox_dir
        if not requested_cwd or not self._source_workspace:
            return self._workdir
        try:
            rel = Path(requested_cwd).resolve().relative_to(self._source_workspace)
            target = (self._workdir / rel).resolve()
            target.relative_to(self._workdir)
            target.mkdir(parents=True, exist_ok=True)
            return target
        except ValueError:
            return self._workdir
