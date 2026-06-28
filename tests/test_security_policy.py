from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.executors.base import ExecRequest, LocalExecutor
from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.code_exec import CodeExecTool
from echo_agent.agent.tools.shell import ShellTool
from echo_agent.config.loader import load_config
from echo_agent.config.schema import Config, ExecToolConfig, ToolsConfig
from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus
from echo_agent.security.guards import evaluate_shell_command
from echo_agent.security.tool_policy import is_tool_allowed


def test_default_security_posture_is_restrictive() -> None:
    cfg = load_config()

    assert cfg.security.profile == "personal_cli"
    assert cfg.tools.exec.enabled is True
    assert cfg.tools.web.enabled is True
    assert cfg.tools.code_exec.enabled is True
    assert cfg.execution.default_executor == "local"
    assert cfg.execution.network_policy == "allow"
    assert cfg.permissions.approval.default_policy == "approve"
    assert "cronjob" in cfg.permissions.approval.require_approval


def test_daemon_profile_requires_explicit_high_risk_tool_allow() -> None:
    cfg = Config(
        security={"profile": "daemon"},
        tools=ToolsConfig(profile="full"),
    )

    assert not is_tool_allowed(cfg, "exec")
    assert is_tool_allowed(
        Config(security={"profile": "daemon"}, tools=ToolsConfig(profile="full", also_allow=["exec"])),
        "exec",
    )


def test_public_gateway_blocks_write_tools_unless_explicitly_allowed() -> None:
    cfg = Config(security={"profile": "public_gateway"}, tools=ToolsConfig(profile="coding"))

    assert not is_tool_allowed(cfg, "write_file")
    assert is_tool_allowed(
        Config(security={"profile": "public_gateway"}, tools=ToolsConfig(profile="coding", also_allow=["write_file"])),
        "write_file",
    )


def test_network_deny_hard_blocks_web_tools() -> None:
    cfg = Config(tools=ToolsConfig(profile="full", also_allow=["web_fetch"]))

    assert not is_tool_allowed(cfg, "web_fetch")


def test_network_deny_blocks_tools_by_capability() -> None:
    class CustomNetworkTool:
        name = "custom_api"
        capabilities = ("network.outbound",)

    cfg = Config(tools=ToolsConfig(profile="full", also_allow=["custom_api"]))

    assert not is_tool_allowed(cfg, CustomNetworkTool())


def test_exec_allowlist_miss_requires_approval() -> None:
    decision = evaluate_shell_command(
        "python3 -m pytest",
        exec_policy=ExecToolConfig(enabled=True, safe_bins=["ls"], ask="on_miss"),
        network_policy="deny",
    )

    assert decision.action == "ask"
    assert decision.pattern_key == "allowlist_miss"


def test_network_command_is_denied_under_deny_policy() -> None:
    decision = evaluate_shell_command(
        "curl https://example.com",
        exec_policy=ExecToolConfig(enabled=True, safe_bins=["curl"], ask="on_miss"),
        network_policy="deny",
    )

    assert decision.action == "deny"
    assert decision.pattern_key == "network_denied"


@pytest.mark.asyncio
async def test_shell_tool_allows_approved_allowlist_miss(tmp_path) -> None:
    policy = ExecToolConfig(enabled=True, safe_bins=["ls"], ask="on_miss")
    tool = ShellTool(str(tmp_path), exec_policy=policy, network_policy="deny")

    denied = await tool.execute({"command": "python3 --version"})
    approved = await tool.execute(
        {"command": "python3 --version"},
        ToolExecutionContext(approved_actions=frozenset({"allowlist_miss"})),
    )

    assert not denied.success
    assert approved.success


@pytest.mark.asyncio
async def test_code_exec_blocks_network_when_network_is_denied(tmp_path) -> None:
    tool = CodeExecTool(
        str(tmp_path),
        allowed_languages=["python"],
        exec_policy=ExecToolConfig(enabled=True, security="full", ask="off"),
        network_policy="deny",
    )

    result = await tool.execute({"language": "python", "code": "import requests\nrequests.get('https://example.com')"})

    assert not result.success
    assert "network access is denied" in result.error.lower()


@pytest.mark.asyncio
async def test_local_executor_blocks_network_command_when_denied(tmp_path) -> None:
    executor = LocalExecutor(str(tmp_path), network_policy="deny")

    result = await executor.execute(ExecRequest(command="curl https://example.com", timeout=1))

    assert not result.success
    assert "Network access is denied" in result.stderr


@pytest.mark.asyncio
async def test_approval_manager_waits_for_decision() -> None:
    manager = ApprovalManager(require_approval=["exec"], default_policy="ask")
    req = manager.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")

    async def approve_later() -> None:
        await asyncio.sleep(0.01)
        manager.approve(req.id, decided_by="admin")

    task = asyncio.create_task(approve_later())
    decided = await manager.wait_for_decision(req.id, timeout_seconds=1)
    await task

    assert decided is not None
    assert decided.status == ApprovalStatus.APPROVED


@pytest.mark.asyncio
async def test_approval_manager_times_out_to_expired() -> None:
    # The real timeout path is the *only* producer of EXPIRED status: on timeout
    # the request must leave _pending, be marked EXPIRED, and land in history so
    # the command layer can later explain "expired, please re-trigger".
    manager = ApprovalManager(require_approval=["exec"], default_policy="ask")
    req = manager.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")

    decided = await manager.wait_for_decision(req.id, timeout_seconds=0)

    assert decided is not None
    assert decided.status == ApprovalStatus.EXPIRED
    assert decided.reason == "approval timed out"
    # No longer pending, but discoverable in history for the inactive-approval lookup.
    assert manager.get(req.id) is None
    assert manager._find_history(req.id) is decided


@pytest.mark.asyncio
async def test_approval_manager_wait_returns_history_when_already_decided() -> None:
    # If a request was decided before wait_for_decision is entered (not in _pending),
    # the wait must short-circuit to the historic record rather than block.
    manager = ApprovalManager(require_approval=["exec"], default_policy="ask")
    req = manager.request_approval("exec", tool_name="exec", params={"command": "ls"}, user_id="u1")
    manager.deny(req.id, reason="nope", decided_by="admin")

    decided = await manager.wait_for_decision(req.id, timeout_seconds=5)

    assert decided is not None
    assert decided.status == ApprovalStatus.DENIED
    assert decided.reason == "nope"


@pytest.mark.asyncio
async def test_approval_manager_wait_unknown_id_returns_none() -> None:
    manager = ApprovalManager(require_approval=["exec"], default_policy="ask")
    assert await manager.wait_for_decision("deadbeef00", timeout_seconds=5) is None
