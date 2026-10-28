"""棘轮:死字段只能变少,不能新增。

原实现断言"schema 无任何 dead 字段"。意图(别让死字段堆积)是对的,但硬闸门产生了
反向激励:如实把一个失效字段标成 dead 会让 CI 红,而把它继续留标 effective 才绿 ——
恰恰生产出"标着生效、实际无作用"的配置。

改为登记制:已知欠债列在 _KNOWN_DEAD 里,新增死字段仍然失败。修好一个就从表里删掉
一行,棘轮只能向前。字段是否带 reason/disposition 由 test_config_metadata_guard 负责。
"""
from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config

# 已登记的死字段。每项都是欠债,不是豁免;处置进度见 render_backlog() 生成的 backlog。
_KNOWN_DEAD: set[str] = {
    # disposition=fix:ToT 只做广度、LATS 无 MCTS 深度,接线前需先实现深度语义。
    "planning.max_tree_depth",
}


def _dead_fields() -> set[str]:
    return {
        f.snake_path for f in iter_fields(Config)
        if f.extra.get("status") == "dead"
    }


def test_no_new_dead_fields():
    new = _dead_fields() - _KNOWN_DEAD
    assert not new, (
        f"新增死字段: {sorted(new)}。请接线使其生效,或在 _KNOWN_DEAD 登记并说明处置。"
    )


def test_known_dead_list_has_no_stale_entries():
    """修好的字段必须从登记表移除,否则它会掩护未来的同名回归。"""
    stale = _KNOWN_DEAD - _dead_fields()
    assert not stale, f"登记表存在已修复条目,请删除:{sorted(stale)}"
