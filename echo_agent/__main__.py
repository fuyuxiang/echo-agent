"""Echo Agent CLI entry point — argument parsing and command dispatch.

All bootstrap and lifecycle logic lives in :mod:`echo_agent.app`.
"""

from __future__ import annotations

import argparse
import asyncio

# Backward-compat re-exports: external code and older docs referenced these
# names on the script module before the composition root moved to app.py.
from echo_agent.app import (  # noqa: F401
    BootstrapResult as _BootstrapResult,
    bootstrap as _bootstrap,
    run as _run,
    run_gateway as _run_gateway,
)


def _run_eval(args) -> None:
    from pathlib import Path as _Path

    config_path = args.config or getattr(args, "top_config", None)
    workspace = args.workspace or getattr(args, "top_workspace", None)

    from echo_agent.config.loader import load_config, resolve_config_file
    config_file = resolve_config_file(config_path)
    config = load_config(config_path=config_file)

    dataset_path = args.dataset or config.evaluation.dataset_path
    path = _Path(dataset_path)
    if not path.exists():
        print(f"Dataset not found: {path}")
        print("Create a YAML file with test cases. Example:")
        print("  - id: test_001")
        print("    input: 'Hello'")
        print("    expected_contains: ['hello', 'hi']")
        return

    from echo_agent.evaluation import EvalRunner, EvalDataset

    dataset = EvalDataset.from_path(path)
    if args.tag:
        cases = dataset.filter_by_tag(args.tag)
        dataset = EvalDataset(cases)

    if not dataset.cases:
        print("No test cases found.")
        return

    async def run():
        from echo_agent.app import bootstrap

        overrides = {"workspace": workspace} if workspace else None
        ctx = await bootstrap(config_path=config_path, overrides=overrides)
        await ctx.bus.start()
        await ctx.agent.start()

        try:
            runner = EvalRunner(ctx.agent, parallel=args.parallel, timeout=config.evaluation.timeout_per_case)
            report = await runner.run_dataset(dataset)

            from echo_agent.evaluation.reporter import EvalReporter
            reporter = EvalReporter()
            print(reporter.to_table(report))

            if args.output:
                _Path(args.output).write_text(reporter.to_json(report), encoding="utf-8")
                print(f"\nResults saved to {args.output}")
        finally:
            from echo_agent.app import AppRuntime
            await AppRuntime._stop_step("agent", ctx.agent.stop())
            await AppRuntime._stop_step("bus", ctx.bus.stop())
            await AppRuntime._stop_step("storage", ctx.storage.close())

    asyncio.run(run())


def main() -> None:
    try:
        _dispatch()
    except Exception as e:
        from echo_agent.config.loader import ConfigError
        if isinstance(e, ConfigError):
            import sys
            print(f"配置错误 / Configuration error:\n{e}", file=sys.stderr)
            sys.exit(1)
        raise


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="echo-agent", description="Echo Agent — modular AI agent framework")
    subparsers = parser.add_subparsers(dest="command")

    # run
    run_parser = subparsers.add_parser("run", help="Start the agent")
    run_parser.add_argument("-c", "--config", help="Path to config file")
    run_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # setup
    setup_parser = subparsers.add_parser("setup", help="Run the setup wizard")
    setup_parser.add_argument(
        "section", nargs="?", default=None,
        help="Setup section: language, model, permissions, terminal, agent, tools, channel, gateway, observability, evolution, doctor",
    )
    setup_parser.add_argument("-c", "--config", help="Path to config file")
    setup_parser.add_argument("-w", "--workspace", help="Workspace directory")
    setup_parser.add_argument("--lang", choices=["en", "zh", "auto"], default=None,
                              help="Override interface language (default: auto-detect from OS)")
    setup_parser.add_argument("--flow", choices=["quickstart", "full"], default=None,
                              help="Skip the menu and run a specific flow")

    # status
    status_parser = subparsers.add_parser("status", help="Show current configuration status")
    status_parser.add_argument("-c", "--config", help="Path to config file")
    status_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # cost
    cost_parser = subparsers.add_parser("cost", help="Show cost attribution report")
    cost_parser.add_argument("-c", "--config", help="Path to config file")
    cost_parser.add_argument("-w", "--workspace", help="Workspace directory")
    cost_parser.add_argument("--days", type=int, default=7,
                             help="Trend window in days (default: 7)")

    # gateway
    gw_parser = subparsers.add_parser("gateway", help="Start the gateway server")
    gw_parser.add_argument("-c", "--config", help="Path to config file")
    gw_parser.add_argument("-w", "--workspace", help="Workspace directory")
    gw_parser.add_argument("--host", help="Gateway host")
    gw_parser.add_argument("--port", type=int, help="Gateway port")

    # eval
    eval_parser = subparsers.add_parser("eval", help="Run evaluation test suite")
    eval_parser.add_argument("--dataset", "-d", default="", help="Path to eval dataset (YAML/JSON)")
    eval_parser.add_argument("--tag", "-t", default="", help="Filter cases by tag")
    eval_parser.add_argument("--parallel", "-p", type=int, default=3, help="Parallel cases")
    eval_parser.add_argument("--output", "-o", default="", help="Output file for results")
    eval_parser.add_argument("-c", "--config", help="Path to config file")
    eval_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # service
    svc_parser = subparsers.add_parser("service", help="Manage systemd service (Linux)")
    svc_parser.add_argument("action", choices=["install", "uninstall", "start", "stop", "restart", "status", "logs"], help="Service action")
    svc_parser.add_argument("-w", "--workspace", help="Workspace directory (used by install)")

    # plugin
    plugin_parser = subparsers.add_parser("plugin", help="Manage plugins")
    plugin_parser.add_argument("action", choices=["list", "info", "enable", "disable", "check"], help="Plugin action")
    plugin_parser.add_argument("name", nargs="?", default="", help="Plugin name (for info/enable/disable)")
    plugin_parser.add_argument("-c", "--config", help="Path to config file")
    plugin_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # evolution
    evo_parser = subparsers.add_parser("evolution", help="Manage the self-evolving skill harness")
    evo_parser.add_argument(
        "action",
        choices=[
            "status", "run", "list-candidates", "show-candidate",
            "promote", "rollback", "init-dataset",
        ],
        help="Evolution action",
    )
    evo_parser.add_argument("target", nargs="?", default="", help="Skill name (rollback) or candidate id (show-candidate/promote)")
    evo_parser.add_argument("--status", dest="status_filter", default="", help="Filter list-candidates by status")
    evo_parser.add_argument("-c", "--config", help="Path to config file")
    evo_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # config
    config_parser = subparsers.add_parser("config", help="Inspect and validate configuration")
    config_parser.add_argument(
        "action",
        choices=["dump", "explain", "validate", "gen-docs"],
        help="Config action",
    )
    config_parser.add_argument("key", nargs="?", default="", help="Dotted config key (for explain)")
    config_parser.add_argument("--format", choices=["yaml", "json"], default="yaml",
                               help="Output format for dump (default: yaml)")
    config_parser.add_argument("-c", "--config", help="Path to config file")
    config_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # top-level flags for backward compat
    parser.add_argument("-c", "--config", help="Path to config file", dest="top_config")
    parser.add_argument("-w", "--workspace", help="Workspace directory", dest="top_workspace")

    return parser


def _dispatch() -> None:
    parser = _build_parser()
    args = parser.parse_args()

    if args.command == "setup":
        from echo_agent.cli.setup import run_setup_wizard
        lang_arg = getattr(args, "lang", None)
        if lang_arg == "auto":
            lang_arg = None
        run_setup_wizard(
            section=args.section,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            lang=lang_arg,
            flow=getattr(args, "flow", None),
        )
        return

    if args.command == "status":
        from echo_agent.cli.status import show_status
        show_status(config_path=args.config or args.top_config, workspace=args.workspace or args.top_workspace)
        return

    if args.command == "cost":
        from echo_agent.cli.cost import show_cost
        show_cost(
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            days=args.days,
        )
        return

    if args.command == "gateway":
        from echo_agent.app import run_gateway
        try:
            asyncio.run(run_gateway(config_path=args.config or args.top_config, host=args.host, port=args.port, workspace=args.workspace or args.top_workspace))
        except KeyboardInterrupt:
            pass
        return

    if args.command == "eval":
        _run_eval(args)
        return

    if args.command == "service":
        from echo_agent.cli.service import run_action
        run_action(args.action, workspace=args.workspace or args.top_workspace)
        return

    if args.command == "plugin":
        from echo_agent.cli.plugins_cmd import run_plugin_command
        run_plugin_command(
            action=args.action,
            name=args.name,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
        )
        return

    if args.command == "evolution":
        from echo_agent.cli.evolution_cmd import run_evolution_command
        target = getattr(args, "target", "") or ""
        skill = ""
        candidate_id = ""
        if args.action == "rollback":
            skill = target
        elif args.action in ("show-candidate", "promote"):
            candidate_id = target
        try:
            run_evolution_command(
                action=args.action,
                skill=skill,
                status_filter=getattr(args, "status_filter", "") or "",
                candidate_id=candidate_id,
                config_path=args.config or args.top_config,
                workspace=args.workspace or args.top_workspace,
            )
        except KeyboardInterrupt:
            pass
        return

    if args.command == "config":
        from echo_agent.cli.config_cmd import run_config_command
        import sys as _sys
        rc = run_config_command(
            action=args.action,
            key=getattr(args, "key", "") or "",
            fmt=getattr(args, "format", "yaml"),
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
        )
        _sys.exit(rc)

    # "run" command or no command (backward compat)
    config_path = getattr(args, "config", None) or args.top_config
    workspace = getattr(args, "workspace", None) or args.top_workspace

    from echo_agent.cli.setup import prompt_first_run_setup
    if prompt_first_run_setup(config_path=config_path, workspace=workspace):
        return

    from echo_agent.app import run
    try:
        asyncio.run(run(config_path=config_path, workspace=workspace))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
