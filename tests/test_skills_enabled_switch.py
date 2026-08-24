"""Tests for the skills.enabled master switch.

The field existed in the schema but nothing read it: setting it false left all
five skill tools registered and the skill list still injected into the system
prompt. The switch is now honored the same way memory.enabled and
knowledge.enabled are — by not constructing the store at all, which the
downstream `if skill_store:` guards already understand.
"""

from __future__ import annotations

from unittest.mock import MagicMock

from pathlib import Path
from unittest.mock import MagicMock as _MM

from echo_agent.agent.context import build_skills_context
from echo_agent.agent.tools import discover_tools
from echo_agent.config.schema import Config


_SKILL_TOOLS = {"skills_list", "skill_view", "skill_manage", "skill_install", "skill_run"}
# discover_tools applies the exposure policy on the way out, and the default
# profile filters skill_install (dangerous + skill.install capability). So the
# "on" assertion checks the tools that survive policy, while the "off" assertion
# checks that none of the five appear at all.
_SKILL_TOOLS_AFTER_DEFAULT_POLICY = _SKILL_TOOLS - {"skill_install"}


class TestSchemaWiring:
    def test_enabled_by_default(self):
        assert Config().skills.enabled is True


class TestToolRegistration:
    def _tool_names(self, skill_store, tmp_path):
        tools = discover_tools(
            config=Config(),
            workspace=Path(tmp_path),
            bus=_MM(),
            skill_store=skill_store,
        )
        return {t.name for t in tools}

    def test_skill_tools_registered_when_store_present(self, tmp_path):
        names = self._tool_names(MagicMock(), tmp_path)
        missing = _SKILL_TOOLS_AFTER_DEFAULT_POLICY - names
        assert not missing, f"missing: {missing}"

    def test_no_skill_tools_when_store_absent(self, tmp_path):
        """skill_store=None is the "skills off" signal discover_tools reads."""
        names = self._tool_names(None, tmp_path)
        assert not (_SKILL_TOOLS & names)


class TestContextInjection:
    def test_no_skills_section_when_disabled(self):
        """A disabled subsystem must not keep spending prompt budget."""
        assert build_skills_context(None) == ""

    def test_skills_section_present_when_enabled(self):
        store = MagicMock()
        meta = MagicMock()
        meta.name = "demo"
        meta.category = "utility"
        meta.description = "does things"
        store.list_all.return_value = [meta]
        out = build_skills_context(store)
        assert "demo" in out
        assert "does things" in out
