"""Tests for security capability mapping and risk classification."""


from echo_agent.security.capabilities import (
    tool_capabilities,
    tool_name,
)
from echo_agent.security.risk_classifier import RiskLevel, classify_risk


# ── tool_name ───────────────────────────────────────────────────────────────


class TestToolName:
    def test_string_passthrough(self):
        assert tool_name("read_file") == "read_file"

    def test_object_with_name_attr(self):
        class FakeTool:
            name = "write_file"

        assert tool_name(FakeTool()) == "write_file"


# ── tool_capabilities ───────────────────────────────────────────────────────


class TestToolCapabilities:
    def test_known_tool_returns_correct_frozenset(self):
        caps = tool_capabilities("read_file")
        assert caps == frozenset({"fs.read"})

    def test_unknown_tool_returns_empty(self):
        caps = tool_capabilities("totally_unknown_tool")
        assert caps == frozenset()

    def test_mcp_prefix_tool(self):
        caps = tool_capabilities("mcp_weather_fetch")
        assert caps == frozenset({"mcp.call"})

    def test_object_with_capabilities_attr(self):
        class FakeTool:
            name = "custom"
            capabilities = ["custom.read", "custom.write"]

        caps = tool_capabilities(FakeTool())
        assert caps == frozenset({"custom.read", "custom.write"})


# ── classify_risk ───────────────────────────────────────────────────────────


class TestClassifyRisk:
    def test_known_tool_read_file(self):
        assert classify_risk("read_file") == RiskLevel.READ_ONLY

    def test_known_tool_exec(self):
        assert classify_risk("exec") == RiskLevel.EXEC

    def test_unknown_tool_with_tool_risk_level(self):
        result = classify_risk("my_custom_tool", tool_risk_level="dangerous")
        assert result == RiskLevel.DANGEROUS

    def test_unknown_tool_defaults_to_write(self):
        result = classify_risk("completely_unknown_tool")
        assert result == RiskLevel.WRITE
