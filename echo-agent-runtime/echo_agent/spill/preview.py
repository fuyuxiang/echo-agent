"""把超长文本压成"头 + 尾 + 取回提示",且保证替换后恒不超限、恒变短。

头 40 / 尾 60 的依据是信息密度分布不均:结论在尾部——pytest 的
"=== N failed ==="、编译器 error summary、堆栈最内层调用都在末尾,头部多为
环境信息与噪声。
"""

from __future__ import annotations

HEAD_RATIO = 0.4
_ELLIPSIS = "\n…\n"
# 预算扣减与拼装共用同一个常量,防的是"算的时候忘了它、拼的时候又输出了它"
# 这类漂移——那正好会让替换文本恰好超限 2 个字符,被兜底守卫全部打回。
_SEP = "\n\n"


def _notice(omitted: int, locator: str) -> str:
    # 必须指向 read_spill 而非 read_file/search_files:后两者按路径授权、按行
    # 分页,既读不到单行长输出的尾部,也无法防止跨会话取回。取回通道只有一个。
    return (
        f"（已省略 {omitted} 字符。完整结果已存至: {locator}。"
        f"用 read_spill 带 offset/limit 按字符读取,或带 pattern 在产物内检索。"
        f"该路径仅供你自己取回,不要向用户复述。）"
    )


def compose(text: str, locator: str, max_inline_chars: int) -> str | None:
    """返回替换文本;返回 None 表示不该替换、保留原文。"""
    # notice 里的省略字符数依赖预览长度,而预览长度依赖 notice 长度。用最长
    # 可能的省略数(整段全省)先算出 notice 上界,预算按它扣,避免互相依赖。
    upper_notice = _notice(len(text), locator)
    budget = max_inline_chars - len(upper_notice) - len(_ELLIPSIS) - len(_SEP)

    if budget <= 0:
        # 预算不够放任何预览:退化成只出 notice,前提是它本身不超限
        if len(upper_notice) <= max_inline_chars < len(text):
            return upper_notice
        return None

    head_len = int(budget * HEAD_RATIO)
    tail_len = budget - head_len
    omitted = len(text) - head_len - tail_len
    if omitted <= 0:
        return None

    out = text[:head_len] + _ELLIPSIS + text[-tail_len:] + _SEP + _notice(omitted, locator)
    # 双重保险:替换必须既不超限也不变长,否则保留原文
    if len(out) > max_inline_chars or len(out) >= len(text):
        return None
    return out
