"""Container and remote executors."""

from __future__ import annotations

import asyncio
import uuid
from typing import Any

from loguru import logger

from echo_agent.agent.executors.base import BaseExecutor, ExecRequest, ExecResponse


class ContainerExecutor(BaseExecutor):
    """Execute commands inside a Docker container."""

    name = "container"

    def __init__(self, image: str = "", network_policy: str = "restricted"):
        self._image = image
        self._network_policy = network_policy
        self._container_id: str | None = None

    async def setup(self) -> None:
        if not self._image:
            raise ValueError("Container image not configured")
        try:
            proc = await asyncio.create_subprocess_exec(
                "docker", "create", "--rm",
                "--network", "none" if self._network_policy == "deny" else "bridge",
                "--name", f"echo-agent-{uuid.uuid4().hex[:8]}",
                self._image, "sleep", "infinity",
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            stdout, stderr = await proc.communicate()
            if proc.returncode != 0:
                raise RuntimeError(f"docker create failed: {stderr.decode()}")
            self._container_id = stdout.decode().strip()
            await asyncio.create_subprocess_exec("docker", "start", self._container_id)
            logger.info("Container {} started from {}", self._container_id[:12], self._image)
        except FileNotFoundError:
            raise RuntimeError("Docker not found — install Docker to use container execution")

    async def teardown(self) -> None:
        if self._container_id:
            try:
                await asyncio.create_subprocess_exec("docker", "rm", "-f", self._container_id)
            except Exception as e:
                logger.warning("Failed to remove container: {}", e)

    async def execute(self, request: ExecRequest) -> ExecResponse:
        if not self._container_id:
            await self.setup()

        env_args = []
        merged_env = self.inject_credentials({}, request.credentials)
        merged_env.update(request.env)
        for k, v in merged_env.items():
            env_args.extend(["-e", f"{k}={v}"])

        cmd = ["docker", "exec"] + env_args
        if request.cwd:
            cmd.extend(["-w", request.cwd])
        cmd.extend([self._container_id, "sh", "-c", request.command])

        try:
            proc = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=request.timeout)
            return ExecResponse(
                success=proc.returncode == 0,
                stdout=stdout.decode(errors="replace"),
                stderr=stderr.decode(errors="replace"),
                return_code=proc.returncode or 0,
                executor=self.name,
            )
        except asyncio.TimeoutError:
            return ExecResponse(success=False, stderr=f"Timeout after {request.timeout}s", return_code=-1, executor=self.name)
        except Exception as e:
            return ExecResponse(success=False, stderr=str(e), return_code=-1, executor=self.name)


class RemoteExecutor(BaseExecutor):
    """Execute commands on a remote host via SSH."""

    name = "remote"

    def __init__(self, host: str = "", user: str = "root", key_path: str = ""):
        self._host = host
        self._user = user
        self._key_path = key_path

    async def setup(self) -> None:
        if not self._host:
            raise ValueError("Remote host not configured")

    async def teardown(self) -> None:
        pass

    async def execute(self, request: ExecRequest) -> ExecResponse:
        ssh_cmd = ["ssh", "-o", "StrictHostKeyChecking=no"]
        if self._key_path:
            ssh_cmd.extend(["-i", self._key_path])
        ssh_cmd.append(f"{self._user}@{self._host}")

        env_prefix = " ".join(f"{k}={v}" for k, v in request.env.items())
        full_cmd = f"{env_prefix} {request.command}" if env_prefix else request.command
        if request.cwd:
            full_cmd = f"cd {request.cwd} && {full_cmd}"
        ssh_cmd.append(full_cmd)

        try:
            proc = await asyncio.create_subprocess_exec(
                *ssh_cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=request.timeout)
            return ExecResponse(
                success=proc.returncode == 0,
                stdout=stdout.decode(errors="replace"),
                stderr=stderr.decode(errors="replace"),
                return_code=proc.returncode or 0,
                executor=self.name,
            )
        except asyncio.TimeoutError:
            return ExecResponse(success=False, stderr=f"Timeout after {request.timeout}s", return_code=-1, executor=self.name)
        except Exception as e:
            return ExecResponse(success=False, stderr=str(e), return_code=-1, executor=self.name)
