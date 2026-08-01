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


def _run_eval(args) -> int:
    """Run the eval suite. Returns a process exit code: 0 only when the suite
    ran and every case passed; non-zero on missing dataset, empty selection,
    bad arguments, or any case failure (so CI can gate on it)."""
    import sys as _sys
    from pathlib import Path as _Path

    config_path = args.config or getattr(args, "top_config", None)
    workspace = args.workspace or getattr(args, "top_workspace", None)

    # --parallel 0 would build asyncio.Semaphore(0) and hang forever; a
    # negative value raises inside Semaphore. Reject both up front.
    if args.parallel < 1:
        print(f"--parallel must be >= 1 (got {args.parallel}).", file=_sys.stderr)
        return 2

    from echo_agent.config.loader import load_config, resolve_config_file
    config_file = resolve_config_file(config_path, search_dir=workspace)
    config = load_config(config_path=config_file)

    dataset_path = args.dataset or config.evaluation.dataset_path
    path = _Path(dataset_path).expanduser()
    # Resolve a relative dataset path against the workspace / config-file dir
    # (matching how the daemon resolves paths), not the process cwd.
    if not path.is_absolute():
        base = None
        if workspace:
            base = _Path(workspace).expanduser()
        elif config_file:
            base = _Path(config_file).parent
        if base is not None:
            path = base / path
    if not path.exists():
        print(f"Dataset not found: {path}", file=_sys.stderr)
        print("Create a YAML file with test cases. Example:")
        print("  - id: test_001")
        print("    input: 'Hello'")
        print("    expected_contains: ['hello', 'hi']")
        return 1

    from echo_agent.evaluation import EvalRunner, EvalDataset

    dataset = EvalDataset.from_path(path)
    if args.tag:
        cases = dataset.filter_by_tag(args.tag)
        dataset = EvalDataset(cases)

    if not dataset.cases:
        print(f"No test cases found (dataset={path}, tag={args.tag or '-'}).", file=_sys.stderr)
        return 1

    async def run() -> int:
        from echo_agent.app import bootstrap

        overrides = {"workspace": workspace} if workspace else None
        ctx = await bootstrap(config_path=config_path, overrides=overrides)
        await ctx.bus.start()
        await ctx.agent.start()

        try:
            # Pass the provider so expected_output cases get semantic judging,
            # not just the non-semantic string-similarity metric.
            runner = EvalRunner(
                ctx.agent,
                parallel=args.parallel,
                timeout=config.evaluation.timeout_per_case,
                provider=ctx.provider,
            )
            report = await runner.run_dataset(dataset)

            from echo_agent.evaluation.reporter import EvalReporter
            reporter = EvalReporter()
            print(reporter.to_table(report))

            if args.output:
                _Path(args.output).write_text(reporter.to_json(report), encoding="utf-8")
                print(f"\nResults saved to {args.output}")
            return 0 if report.passed_cases == report.total_cases else 1
        finally:
            from echo_agent.app import AppRuntime
            await AppRuntime._stop_step("agent", ctx.agent.stop())
            await AppRuntime._stop_step("bus", ctx.bus.stop())
            await AppRuntime._stop_step("storage", ctx.storage.close())

    return asyncio.run(run())


def main() -> None:
    try:
        _dispatch()
    except Exception as e:
        from echo_agent.cli.prompt import PromptAborted
        from echo_agent.config.loader import ConfigError
        if isinstance(e, PromptAborted):
            # A prompt was cancelled (Ctrl-C / Ctrl-D / empty piped stdin) and the
            # command did not run to completion. 130 is the shell convention for
            # "interrupted", and crucially it is not 0: a wrapper script must not
            # read a cancelled command as a successful one. Commands that treat a
            # cancellation as a normal outcome (the setup wizard) handle it
            # themselves and never reach here.
            import sys
            print("已取消 / Cancelled.", file=sys.stderr)
            sys.exit(130)
        if isinstance(e, ConfigError):
            import sys
            print(f"配置错误 / Configuration error:\n{e}", file=sys.stderr)
            sys.exit(1)
        raise


def _setup_section_names() -> str:
    """Comma-joined setup sections for ``setup --help``.

    Read from the wizard's own registry so the advertised sections can never
    drift from the implemented ones. Falls back to a plain hint if importing the
    wizard fails, since ``--help`` must never crash.
    """
    try:
        from echo_agent.cli.setup import section_names
    except Exception:  # noqa: BLE001 - help text is not worth a traceback
        return "run 'echo-agent setup' and pick from the menu"
    return ", ".join(section_names())


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="echo-agent", description="Echo Agent — modular AI agent framework")
    from echo_agent import __version__
    # __version__ 取自已安装包元数据;源码树直接运行会显示 0.0.0+unknown。
    parser.add_argument("--version", action="version", version=f"echo-agent {__version__}")
    subparsers = parser.add_subparsers(dest="command")

    # run
    run_parser = subparsers.add_parser("run", help="Start the agent")
    run_parser.add_argument("-c", "--config", help="Path to config file")
    run_parser.add_argument("-w", "--workspace", help="Workspace directory")
    run_parser.add_argument("--force", action="store_true",
                            help="跳过同 workspace 单实例互斥，强制多开（会造成重复回复/并发写库风险）")

    # setup
    setup_parser = subparsers.add_parser("setup", help="Run the setup wizard")
    setup_parser.add_argument(
        "section", nargs="?", default=None,
        help=f"Setup section: {_setup_section_names()}",
    )
    setup_parser.add_argument("-c", "--config", help="Path to config file")
    setup_parser.add_argument("-w", "--workspace", help="Workspace directory")
    setup_parser.add_argument("--lang", choices=["en", "zh", "auto"], default=None,
                              help="Override interface language (default: auto-detect from OS)")
    setup_parser.add_argument("--flow", choices=["quickstart", "full"], default=None,
                              help="Skip the menu and run a specific flow")
    setup_parser.add_argument("--json", action="store_true", dest="json",
                              help="Emit machine-readable JSON for the 'doctor' section (no ANSI)")

    # status
    status_parser = subparsers.add_parser("status", help="Show current configuration status")
    status_parser.add_argument("-c", "--config", help="Path to config file")
    status_parser.add_argument("-w", "--workspace", help="Workspace directory")
    status_parser.add_argument("--json", action="store_true", dest="json",
                               help="Emit machine-readable JSON (no ANSI)")

    # cost
    cost_parser = subparsers.add_parser("cost", help="Show cost attribution report")
    cost_parser.add_argument("-c", "--config", help="Path to config file")
    cost_parser.add_argument("-w", "--workspace", help="Workspace directory")
    cost_parser.add_argument("--days", type=int, default=7,
                             help="Trend window in days (default: 7)")
    cost_parser.add_argument("--json", action="store_true", dest="json",
                             help="Emit machine-readable JSON (no ANSI)")

    # gateway — foreground run (default) or service lifecycle management
    gw_parser = subparsers.add_parser(
        "gateway",
        help="Run the gateway in the foreground, or manage it as a system service",
    )
    gw_parser.add_argument(
        "action", nargs="?", default=None,
        choices=["install", "uninstall", "start", "stop", "restart", "status", "logs"],
        help="Service action (omit to run the gateway in the foreground)",
    )
    gw_parser.add_argument("-c", "--config", help="Path to config file")
    gw_parser.add_argument("-w", "--workspace", help="Workspace directory")
    gw_parser.add_argument("--host", help="Gateway host (foreground run only)")
    gw_parser.add_argument("--port", type=int, help="Gateway port (foreground run only)")
    gw_parser.add_argument("--system", action="store_true",
                           help="Manage a system-scope service instead of a user-scope one (Linux)")
    gw_parser.add_argument("--force", action="store_true",
                           help="Rewrite the service file even if one is already installed")
    gw_parser.add_argument("-f", "--follow", action="store_true",
                           help="Follow log output (logs action)")

    # cli — thin client attaching to a running local gateway
    cli_parser = subparsers.add_parser(
        "cli", help="Attach to a running local gateway as a thin client"
    )
    cli_parser.add_argument("--port", type=int, default=None, help="Gateway port (default: from config / 58123)")
    cli_parser.add_argument("--token", default=None, help="API token (default: from gateway config)")
    cli_parser.add_argument("--user", default="local", help="Client user id for the cli: session (default: local)")
    cli_parser.add_argument("-c", "--config", help="Path to config file")
    cli_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # dashboard — build the web SPA on demand
    dash_parser = subparsers.add_parser(
        "dashboard", help="Manage the web Dashboard build"
    )
    dash_parser.add_argument(
        "action", choices=["build"], help="Action (currently only: build)"
    )
    dash_parser.add_argument(
        "--force", action="store_true",
        help="Rebuild even when the existing artifact looks up to date",
    )

    # cron — inspect and (re-)authorize scheduled jobs for unattended execution
    cron_parser = subparsers.add_parser(
        "cron", help="Inspect and authorize scheduled jobs for unattended execution"
    )
    cron_parser.add_argument("action", choices=["list", "authorize", "revoke"])
    cron_parser.add_argument("job_id", nargs="?", default="", help="Job id (authorize/revoke)")
    cron_parser.add_argument("-y", "--yes", action="store_true", help="Skip the confirmation prompt")
    cron_parser.add_argument("-c", "--config", help="Path to config file")
    cron_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # eval
    eval_parser = subparsers.add_parser("eval", help="Run evaluation test suite")
    eval_parser.add_argument("--dataset", "-d", default="", help="Path to eval dataset (YAML/JSON)")
    eval_parser.add_argument("--tag", "-t", default="", help="Filter cases by tag")
    eval_parser.add_argument("--parallel", "-p", type=int, default=3, help="Parallel cases")
    eval_parser.add_argument("--output", "-o", default="", help="Output file for results")
    eval_parser.add_argument("-c", "--config", help="Path to config file")
    eval_parser.add_argument("-w", "--workspace", help="Workspace directory")

    # service — deprecated alias for `gateway <action>` (kept for install.sh
    # and existing user scripts; maps to the legacy Linux system-scope unit)
    svc_parser = subparsers.add_parser(
        "service", help="[deprecated] Use `echo-agent gateway <action>` instead"
    )
    svc_parser.add_argument("action", choices=["install", "uninstall", "start", "stop", "restart", "status", "logs"], help="Service action")
    svc_parser.add_argument("-w", "--workspace", help="Workspace directory (used by install)")

    # plugin
    plugin_parser = subparsers.add_parser("plugin", help="Manage plugins")
    plugin_parser.add_argument("action", choices=["list", "info", "enable", "disable", "check"], help="Plugin action")
    plugin_parser.add_argument("name", nargs="?", default="", help="Plugin name (for info/enable/disable)")
    plugin_parser.add_argument("-c", "--config", help="Path to config file")
    plugin_parser.add_argument("-w", "--workspace", help="Workspace directory")
    plugin_parser.add_argument("--json", action="store_true", dest="json",
                               help="Emit machine-readable JSON (no ANSI)")

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

    # skill (admission: staging / approve / reject) — not gated on evolution.enabled
    skill_parser = subparsers.add_parser("skill", help="Manage skill-distillation admission (staging/approve/reject)")
    skill_parser.add_argument("skill_action", choices=["list-staged", "approve", "reject"])
    skill_parser.add_argument("candidate_id", nargs="?", default="")
    skill_parser.add_argument("--reason", default="")
    skill_parser.add_argument("-c", "--config", help="Path to config file")
    skill_parser.add_argument("-w", "--workspace", help="Workspace directory")

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

    # checkpoint
    cp_parser = subparsers.add_parser("checkpoint", help="Inspect and roll back file checkpoints")
    cp_parser.add_argument("action", choices=["list", "show", "restore", "prune"], help="Checkpoint action")
    cp_parser.add_argument("sha", nargs="?", default="", help="Commit SHA (for show/restore)")
    cp_parser.add_argument("-c", "--config", help="Path to config file")
    cp_parser.add_argument("-w", "--workspace", help="Workspace directory")
    cp_parser.add_argument("-y", "--yes", action="store_true", help="Skip restore confirmation")
    cp_parser.add_argument("--json", action="store_true", dest="json",
                           help="Emit machine-readable JSON (no ANSI)")

    # migrate
    mig_parser = subparsers.add_parser("migrate", help="Run data migrations (memory scope)")
    mig_parser.add_argument("action", choices=["run", "rollback", "status", "memory-md"], help="Migration action")
    mig_parser.add_argument("--dry-run", action="store_true", help="Report changes without writing")
    mig_parser.add_argument("-c", "--config", help="Path to config file")
    mig_parser.add_argument("-w", "--workspace", help="Workspace directory")
    mig_parser.add_argument("-y", "--yes", action="store_true", help="Skip confirmation")
    mig_parser.add_argument(
        "--adopt-empty", action="store_true",
        help="run 时额外把空 scope 的 USER 记忆收编给 owner_key",
    )

    # deps — skill dependency management (status/install/refresh). Its own
    # argparse lives in dependencies.cli; capture the tail verbatim and delegate.
    deps_parser = subparsers.add_parser(
        "deps",
        help="Manage skill dependencies (status/install/refresh)",
    )
    deps_parser.add_argument(
        "deps_args", nargs=argparse.REMAINDER,
        help="deps subcommand and its arguments (e.g. status, install <feature>, refresh)",
    )

    # top-level flags for backward compat
    parser.add_argument("-c", "--config", help="Path to config file", dest="top_config")
    parser.add_argument("-w", "--workspace", help="Workspace directory", dest="top_workspace")

    return parser


def _dispatch() -> None:
    parser = _build_parser()
    args = parser.parse_args()

    if args.command == "setup":
        from echo_agent.cli.setup import run_setup_wizard
        import sys as _sys
        lang_arg = getattr(args, "lang", None)
        if lang_arg == "auto":
            lang_arg = None
        rc = run_setup_wizard(
            section=args.section,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            lang=lang_arg,
            flow=getattr(args, "flow", None),
            as_json=getattr(args, "json", False),
        )
        _sys.exit(rc)

    if args.command == "status":
        from echo_agent.cli.status import show_status
        import sys as _sys
        rc = show_status(
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            as_json=getattr(args, "json", False),
        )
        _sys.exit(rc)

    if args.command == "cost":
        from echo_agent.cli.cost import show_cost
        import sys as _sys
        rc = show_cost(
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            days=args.days,
            as_json=getattr(args, "json", False),
        )
        _sys.exit(rc)

    if args.command == "gateway":
        if args.action:
            from echo_agent.cli.service import run_service_action
            import sys as _sys
            rc = run_service_action(
                args.action,
                workspace=args.workspace or args.top_workspace,
                system=args.system,
                force=args.force,
                follow=args.follow,
                config=args.config or args.top_config,
            )
            _sys.exit(rc)
        from echo_agent.app import run_gateway
        try:
            asyncio.run(run_gateway(config_path=args.config or args.top_config, host=args.host, port=args.port, workspace=args.workspace or args.top_workspace, force=args.force))
        except KeyboardInterrupt:
            pass
        return

    if args.command == "eval":
        import sys as _sys
        _sys.exit(_run_eval(args))

    if args.command == "deps":
        from echo_agent.dependencies.cli import main as deps_main
        import sys as _sys
        # 始终传列表(哪怕为空):传 None 会让内层 argparse 回读真实
        # sys.argv 从而把外层的 "deps" 当成子命令,报 invalid choice。
        # --json 经 argparse.REMAINDER 原样透传给内层解析器。
        rc = deps_main(args.deps_args)
        _sys.exit(rc if rc is not None else 0)

    if args.command == "service":
        from echo_agent.cli.service import run_action
        import sys as _sys
        _sys.exit(run_action(args.action, workspace=args.workspace or args.top_workspace))

    if args.command == "plugin":
        from echo_agent.cli.plugins_cmd import run_plugin_command
        import sys as _sys
        rc = run_plugin_command(
            action=args.action,
            name=args.name,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            as_json=getattr(args, "json", False),
        )
        _sys.exit(rc)

    if args.command == "evolution":
        from echo_agent.cli.evolution_cmd import run_evolution_command
        import sys as _sys
        target = getattr(args, "target", "") or ""
        skill = ""
        candidate_id = ""
        if args.action == "rollback":
            skill = target
        elif args.action in ("show-candidate", "promote"):
            candidate_id = target
        try:
            rc = run_evolution_command(
                action=args.action,
                skill=skill,
                status_filter=getattr(args, "status_filter", "") or "",
                candidate_id=candidate_id,
                config_path=args.config or args.top_config,
                workspace=args.workspace or args.top_workspace,
            )
        except KeyboardInterrupt:
            rc = 130
        _sys.exit(rc)

    if args.command == "skill":
        from echo_agent.cli.skill_admission_cmd import run_skill_command
        import sys as _sys
        try:
            rc = run_skill_command(
                args.skill_action,
                candidate_id=args.candidate_id,
                reason=args.reason,
                config_path=args.config or args.top_config,
                workspace=args.workspace or args.top_workspace,
            )
        except KeyboardInterrupt:
            rc = 130
        _sys.exit(rc)

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

    if args.command == "checkpoint":
        from echo_agent.cli.checkpoint_cmd import run_checkpoint_command
        import sys as _sys
        rc = run_checkpoint_command(
            args.action,
            sha=args.sha,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            yes=args.yes,
            as_json=getattr(args, "json", False),
        )
        _sys.exit(rc)

    if args.command == "migrate":
        from echo_agent.cli.migrate_cmd import run_migrate_command
        import sys as _sys
        rc = run_migrate_command(
            args.action,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            dry_run=args.dry_run,
            yes=args.yes,
            adopt_empty=args.adopt_empty,
        )
        _sys.exit(rc)

    if args.command == "cron":
        from echo_agent.cli.cron_cmd import run_cron_command
        import sys as _sys
        _sys.exit(run_cron_command(
            args.action,
            args.job_id,
            config_path=args.config or args.top_config,
            workspace=args.workspace or args.top_workspace,
            assume_yes=args.yes,
        ))

    if args.command == "dashboard":
        import sys as _sys

        from echo_agent.gateway.dashboard_build import (
            build_dashboard,
            describe_outcome,
            find_web_dir,
            maybe_build_dashboard,
        )

        web_dir = find_web_dir()
        if web_dir is None:
            print(
                "当前为 wheel 安装，Dashboard 已随包发布，无需构建。\n"
                "（源码安装才需要本地构建前端。）"
            )
            _sys.exit(0)
        if args.force:
            outcome = build_dashboard(
                web_dir,
                on_output=lambda line: print(f"    {line}", flush=True),
                confirm=lambda msg: input(f"{msg} [Y/n] ").strip().lower() in ("", "y", "yes"),
            )
        else:
            outcome = maybe_build_dashboard(interactive=True)
            if outcome is None:
                print("Dashboard 产物已是最新，无需构建。（强制重建：--force）")
                _sys.exit(0)
        print(describe_outcome(outcome))
        # Exit on whether the BUILD COMMAND succeeded, not whether anything
        # usable is in dist. artifact_usable is what the gateway uses to
        # decide whether to serve the bundle; the CLI's job is "did the build
        # we just asked for succeed". If the build failed but a previous
        # bundle is still in place, the gateway will keep serving it — the
        # user gets a non-zero exit so they know their ask did not happen.
        _sys.exit(0 if outcome.build_succeeded else 1)

    if args.command == "cli":
        import sys as _sys

        from echo_agent.cli import attach_client

        # The cli path never reaches app.configure_logging (that runs inside
        # bootstrap), so loguru's default stderr sink printed every
        # "Loading config from ..." DEBUG line straight into the user's terminal.
        # A thin client should not narrate runtime logs.
        from echo_agent.app import configure_logging
        configure_logging("WARNING")

        info = attach_client.resolve_connection(
            args.config or args.top_config,
            args.workspace or args.top_workspace,
        )
        port = args.port if args.port is not None else info.port
        token = args.token if args.token is not None else info.token
        rc = attach_client.run_cli_attach(
            host=info.host, port=port, ws_path=info.ws_path,
            user_id=args.user, token=token,
            api_prefix=info.api_prefix, save_dir=info.save_dir,
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
        asyncio.run(run(config_path=config_path, workspace=workspace, force=getattr(args, "force", False)))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
