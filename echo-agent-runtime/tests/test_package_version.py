"""The public version follows the authoritative source/build metadata."""

from importlib.metadata import PackageNotFoundError, version
from pathlib import Path
import tomllib

import echo_agent


def test_source_checkout_uses_pyproject_even_with_stale_editable_metadata() -> None:
    pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
    expected = tomllib.loads(pyproject.read_text(encoding="utf-8"))["project"]["version"]
    assert echo_agent.__version__ == expected


def test_non_source_install_falls_back_to_distribution_metadata(
    monkeypatch,
) -> None:
    monkeypatch.setattr(echo_agent, "_source_tree_version", lambda: "")
    expected = "0.0.0+unknown"
    try:
        expected = version("echo-agent")
    except PackageNotFoundError:
        pass
    assert echo_agent._resolve_version() == expected


def test_source_version_reader_rejects_unrelated_or_invalid_pyproject(tmp_path) -> None:
    unrelated = tmp_path / "pyproject.toml"
    unrelated.write_text('[project]\nname = "another-package"\nversion = "9.9.9"\n')
    assert echo_agent._source_tree_version(unrelated) == ""
    unrelated.write_text("not = [valid")
    assert echo_agent._source_tree_version(unrelated) == ""
