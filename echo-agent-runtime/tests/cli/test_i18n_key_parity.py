"""zh 与 en 的键集合必须完全一致 —— 缺键会让另一语言的用户看到键名本身。

既有的 tests/cli/setup/test_i18n_keys.py 只逐个点名少数键，新增一段文案时很容易
漏掉另一个 bundle 而无人发现。这里对整棵字典做对称差集，覆盖以后所有新增段。
"""
from __future__ import annotations

from echo_agent.cli.i18n import en, zh


def _flatten(bundle, prefix=""):
    keys = set()
    for key, value in bundle.items():
        path = f"{prefix}{key}"
        if isinstance(value, dict):
            keys |= _flatten(value, f"{path}.")
        else:
            keys.add(path)
    return keys


def test_zh_and_en_have_identical_keys():
    zh_keys = _flatten(zh.MESSAGES)
    en_keys = _flatten(en.MESSAGES)
    assert zh_keys - en_keys == set(), f"missing in en: {sorted(zh_keys - en_keys)}"
    assert en_keys - zh_keys == set(), f"missing in zh: {sorted(en_keys - zh_keys)}"


def test_startup_placeholders_match_across_locales():
    """同一键在两个 bundle 里的占位符必须一致。

    ``t()`` 在 ``format`` 抛 KeyError 时返回未格式化的原串，所以 en 有 {host} 而 zh
    漏掉不会报错，只会让中文用户看到一个没填进去的模板 —— 静默且难查。
    """
    import string

    def _fields(text: str) -> set[str]:
        return {
            name for _lit, name, _spec, _conv in string.Formatter().parse(text) if name
        }

    for key, en_text in en.MESSAGES["startup"].items():
        zh_text = zh.MESSAGES["startup"][key]
        assert _fields(en_text) == _fields(zh_text), f"placeholder mismatch in startup.{key}"
