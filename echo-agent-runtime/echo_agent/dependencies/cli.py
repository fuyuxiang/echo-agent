"""CLI commands for managing skill dependencies.

Provides:
    echo-agent deps status   — show install status of all skill deps
    echo-agent deps install  — install deps for a specific skill
    echo-agent deps refresh  — update all previously installed deps
"""

from __future__ import annotations

import argparse
import json
import sys


def _status(as_json: bool = False) -> int:
    """Print a status table of all skill dependencies.

    Returns a process exit code: 0 when every feature is ready, 1 when any
    feature is missing dependencies (so scripts/CI can gate on it). ``as_json``
    emits a structured document with color forced off.
    """
    from echo_agent.cli.colors import set_color_override
    from echo_agent.dependencies.lazy_deps import check_all_features

    report = check_all_features()
    available = [f for f, info in report.items() if info["available"]]
    missing = [f for f, info in report.items() if not info["available"]]

    if as_json:
        set_color_override(False)
        try:
            payload = {
                "total": len(report),
                "ready": sorted(available),
                "missing": [
                    {
                        "feature": f,
                        "packages": [str(p) for p in report[f]["missing"]],  # type: ignore[union-attr]
                        "command": report[f]["command"],
                    }
                    for f in sorted(missing)
                ],
            }
            print(json.dumps(payload, ensure_ascii=False, indent=2))
        finally:
            set_color_override(None)
        return 1 if missing else 0

    print(f"\n{'='*60}")
    print(" Echo Agent Skill Dependencies Status")
    print(f"{'='*60}\n")

    if available:
        print(f" Ready ({len(available)}):")
        for f in sorted(available):
            print(f"   [OK] {f}")

    if missing:
        print(f"\n Missing ({len(missing)}):")
        for f in sorted(missing):
            info = report[f]
            pkgs = ", ".join(str(p) for p in info["missing"])  # type: ignore[arg-type]
            print(f"   [  ] {f}")
            print(f"        Needs: {pkgs}")
            if info["command"]:
                print(f"        Run:   {info['command']}")

    print(f"\n{'='*60}")
    print(f" Total: {len(report)} features, {len(available)} ready, {len(missing)} missing")
    print(f"{'='*60}\n")
    return 1 if missing else 0


def _install(feature: str) -> None:
    """Install dependencies for a specific feature."""
    from echo_agent.dependencies.lazy_deps import (
        SKILL_DEPS,
        FeatureUnavailable,
        ensure,
        is_available,
    )

    if feature not in SKILL_DEPS:
        print(f"Unknown feature: {feature!r}")
        print(f"Available: {', '.join(sorted(SKILL_DEPS.keys()))}")
        sys.exit(1)

    if is_available(feature):
        print(f"Feature {feature!r} is already satisfied.")
        return

    try:
        ensure(feature, prompt=True)
        print(f"\nFeature {feature!r} installed successfully.")
    except FeatureUnavailable as e:
        print(f"\nFailed: {e}", file=sys.stderr)
        sys.exit(1)


def _install_all() -> None:
    """Install dependencies for ALL features that have non-empty specs."""
    from echo_agent.dependencies.lazy_deps import (
        SKILL_DEPS,
        FeatureUnavailable,
        ensure,
        is_available,
    )

    to_install = [
        f for f, specs in SKILL_DEPS.items()
        if specs and not is_available(f)
    ]

    if not to_install:
        print("All features already satisfied.")
        return

    print(f"Will install dependencies for {len(to_install)} features:")
    for f in to_install:
        print(f"  - {f}")

    try:
        answer = input("\nProceed? [Y/n] ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        answer = "n"
    if answer and answer not in {"y", "yes", ""}:
        print("Cancelled.")
        return

    success = 0
    failed = 0
    for f in to_install:
        try:
            ensure(f, prompt=False)
            print(f"  [OK] {f}")
            success += 1
        except FeatureUnavailable as e:
            print(f"  [FAIL] {f}: {e.reason}")
            failed += 1

    print(f"\nDone: {success} installed, {failed} failed.")


def _refresh() -> None:
    """Refresh all previously activated features to match current specs."""
    from echo_agent.dependencies.lazy_deps import refresh_active_features

    print("Refreshing previously installed features...")
    results = refresh_active_features(prompt=False)

    if not results:
        print("No features have been previously activated.")
        return

    for feature, status in sorted(results.items()):
        symbol = {"current": "OK", "refreshed": "UP"}.get(status, "!!")
        print(f"  [{symbol}] {feature}: {status}")


def main(argv: list[str] | None = None) -> int:
    """Entry point for deps CLI. Returns a process exit code."""
    parser = argparse.ArgumentParser(
        prog="echo-agent deps",
        description="Manage skill dependencies",
    )
    # Accepted at both the top level and per-subcommand so `deps --json status`
    # and `deps status --json` both work (argparse.REMAINDER passes the tail
    # verbatim from the outer parser).
    parser.add_argument("--json", action="store_true", dest="json",
                        help="Emit machine-readable JSON (status only)")
    sub = parser.add_subparsers(dest="command")

    p_status = sub.add_parser("status", help="Show dependency status for all skills")
    p_status.add_argument("--json", action="store_true", dest="json",
                          help="Emit machine-readable JSON")

    p_install = sub.add_parser("install", help="Install deps for a skill feature")
    p_install.add_argument(
        "feature", nargs="?", default=None,
        help="Feature name (e.g., skill.excel-author). Omit to install all.",
    )

    sub.add_parser("refresh", help="Update all previously installed deps")

    args = parser.parse_args(argv)
    as_json = getattr(args, "json", False)

    if args.command == "status":
        return _status(as_json=as_json)
    elif args.command == "install":
        if args.feature:
            _install(args.feature)
        else:
            _install_all()
        return 0
    elif args.command == "refresh":
        _refresh()
        return 0
    else:
        parser.print_help()
        return 0


if __name__ == "__main__":
    sys.exit(main())
