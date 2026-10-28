"""Reading bundled skill CLI help must never install a package."""

import runpy
import sys
from pathlib import Path

import pytest


_SCRIPTS = [
    "research/deep-research/scripts/research_report.py",
    "research/web-search/scripts/web_search.py",
    "research/web-extract/scripts/extract_url.py",
    "research/rss-watcher/scripts/feed_monitor.py",
    "devops/system-monitor/scripts/system_check.py",
    "creative/excel-author/scripts/create_xlsx.py",
    "creative/ppt-author/scripts/create_pptx.py",
    "creative/meme-gen/scripts/make_meme.py",
    "media/voice-note/scripts/voice_process.py",
    "media/tts-voice/scripts/text_to_speech.py",
]


@pytest.mark.parametrize("relative", _SCRIPTS)
def test_skill_help_never_requests_dependency_install(relative, monkeypatch, capsys):
    import echo_agent.dependencies
    import echo_agent.dependencies.skill_require

    def forbidden(*args, **kwargs):
        raise AssertionError("--help must not request a package installation")

    monkeypatch.setattr(echo_agent.dependencies, "require", forbidden)
    monkeypatch.setattr(echo_agent.dependencies.skill_require, "require", forbidden)
    path = Path(__file__).resolve().parents[1] / "skills" / relative
    monkeypatch.setattr(sys, "argv", [str(path), "--help"])
    with pytest.raises(SystemExit) as stopped:
        runpy.run_path(str(path), run_name="__main__")
    assert stopped.value.code == 0
    assert "usage:" in capsys.readouterr().out
