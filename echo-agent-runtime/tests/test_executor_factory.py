"""Tests for echo_agent.agent.executors.factory — executor creation."""

from pathlib import Path

import pytest

from echo_agent.agent.executors.factory import create_executor
from echo_agent.agent.executors.base import LocalExecutor
from echo_agent.agent.executors.remote import RemoteExecutor
from echo_agent.config.schema import ExecutionConfig


@pytest.fixture
def workspace(tmp_path: Path) -> Path:
    return tmp_path / "workspace"


class TestCreateExecutor:
    def test_host_auto_uses_default_executor(self, workspace: Path):
        config = ExecutionConfig(default_executor="local")
        executor = create_executor(config, workspace, host="auto")
        assert isinstance(executor, LocalExecutor)

    def test_kind_local(self, workspace: Path):
        config = ExecutionConfig()
        executor = create_executor(config, workspace, host="local")
        assert isinstance(executor, LocalExecutor)

    def test_kind_remote(self, workspace: Path):
        config = ExecutionConfig(
            remote_host="10.0.0.1",
            remote_user="deploy",
            remote_key_path="/home/user/.ssh/id_rsa",
        )
        executor = create_executor(config, workspace, host="remote")
        assert isinstance(executor, RemoteExecutor)

    def test_unknown_kind_raises_value_error(self, workspace: Path):
        config = ExecutionConfig()
        with pytest.raises(ValueError, match="Unsupported executor"):
            create_executor(config, workspace, host="quantum")
