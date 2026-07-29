"""CLI cron authorization commands.

After the authorization contract landed, jobs stored before it — and jobs
created through the REST API without an explicit grant — fire but are denied
their own WRITE/EXEC work. This is the operator's path to inspect such a job and
re-authorize it, having actually seen what it runs and where it delivers.
"""
from __future__ import annotations

import getpass
from typing import Any

from echo_agent.scheduler.authorization import grant, verify


def _load_scheduler(config_path: str | None, workspace: str | None) -> Any | None:
    """Open the scheduler store standalone, without starting the agent.

    Scheduler takes a store_path and loads its jobs in __init__, and update_job
    calls _save() itself, so a read-modify-write from the CLI needs no running
    loop and no agent. Patched wholesale in tests, which exercise the command's
    own logic rather than config resolution.

    Deliberately mirrors app.py's store location (``<workspace>/data/
    scheduler.json``) — pointing at a different file would silently edit an empty
    store and report success while the running gateway kept the old jobs.
    """
    from pathlib import Path

    from echo_agent.cli.workspace import resolve_effective_workspace
    from echo_agent.config.loader import load_config, resolve_config_file
    from echo_agent.scheduler.service import Scheduler

    # resolve_effective_workspace is the single authoritative rule for "which
    # workspace" across subcommands; reimplementing it here would drift from
    # whatever the running gateway resolved.
    config_file = config_path or resolve_config_file(search_dir=workspace)
    config = load_config(config_file)
    ws = Path(resolve_effective_workspace(config, config_file, workspace))

    store_path = ws / "data" / "scheduler.json"
    if not store_path.exists():
        print(f"未找到调度器存储：{store_path}")
        return None
    return Scheduler(store_path=store_path)


def _state_label(job: Any) -> str:
    if verify(job):
        return "已授权"
    if getattr(job, "authorization", None) is not None:
        return "需要重新授权"
    return "未授权"


def _describe(job: Any) -> str:
    payload = job.payload if isinstance(job.payload, dict) else {}
    instruction = str(payload.get("command") or payload.get("message") or "")
    channel = str(payload.get("deliver_channel") or payload.get("channel") or "")
    chat_id = str(payload.get("deliver_chat_id") or payload.get("chat_id") or "")
    target = f"{channel}:{chat_id}" if channel or chat_id else "(无投递目标，产出不会发给任何人)"
    return (
        f"任务   {job.id}  {job.name}\n"
        f"频率   {job.cron_expr or '(非 cron 触发)'}\n"
        f"投递   {target}\n"
        f"指令   {instruction}\n"
    )


def run_cron_command(
    action: str,
    job_id: str,
    *,
    config_path: str | None,
    workspace: str | None,
    assume_yes: bool,
) -> int:
    scheduler = _load_scheduler(config_path, workspace)
    if scheduler is None:
        print("调度器不可用：请确认配置中已启用 scheduler。")
        return 1

    if action == "list":
        jobs = scheduler.list_jobs()
        if not jobs:
            print("没有定时任务。")
            return 0
        for job in jobs:
            print(f"{job.id}  [{_state_label(job)}]  {job.name}  {job.cron_expr}")
        return 0

    job = scheduler.get_job(job_id)
    if job is None:
        print(f"未找到任务 {job_id}。")
        return 1

    if action == "revoke":
        scheduler.update_job(job_id, authorization=None, set_authorization=True)
        print(f"已撤销任务 {job_id} 的无人值守授权。")
        return 0

    if action != "authorize":
        print(f"未知操作：{action}")
        return 1

    # Show the work before asking. Authorizing a job means letting it run
    # WRITE/EXEC tools with nobody watching, so the instruction and the delivery
    # target both have to be on screen at the moment of consent.
    print()
    print("即将授权以下任务在无人值守下执行写入/命令类工具：")
    print()
    print(_describe(job))

    if not assume_yes:
        answer = input("确认授权？输入 y 继续，其他任意键取消：").strip().lower()
        if answer != "y":
            print("已取消，任务保持未授权。")
            return 1

    operator = getpass.getuser() or "cli-user"
    scheduler.update_job(
        job_id,
        authorization=grant(job, operator=operator, source="cli"),
        set_authorization=True,
    )
    print(f"已授权任务 {job_id}（操作者 {operator}）。")
    return 0
