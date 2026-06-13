"""CLI commands for managing skill dependencies.

Provides:
    echo-agent deps status   — show install status of all skill deps
    echo-agent deps install  — install deps for a specific skill
    echo-agent deps refresh  — update all previously installed deps
"""

from __future__ import annotations

import argparse
import sys


def _status() -> None:
    """Print a status table of all skill dependencies."""
    from echo_agent.dependencies.lazy_deps import check_all_features

    report = check_all_features()
    available = [f for f, info in report.items() if info["available"]]
    missing = [f for f, info in report.items() if not info["available"]]

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


def main(argv: list[str] | None = None) -> None:
    """Entry point for deps CLI."""
    parser = argparse.ArgumentParser(
        prog="echo-agent deps",
        description="Manage skill dependencies",
    )
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("status", help="Show dependency status for all skills")

    p_install = sub.add_parser("install", help="Install deps for a skill feature")
    p_install.add_argument(
        "feature", nargs="?", default=None,
        help="Feature name (e.g., skill.excel-author). Omit to install all.",
    )

    sub.add_parser("refresh", help="Update all previously installed deps")

    args = parser.parse_args(argv)

    if args.command == "status":
        _status()
    elif args.command == "install":
        if args.feature:
            _install(args.feature)
        else:
            _install_all()
    elif args.command == "refresh":
        _refresh()
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
