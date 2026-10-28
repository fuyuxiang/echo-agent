"""Repack the full sdist as a standalone Runtime tree with a prebuilt Dashboard."""

from __future__ import annotations

import argparse
from pathlib import Path, PurePosixPath
import tarfile
import tomllib


def build_archive(root: Path) -> Path:
    version = tomllib.loads((root / "pyproject.toml").read_text())["project"]["version"]
    source = root / "dist" / f"echo_agent-{version}.tar.gz"
    if not source.is_file():
        raise RuntimeError(f"Build the sdist first: {source}")
    output = root / "dist" / f"echo-agent-runtime-{version}.tar.gz"
    dashboard = "echo_agent/_bundled/dashboard/"
    with tarfile.open(source) as src, tarfile.open(output, "w:gz") as dst:
        for member in src:
            parts = PurePosixPath(member.name).parts
            if len(parts) < 2:
                continue
            relative = PurePosixPath(*parts[1:])
            if relative.is_absolute() or ".." in relative.parts or not (member.isfile() or member.isdir()):
                raise RuntimeError(f"Unsafe source archive entry: {member.name}")
            name = relative.as_posix()
            if name.startswith(dashboard):
                name = "web/dist/" + name[len(dashboard):]
            data = src.extractfile(member) if member.isfile() else None
            member.name = "echo-agent-runtime/" + name
            member.uid = member.gid = 0
            member.uname = member.gname = ""
            dst.addfile(member, data)
    print(output)
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    build_archive(parser.parse_args().root)
