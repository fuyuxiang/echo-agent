"""Verify deployable resources and reject development caches in release archives."""

from __future__ import annotations

import argparse
from pathlib import Path, PurePosixPath
import tarfile
import tomllib
import zipfile


def verify(root: Path) -> None:
    version = tomllib.loads((root / "pyproject.toml").read_text())["project"]["version"]
    artifacts = [
        root / "dist" / f"echo_agent-{version}-py3-none-any.whl",
        root / "dist" / f"echo_agent-{version}.tar.gz",
        root / "dist" / f"echo-agent-runtime-{version}.tar.gz",
    ]
    common = {
        "echo_agent/config/default.yaml", "echo_agent/gateway/static/index.html",
        "echo_agent/_vendor/fastembed/__init__.py", "echo_agent/_vendor/fastembed/LICENSE",
        "echo_agent/_vendor/fastembed/NOTICE", "echo_agent/_vendor/fastembed/VENDOR.md",
        "echo_agent/_vendor/fastembed/py.typed",
        "echo_agent/_vendor/fastembed/text/text_embedding.py",
        "echo_agent/_vendor/fastembed/rerank/cross_encoder/text_cross_encoder.py",
    }
    source_files = {
        "README.md", "README.en.md", "LICENSE", "pyproject.toml", "hatch_build.py",
        "web/package.json", "web/pnpm-lock.yaml", "web/pnpm-workspace.yaml",
        "web/src/main.tsx", "web/vite.config.ts", "scripts/install.sh", "scripts/build.sh",
        "deploy/echo-agent.yaml.example", "deploy/Dockerfile", "data/eval/baseline.yaml",
    }
    skill_files = {
        "skills/productivity/calendar/scripts/calendar_client.py",
        "skills/research/web-search/SKILL.md",
        "skills/research/web-search/scripts/web_search.py",
        "skills/research/web-extract/SKILL.md",
        "skills/research/web-extract/scripts/extract_url.py",
        "skills/development/code-runner/SKILL.md",
        "skills/development/code-runner/scripts/safe_exec.py",
        "skills/research/deep-research/scripts/research_report.py",
    }
    for artifact in artifacts:
        wheel = artifact.suffix == ".whl"
        runtime_archive = artifact.name.startswith("echo-agent-runtime-")
        if wheel:
            with zipfile.ZipFile(artifact) as archive:
                names = set(archive.namelist())
        else:
            with tarfile.open(artifact) as archive:
                names = {name.split("/", 1)[-1] for name in archive.getnames()}
        dashboard = "web/dist/" if runtime_archive else "echo_agent/_bundled/dashboard/"
        required = common | {dashboard + "index.html", "echo_agent/skills/web_search.py"}
        required |= {"echo_agent/_bundled/" + skill if wheel else skill for skill in skill_files}
        if not wheel:
            required |= source_files
        forbidden = {"node_modules", ".git", ".venv", "venv", "__pycache__", ".env", "client", "docs"}
        stray = sorted(n for n in names if forbidden.intersection(PurePosixPath(n).parts) or n.endswith(".pyc"))
        assets = [n for n in names if n.startswith(dashboard + "assets/") and n.endswith(".js")]
        missing = sorted(required - names)
        if missing or stray or not assets:
            raise RuntimeError(f"{artifact.name}: missing={missing}, unwanted={stray[:5]}, JS assets={len(assets)}")
        print(f"Verified {artifact.name}: Dashboard, skills, config, vendor licenses" + (", full source" if not wheel else ""))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    verify(parser.parse_args().root)
