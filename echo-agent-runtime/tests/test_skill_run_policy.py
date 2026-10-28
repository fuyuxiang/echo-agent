"""Regression tests for skill_run as a governed exec path.

skill_run declared risk_level="exec" but carried no capabilities and appeared in
none of the deny lists, so it was exposed under daemon and public_gateway after
those profiles had carefully refused exec/execute_code/process — and one of the
builtin skills shipped a general-purpose shell runner reachable through it.
tools.exec.enabled=false did not stop it either, and a single "approve always"
covered every script of every skill.

These tests pin each of those holes shut.
"""

from __future__ import annotations

import pytest

from echo_agent.config.schema import Config
from echo_agent.permissions.allowlist import build_pattern_key
from echo_agent.security import guards
from echo_agent.security.capabilities import tool_capabilities
from echo_agent.security.risk_classifier import RiskLevel, classify_risk
from echo_agent.security.tool_policy import (
    DAEMON_DENY_BY_DEFAULT,
    HIGH_RISK_TOOLS,
    is_tool_allowed,
)


def _config(security_profile: str, tools_profile: str = "full") -> Config:
    c = Config()
    c.security.profile = security_profile
    c.tools.profile = tools_profile
    return c


class TestCapabilities:
    def test_skill_run_declares_exec_capabilities(self):
        caps = tool_capabilities("skill_run")
        assert "process.exec" in caps
        # It runs Python source, exactly like execute_code.
        assert "code.exec" in caps

    def test_tool_class_and_table_agree(self):
        """is_tool_allowed is called with both a name and an object; the two
        sources must not disagree or policy would depend on the call site."""
        from echo_agent.agent.tools.skill_run import SkillRunTool

        assert frozenset(SkillRunTool.capabilities) == tool_capabilities("skill_run")

    def test_listed_as_high_risk(self):
        assert "skill_run" in HIGH_RISK_TOOLS
        assert "skill_run" in DAEMON_DENY_BY_DEFAULT


class TestProfileExposure:
    @pytest.mark.parametrize("profile", ["daemon", "public_gateway"])
    def test_denied_where_exec_is_denied(self, profile):
        """The profiles that refuse exec must refuse skill_run for the same reason."""
        c = _config(profile)
        assert is_tool_allowed(c, "exec") is False
        assert is_tool_allowed(c, "skill_run") is False

    def test_allowed_on_personal_cli(self):
        """Not a blanket ban: the trusted single-user CLI still runs skills."""
        c = _config("personal_cli")
        assert is_tool_allowed(c, "skill_run") is True

    def test_explicit_also_allow_overrides(self):
        """An operator can still opt a daemon deployment back in, deliberately."""
        c = _config("daemon")
        c.tools.also_allow = ["skill_run"]
        assert is_tool_allowed(c, "skill_run") is True

    def test_read_only_skill_tools_survive(self):
        """Reading skills is not running them — public_gateway keeps discovery."""
        c = _config("public_gateway")
        assert is_tool_allowed(c, "skills_list") is True
        assert is_tool_allowed(c, "skill_view") is True


class TestRiskClassification:
    def test_classified_exec(self):
        assert classify_risk("skill_run") is RiskLevel.EXEC

    def test_declaration_and_map_agree(self):
        from echo_agent.agent.tools.skill_run import SkillRunTool

        assert classify_risk("skill_run", tool_risk_level=SkillRunTool.risk_level) is RiskLevel.EXEC


class TestExecKillSwitch:
    def test_disabled_exec_blocks_skill_run(self):
        """tools.exec.enabled=false must stop skill scripts too.

        This was the sharpest form of the bypass: an operator who had turned
        exec off entirely still had a working code-execution path.
        """
        c = Config()
        c.tools.exec.enabled = False
        decision = guards.evaluate_tool_call(
            c, "skill_run",
            {"name": "workflow-chain", "script": "scripts/workflow_engine.py",
             "args": ["inline", "id"]},
        )
        assert decision.action == "deny"
        assert "disabled" in decision.reason

    def test_enabled_exec_allows_skill_run(self):
        c = Config()
        c.tools.exec.enabled = True
        decision = guards.evaluate_tool_call(
            c, "skill_run", {"name": "calculator", "script": "scripts/calc.py"},
        )
        assert decision.action == "allow"

    def test_shares_exec_approval_action(self):
        """Approval bookkeeping should treat it as exec, not as its own class."""
        c = Config()
        c.tools.exec.enabled = True
        decision = guards.evaluate_tool_call(
            c, "skill_run", {"name": "s", "script": "scripts/x.py"},
        )
        assert decision.approval_action == "exec"

    def test_args_are_not_scanned_as_shell(self):
        """argv is not a shell string: a metacharacter in args is data.

        Scanning it would flag legitimate arguments (--query "rm old files") and
        train users to wave prompts through, which costs more than it buys.
        """
        c = Config()
        c.tools.exec.enabled = True
        decision = guards.evaluate_tool_call(
            c, "skill_run",
            {"name": "s", "script": "scripts/x.py", "args": ["--query", "rm -rf everything"]},
        )
        assert decision.action == "allow"


class TestPatternKey:
    def test_scoped_to_skill_and_script(self):
        """"Approve always" must not become a permit for every skill script."""
        key = build_pattern_key(
            "skill_run",
            {"name": "web-search", "script": "scripts/web_search.py", "args": ["x"]},
        )
        assert key == "skill_run:web-search/scripts/web_search.py"

    def test_distinct_scripts_get_distinct_keys(self):
        a = build_pattern_key("skill_run", {"name": "s", "script": "scripts/a.py"})
        b = build_pattern_key("skill_run", {"name": "s", "script": "scripts/b.py"})
        assert a != b

    def test_args_do_not_affect_key(self):
        """Granularity stops at the script: per-argument keys would never hit."""
        a = build_pattern_key("skill_run", {"name": "s", "script": "x.py", "args": ["1"]})
        b = build_pattern_key("skill_run", {"name": "s", "script": "x.py", "args": ["2"]})
        assert a == b

    def test_missing_fields_do_not_crash(self):
        assert build_pattern_key("skill_run", {}) == "skill_run:unknown/unknown"
