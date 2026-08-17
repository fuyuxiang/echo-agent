"""决定何时落盘,并合成替换文本。

作用对象是 ToolResult.text 所指向的那个字段:成功时 output,失败时 error。
只动模型真正会读的那个,另一字段与 metadata 原样保留,程序化取值不受影响。
"""

from __future__ import annotations

from loguru import logger

from echo_agent.spill.preview import compose
from echo_agent.spill.store import SpillStore
from echo_agent.tools.base import ToolResult


class SpillPolicy:
    """把超长的模型可见文本换成"预览 + 取回路径"。"""

    # read_file 必须跳过,否则形成 read -> spill -> 再 read 死循环。
    # search_files 不跳过:它搜内容而非读回自身输出,无循环风险,而单条匹配行
    # 可能极长。
    SKIP_TOOLS = frozenset({"read_file"})

    def __init__(self, store: SpillStore, max_inline_chars: int, enabled: bool = True):
        self._store = store
        self._cap = max_inline_chars
        self._enabled = enabled

    def apply(self, tool_name: str, session_key: str, result: ToolResult) -> ToolResult:
        if not self._enabled or tool_name in self.SKIP_TOOLS:
            return result

        field = "output" if result.success else "error"
        text = getattr(result, field)
        if not text or len(text) <= self._cap:
            return result

        try:
            path = self._store.save(session_key or "unscoped", tool_name, text)
        except Exception as e:
            # best-effort:落盘失败保留原内联结果,绝不把成功的调用变成失败
            logger.warning("spill 落盘失败,保留内联结果 tool={} err={}", tool_name, e)
            return result

        replacement = compose(text, str(path), self._cap)
        if replacement is None:
            return result

        setattr(result, field, replacement)
        result.metadata["spilled"] = True
        result.metadata["spill_path"] = str(path)
        result.metadata["omitted_chars"] = len(text) - len(replacement)
        return result
