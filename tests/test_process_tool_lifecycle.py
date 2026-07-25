"""ProcessTool lifecycle contract: per-instance table, stderr drain, timeout, aclose."""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.tools.process import ProcessTool


async def _start(tool: ProcessTool, command: str, *, timeout: int = 300) -> str:
    result = await tool.execute({"action": "start", "command": command, "timeout": timeout})
    assert result.success, result.error
    return result.metadata["process_id"]


@pytest.mark.asyncio
async def test_process_table_is_per_instance(tmp_path):
    tool_a = ProcessTool(str(tmp_path))
    tool_b = ProcessTool(str(tmp_path))
    pid = await _start(tool_a, "true")
    # tool_b must not see tool_a's process — no module-level cross-talk.
    listing = tool_b._list()
    assert pid not in listing.output
    assert "No background processes." in listing.output
    await tool_a.aclose()
    await tool_b.aclose()


@pytest.mark.asyncio
async def test_heavy_stderr_does_not_deadlock(tmp_path):
    tool = ProcessTool(str(tmp_path))
    # Write a lot of data to stderr AND stdout. If the collector only drains
    # stdout, the stderr pipe buffer (~64KB) fills and the child blocks forever.
    # Run a script file rather than `python3 -c ...`: the inline-interpreter
    # form is hard-denied by the exec policy before the subprocess ever starts.
    script = tmp_path / "heavy_stderr.py"
    script.write_text(
        "import sys\n"
        "sys.stderr.write('E' * 200000)\n"
        "sys.stdout.write('O' * 10000)\n"
        "sys.stderr.flush()\n"
        "sys.stdout.flush()\n"
    )
    cmd = f"python3 {script}"
    pid = await _start(tool, cmd)
    proc = tool._processes[pid]["process"]
    # Must terminate on its own; a deadlocked child never exits.
    await asyncio.wait_for(proc.wait(), timeout=10)
    # Give the collector a beat to flush the final chunks.
    collector = tool._processes[pid]["collector"]
    await asyncio.wait_for(collector, timeout=5)
    poll = await tool._poll(pid)
    assert "exited(0)" in poll.output
    assert "[stderr]" in poll.output  # stderr_buf was actually populated
    # Ring truncation kept stderr bounded.
    assert len(tool._processes[pid]["stderr_buf"]) <= 100_000
    await tool.aclose()


@pytest.mark.asyncio
async def test_timeout_kills_and_marks_process(tmp_path):
    tool = ProcessTool(str(tmp_path))
    pid = await _start(tool, "sleep 30", timeout=1)
    proc = tool._processes[pid]["process"]
    # Watchdog must fire ~1s in, terminate/kill the child, and mark it timed_out.
    await asyncio.wait_for(proc.wait(), timeout=10)
    assert proc.returncode is not None
    assert tool._processes[pid]["timed_out"] is True
    poll = await tool._poll(pid)
    assert "timed out" in poll.output.lower()
    await tool.aclose()


@pytest.mark.asyncio
async def test_aclose_terminates_processes_and_clears_table(tmp_path):
    tool = ProcessTool(str(tmp_path))
    pid = await _start(tool, "sleep 30")
    proc = tool._processes[pid]["process"]
    assert proc.returncode is None
    await tool.aclose()
    # Process killed and table emptied.
    assert proc.returncode is not None
    assert tool._processes == {}
    # Idempotent: a second aclose over an empty table must not raise.
    await tool.aclose()


@pytest.mark.asyncio
async def test_agent_stop_closes_process_tool(tmp_path, monkeypatch):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config
    from echo_agent.bus.queue import MessageBus
    from echo_agent.models.provider import LLMProvider, LLMResponse

    # A minimal real provider — AgentLoop.__init__ dereferences
    # provider.chat_with_retry (via MemoryConsolidator), so provider=None crashes
    # construction before stop() is ever reached.
    class _StubProvider(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
            return LLMResponse(content="ok", finish_reason="stop")

        def get_default_model(self):
            return "stub"

    config = load_config(overrides={"workspace": str(tmp_path)})
    config.tools.exec.enabled = True
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_StubProvider(), workspace=tmp_path)
    proc_tool = loop.tools.get("process")
    assert proc_tool is not None
    closed = {"n": 0}
    orig = proc_tool.aclose

    async def _spy():
        closed["n"] += 1
        await orig()

    monkeypatch.setattr(proc_tool, "aclose", _spy)
    await loop.stop()
    assert closed["n"] == 1


# ── 已退出条目的回收:进程表不能无上界增长 ────────────────────────────────────


@pytest.mark.asyncio
async def test_exited_entries_reclaimed_by_ttl(tmp_path):
    """已退出进程的条目超过保留期后回收;在此之前 poll 仍能取回输出。

    原实现只在 _stop(模型显式调用)与 aclose(agent 关停)清表,自然退出和被
    watchdog 杀掉的进程条目永久驻留,每条还留着最多 100KB 输出缓冲。
    """
    tool = ProcessTool(str(tmp_path))
    pid = await _start(tool, "echo hi")
    await asyncio.wait_for(tool._processes[pid]["collector"], timeout=5)

    # 保留期内:条目仍在,输出可取回。
    poll = await tool.execute({"action": "poll", "process_id": pid})
    assert poll.success and "hi" in poll.output

    # 把完成时间推回到保留期之外,再起一个进程触发回收。
    tool._processes[pid]["finished_at"] -= tool._EXITED_TTL_SECONDS + 1
    pid2 = await _start(tool, "echo second")
    assert pid not in tool._processes, "过期的已退出条目必须被回收"
    assert pid2 in tool._processes, "新进程条目必须保留"
    await tool.aclose()


@pytest.mark.asyncio
async def test_exited_entries_bounded_by_capacity(tmp_path):
    """已退出条目数超过上限时按最老优先淘汰,近期结果仍可 poll。

    回收由操作触发(start/list),所以最后一个退出的条目要等下一次操作才被计入 ——
    这里显式调一次 list,也贴近真实用法。
    """
    tool = ProcessTool(str(tmp_path))
    tool._MAX_EXITED_ENTRIES = 3
    pids = []
    for i in range(6):
        pid = await _start(tool, f"echo {i}")
        await asyncio.wait_for(tool._processes[pid]["collector"], timeout=5)
        pids.append(pid)

    await tool.execute({"action": "list"})
    exited = [p for p in tool._processes if tool._processes[p]["process"].returncode is not None]
    assert len(exited) <= 3, f"已退出条目应被容量上限约束,实际 {len(exited)}"
    assert pids[-1] in tool._processes, "最新条目必须保留"
    await tool.aclose()


@pytest.mark.asyncio
async def test_running_process_never_reclaimed(tmp_path):
    """在跑的进程无论多老都不回收 —— 回收它会让 poll/stop 失去目标。"""
    tool = ProcessTool(str(tmp_path))
    tool._MAX_EXITED_ENTRIES = 0
    tool._EXITED_TTL_SECONDS = 0.0
    live = await _start(tool, "sleep 30")
    for i in range(3):
        done = await _start(tool, f"echo {i}")
        await asyncio.wait_for(tool._processes[done]["collector"], timeout=5)
    assert live in tool._processes, "活跃进程不得被回收"
    await tool.aclose()


# ── 回收的两个前置条件:输出已排空、进程组已排空 ────────────────────────────


@pytest.mark.asyncio
async def test_entry_kept_while_background_work_runs(tmp_path):
    """回归:leader 退出但进程组还在跑时,表项必须保留。

    背景命令(`sleep &`)的 shell 立刻以 0 退出,原实现只看 returncode 就把表项当
    "已退出"回收,于是 stop/aclose 再也没有句柄去停那个仍在运行的后台进程。
    """
    tool = ProcessTool(str(tmp_path))
    tool._MAX_EXITED_ENTRIES = 0
    tool._EXITED_TTL_SECONDS = 0.0

    # 孙进程重定向自己的 stdio,才不会撑着 leader 的管道 —— 否则 leader 不会先退出。
    pid = await _start(tool, "sleep 60 >/dev/null 2>&1 &")
    proc = tool._processes[pid]["process"]
    await asyncio.wait_for(proc.wait(), timeout=5)
    assert proc.returncode == 0, "leader 应在启动后台任务后立即退出"
    await asyncio.wait_for(tool._processes[pid]["collector"], timeout=5)

    tool._reap_finished()
    assert pid in tool._processes, "组内仍有后台工作时不得回收表项"

    # poll 也要如实报告"leader 退了但后台还在跑",而不是干脆说 exited(0)。
    poll = await tool._poll(pid)
    assert "background work running" in poll.output

    await tool.aclose()
    assert tool._processes == {}


@pytest.mark.asyncio
async def test_watchdog_bounds_backgrounded_work(tmp_path):
    """回归:timeout 必须约束整棵进程树,而不只是 leader。

    原 watchdog 是 `await wait_for(proc.wait(), timeout)`,leader 一退出就 return
    —— 而 `cmd &` 的 shell 恰好立刻退出,于是后台工作彻底没有超时上界,能一直跑到
    agent 进程自己结束。
    """
    from echo_agent.agent.proc_lifecycle import process_group_alive

    tool = ProcessTool(str(tmp_path))
    tool._GROUP_WAIT_INTERVAL = 0.05
    # 孙进程重定向 stdio,否则 leader 会被管道拖住不先退出。
    pid = await _start(tool, "sleep 60 >/dev/null 2>&1 &", timeout=1)
    proc = tool._processes[pid]["process"]
    await asyncio.wait_for(proc.wait(), timeout=5)
    assert proc.returncode == 0, "leader 应立即退出"

    # watchdog 必须在 timeout 到点后回收整组并标记超时。
    await asyncio.wait_for(tool._processes[pid]["watchdog"], timeout=15)
    assert tool._processes[pid]["timed_out"] is True, "后台工作未被超时约束"
    assert not process_group_alive(proc), "超时后整组必须被回收"
    await tool.aclose()


@pytest.mark.asyncio
async def test_watchdog_does_not_flag_tree_that_finishes_in_time(tmp_path):
    """树在期限内自然排空时不得标记超时,也不得误杀。"""
    tool = ProcessTool(str(tmp_path))
    tool._GROUP_WAIT_INTERVAL = 0.05
    pid = await _start(tool, "sleep 0.2 >/dev/null 2>&1 &", timeout=10)
    await asyncio.wait_for(tool._processes[pid]["watchdog"], timeout=15)
    assert tool._processes[pid]["timed_out"] is False
    await tool.aclose()
