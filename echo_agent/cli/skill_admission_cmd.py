"""CLI subcommand for skill-distillation admission (staging / approve / reject)."""

from __future__ import annotations

import asyncio
import sys


def run_skill_command(
    action: str,
    *,
    candidate_id: str = "",
    reason: str = "",
    config_path: str | None = None,
    workspace: str | None = None,
) -> int:
    """Dispatcher for ``echo-agent skill <action>``; returns an exit code.

    Argument guards still exit immediately (nothing has run, no result to
    report); everything else propagates a code so ``__main__`` owns the single
    exit point.
    """
    if action == "list-staged":
        return asyncio.run(_list_staged(config_path, workspace))
    if action == "approve":
        if not candidate_id:
            print("Usage: echo-agent skill approve <candidate-id>")
            sys.exit(1)
        return asyncio.run(
            _decide(config_path, workspace, candidate_id, approve=True, reason=reason)
        )
    if action == "reject":
        if not candidate_id:
            print("Usage: echo-agent skill reject <candidate-id> [--reason ...]")
            sys.exit(1)
        return asyncio.run(
            _decide(config_path, workspace, candidate_id, approve=False, reason=reason)
        )
    print(f"Unknown skill action: {action}")
    print("Available: list-staged, approve, reject")
    sys.exit(1)


async def _build_admission(config_path, workspace):
    from echo_agent.app import bootstrap as _bootstrap
    from echo_agent.evolution.store import TrajectoryStore
    from echo_agent.skills.admission import SkillAdmission

    overrides = {"workspace": workspace} if workspace else None
    ctx = await _bootstrap(config_path=config_path, overrides=overrides)
    cstore = TrajectoryStore(ctx.storage)
    await cstore.init_schema()
    adm = SkillAdmission(
        skill_store=ctx.agent.skill_store,
        candidate_store=cstore,
        policy=ctx.config.skills.admission_policy,
        auto_write_risk=ctx.config.skills.auto_write_risk,
    )
    return ctx, adm


async def _list_staged(config_path, workspace) -> int:
    ctx, adm = await _build_admission(config_path, workspace)
    try:
        staged = await adm.list_staged(limit=200)
        if not staged:
            print("(no staged skill candidates)")
            return 0
        print(f"{'ID':<22} {'OP':<8} {'RISK':<6} {'SKILL':<24} SOURCE")
        for c in staged:
            print(f"{c.id:<22} {c.operation:<8} {c.risk:<6} {c.skill_name:<24} {c.source}")
        return 0
    finally:
        await ctx.storage.close()


async def _decide(config_path, workspace, candidate_id, *, approve: bool, reason: str) -> int:
    ctx, adm = await _build_admission(config_path, workspace)
    try:
        res = await (adm.approve(candidate_id) if approve else adm.reject(candidate_id, reason))
        print(f"{res.outcome}: {res.message}")
        # ``approve`` that ends up merely staged (policy held it back) is not a
        # success from a script's point of view; ``reject`` succeeding is.
        if approve:
            return 0 if res.outcome == "written" else 1
        return 0
    finally:
        await ctx.storage.close()
