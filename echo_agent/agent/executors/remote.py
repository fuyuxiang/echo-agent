"""Container and remote executors."""

from __future__ import annotations

import asyncio
import shlex
import uuid
from pathlib import Path

from loguru import logger

from echo_agent.agent.executors.base import BaseExecutor, ExecRequest, ExecResponse
from echo_agent.agent.proc_lifecycle import communicate_owned, spawn_exec
from echo_agent.security.guards import command_uses_network


class ContainerExecutor(BaseExecutor):
    """Execute commands inside a Docker container.

    Each invocation owns and reaps the local ``docker`` CLI process tree. A
    process deliberately daemonized *inside* the container is owned by the
    Docker daemon, not that local PGID; executor teardown's ``docker rm -f`` is
    the lifecycle backstop for those container-internal processes.
    """

    name = "container"
    _CONTROL_TIMEOUT_SECONDS = 30.0

    def __init__(self, image: str = "", network_policy: str = "restricted", workspace: str = ""):
        self._image = image
        self._network_policy = network_policy
        self._workspace = Path(workspace).resolve() if workspace else None
        self._container_id: str | None = None
        # `docker create --name` can commit in the daemon before its CLI reply
        # reaches us. Keep the requested name separately so cancellation in that
        # window can remove by name without exposing it as an executable id.
        self._setup_cleanup_target: str | None = None

    async def setup(self) -> None:
        if not self._image:
            raise ValueError("Container image not configured")
        try:
            mount_args: list[str] = []
            if self._workspace:
                mount_args = ["-v", f"{self._workspace}:/workspace", "-w", "/workspace"]
            container_name = f"echo-agent-{uuid.uuid4().hex[:8]}"
            self._setup_cleanup_target = container_name
            return_code, stdout, stderr = await self._run_docker_control(
                "create", "--rm",
                "--network", "none" if self._network_policy == "deny" else "bridge",
                "--name", container_name,
                *mount_args,
                self._image, "sleep", "infinity",
            )
            if return_code != 0:
                # The daemon definitively rejected create. Do not `rm` the
                # reserved random name: an extremely unlikely name collision
                # would otherwise delete the pre-existing container we do not own.
                self._setup_cleanup_target = None
                raise RuntimeError(f"docker create failed: {stderr.decode()}")
            self._container_id = stdout.decode().strip()
            if not self._container_id:
                raise RuntimeError("docker create returned an empty container id")
            return_code, _, stderr = await self._run_docker_control(
                "start", self._container_id,
            )
            if return_code != 0:
                raise RuntimeError(f"docker start failed: {stderr.decode()}")
            self._setup_cleanup_target = None
            logger.info("Container {} started from {}", self._container_id[:12], self._image)
        except asyncio.CancelledError:
            # A create may have committed before this task was cancelled. Remove
            # that container before propagating cancellation so setup is atomic
            # from the executor's perspective.
            await self.teardown()
            raise
        except FileNotFoundError as e:
            # No CLI process was spawned. If this happened at `start`, the
            # successfully returned id still drives teardown; if it happened at
            # `create`, there is nothing that belongs to us to remove by name.
            self._setup_cleanup_target = None
            await self.teardown()
            raise RuntimeError(
                "Docker not found — install Docker to use container execution"
            ) from e
        except Exception:
            await self.teardown()
            raise

    async def teardown(self) -> None:
        cleanup_target = self._container_id or self._setup_cleanup_target
        if not cleanup_target:
            return
        try:
            return_code, _, stderr = await self._run_docker_control(
                "rm", "-f", cleanup_target,
            )
            if return_code != 0:
                logger.warning(
                    "Failed to remove container {}: {}",
                    cleanup_target[:12], stderr.decode(errors="replace"),
                )
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("Failed to remove container {}: {}", cleanup_target[:12], e)
        finally:
            # Never retain a stale id after teardown. In particular, execute()
            # must not attempt `docker exec` against a container whose removal
            # was requested but whose CLI response failed or timed out.
            self._container_id = None
            self._setup_cleanup_target = None

    async def _run_docker_control(self, *args: str) -> tuple[int, bytes, bytes]:
        """Run and reap a bounded Docker lifecycle command."""
        try:
            proc = await spawn_exec(
                "docker", *args,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            stdout, stderr = await communicate_owned(
                proc, timeout=self._CONTROL_TIMEOUT_SECONDS,
            )
            return proc.returncode or 0, stdout, stderr
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError as e:
            action = args[0] if args else "command"
            raise RuntimeError(
                f"docker {action} timed out after {self._CONTROL_TIMEOUT_SECONDS:g}s"
            ) from e
        except Exception:
            raise

    async def execute(self, request: ExecRequest) -> ExecResponse:
        if not self._container_id:
            await self.setup()

        env_args = []
        merged_env = self.inject_credentials({}, request.credentials)
        merged_env.update(request.env)
        for k, v in merged_env.items():
            env_args.extend(["-e", f"{k}={v}"])

        cmd = ["docker", "exec"]
        if request.stdin:
            cmd.append("-i")
        cmd += env_args
        cwd = self._container_cwd(request.cwd)
        if cwd:
            cmd.extend(["-w", cwd])
        cmd.extend([self._container_id, "sh", "-c", request.command])

        try:
            proc = await spawn_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                stdin=asyncio.subprocess.PIPE if request.stdin else None,
            )
            stdout, stderr = await communicate_owned(
                proc,
                request.stdin.encode() if request.stdin else None,
                timeout=request.timeout,
            )
            return ExecResponse(
                success=proc.returncode == 0,
                stdout=stdout.decode(errors="replace"),
                stderr=stderr.decode(errors="replace"),
                return_code=proc.returncode or 0,
                executor=self.name,
            )
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            return ExecResponse(success=False, stderr=f"Timeout after {request.timeout}s", return_code=-1, executor=self.name)
        except Exception as e:
            return ExecResponse(success=False, stderr=str(e), return_code=-1, executor=self.name)

    def _container_cwd(self, cwd: str) -> str:
        if not self._workspace:
            return cwd
        if not cwd:
            return "/workspace"
        try:
            rel = Path(cwd).resolve().relative_to(self._workspace)
            return str(Path("/workspace") / rel)
        except ValueError:
            return "/workspace"


class RemoteExecutor(BaseExecutor):
    """Execute commands on a remote host via SSH with proper input sanitization.

    The lifecycle owner can reclaim the local ``ssh`` client and any local
    descendants only. A remote command that deliberately daemonizes and closes
    the SSH transport is no longer in that process group; stopping it requires a
    remote PID/service protocol and is outside this executor's guarantee.
    """

    name = "remote"

    def __init__(
        self,
        host: str = "",
        user: str = "root",
        key_path: str = "",
        strict_host_key: str = "accept-new",
        connect_timeout: int = 10,
        network_policy: str = "restricted",
    ):
        self._host = host
        self._user = user
        self._key_path = key_path
        self._strict_host_key = strict_host_key
        self._connect_timeout = connect_timeout
        self._network_policy = network_policy

    async def setup(self) -> None:
        if not self._host:
            raise ValueError("Remote host not configured")

    async def teardown(self) -> None:
        pass

    def _build_ssh_base(self) -> list[str]:
        cmd = [
            "ssh",
            "-o", f"StrictHostKeyChecking={self._strict_host_key}",
            "-o", f"ConnectTimeout={self._connect_timeout}",
        ]
        if self._key_path:
            cmd.extend(["-i", self._key_path])
        cmd.append(f"{self._user}@{self._host}")
        return cmd

    def _build_remote_command(self, request: ExecRequest) -> str:
        merged_env = self.inject_credentials({}, request.credentials)
        merged_env.update(request.env)

        env_prefix = " ".join(
            f"{shlex.quote(k)}={shlex.quote(v)}" for k, v in merged_env.items()
        )
        safe_cmd = request.command
        if env_prefix:
            safe_cmd = f"{env_prefix} {safe_cmd}"
        if request.cwd:
            safe_cmd = f"cd {shlex.quote(request.cwd)} && {safe_cmd}"
        return safe_cmd

    async def execute(self, request: ExecRequest) -> ExecResponse:
        if self._network_policy == "deny" and command_uses_network(request.command):
            return ExecResponse(success=False, stderr="Network access is denied by execution policy", return_code=-1, executor=self.name)
        ssh_cmd = self._build_ssh_base()
        ssh_cmd.append(self._build_remote_command(request))

        try:
            proc = await spawn_exec(
                *ssh_cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                stdin=asyncio.subprocess.PIPE if request.stdin else None,
            )
            stdout, stderr = await communicate_owned(
                proc,
                request.stdin.encode() if request.stdin else None,
                timeout=request.timeout,
            )
            return ExecResponse(
                success=proc.returncode == 0,
                stdout=stdout.decode(errors="replace"),
                stderr=stderr.decode(errors="replace"),
                return_code=proc.returncode or 0,
                executor=self.name,
            )
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            return ExecResponse(success=False, stderr=f"Timeout after {request.timeout}s", return_code=-1, executor=self.name)
        except Exception as e:
            return ExecResponse(success=False, stderr=str(e), return_code=-1, executor=self.name)
