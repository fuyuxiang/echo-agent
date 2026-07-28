"""间距规则与符号集：两个纯函数模块，无需挂载即可断言。

间距此前来自每个块自己的 CSS margin，于是一串十行的 trace 和"trace 收尾 →
答案开始"这种真正的分界线拥有同样的空隙，而渲染为空的块还会留下悬空空行。
符号此前直接写死 emoji，宽度受 VS16 影响在各终端不一致，行首永远对不齐。
"""

from __future__ import annotations

from echo_agent.cli.tui.blocks import ExpandableBlock
from echo_agent.cli.tui.glyphs import ASCII, EMOJI, GLYPHS, NARROW, cog_glyph, resolve_glyphs
from echo_agent.cli.tui.layout import lead_gap
from echo_agent.cli.tui.turn_layout import TRACE_DEPTH, rail_prefix


# ── 分组间距 ──────────────────────────────────────────────────────────

def test_no_gap_at_the_top_of_the_screen():
    assert lead_gap(None, "model") is False
    assert lead_gap(None, "trail") is False


def test_no_gap_inside_one_group():
    # 连续的工具/思考行是一串，不能被空行拆散
    assert lead_gap("trail", "trail") is False
    assert lead_gap("model", "model") is False


def test_gap_at_a_group_boundary():
    # trace 收尾到答案开始，是全屏最需要分界的地方
    assert lead_gap("trail", "model") is True
    assert lead_gap("model", "trail") is True
    assert lead_gap("model", "note") is True


def test_self_spaced_groups_never_get_an_extra_gap():
    # user/ui 自带 CSS margin，再叠一行就成了两行空白
    assert lead_gap("trail", "user") is False
    assert lead_gap("model", "ui") is False


def test_no_gap_after_a_group_that_paints_its_own_trailing_space():
    # 用户标题下方已有 margin，紧随其后的首个 trace 行不能再顶一行空白
    assert lead_gap("user", "trail") is False
    assert lead_gap("ui", "model") is False


def test_gap_depends_only_on_the_predecessor():
    """流式安全的关键：间距只由前一个块的分组决定，与自身内容无关，
    因此一条回复在流式过程中和落定之后算出的间距完全一致，不会跳动。"""
    assert lead_gap("trail", "model") is lead_gap("trail", "model")


# ── 符号集 ────────────────────────────────────────────────────────────

def test_narrow_is_the_default():
    assert resolve_glyphs({}).name == "narrow"
    assert resolve_glyphs({"ECHO_TUI_ICONS": "unknown"}).name == "narrow"


def test_icon_set_is_selectable():
    assert resolve_glyphs({"ECHO_TUI_ICONS": "emoji"}) is EMOJI
    assert resolve_glyphs({"ECHO_TUI_ICONS": "ASCII"}) is ASCII


def test_ascii_set_is_pure_7bit():
    """ASCII 档要能在最保守的终端（以及重定向到文件的日志里）正常显示。"""
    fields = [
        ASCII.reply, ASCII.user, ASCII.tool, ASCII.ok, ASCII.fail,
        ASCII.pending, ASCII.unfinished, ASCII.collapsed, ASCII.expanded,
        ASCII.rail, ASCII.branch, ASCII.branch_last, ASCII.sep,
        *ASCII.cognitive.values(),
    ]
    for text in fields:
        assert text.isascii(), text


def test_every_cognitive_type_has_a_marker_in_every_set():
    types = set(NARROW.cognitive) | set(EMOJI.cognitive) | set(ASCII.cognitive)
    for glyphs in (NARROW, EMOJI, ASCII):
        for cog_type in types:
            assert cog_glyph(cog_type, glyphs)


def test_unknown_cognitive_type_falls_back_instead_of_crashing():
    # 网关可以先于客户端上线新的 cog_type，缺符号不能让整行渲染失败
    assert cog_glyph("brand_new_type", NARROW)


# ── 轮次缩进 ──────────────────────────────────────────────────────────

def test_top_level_blocks_have_no_indent():
    # 用户标题与答案本体是对话主干，不缩进
    assert rail_prefix(0) == ""
    assert rail_prefix(-1) == ""


def test_trace_blocks_are_indented_one_level():
    assert rail_prefix(TRACE_DEPTH) == GLYPHS.rail
    assert rail_prefix(2) == GLYPHS.rail * 2


def test_sibling_indent_is_stable_across_frames():
    """兄弟块只用重复的竖线，不用 ├─/└─：哪一条 trace 是"最后一条"要等下一条
    到达（或本轮结束）才知道，用弯头就意味着每来一帧都要重渲染上一个块 ——
    而流式过程中根本没有正确答案可渲染。前缀在挂载时定一次，之后永不变。"""
    assert rail_prefix(TRACE_DEPTH) == rail_prefix(TRACE_DEPTH)
    assert GLYPHS.branch not in rail_prefix(TRACE_DEPTH)
    assert GLYPHS.branch_last not in rail_prefix(TRACE_DEPTH)


def test_child_rows_use_an_elbow_and_align_under_it():
    """块自己的明细行在 render_detail 里一次性生成，"最后一行"是已知的，
    所以这里的 └─ 既正确又稳定；续行填充必须与弯头等宽才能对齐。"""
    def plain(s: str) -> str:
        return s.replace("[$text-muted]", "").replace("[/]", "")

    block = ExpandableBlock()
    block.depth = TRACE_DEPTH
    stem = rail_prefix(TRACE_DEPTH)
    mid_head, mid_cont = (plain(s) for s in block.child_rail(last=False))
    end_head, end_cont = (plain(s) for s in block.child_rail(last=True))
    # stem 是本轮的缩进轨，四段都保留；变化的只是它后面的子级段
    for seg in (mid_head, mid_cont, end_head, end_cont):
        assert seg.startswith(stem)
    assert mid_head[len(stem):] == GLYPHS.branch
    assert end_head[len(stem):] == GLYPHS.branch_last
    assert mid_cont[len(stem):].startswith(GLYPHS.rail)
    assert end_cont[len(stem):].strip() == ""  # 末行之后不再有子级竖线
    # 续行必须与弯头等宽，否则换行的明细会比它续的那行左移
    assert len(mid_head) == len(mid_cont) == len(end_head) == len(end_cont)
