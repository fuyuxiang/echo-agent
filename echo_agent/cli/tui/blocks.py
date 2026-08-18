"""Transcript block widgets. Rendering logic is kept in plain methods
(render_summary/render_detail) so it's unit-testable without a live screen."""

from __future__ import annotations

import os
import re

from rich.markdown import Markdown
from rich.markup import escape
from rich.table import Table
from rich.text import Text
from rich.theme import Theme as RichTheme
from textual.widgets import Static

from echo_agent.cli.tui.glyphs import GLYPHS, cog_glyph
from echo_agent.cli.tui.protocol import CogEvent
from echo_agent.cli.tui.turn_layout import TRACE_DEPTH, rail_prefix


def _markup_safe(s: str) -> str:
    """Escape user text for safe embedding inside Rich/Textual markup spans.

    Rich's escape() only neutralizes sequences it considers valid tags, but
    older Textual versions misparse bare brackets inside $-token style spans
    (e.g. [$text-muted]...[/]).  Replacing both bracket characters is a
    belt-and-suspenders guard that costs nothing for display-only summaries.
    """
    return s.replace("[", "⟨").replace("]", "⟩")

_TOOL_VERB = {
    "read_file": "读取", "write_file": "写入", "edit_file": "编辑",
    "patch": "打补丁", "list_dir": "列出", "search_files": "搜索",
    "session_search": "检索会话", "knowledge_search": "查知识库",
    "exec": "执行", "process": "运行进程", "web_fetch": "抓取网页",
    "web_search": "联网搜索", "memory": "记忆", "todo": "更新待办",
}

# 每个工具用哪个参数当"操作对象"。缺省走兜底：第一个字符串参数。
_OBJECT_KEY = {
    "read_file": "path", "write_file": "path", "edit_file": "path",
    "patch": "path", "list_dir": "path", "exec": "command",
    "process": "command", "web_fetch": "url", "web_search": "query",
}


def humanize_tool(name: str) -> str:
    """Tool id -> Chinese verb. Unknown tools fall back to the raw id."""
    return _TOOL_VERB.get(name, name)


# Glyphs older gateways prefixed onto their cognitive summary text. The client
# now owns the line marker (glyphs.py), so a summary arriving with one of these
# would render two markers side by side. Stripped on read rather than trusted,
# because the gateway and the cli are versioned independently.
_LEGACY_SUMMARY_GLYPHS = ("🧠", "✍", "💭", "🔧", "⚠️", "⚠", "💰", "⏳", "🧬", "•")


def strip_legacy_glyph(summary: str) -> str:
    text = str(summary).lstrip()
    for glyph in _LEGACY_SUMMARY_GLYPHS:
        if text.startswith(glyph):
            return text[len(glyph):].lstrip()
    return text


class ExpandableBlock(Static):
    """A Static whose summary/detail flips via ``toggle()``, wired to real user
    input: it is focusable (Tab/arrow reach it), clickable (mouse), and toggles
    on Enter/Space. Subclasses implement render_summary/render_detail and set
    ``expanded``. Previously ToolCallBlock/CognitiveBlock had toggle() but no
    input path, so their detail/diff view was unreachable except via the
    last-block ctrl+r/ctrl+o shortcuts — this makes every such block openable."""

    can_focus = True
    # Tool/cognitive traces are the agent's working area; see layout.lead_gap.
    block_group = "trail"
    # Which turn this block belongs to, and how far inside it the line sits.
    # Set by the transcript at mount time (see turn_layout for why a turn is a
    # label on flat blocks rather than a nested container widget). The default
    # keeps standalone construction — unit tests, /copy walks — working.
    turn_seq = 0
    depth = TRACE_DEPTH
    # Whether the /details setting asks for this block's detail view. Distinct
    # from `expanded`, which is what is actually on screen: a block with no
    # payload stays summarized however the setting reads. Class-level default so
    # subclasses that never take the argument still answer set_detail_default.
    _want_expanded = False

    @property
    def rail(self) -> str:
        """Muted indent rail placed before this block's own line marker, so a
        turn's trace lines read as one indented run under their title instead of
        sitting flush with the conversation."""
        prefix = rail_prefix(self.depth)
        return f"[$text-muted]{prefix}[/]" if prefix else ""

    def child_rail(self, *, last: bool) -> tuple[str, str]:
        """Rails for a detail line one level inside this block: the elbow segment
        for the line that carries the label, and the continuation segment for its
        wrapped/subsequent lines.

        Unlike sibling blocks, a block's own detail rows ARE all known at render
        time (render_detail builds them in one pass), so here the ``└─`` elbow is
        both correct and stable — it is only across independently-mounted blocks
        that "which one is last" cannot be answered.
        """
        stem = rail_prefix(self.depth)
        elbow = GLYPHS.branch_last if last else GLYPHS.branch
        # Pad the continuation out to the elbow's own width, not the rail's: the
        # elbow is "├─ " while the rail is "│ ", so reusing the rail width alone
        # would shift every wrapped line one column left of the text it continues.
        cont = "" if last else GLYPHS.rail
        cont = cont.ljust(len(elbow))
        return (
            f"[$text-muted]{stem}{elbow}[/]",
            f"[$text-muted]{stem}{cont}[/]",
        )

    def render_summary(self) -> str:  # pragma: no cover - overridden
        raise NotImplementedError

    def _has_detail(self) -> bool:  # pragma: no cover - overridden
        """Whether this block has anything to open."""
        return True

    def set_detail_default(self, *, expanded: bool) -> None:
        """Re-apply a changed /details setting to an already-mounted block.

        Existing lines are re-rendered rather than left alone, so /details reads
        as a view setting over the whole transcript instead of only affecting
        whatever the agent happens to do next. Blocks with nothing to open keep
        their bare summary, so the marker never contradicts the content.
        """
        self._want_expanded = expanded
        want = expanded and self._has_detail()
        if want == self.expanded:
            return
        self.expanded = want
        self.update(self.render_detail() if self.expanded else self.render_summary())

    def render_detail(self) -> str:  # pragma: no cover - overridden
        raise NotImplementedError

    def toggle(self) -> None:
        self.expanded = not self.expanded
        self.update(self.render_detail() if self.expanded else self.render_summary())

    def on_click(self) -> None:
        # Toggle on click, but hand keyboard focus straight back to the prompt.
        # The block is focusable (so Tab can reach it), and a click would
        # otherwise leave focus parked here — this block only responds to
        # enter/space, so the user's next keystroke would be swallowed and the
        # input box would look frozen. Tab/arrow focus is unaffected (that path
        # doesn't go through on_click). Best-effort: no prompt mounted → skip.
        self.toggle()
        try:
            self.app.query_one("PromptInput").focus()
        except Exception:
            pass

    def key_enter(self) -> None:
        self.toggle()

    def key_space(self) -> None:
        self.toggle()



# Tools whose result text is a unified-style diff worth coloring line by line.
_DIFF_TOOLS = {"edit_file", "patch", "write_file"}


def colorize_diff(text: str, max_lines: int = 40) -> str:
    """Color a unified-diff-ish blob: +added lines green, -removed lines red,
    @@ hunk headers muted. Each line is escaped before the color tag is added so
    diff content can never inject markup. Returns Rich markup, capped so a huge
    diff can't flood the transcript."""
    out: list[str] = []
    lines = text.splitlines()
    for raw in lines[:max_lines]:
        line = escape(raw)
        if raw.startswith("+") and not raw.startswith("+++"):
            out.append(f"[$success]{line}[/]")
        elif raw.startswith("-") and not raw.startswith("---"):
            out.append(f"[$error]{line}[/]")
        elif raw.startswith("@@") or raw.startswith("+++") or raw.startswith("---"):
            out.append(f"[$text-muted]{line}[/]")
        else:
            out.append(line)
    if len(lines) > max_lines:
        out.append(f"[$text-muted]… (还有 {len(lines) - max_lines} 行)[/]")
    return "\n".join(out)


def _clip(s: str, n: int) -> str:
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[: n - 1] + "…"


def _fmt_duration_ms(ms: int | None) -> str:
    """Human duration for a tool line, or "" when it isn't worth a column.

    Anything under a second is dropped: on a transcript where most calls are
    instant reads, "0.1s" on every line is noise that hides the one call that
    actually took half a minute.
    """
    if ms is None:
        return ""
    try:
        value = float(ms)
    except (TypeError, ValueError):
        return ""
    if value < 1000:
        return ""
    seconds = value / 1000
    if seconds < 60:
        return f"{seconds:.1f}s"
    minutes, rest = divmod(int(seconds), 60)
    return f"{minutes}m {rest}s"


# Parameter names whose value must never be shown verbatim. The approval panel
# and the tool detail view render whatever the model passed, and that routinely
# includes credentials — which then sit in the transcript and get written to disk
# by /save. Matched as a substring on the lowercased key, so "api_key",
# "authorization" and "DB_PASSWORD" are all covered.
_SECRET_KEY_HINTS = (
    "password", "passwd", "secret", "token", "api_key", "apikey",
    "credential", "authorization", "auth_header", "private_key", "access_key",
)


def _is_secret_key(key: str) -> bool:
    lowered = str(key).lower()
    return any(hint in lowered for hint in _SECRET_KEY_HINTS)


def _mask(value: str) -> str:
    """Replace a secret value with a length-preserving placeholder, keeping the
    last 4 characters so the user can still tell two credentials apart."""
    text = str(value)
    if len(text) <= 4:
        return "••••"
    return "••••" + text[-4:]


_BEARER_RE = re.compile(r"(Bearer\s+)\S+", re.IGNORECASE)
_URL_SECRET_RE = re.compile(
    r"([?&](?:token|key|api_key|apikey|secret|access_token|password)=)([^&\s]+)",
    re.IGNORECASE,
)
_HEADER_FLAG_RE = re.compile(
    r"(-H\s+['\"]?(?:Authorization|X-Api-Key)['\"]?\s*:\s*)\S+",
    re.IGNORECASE,
)


def _mask_sensitive_strings(text: str) -> str:
    """Mask Bearer tokens, URL secret params, and CLI header flags in a string."""
    text = _BEARER_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _URL_SECRET_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _HEADER_FLAG_RE.sub(lambda m: m.group(1) + "••••", text)
    return text


def _redact_value(key: str, value, *, value_width: int = 60) -> str:
    """Recursively redact a value: mask if the key is secret, otherwise recurse
    into dicts/lists looking for nested secrets."""
    if _is_secret_key(key):
        return _mask(value)
    if isinstance(value, dict):
        parts = []
        for k, v in value.items():
            parts.append(f"{k}={_redact_value(k, v, value_width=value_width)}")
        shown = "{" + ", ".join(parts) + "}"
        return _clip(shown, value_width)
    if isinstance(value, list):
        items = [_redact_value(key, item, value_width=value_width) for item in value]
        shown = "[" + ", ".join(items) + "]"
        return _clip(shown, value_width)
    shown = _clip(value, value_width)
    return _mask_sensitive_strings(shown)


def format_params(params: dict, *, value_width: int = 60) -> list[str]:
    """Render call parameters as one ``key=value`` line per entry, with secrets
    masked recursively.

    Both the approval panel and the tool detail view used to print ``str(dict)``,
    i.e. a raw Python repr: it wrapped unreadably for anything non-trivial and
    leaked credentials verbatim on the exact screen where the user is asked to
    authorize a high-risk action.
    """
    lines: list[str] = []
    for key, value in (params or {}).items():
        shown = _redact_value(key, value, value_width=value_width)
        lines.append(f"{key}={shown}")
    return lines


def pick_object(name: str, params: dict) -> str:
    """The primary argument shown as the tool's operand."""
    params = params or {}
    if name == "search_files":
        pat = params.get("pattern")
        return f'"{pat}"' if pat else ""
    key = _OBJECT_KEY.get(name)
    val = params.get(key) if key else None
    if val is None:
        # Fallback: first string-valued argument.
        val = next((v for v in params.values() if isinstance(v, str)), "")
    if name in ("read_file", "write_file", "edit_file", "patch") and val:
        val = os.path.basename(str(val))
    return _clip(val, 48) if val else ""


def summarize_result(
    name: str, result_meta: dict | None, result_text: str, success: bool
) -> str:
    """Turn the producer-supplied count (result_meta) into Chinese words;
    fall back to a text preview. Never recount on the truncated result_text."""
    if not success:
        return "失败"
    meta = result_meta or {}
    if name == "read_file" and "total_lines" in meta:
        return f"{meta['total_lines']} 行"
    if name == "search_files" and "count" in meta:
        return f"找到 {meta['count']} 处"
    if name == "list_dir" and "count" in meta:
        return f"{meta['count']} 个"
    if name in ("exec", "process"):
        return "完成"
    preview = _clip(result_text or "", 40)
    return preview or "完成"


# Block-letter ECHO logo. A 3-stop gradient (primary → accent → secondary) is
# applied per line so the wordmark reads as one branded object; the
# gradient banner is kept compact (5 rows) so it never dominates the first screen.
_LOGO_ART = (
    "███████╗ ██████╗██╗  ██╗ ██████╗ ",
    "██╔════╝██╔════╝██║  ██║██╔═══██╗",
    "█████╗  ██║     ███████║██║   ██║",
    "██╔══╝  ██║     ██╔══██║██║   ██║",
    "███████╗╚██████╗██║  ██║╚██████╔╝",
)
# One theme color per logo row, top→bottom.
_LOGO_GRADIENT = ("$primary", "$primary", "$accent", "$secondary", "$secondary")


class Banner(Static):
    """Brand banner shown on the transcript's first screen: a gradient block-
    letter wordmark, a tagline, and the welcome hint. Brand strings and colors
    are injected so a white-label build restyles it without code edits.

    Kept as a pure render (build_text) so it is unit-testable without a live
    screen, mirroring the other blocks."""

    block_group = "ui"

    def __init__(
        self,
        session_key: str = "",
        *,
        name: str = "echo",
        tagline: str = "agent",
        welcome: str = "输入消息开始对话  ·  /help 查看命令  ·  Ctrl+C 停止任务/退出",
    ) -> None:
        self.session_key = session_key
        self.brand_name = name
        self.brand_tagline = tagline
        self.brand_welcome = welcome
        # Narrow terminals collapse the 5-row ASCII wordmark to a single-line
        # brand so it doesn't dominate a short screen; recomputed on resize.
        self._narrow = False
        super().__init__(self.build_text())

    # Below this width the block-letter logo is too wide / too tall relative to
    # the screen, so fall back to a one-line wordmark.
    NARROW_WIDTH = 40

    def on_resize(self) -> None:
        try:
            width = self.size.width or self.app.size.width
        except Exception:
            width = 0
        narrow = bool(width) and width < self.NARROW_WIDTH
        if narrow != self._narrow:
            self._narrow = narrow
            self.update(self.build_text())

    def build_text(self) -> str:
        # Custom brand names can't use the fixed ECHO art, so fall back to a
        # simple bold wordmark when the name isn't the default "echo" OR the
        # terminal is too narrow for the 5-row block letters.
        if self._narrow or self.brand_name.lower() != "echo":
            logo = f"[bold $primary]{escape(self.brand_name)}[/]"
        else:
            logo = "\n".join(
                f"[{color}]{escape(row)}[/]"
                for row, color in zip(_LOGO_ART, _LOGO_GRADIENT)
            )
        sess = f"  ·  会话 {self.session_key}" if self.session_key else ""
        tagline = f"[$text-muted]· {escape(self.brand_tagline)}[/]{sess}"
        return f"{logo}\n{tagline}\n[$text-muted]{escape(self.brand_welcome)}[/]"


class UserTurn(Static):
    block_group = "user"
    # Stamped by the transcript at mount time; see turn_layout.
    turn_seq = 0

    def __init__(self, text: str) -> None:
        # Keep the sigil-free original so /copy can export a clean transcript
        # without the "❯ " decoration.
        self.raw_text = text
        self.text_content = f"{GLYPHS.user} {text}"
        # Markup keeps the sigil in the accent colour and the task text bright,
        # so the title reads as the strongest element in each turn.
        super().__init__(
            f"[bold $primary]{GLYPHS.user}[/] [b]{escape(text)}[/b]"
        )


class ThemedMarkdown(Markdown):
    """Rich Markdown whose named styles (headings, code, links, block quotes)
    are mapped to Echo theme colours instead of Rich's default ANSI palette.

    Rich resolves ``markdown.h1``/``markdown.code``/… from the console's style
    stack; the defaults render headings in bright magenta and code in a fixed
    hue, both of which clash with the teal/indigo brand and fail contrast on the
    light palette. We push a per-render theme so those styles resolve to our
    palette, then pop it — no global console mutation."""

    def __init__(self, text: str, *, palette: dict[str, str]) -> None:
        super().__init__(text, inline_code_theme="ansi_dark")
        self._palette = palette

    def __rich_console__(self, console, options):
        primary = self._palette.get("primary", "")
        secondary = self._palette.get("secondary", "")
        accent = self._palette.get("accent", "")
        muted = self._palette.get("muted", "")
        theme = RichTheme({
            "markdown.h1": f"bold {primary}" if primary else "bold",
            "markdown.h2": f"bold {primary}" if primary else "bold",
            "markdown.h3": f"bold {accent}" if accent else "bold",
            "markdown.h4": f"bold {accent}" if accent else "bold",
            "markdown.h5": "bold",
            "markdown.h6": "bold",
            "markdown.link": f"underline {secondary}" if secondary else "underline",
            "markdown.link_url": secondary or "",
            "markdown.item.bullet": accent or "",
            "markdown.block_quote": muted or "",
            "markdown.hr": muted or "",
        }, inherit=True)
        with console.use_theme(theme):
            yield from super().__rich_console__(console, options)


class AgentReply(Static):
    """Agent reply body. Streaming tokens render as plain (escaped) text —
    partial markdown is inevitably broken, and re-parsing every token would
    flicker. The finished reply is rendered as markdown via ``set_markdown``;
    ``set_final`` stays a plain-text path for status lines (notices, server
    errors) that reuse this widget but carry hand-built Rich markup, not
    markdown."""

    # Stamped by the transcript at mount time; see turn_layout. /copy uses it to
    # collect every reply block belonging to the newest turn.
    turn_seq = 0

    def __init__(self) -> None:
        self._buf = ""
        # True once set_markdown has rendered this reply, i.e. its colours are
        # baked in and a theme switch needs an explicit repaint(). Markup paths
        # (streaming/status lines) re-resolve $vars themselves, so they stay False.
        self._is_markdown = False
        # Status lines (server errors, etc.) reuse this widget but are NOT real
        # agent replies. Flagged so /copy skips them and stays pointed at the
        # last genuine answer.
        self.is_status = False
        super().__init__(f"[$primary]{GLYPHS.reply}[/] ")

    @property
    def block_group(self) -> str:
        """A real reply is the model's voice; a status line (notice, server
        error) belongs to the quieter note band — so a notice following a reply
        still gets its separating blank line. See layout.lead_gap."""
        return "note" if self.is_status else "model"

    @property
    def text(self) -> str:
        """The reply body as plain text (markdown source / status line),
        without the ``●`` sigil — used by /copy."""
        return self._buf

    def append_token(self, t: str) -> None:
        self._buf += t
        self._is_markdown = False
        self.update(f"[$primary]{GLYPHS.reply}[/] {escape(self._buf)}")

    def clear_stream(self) -> None:
        """Drop the streamed text, keeping the widget in place.

        Used when the server retracts an optimistic draft: the next iteration's
        tokens must start from empty rather than extend the abandoned text. The
        widget stays mounted so the reply keeps its position in the transcript.
        """
        self._buf = ""
        self._is_markdown = False
        self.update(f"[$primary]{GLYPHS.reply}[/] ")

    def set_final(self, text: str) -> None:
        self._buf = text
        self._is_markdown = False
        self.update(f"[$primary]{GLYPHS.reply}[/] {escape(self._buf)}")

    def set_markup(self, markup: str) -> None:
        """Render pre-built Rich markup verbatim (NOT escaped), keeping the ●
        sigil. Used for client-local notices (/help, /theme) whose markup we
        author ourselves and trust — unlike streamed reply text, which is always
        escaped. _buf keeps a plain-ish copy so /copy stays harmless if reached
        (notices set is_status and are skipped anyway)."""
        self._buf = markup
        self._is_markdown = False
        self.update(f"[$primary]{GLYPHS.reply}[/] {markup}")

    def _bullet_color(self) -> str:
        """Resolve the theme's ``primary`` colour so the ``●`` matches the
        streaming sigil. Rich renderables bypass Textual's ``$var`` markup
        substitution, so we look the colour up here. Falls back to a fixed hue
        when no app/theme is attached (e.g. pure unit tests)."""
        try:
            theme = self.app.current_theme
            if theme is not None and theme.primary:
                return theme.primary
        except Exception:
            pass
        return "#8899ff"

    def set_markdown(self, text: str) -> None:
        """Render the finished reply as markdown, keeping the ``●`` sigil inline
        with the body's first line. A two-column grid places the accent bullet
        beside the markdown so the turn still reads as "the agent is speaking",
        and wrapped/subsequent lines stay aligned under the body.

        Markdown styles (headings, code, links, emphasis) are mapped to the Echo
        theme instead of Rich's default ANSI palette — the defaults rendered
        headings in bright magenta, clashing with the teal/indigo brand and
        failing contrast on the light palette."""
        self._buf = text
        self._is_markdown = True
        grid = Table.grid(padding=(0, 1, 0, 0))
        grid.add_column()
        grid.add_column()
        grid.add_row(
            Text(GLYPHS.reply, style=self._bullet_color()),
            ThemedMarkdown(text, palette=self._md_palette()),
        )
        self.update(grid)

    def _md_palette(self) -> dict[str, str]:
        """Resolve theme colours for markdown styles. Rich renderables bypass
        Textual's ``$var`` markup substitution, so we look the hues up from the
        active theme here. Empty dict when no theme is attached (unit tests) →
        ThemedMarkdown falls back to Rich defaults without crashing."""
        try:
            theme = self.app.current_theme
            if theme is not None:
                return {
                    "primary": theme.primary or "",
                    "secondary": theme.secondary or "",
                    "accent": theme.accent or theme.primary or "",
                    "muted": (theme.variables or {}).get("text-muted", ""),
                }
        except Exception:
            pass
        return {}

    def repaint(self) -> None:
        """Re-render with the CURRENTLY active theme.

        set_markdown resolves theme hues eagerly (Rich renderables don't
        participate in Textual's ``$var`` substitution) and bakes them into the
        renderable, so a later ``/theme`` switch left every reply already on
        screen in the old palette — dark-palette teal and its low-contrast muted
        grey on the light theme's white surface, which is exactly the
        unreadability the light palette exists to fix. Markup-based lines
        (streaming text, notices, errors) re-resolve ``$var`` on their own, so
        only the markdown path needs redoing.
        """
        if self._is_markdown:
            self.set_markdown(self._buf)


class CognitiveBlock(ExpandableBlock):
    def __init__(self, ev: CogEvent, *, expanded: bool = False) -> None:
        self.ev = ev
        # `expanded` is a constructor argument rather than something the caller
        # flips afterwards: the initial content is chosen here, so assigning the
        # attribute post-construction would leave the widget showing a summary
        # that disagrees with its own state until the next repaint.
        self._want_expanded = expanded
        self.expanded = expanded and self._has_detail()
        super().__init__(
            self.render_detail() if self.expanded else self.render_summary()
        )

    def _has_detail(self) -> bool:
        """Whether there is anything to open. A frame can arrive with an empty
        payload (the gateway trims long ones), and marking such a line ``▾`` —
        open — while it shows exactly one row is a cue that contradicts itself."""
        d = self.ev.data
        return bool(d.get("items") or (self.ev.cog_type == "thinking" and d.get("text")))

    @property
    def is_streaming(self) -> bool:
        """Whether more text is still coming for this line (a partial thinking
        snapshot). Streaming lines carry a live tail in their summary so the
        collapsed default still shows movement; see render_summary."""
        return bool(self.ev.data.get("streaming"))

    def update_event(self, ev: CogEvent) -> None:
        """Replace the payload with a newer snapshot of the SAME logical event.

        Used for streamed thinking, where each frame supersedes the last: the
        block is re-rendered in place rather than a second one mounted, so a
        round of reasoning stays one line in the transcript however many frames
        it took to arrive. The user's own expand state is preserved unless the
        arriving payload makes it impossible.
        """
        self.ev = ev
        self.expanded = (self.expanded or self._want_expanded) and self._has_detail()
        self.update(
            self.render_detail() if self.expanded else self.render_summary()
        )

    def mark_stream_ended(self) -> None:
        """Settle a partial thinking line whose closing frame will never arrive
        (the turn was interrupted or died). The trace collected so far is kept —
        it is genuine, and on an interrupt it is often the most informative thing
        left on screen — but the line stops claiming to be in progress."""
        if not self.is_streaming:
            return
        self.ev.data["streaming"] = False
        self.ev.summary = f"思考 {GLYPHS.unfinished} 未完成"
        self.update(
            self.render_detail() if self.expanded else self.render_summary()
        )

    def _stream_tail(self) -> str:
        """The most recent non-empty line of a streaming trace, clipped to one
        row's worth. Shown beside "思考中" so a collapsed thinking line still
        conveys that the model is producing something, and roughly what."""
        text = str(self.ev.data.get("text", ""))
        for raw in reversed(text.splitlines()):
            line = raw.strip()
            if line:
                return _clip(line, 48)
        return ""

    def render_summary(self) -> str:
        icon = cog_glyph(self.ev.cog_type)
        # A closed disclosure marker on the blocks that actually have a detail
        # view, so "there is more here" is visible rather than something the
        # user has to already know. The keyboard shortcut is named too, since
        # ctrl+r/ctrl+o reach these without moving focus.
        marker = GLYPHS.expanded if self.expanded else GLYPHS.collapsed
        if not self._has_detail():
            # No payload to open: no marker and no shortcut hint, so the cue never
            # promises content that isn't there.
            chev, hint = " ", ""
        elif self.ev.cog_type == "memory_recalled":
            chev, hint = marker, " ctrl+r"
        elif self.ev.cog_type == "thinking":
            chev, hint = marker, " ctrl+o"
        else:
            chev, hint = " ", ""
        text = strip_legacy_glyph(self.ev.summary)
        if self.is_streaming and not self.expanded:
            # Mid-stream the shortcut hint is dropped: the tail is already the
            # interesting part of the row, and the two together overflow a
            # narrow terminal into a wrap that jitters on every frame.
            tail = self._stream_tail()
            if tail:
                text = f"{text} {GLYPHS.sep} {tail}"
                hint = ""
        # Cognitive traces are secondary information — render the whole line in
        # the muted indigo tone so it recedes behind replies and tool actions.
        return (
            f"{self.rail}[$secondary]{chev}{icon}[/] "
            f"[$text-muted]{escape(text)}{escape(hint)}[/]"
        )

    def render_detail(self) -> str:
        d = self.ev.data
        lines = [self.render_summary()]
        items = list(d.get("items", []))
        for idx, it in enumerate(items):
            src = it.get("source", "")
            badge = f"\\[{escape(src)}]" if src else ""
            content = escape(str(it.get("content", "")))
            elbow, _ = self.child_rail(last=idx == len(items) - 1)
            lines.append(f"{elbow}{content} [$text-muted]{badge}[/]".rstrip())
        if self.ev.cog_type == "thinking" and d.get("text"):
            # Reasoning text is a paragraph, not a list entry: indent every line
            # under one elbow so a wrapped thought stays visually attached to it.
            elbow, cont = self.child_rail(last=True)
            for idx, raw in enumerate(str(d["text"]).strip().splitlines()):
                prefix = elbow if idx == 0 else cont
                lines.append(f"{prefix}[$text-muted]{escape(raw)}[/]")
        return "\n".join(lines)


class ToolCallBlock(ExpandableBlock):
    """One tool invocation, rendered as ``● 动词 对象 · 结果 · 耗时 ✓``. Flips in
    place from running to done, paired across the two frames by tool_call_id."""

    def __init__(
        self,
        tool_call_id: str,
        name: str,
        params: dict,
        status: str = "running",
        result_meta: dict | None = None,
        result_text: str = "",
        duration_ms: int | None = None,
        expanded: bool = False,
    ) -> None:
        self.tool_call_id = tool_call_id
        # Stored as tool_name to avoid clashing with Textual Widget's read-only
        # `name` property (which has no setter).
        self.tool_name = name
        self.params = params or {}
        self.status = status
        self.result_meta = result_meta
        self.result_text = result_text
        self.duration_ms = duration_ms
        # Detail-open state is passed in (see details.py): deciding it here means
        # the first paint is already correct, instead of rendering the summary and
        # then toggling, which flashes and — with the transcript's bottom anchor
        # engaged — jogs the scroll position.
        #
        # Kept as a separate wish because a running call may have no detail YET
        # (no params) and gain one on its done frame; without this the "open tool
        # details" setting would silently skip exactly those calls.
        self._want_expanded = expanded
        self.expanded = expanded and self._has_detail()
        super().__init__(
            self.render_detail() if self.expanded else self.render_summary()
        )

    def _has_detail(self) -> bool:
        return bool(self.params or self.result_text)

    def render_summary(self) -> str:
        verb = _markup_safe(humanize_tool(self.tool_name))
        obj = _markup_safe(pick_object(self.tool_name, self.params))
        sep = f"[$text-muted]{GLYPHS.sep}[/]"
        # A disclosure marker only where there IS a detail view, so the cue never
        # promises content that isn't there.
        chev = (
            (GLYPHS.expanded if self.expanded else GLYPHS.collapsed)
            if self._has_detail() else " "
        )
        # verb in accent, operand muted so the eye separates "what" from "on what".
        head = f"{self.rail}[$accent]{chev}{GLYPHS.tool}[/] [b]{verb}[/b]"
        if obj:
            head += f" [$text-muted]{obj}[/]"
        if self.status == "running":
            return f"{head} [$text-muted]{GLYPHS.pending}[/]"
        if self.status == "interrupted":
            # The turn ended (error / interrupt) before this tool's paired done
            # frame arrived. Say so instead of leaving the running "…", which
            # read as "still executing" for work that had already stopped.
            return (
                f"{head} {sep} [$text-muted]未完成[/] "
                f"[$warning]{GLYPHS.unfinished}[/]"
            )
        ok = self.status == "ok"
        mark = (
            f"[$success]{GLYPHS.ok}[/]" if ok else f"[$error]{GLYPHS.fail}[/]"
        )
        summary = _markup_safe(summarize_result(
            self.tool_name, self.result_meta, self.result_text, ok
        ))
        tone = "$text-muted" if ok else "$error"
        line = f"{head} {sep} [{tone}]{summary}[/]"
        # Duration was carried on the frame and stored but never shown, so a
        # 30-second command looked identical to an instant one. Sub-second calls
        # stay unlabelled — the number would be noise on every trivial read.
        took = _fmt_duration_ms(self.duration_ms)
        if took:
            line += f" {sep} [$text-muted]{took}[/]"
        return f"{line} {mark}"

    def render_detail(self) -> str:
        lines = [self.render_summary()]
        rows: list[tuple[str, list[str]]] = []
        if self.params:
            # One line per parameter with secrets masked — a raw str(dict) both
            # wrapped unreadably and leaked credentials into the transcript.
            rows.append(("参数", [_markup_safe(e) for e in format_params(self.params)]))
        if self.result_text:
            # Edit-family tools return a diff — color it so added/removed lines
            # read at a glance. Everything else keeps the compact text preview.
            looks_like_diff = "\n" in self.result_text and any(
                ln[:1] in "+-@" for ln in self.result_text.splitlines()
            )
            if self.tool_name in _DIFF_TOOLS and looks_like_diff:
                rows.append(("变更", colorize_diff(self.result_text).split("\n")))
            else:
                rows.append(
                    ("结果", [_markup_safe(_clip(self.result_text, 200))])
                )
        for idx, (label, body) in enumerate(rows):
            elbow, cont = self.child_rail(last=idx == len(rows) - 1)
            lines.append(f"{elbow}[$text-muted]{label}[/]")
            for entry in body:
                lines.append(f"{cont}  {entry}")
        return "\n".join(lines)

    def mark_done(
        self, status: str, result_meta: dict | None, result_text: str, duration_ms: int | None
    ) -> None:
        self.status = status
        self.result_meta = result_meta
        self.result_text = result_text
        self.duration_ms = duration_ms
        # The result is what a "tools expanded" reader wants to see, and it only
        # exists now — a call whose running frame had no params could not honour
        # the setting at mount time.
        if self._want_expanded and self._has_detail():
            self.expanded = True
        self.update(self.render_detail() if self.expanded else self.render_summary())

    def mark_interrupted(self) -> None:
        """Flip a still-running line to "未完成" when the turn ended without its
        paired done frame (gateway error, user interrupt). Only touches running
        blocks so a completed result is never overwritten."""
        if self.status != "running":
            return
        self.status = "interrupted"
        self.update(self.render_detail() if self.expanded else self.render_summary())


class ApprovalBlock(Static):
    # "ui": keeps its own CSS margin (it is a framed panel the user must act on),
    # so the transcript must not add a spacer line around it.
    block_group = "ui"

    def __init__(self, request_id: str, action: str, params: dict, risk: str) -> None:
        self.request_id = request_id
        self.action = action
        self.params = params
        self.risk = risk
        self.decision: str | None = None
        super().__init__(self._body())

    def _body(self) -> str:
        action = escape(str(self.action))
        alert = cog_glyph("approval_request")
        sep = f"[$text-muted]{GLYPHS.sep}[/]"
        if self.decision == "approve":
            return (
                f"[$warning]{alert}[/] {action} {sep} "
                f"[$success]{GLYPHS.ok} 已批准[/]"
            )
        if self.decision == "deny":
            return (
                f"[$warning]{alert}[/] {action} {sep} "
                f"[$error]{GLYPHS.fail} 已拒绝[/]"
            )
        lines = [
            f"[$warning]{alert} 需要确认:[/] [b]{action}[/b]",
            f"    [$text-muted]{escape(str(self.risk))}[/]",
        ]
        # This is the screen the user authorizes a high-risk action from, so the
        # parameters go one per line (a raw str(dict) wrapped into an unreadable
        # blob) with credential-shaped values masked (they used to be printed
        # verbatim, and /save wrote them to disk).
        param_lines = format_params(self.params, value_width=80)
        if param_lines:
            lines.append("    [$text-muted]参数:[/]")
            for entry in param_lines:
                lines.append(f"      [$text-muted]{escape(entry)}[/]")
        lines.append(
            "    [$success]\\[y] 批准[/]  [$error]\\[n] 拒绝[/]"
            "  [$warning]\\[a] 本会话始终允许[/]"
        )
        return "\n".join(lines)

    def mark(self, decision: str) -> None:
        self.decision = decision
        self.update(self._body())


def _coerce_options(options) -> list:
    """Normalize the raw ``options`` payload into a list of option entries.

    The schema says array-of-string, and validate_params now rejects anything
    else before the tool runs. This stays as a client-side guard because the
    TUI renders whatever the wire hands it (an older gateway, a replayed
    frame): a bare str must NOT be iterated, or its characters each become a
    separate choice — that is how a clarify rendered "[", "'", "全" … instead
    of the three options the model actually offered.

    A str holding a list literal is recovered so the real choices survive;
    ``ast.literal_eval`` is used rather than ``json.loads`` because the shape
    seen in practice is Python-style with single quotes ("['a','b']"), which is
    not valid JSON. literal_eval parses data only — it never executes code.
    Anything unrecoverable degrades to one single option, which is wrong-ish
    but harmless, and never a wall of one-character choices.
    """
    if options is None:
        return []
    if isinstance(options, str):
        text = options.strip()
        if not text:
            return []
        if text[0] in "[({":
            import ast
            try:
                parsed = ast.literal_eval(text)
            except (ValueError, SyntaxError, MemoryError, RecursionError):
                parsed = None
            if isinstance(parsed, (list, tuple, set)):
                return list(parsed)
            if isinstance(parsed, dict):
                return [parsed]
        return [options]
    if isinstance(options, (list, tuple)):
        return list(options)
    return [options]


def _option_to_pair(opt) -> tuple[str, str]:
    """Normalize a single clarify option to a (display, answer) pair.

    Plain strings pass through as both. Dict-shaped options (which the model may
    emit despite the string-only schema) show "value — description" but answer
    with the bare value: the description is a hint for the human, not part of
    the choice, so sending the whole rendered label back would feed the model
    prose it never offered as an option. Anything else falls back to str() for
    both."""
    if isinstance(opt, str):
        return opt, opt
    if isinstance(opt, dict):
        value = opt.get("value")
        desc = opt.get("description")
        if value is not None and desc:
            return f"{value} — {desc}", str(value)
        if value is not None:
            return str(value), str(value)
        s = _option_to_str(opt)
        return s, s
    s = _option_to_str(opt)
    return s, s


def _option_to_str(opt) -> str:
    """Display-only string for an option (used where the answer value is not
    needed, e.g. legacy call sites and the dict fallback in _option_to_pair)."""
    if isinstance(opt, str):
        return opt
    if isinstance(opt, dict):
        value = opt.get("value")
        desc = opt.get("description")
        if value is not None and desc:
            return f"{value} — {desc}"
        if value is not None:
            return str(value)
        if desc:
            return str(desc)
    return str(opt)


class ChoiceBlock(Static):
    """A clarify prompt: a question plus optional numbered choices. The user
    picks by number, arrows+enter, or free text. Rendering is a pure method so
    it is unit-testable without a live screen (like ApprovalBlock)."""

    # See ApprovalBlock: framed panel with its own CSS margin, no extra spacer.
    block_group = "ui"

    def __init__(self, clarify_id: str, question: str, options: list) -> None:
        self.clarify_id = clarify_id
        self.question = question
        # The clarify schema declares options as strings, but the model
        # sometimes returns richer objects like {"value": ..., "description":
        # ...}. Coerce every option to a display string at this boundary so the
        # rest of the flow (rendering, selection, the answer sent back to the
        # server) only ever deals with strings and never chokes on a dict.
        # _coerce_options first guarantees we have a real list to iterate, so a
        # str payload is never walked character by character.
        pairs = [_option_to_pair(o) for o in _coerce_options(options)]
        # Display labels (may be "value — description"); shown in the list.
        self.options = [d for d, _ in pairs]
        # Answer values (bare value for dicts); sent back to the server on pick.
        self._answers = [a for _, a in pairs]
        self.highlighted = 0
        self.answer: str | None = None
        # True once this prompt can no longer be answered (the turn it belonged
        # to ended server-side). The selection hints must stop advertising keys
        # that no longer do anything — a dead block that still reads "按数字选择"
        # is what made the TUI look frozen rather than finished.
        self.cancelled = False
        # Mirrors the app's _clarify_free_input for THIS block, purely so
        # render_body can surface the "Esc returns to the options" way back.
        self.free_input = False
        super().__init__(self.render_body())

    # A virtual "其他(自行输入)" entry is appended after the real options
    # whenever there are options, so picking it (by number, or arrows+enter)
    # switches to free-text input. Its index/number is len(self.options) /
    # len(self.options)+1. It is NOT stored in options/_answers — it has no
    # answer value; it is a mode switch, identified by is_free_input_option.
    @property
    def _free_input_index(self) -> int:
        return len(self.options)

    @property
    def _slot_count(self) -> int:
        # Navigable slots: real options plus the virtual free-input entry.
        return len(self.options) + 1

    def is_free_input_option(self, n: int) -> bool:
        # n is 1-based (as pressed / displayed). True for the virtual entry.
        return bool(self.options) and n == self._free_input_index + 1

    def highlighted_is_free_input(self) -> bool:
        return bool(self.options) and self.highlighted == self._free_input_index

    def render_body(self) -> str:
        q = escape(str(self.question))
        if self.answer is not None:
            return f"[$secondary]❓[/] {q} [$text-muted]—[/] [$success]已选:{escape(str(self.answer))}[/]"
        # A cancelled prompt keeps the question on screen (it is part of the
        # conversation) but drops every selection hint: the keys are gone, so
        # repeating them would send the user hunting for a working keystroke.
        if self.cancelled:
            return (
                f"[$secondary]❓[/] {q} [$text-muted]—[/] "
                f"[$text-muted]该提问已失效（当前轮已结束，未作选择）[/]"
            )
        if not self.options:
            return f"[$secondary]❓[/] {q}\n    [$text-muted](请输入回答)[/]"
        lines = [f"[$secondary]❓[/] [b]{q}[/b]"]
        # Real options followed by the virtual "其他(自行输入)" entry, so it
        # renders and highlights exactly like a normal numbered choice.
        labels = list(self.options) + ["其他（自行输入）"]
        for i, opt in enumerate(labels):
            # Only the first 9 slots get a number: quick-select bindings exist
            # for 1-9 only, and pressing "1" fires immediately, so there is no
            # way to type "10". Numbering them anyway advertised a shortcut that
            # could not be used — and when a clarify had 9+ options that included
            # the "其他" escape hatch, which then looked unreachable.
            # Unnumbered slots are still reachable with ↑↓ + enter.
            label = escape(str(opt))
            if i < 9:
                label = f"{i + 1}. {label}"
            if i == self.highlighted:
                lines.append(f"  [$primary]› {label}[/]")
            else:
                lines.append(f"    [$text-muted]{label}[/]")
        hint = (
            "(按数字选择 · ↑↓ 移动后回车 · 选\"其他\"可自行输入)"
            if len(labels) <= 9 else
            "(前 9 项可按数字选择 · 其余用 ↑↓ 移动后回车 · 选\"其他\"可自行输入)"
        )
        lines.append(f"    [$text-muted]{hint}[/]")
        # Only shown once the user has stepped into free-text entry. Stepping in
        # is easy (any printable character) but used to be irreversible, so the
        # way back needs advertising at the moment it applies.
        if self.free_input:
            lines.append(
                "    [$text-muted](输入框为空时按 Esc 可返回选项)[/]"
            )
        return "\n".join(lines)

    def move(self, delta: int) -> None:
        if not self.options:
            return
        self.highlighted = max(0, min(self._slot_count - 1, self.highlighted + delta))
        self.update(self.render_body())

    def option_for_number(self, n: int) -> str | None:
        # Return the ANSWER value, not the display label: for a dict option the
        # user sees "value — description" but the server must receive only value.
        # The virtual free-input entry has no answer value (see is_free_input_option).
        if not self.options or n < 1 or n > len(self.options):
            return None
        return self._answers[n - 1]

    def highlighted_option(self) -> str | None:
        if not self.options or self.highlighted >= len(self.options):
            return None
        return self._answers[self.highlighted]

    def mark(self, answer: str) -> None:
        self.answer = answer
        self.update(self.render_body())

    def set_free_input(self, value: bool) -> None:
        """Track whether the app is in free-text mode for this prompt, so the
        block can show/hide the way back to option picking."""
        if self.free_input == value:
            return
        self.free_input = value
        self.update(self.render_body())

    def mark_cancelled(self) -> None:
        """This prompt can no longer be answered. Answered blocks are left alone
        (their "已选" state is the accurate record); only an unanswered one is
        downgraded."""
        if self.answer is not None or self.cancelled:
            return
        self.cancelled = True
        self.update(self.render_body())
