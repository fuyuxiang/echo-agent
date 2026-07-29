"""CLI cron authorization commands.

After the authorization contract landed, jobs stored before it — and jobs
created through the REST API without an explicit grant — fire but are denied
their own WRITE/EXEC work. This is the operator's path to inspect such a job and
re-authorize it, having actually seen what it runs and where it delivers.
"""
from __future__ import annotations

import getpass
import os
from pathlib import Path
from typing import Any

# LOCK_FILENAME is the workspace instance-lock file name, shared with
# runtime_lock.acquire_instance_lock so the probe below looks at the same file a
# live agent actually holds.
from echo_agent.runtime_lock import LOCK_FILENAME
from echo_agent.scheduler.authorization import grant, verify

# Mirrors delivery's own target resolution: a job with only a source_session_key
# still gets delivered, so the confirmation screen must not claim otherwise.
from echo_agent.scheduler.delivery import target_from_session_key

_RUNNING_GATEWAY_HINT = (
    "检测到 Gateway 正在运行，离线修改不会生效，且可能被运行中的实例覆盖。\n"
    "请先停止服务后重试，或改用 Dashboard / REST 的 cron 接口授权。"
)


def _load_scheduler(
    config_path: str | None, workspace: str | None
) -> tuple[Any, Path] | None:
    """Open the scheduler store standalone, without starting the agent.

    Scheduler takes a store_path and loads its jobs in __init__, and update_job
    calls _save() itself, so a read-modify-write from the CLI needs no running
    loop and no agent. Patched wholesale in tests, which exercise the command's
    own logic rather than config resolution.

    Returns the scheduler together with the resolved workspace: the caller needs
    that same directory to check for a live agent, and resolving it twice would
    let the two answers drift.

    Deliberately mirrors app.py's store location (``<workspace>/data/
    scheduler.json``) — pointing at a different file would silently edit an empty
    store and report success while the running gateway kept the old jobs.
    """
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
    return Scheduler(store_path=store_path), ws


def _gateway_is_running(workspace: Path) -> bool:
    """Best-effort: is a live echo-agent holding this workspace's instance lock?

    Probed rather than acquired: acquire_instance_lock writes holder info and
    degrades to a no-op success where no lock primitive exists, neither of which
    suits a read-only check. An advisory flock is the right signal because the OS
    drops it when the holder dies, so a crashed run does not lock the CLI out
    forever the way a leftover endpoint file would.

    Any failure answers "not running": this gate exists to stop a silently
    ineffective edit, and it must never be the reason a legitimate one is refused.
    """
    try:
        lock_path = workspace / "data" / LOCK_FILENAME
        if not lock_path.exists():
            return False

        try:
            import fcntl
        except ImportError:  # pragma: no cover - platform-specific (Windows)
            return _gateway_pid_is_alive(workspace)

        with open(lock_path, "a+", encoding="utf-8") as fd:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                # Someone holds it, and only a live process can.
                return True
            # We got it, so nobody else had it; drop it immediately — holding it
            # would block the very gateway the operator is about to restart.
            fcntl.flock(fd, fcntl.LOCK_UN)
            return False
    except Exception:
        return False


def _gateway_pid_is_alive(workspace: Path) -> bool:
    """Windows fallback: no flock, so fall back to the recorded pid.

    Weaker than the lock — a recycled pid reads as alive and a crash leaves the
    endpoint file behind — but it is the only signal available there, and it is
    still better than editing the store under a running gateway.
    """
    try:
        from echo_agent.cli.workspace import read_runtime_endpoint

        endpoint = read_runtime_endpoint(workspace) or {}
        pid = int(endpoint.get("pid") or 0)
        if pid <= 0:
            return False
        os.kill(pid, 0)
        return True
    except Exception:
        return False


def _state_label(job: Any) -> str:
    if verify(job):
        return "已授权"
    if getattr(job, "authorization", None) is not None:
        return "需要重新授权"
    return "未授权"


def _describe(job: Any) -> str:
    payload = job.payload if isinstance(job.payload, dict) else {}
    instruction = str(payload.get("command") or payload.get("message") or "")
    channel = str(payload.get("deliver_channel") or payload.get("channel") or "").strip()
    chat_id = str(payload.get("deliver_chat_id") or payload.get("chat_id") or "").strip()
    # Same fallback delivery applies at fire time. Reading only the explicit keys
    # made a job that does deliver read as "goes nowhere" — the dangerous
    # direction for a consent screen.
    session_key = str(payload.get("source_session_key") or payload.get("session_key") or "").strip()
    if (not channel or not chat_id) and session_key:
        session_channel, session_chat_id = target_from_session_key(session_key)
        channel = channel or session_channel
        chat_id = chat_id or session_chat_id
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
    loaded = _load_scheduler(config_path, workspace)
    if loaded is None:
        print("调度器不可用：请确认配置中已启用 scheduler。")
        return 1
    scheduler, ws = loaded

    if action == "list":
        jobs = scheduler.list_jobs()
        if not jobs:
            print("没有定时任务。")
            return 0
        for job in jobs:
            print(f"{job.id}  [{_state_label(job)}]  {job.name}  {job.cron_expr}")
        return 0

    # From here on the command writes the store. A running agent holds the jobs
    # in memory and rewrites the whole file on every tick / run outcome / stop,
    # so an offline edit is not merely invisible to it — it gets overwritten.
    # For revoke that is a fail-open: the grant comes back. Refuse instead.
    if _gateway_is_running(ws):
        print(_RUNNING_GATEWAY_HINT)
        return 1

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
        try:
            answer = input("确认授权？输入 y 继续，其他任意键取消：").strip().lower()
        except (EOFError, KeyboardInterrupt):
            # No stdin (piped/cron/CI) or Ctrl-C: absence of an answer is not
            # consent. Treated as a decline instead of a traceback.
            print()
            answer = ""
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
