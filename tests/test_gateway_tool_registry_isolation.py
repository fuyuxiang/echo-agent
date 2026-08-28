"""P0 regression: the public_gateway profile must actually exclude high-risk
tools from the tool *registry* (not merely from is_tool_allowed). This guards
against the ordering bug where the profile was applied after tool registration."""

import asyncio

from echo_agent.agent.tools import discover_tools
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.security.tool_policy import PUBLIC_GATEWAY_DENY


def _build_registry(config: Config, tmp_path) -> ToolRegistry:
    """Mirror what AgentLoop does: discover (with policy filtering) then register."""
    reg = ToolRegistry(config=config)
    bus = MessageBus()
    for tool in discover_tools(config=config, workspace=tmp_path, bus=bus):
        reg.register(tool)
    return reg


def test_public_gateway_registry_excludes_high_risk_tools(tmp_path):
    config = Config(security={"profile": "public_gateway"})
    reg = _build_registry(config, tmp_path)
    names = set(reg.tool_names)
    leaked = names & set(PUBLIC_GATEWAY_DENY)
    assert not leaked, f"high-risk tools leaked into gateway registry: {sorted(leaked)}"


def test_personal_cli_registry_keeps_write_tools(tmp_path):
    # Sanity: the exclusion is profile-specific, not a blanket removal.
    config = Config(security={"profile": "personal_cli"})
    reg = _build_registry(config, tmp_path)
    assert "write_file" in reg.tool_names


def test_execute_time_gate_blocks_when_profile_tightened():
    """Defense-in-depth: even if a tool slips into the registry, execute() must
    refuse it once the profile is public_gateway."""

    class _FakeTool:
        name = "write_file"
        max_retries = 0

        def execution_mode(self, params):
            return "side_effect"

        def validate_params(self, params):
            return []

        async def run(self, params, ctx):  # pragma: no cover - should never run
            from echo_agent.tools import ToolResult
            return ToolResult(success=True, output="should not happen")

    config = Config(security={"profile": "public_gateway"})
    reg = ToolRegistry(config=config)
    reg.register(_FakeTool())  # force it in, bypassing policy filtering

    result = asyncio.run(reg.execute("write_file", {}))
    assert result.success is False
    assert "security profile" in (result.error or "")
