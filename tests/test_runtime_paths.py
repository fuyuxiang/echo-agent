"""Tests for echo_agent.runtime_paths — global path helpers."""

from pathlib import Path
from unittest.mock import patch

from echo_agent.runtime_paths import bundled_skills_dir, default_config_path, echo_home


class TestEchoHome:
    def test_returns_dot_echo_agent_under_home(self):
        result = echo_home()
        assert result == Path.home() / ".echo-agent"


class TestDefaultConfigPath:
    def test_returns_yaml_under_echo_home(self):
        result = default_config_path()
        assert result == Path.home() / ".echo-agent" / "echo-agent.yaml"


class TestBundledSkillsDir:
    def test_returns_path_when_exists(self, tmp_path: Path):
        # Create a fake package directory with a _bundled/skills subdir
        skills_dir = tmp_path / "_bundled" / "skills"
        skills_dir.mkdir(parents=True)

        fake_module_file = str(tmp_path / "runtime_paths.py")
        with patch("echo_agent.runtime_paths.__file__", fake_module_file):
            result = bundled_skills_dir()
            assert result is not None
            assert result == skills_dir

    def test_returns_none_when_no_directory_exists(self, tmp_path: Path):
        # Use a directory with no candidate subdirs
        fake_module_file = str(tmp_path / "runtime_paths.py")
        with patch("echo_agent.runtime_paths.__file__", fake_module_file):
            result = bundled_skills_dir()
            assert result is None
