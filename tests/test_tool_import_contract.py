"""Contract tests for the public and backward-compatible Tool import paths."""

from pathlib import Path

from echo_agent import tools as public_tools
from echo_agent.agent.tools import base as legacy_tools
from echo_agent.tools import base as implementation_tools


_REPO_ROOT = Path(__file__).resolve().parent.parent
_PRIVATE_IMPORTS = tuple(
    f"from {module} import"
    for module in (
        "echo_agent.tools.base",
        "echo_agent.agent.tools.base",
    )
)
_PRIVATE_IMPORT_ALLOWLIST = {
    _REPO_ROOT / "echo_agent" / "tools" / "__init__.py",
    _REPO_ROOT / "echo_agent" / "agent" / "tools" / "base.py",
}
_PUBLIC_CONTRACT_NAMES = {
    "Tool",
    "ToolExecutionContext",
    "ToolResult",
    "build_idempotency_key",
}


def test_public_tool_contracts_are_the_implementation_objects():
    """The package facade must not wrap or duplicate the contract classes."""
    assert set(public_tools.__all__) == _PUBLIC_CONTRACT_NAMES
    for name in _PUBLIC_CONTRACT_NAMES:
        assert getattr(public_tools, name) is getattr(implementation_tools, name)


def test_legacy_tool_import_path_re_exports_public_contracts():
    """Existing plugins keep their class identity and isinstance semantics."""
    assert set(legacy_tools.__all__) == _PUBLIC_CONTRACT_NAMES
    for name in _PUBLIC_CONTRACT_NAMES:
        assert getattr(legacy_tools, name) is getattr(public_tools, name)


def test_repo_uses_the_public_tool_contract_import():
    """New product, test, plugin-fixture, and documentation imports use the facade."""
    offenders: list[str] = []
    search_roots = (
        (_REPO_ROOT / "echo_agent", "*.py"),
        (_REPO_ROOT / "tests", "*.py"),
        (_REPO_ROOT / "docs", "*.md"),
    )
    for root, pattern in search_roots:
        for path in root.rglob(pattern):
            if path in _PRIVATE_IMPORT_ALLOWLIST:
                continue
            text = path.read_text(encoding="utf-8")
            for private_import in _PRIVATE_IMPORTS:
                if private_import in text:
                    offenders.append(
                        f"{path.relative_to(_REPO_ROOT)}: {private_import}"
                    )

    assert not offenders, (
        "Tool contracts must be imported from echo_agent.tools; "
        f"private or legacy imports found: {offenders}"
    )
