#!/usr/bin/env python3
"""Publish signed desktop updates through one atomic manifest-generation switch."""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import uuid
from urllib.parse import quote

SEMVER = re.compile(
    r"^(?P<major>0|[1-9]\d*)\."
    r"(?P<minor>0|[1-9]\d*)\."
    r"(?P<patch>0|[1-9]\d*)"
    r"(?:-(?P<pre>[0-9A-Za-z.-]+))?"
    r"(?:\+(?P<build>[0-9A-Za-z.-]+))?$"
)
TARGETS = {
    "windows-x86_64": ("-setup.exe",),
    "darwin-x86_64": (".app.tar.gz",),
    "darwin-aarch64": (".app.tar.gz",),
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", choices=sorted(TARGETS))
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--signature", type=Path)
    parser.add_argument("--batch-dir", type=Path)
    parser.add_argument("--notes-file", type=Path)
    parser.add_argument("--mandatory", action="store_true")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path("/opt/echo-agent-desktop-updates"),
    )
    parser.add_argument(
        "--base-url",
        default="https://123.56.188.16:8787/desktop-updates",
    )
    return parser.parse_args()


def semver_key(value: str) -> tuple[int, int, int, tuple[tuple[int, object], ...]]:
    match = SEMVER.fullmatch(value)
    if not match:
        raise ValueError(f"invalid SemVer: {value}")
    pre = match.group("pre")
    build = match.group("build")
    if build is not None and any(not item for item in build.split(".")):
        raise ValueError(f"invalid SemVer: {value}")
    # Stable releases sort after any prerelease of the same core version.
    if pre is None:
        pre_key: tuple[tuple[int, object], ...] = ((2, ""),)
    else:
        identifiers = []
        for item in pre.split("."):
            if not item or (item.isdigit() and len(item) > 1 and item.startswith("0")):
                raise ValueError(f"invalid SemVer: {value}")
            identifiers.append((0, int(item)) if item.isdigit() else (1, item))
        pre_key = tuple(identifiers)
    return (
        int(match.group("major")),
        int(match.group("minor")),
        int(match.group("patch")),
        pre_key,
    )


def atomic_copy(
    source: Path, destination: Path, expected_sha256: str | None = None
) -> str:
    temporary = destination.with_name(f".{destination.name}.{os.getpid()}.tmp")
    digest = hashlib.sha256()
    with source.open("rb") as reader, temporary.open("wb") as writer:
        while chunk := reader.read(1024 * 1024):
            writer.write(chunk)
            digest.update(chunk)
        writer.flush()
        os.fsync(writer.fileno())
    copied_sha256 = digest.hexdigest()
    if expected_sha256 is not None and copied_sha256 != expected_sha256:
        temporary.unlink(missing_ok=True)
        raise ValueError("source changed while it was being copied")
    os.chmod(temporary, 0o644)
    os.replace(temporary, destination)
    return copied_sha256


def atomic_json(payload: dict[str, object], destination: Path) -> None:
    temporary = destination.with_name(f".{destination.name}.{os.getpid()}.tmp")
    with temporary.open("w", encoding="utf-8") as writer:
        json.dump(payload, writer, ensure_ascii=False, indent=2)
        writer.write("\n")
        writer.flush()
        os.fsync(writer.fileno())
    os.chmod(temporary, 0o644)
    os.replace(temporary, destination)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as reader:
        while chunk := reader.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def artifact_name(version: str, target: str) -> str:
    suffix = "-setup.exe" if target == "windows-x86_64" else ".app.tar.gz"
    return f"EchoAgent-v{version}-{target}{suffix}"


def read_signature(path: Path) -> str:
    signature = path.read_text(encoding="utf-8").strip()
    if len(signature) < 80 or any(char.isspace() for char in signature):
        raise ValueError(f"invalid Tauri updater signature: {path}")
    return signature


def current_manifests(stable: Path) -> dict[str, dict[str, object]]:
    current = stable / "current"
    if not current.is_symlink():
        raise ValueError("stable/current is missing; install the generation-aware server first")
    generations = (stable / "generations").resolve()
    resolved = current.resolve(strict=True)
    if resolved.parent != generations:
        raise ValueError("stable/current points outside the managed generations directory")
    manifests = {}
    for target in TARGETS:
        path = resolved / f"{target}.json"
        if path.exists():
            manifests[target] = json.loads(path.read_text(encoding="utf-8"))
    return manifests


def publish_generation(stable: Path, manifests: dict[str, dict[str, object]]) -> None:
    generations = stable / "generations"
    generations.mkdir(mode=0o755, parents=True, exist_ok=True)
    generation = Path(tempfile.mkdtemp(prefix="release-", dir=generations))
    os.chmod(generation, 0o755)
    for target, manifest in manifests.items():
        atomic_json(manifest, generation / f"{target}.json")
    temporary_link = stable / f".current.{uuid.uuid4().hex}.tmp"
    try:
        os.symlink(f"generations/{generation.name}", temporary_link)
        os.replace(temporary_link, stable / "current")
    finally:
        temporary_link.unlink(missing_ok=True)


def requested_artifacts(args: argparse.Namespace, version: str) -> dict[str, tuple[Path, Path]]:
    if args.batch_dir:
        if args.target or args.artifact or args.signature:
            raise ValueError("--batch-dir cannot be combined with single-target arguments")
        root = args.batch_dir.resolve()
        if not root.is_dir():
            raise ValueError(f"batch directory does not exist: {root}")
        return {
            target: (root / artifact_name(version, target), root / f"{artifact_name(version, target)}.sig")
            for target in TARGETS
        }
    if not args.target or not args.artifact or not args.signature:
        raise ValueError("provide --batch-dir or --target, --artifact and --signature")
    artifact = args.artifact.resolve()
    signature = args.signature.resolve()
    if artifact.name != artifact_name(version, args.target):
        raise ValueError(f"artifact name does not match version/target: {artifact.name}")
    if signature.name != f"{artifact.name}.sig":
        raise ValueError("signature filename must be <artifact>.sig")
    return {args.target: (artifact, signature)}


def main() -> int:
    args = parse_args()
    version = args.version.removeprefix("v")
    try:
        incoming_key = semver_key(version)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2

    try:
        requested = requested_artifacts(args, version)
        signatures = {}
        signature_hashes = {}
        for artifact, signature_path in requested.values():
            if not artifact.is_file() or not signature_path.is_file():
                raise ValueError(f"artifact and signature must both be regular files: {artifact}")
        for target, (_, signature_path) in requested.items():
            signatures[target] = read_signature(signature_path)
            signature_hashes[target] = sha256_file(signature_path)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2

    notes = ""
    if args.notes_file:
        notes = args.notes_file.read_text(encoding="utf-8").strip()
    incoming_hashes = {target: sha256_file(artifact) for target, (artifact, _) in requested.items()}

    root = args.root.resolve()
    stable = root / "stable"
    version_dir = root / "releases" / version
    stable.mkdir(parents=True, exist_ok=True)

    lock_path = stable / ".publish.lock"
    with lock_path.open("a", encoding="utf-8") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        try:
            manifests = current_manifests(stable)
            for target, (artifact, signature_path) in requested.items():
                existing = manifests.get(target)
                existing_version = str(existing.get("version", "")) if existing else ""
                incoming_sha256 = incoming_hashes[target]
                if existing_version == version:
                    if str(existing.get("sha256", "")) != incoming_sha256:
                        raise ValueError("refusing to replace an existing version with different bytes")
                    if str(existing.get("signature", "")) != signatures[target]:
                        raise ValueError("refusing to replace an existing version with a different signature")
                elif existing_version and incoming_key <= semver_key(existing_version):
                    raise ValueError(f"refusing non-forward publish: {version} <= {existing_version}")
                destination = version_dir / artifact.name
                if destination.exists() and sha256_file(destination) != incoming_sha256:
                    raise ValueError(f"refusing to replace immutable release bytes: {destination}")
                destination_signature = version_dir / signature_path.name
                if destination_signature.exists() and sha256_file(destination_signature) != signature_hashes[target]:
                    raise ValueError(f"refusing to replace immutable release signature: {destination_signature}")
        except (ValueError, OSError) as error:
            print(error, file=sys.stderr)
            return 2

        version_dir.mkdir(mode=0o755, parents=True, exist_ok=True)
        try:
            for target, (artifact, signature_path) in requested.items():
                sha256 = atomic_copy(artifact, version_dir / artifact.name, incoming_hashes[target])
                atomic_copy(
                    signature_path,
                    version_dir / f"{artifact.name}.sig",
                    signature_hashes[target],
                )
                checksum = version_dir / f"{artifact.name}.sha256"
                checksum.write_text(f"{sha256}  {artifact.name}\n", encoding="utf-8")
                os.chmod(checksum, 0o644)
                existing = manifests.get(target)
                target_notes = notes or (
                    str(existing.get("notes", "")) if existing and existing.get("version") == version else ""
                )
                manifests[target] = {
                    "version": version,
                    "notes": target_notes or f"EchoAgent {version}",
                    "pub_date": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
                    "mandatory": bool(args.mandatory),
                    "url": f"{args.base_url.rstrip('/')}/releases/{quote(version)}/{quote(artifact.name)}",
                    "signature": signatures[target],
                    "sha256": sha256,
                }
            publish_generation(stable, manifests)
        except (OSError, ValueError) as error:
            print(f"publication failed before manifest switch: {error}", file=sys.stderr)
            return 2

    print(f"published EchoAgent {version} for {', '.join(requested)}")
    print(f"manifests: {stable / 'current'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
