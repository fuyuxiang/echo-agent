"""spill 必须位于最终写回边界之后,而不是 registry.execute 的返回点。

registry 内部 spill 完之后,post_tool_call 插件还能替换整个 result。插件产出的
超长文本若不再过一遍 spill,就会撞上 _MAX_TOOL_RESULT_CHARS 的哑截断——尾部
丢失、且没有取回路径,恰是 spill 本要消除的失效模式。
"""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.spill.policy import SpillPolicy
from echo_agent.spill.store import SpillStore
from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult

SESSION = "sess-a"


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(execution_id="e", trace_id="t", session_key=SESSION)


class _TinyTool(Tool):
    name = "tiny"
    description = "returns a short string"
    parameters = {"type": "object", "properties": {}}

    async def execute(self, params, ctx=None):
        return ToolResult(output="short")


def _registry(tmp_path, cap=6000):
    reg = ToolRegistry(spill_policy=SpillPolicy(SpillStore(tmp_path), max_inline_chars=cap))
    reg.register(_TinyTool())
    return reg


def test_registry_exposes_a_public_spill_entry(tmp_path):
    """插件后处理需要能再调一次,故收口点必须是公开 API,不是私有方法。"""
    reg = _registry(tmp_path)
    assert callable(getattr(reg, "apply_spill", None))


def test_plugin_replaced_result_gets_spilled(tmp_path):
    """核心断言:插件把结果换成超长文本后,仍然落盘 + 给取回路径。"""
    reg = _registry(tmp_path)
    plugin_output = ToolResult(output="P" * 200_000)
    out = reg.apply_spill("tiny", _ctx(), plugin_output)
    assert out.metadata.get("spilled") is True
    assert len(out.output) <= 6000
    assert "read_spill" in out.output


def test_respill_is_idempotent_for_untouched_results(tmp_path):
    """没被插件改动的结果二次过 spill 必须原样返回,不能落第二个文件。"""
    reg = _registry(tmp_path)
    first = reg.apply_spill("tiny", _ctx(), ToolResult(output="Q" * 200_000))
    path = first.metadata["spill_path"]
    second = reg.apply_spill("tiny", _ctx(), first)
    assert second.output == first.output
    assert second.metadata["spill_path"] == path
    artifacts = list(SpillStore(tmp_path).session_dir(SESSION).iterdir())
    assert len(artifacts) == 1


def test_respill_helper_survives_registry_without_spill():
    """spill 未装配(或 registry 是替身)时补调必须是 no-op,不能抛。"""
    class _Bare:
        pass

    stage = InferenceStage.__new__(InferenceStage)
    stage._tools = _Bare()
    result = ToolResult(output="Z" * 100)
    assert stage._respill("tiny", _ctx(), result) is result


def test_respill_helper_keeps_plugin_result_when_spill_raises():
    """spill 自身出错不该丢掉插件的结果。"""
    class _Exploding:
        def apply_spill(self, *a, **k):
            raise RuntimeError("boom")

    stage = InferenceStage.__new__(InferenceStage)
    stage._tools = _Exploding()
    result = ToolResult(output="Z" * 100)
    assert stage._respill("tiny", _ctx(), result) is result


def test_respill_helper_applies_policy(tmp_path):
    stage = InferenceStage.__new__(InferenceStage)
    stage._tools = _registry(tmp_path)
    out = stage._respill("tiny", _ctx(), ToolResult(output="R" * 200_000))
    assert out.metadata.get("spilled") is True


@pytest.mark.parametrize("succeeded", [True, False])
def test_orphan_artifact_is_removed_when_compose_declines(tmp_path, succeeded):
    """compose 拒绝替换时,已写的文件必须删掉。

    模型从未拿到那个路径,留着就是个无人引用的孤儿,要等保留期才被回收——而
    它装的正是完整的敏感输出。
    """
    store = SpillStore(tmp_path)
    # cap 足够大以通过配置校验,但小于 notice + 预览所需 → compose 返回 None。
    policy = SpillPolicy(store, max_inline_chars=120)
    text = "z" * 500
    result = (ToolResult(output=text) if succeeded
              else ToolResult(success=False, error=text))
    out = policy.apply("exec", SESSION, result)

    # 保留原文(未替换),且不留文件。
    assert (out.output if succeeded else out.error) == text
    assert "spilled" not in out.metadata
    session_dir = store.session_dir(SESSION)
    assert not session_dir.exists() or list(session_dir.iterdir()) == []


def test_shell_combined_output_respects_the_acquisition_cap(tmp_path):
    """stdout 与 stderr 各自贴着上限时,合并结果不能是两倍。

    combined 才是模型实际读到的那一份。采集上限若只作用于两个分量,上限就名不
    副实——配 2 MB 时模型可能收到 4 MB,而 spill 的替换预算是按前者算的。
    走真实 execute 路径,不直接测 _bound:要验证的正是合并之后有没有再套一次。
    """
    from echo_agent.agent.executors.base import ExecResponse
    from echo_agent.agent.tools.shell import ShellTool

    cap = 1000

    class _StubExecutor:
        name = "stub"

        async def execute(self, request):
            return ExecResponse(
                success=False, stdout="O" * 5000, stderr="E" * 5000,
                return_code=1, executor="stub",
            )

    tool = ShellTool(str(tmp_path), max_output=cap, executor=_StubExecutor())
    res = asyncio.run(tool.execute({"command": "build"}, _ctx()))
    # 允许截断提示本身的额外字符,但不能接近两倍上限。
    assert len(res.output) < cap * 2
