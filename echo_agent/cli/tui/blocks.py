"""Transcript block widgets. Rendering logic is kept in plain methods
(render_summary/render_detail) so it's unit-testable without a live screen."""

from __future__ import annotations

import os

from rich.markdown import Markdown
from rich.markup import escape
from rich.table import Table
from rich.text import Text
from rich.theme import Theme as RichTheme
from textual.widgets import Static

from echo_agent.cli.tui.protocol import CogEvent

_ICON = {
    "memory_recalled": "🧠", "memory_written": "✍", "thinking": "💭",
    "tool_call": "🔧", "approval_request": "⚠️", "cost_update": "💰",
    "heartbeat": "⏳", "evolution": "🧬",
}

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


class ExpandableBlock(Static):
    """A Static whose summary/detail flips via ``toggle()``, wired to real user
    input: it is focusable (Tab/arrow reach it), clickable (mouse), and toggles
    on Enter/Space. Subclasses implement render_summary/render_detail and set
    ``expanded``. Previously ToolCallBlock/CognitiveBlock had toggle() but no
    input path, so their detail/diff view was unreachable except via the
    last-block ctrl+r/ctrl+o shortcuts — this makes every such block openable."""

    can_focus = True

    def render_summary(self) -> str:  # pragma: no cover - overridden
        raise NotImplementedError

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


def format_params(params: dict, *, value_width: int = 60) -> list[str]:
    """Render call parameters as one ``key=value`` line per entry, with secrets
    masked.

    Both the approval panel and the tool detail view used to print ``str(dict)``,
    i.e. a raw Python repr: it wrapped unreadably for anything non-trivial and
    leaked credentials verbatim on the exact screen where the user is asked to
    authorize a high-risk action.
    """
    lines: list[str] = []
    for key, value in (params or {}).items():
        if _is_secret_key(key):
            shown = _mask(value)
        else:
            shown = _clip(value, value_width)
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
    def __init__(self, text: str) -> None:
        # Keep the sigil-free original so /copy can export a clean transcript
        # without the "❯ " decoration.
        self.raw_text = text
        self.text_content = f"❯ {text}"
        # Markup keeps the sigil in the accent colour and the task text bright,
        # so the title reads as the strongest element in each turn.
        super().__init__(f"[bold $primary]❯[/] [b]{escape(text)}[/b]")


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
    ``set_final`` stays a plain-text path for status lines (heartbeat/error)
    that reuse this widget but carry hand-built Rich markup, not markdown."""

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
        super().__init__("[$primary]●[/] ")

    @property
    def text(self) -> str:
        """The reply body as plain text (markdown source / status line),
        without the ``●`` sigil — used by /copy."""
        return self._buf

    def append_token(self, t: str) -> None:
        self._buf += t
        self._is_markdown = False
        self.update(f"[$primary]●[/] {escape(self._buf)}")

    def clear_stream(self) -> None:
        """Drop the streamed text, keeping the widget in place.

        Used when the server retracts an optimistic draft: the next iteration's
        tokens must start from empty rather than extend the abandoned text. The
        widget stays mounted so the reply keeps its position in the transcript.
        """
        self._buf = ""
        self._is_markdown = False
        self.update("[$primary]●[/] ")

    def set_final(self, text: str) -> None:
        self._buf = text
        self._is_markdown = False
        self.update(f"[$primary]●[/] {escape(self._buf)}")

    def set_markup(self, markup: str) -> None:
        """Render pre-built Rich markup verbatim (NOT escaped), keeping the ●
        sigil. Used for client-local notices (/help, /theme) whose markup we
        author ourselves and trust — unlike streamed reply text, which is always
        escaped. _buf keeps a plain-ish copy so /copy stays harmless if reached
        (notices set is_status and are skipped anyway)."""
        self._buf = markup
        self._is_markdown = False
        self.update(f"[$primary]●[/] {markup}")

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
            Text("●", style=self._bullet_color()),
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
    def __init__(self, ev: CogEvent) -> None:
        self.ev = ev
        self.expanded = False
        super().__init__(self.render_summary())

    def render_summary(self) -> str:
        icon = _ICON.get(self.ev.cog_type, "•")
        hint = " (ctrl+r)" if self.ev.cog_type == "memory_recalled" else (
            " (ctrl+o)" if self.ev.cog_type == "thinking" else "")
        # Cognitive traces are secondary information — render the whole line in
        # the muted indigo tone so it recedes behind replies and tool actions.
        return f"[$secondary]{icon}[/] [$text-muted]{escape(self.ev.summary)}{escape(hint)}[/]"

    def render_detail(self) -> str:
        d = self.ev.data
        lines = [self.render_summary()]
        for it in d.get("items", []):
            src = it.get("source", "")
            badge = f"\\[{escape(src)}]" if src else ""
            content = escape(str(it.get("content", "")))
            lines.append(f"    [$text-muted]·[/] {content} [$text-muted]{badge}[/]".rstrip())
        if self.ev.cog_type == "thinking" and d.get("text"):
            lines.append(f"    [$text-muted]{escape(str(d['text']))}[/]")
        return "\n".join(lines)


class ToolCallBlock(ExpandableBlock):
    """One tool invocation. Flips in place from running (🔧 … ) to done
    (🔧 … · summary ✓/✗). Paired across the two frames by tool_call_id."""

    def __init__(
        self,
        tool_call_id: str,
        name: str,
        params: dict,
        status: str = "running",
        result_meta: dict | None = None,
        result_text: str = "",
        duration_ms: int | None = None,
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
        self.expanded = False
        super().__init__(self.render_summary())

    def render_summary(self) -> str:
        verb = escape(humanize_tool(self.tool_name))
        obj = escape(pick_object(self.tool_name, self.params))
        # verb in accent, operand muted so the eye separates "what" from "on what".
        head = f"🔧 [b]{verb}[/b]"
        if obj:
            head += f" [$text-muted]{obj}[/]"
        if self.status == "running":
            return f"{head} [$text-muted]…[/]"
        if self.status == "interrupted":
            # The turn ended (error / interrupt) before this tool's paired done
            # frame arrived. Say so instead of leaving the running "…", which
            # read as "still executing" for work that had already stopped.
            return f"{head} [$text-muted]·[/] [$text-muted]未完成[/] [$warning]—[/]"
        ok = self.status == "ok"
        mark = "[$success]✓[/]" if ok else "[$error]✗[/]"
        summary = escape(summarize_result(
            self.tool_name, self.result_meta, self.result_text, ok
        ))
        tone = "$text-muted" if ok else "$error"
        return f"{head} [$text-muted]·[/] [{tone}]{summary}[/] {mark}"

    def render_detail(self) -> str:
        lines = [self.render_summary()]
        if self.params:
            # One line per parameter with secrets masked — a raw str(dict) both
            # wrapped unreadably and leaked credentials into the transcript. The
            # "参数" header keeps the group labelled now that the entries are
            # split across lines.
            lines.append("    [$text-muted]↳ 参数[/]")
            for entry in format_params(self.params):
                lines.append(f"      [$text-muted]{escape(entry)}[/]")
        if self.result_text:
            # Edit-family tools return a diff — color it so added/removed lines
            # read at a glance. Everything else keeps the compact text preview.
            looks_like_diff = "\n" in self.result_text and any(
                ln[:1] in "+-@" for ln in self.result_text.splitlines()
            )
            if self.tool_name in _DIFF_TOOLS and looks_like_diff:
                lines.append("    [$text-muted]↳ 变更[/]")
                lines.append(colorize_diff(self.result_text))
            else:
                lines.append(
                    f"    [$text-muted]↳ 结果 {escape(_clip(self.result_text, 200))}[/]"
                )
        return "\n".join(lines)

    def mark_done(
        self, status: str, result_meta: dict | None, result_text: str, duration_ms: int | None
    ) -> None:
        self.status = status
        self.result_meta = result_meta
        self.result_text = result_text
        self.duration_ms = duration_ms
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
    def __init__(self, request_id: str, action: str, params: dict, risk: str) -> None:
        self.request_id = request_id
        self.action = action
        self.params = params
        self.risk = risk
        self.decision: str | None = None
        super().__init__(self._body())

    def _body(self) -> str:
        action = escape(str(self.action))
        if self.decision == "approve":
            return f"[$warning]⚠️[/] {action} — [$success]✅ 已批准[/]"
        if self.decision == "deny":
            return f"[$warning]⚠️[/] {action} — [$error]❌ 已拒绝[/]"
        lines = [
            f"[$warning]⚠️ 需要确认:[/] [b]{action}[/b]",
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

    def __init__(self, clarify_id: str, question: str, options: list) -> None:
        self.clarify_id = clarify_id
        self.question = question
        # The clarify schema declares options as strings, but the model
        # sometimes returns richer objects like {"value": ..., "description":
        # ...}. Coerce every option to a display string at this boundary so the
        # rest of the flow (rendering, selection, the answer sent back to the
        # server) only ever deals with strings and never chokes on a dict.
        pairs = [_option_to_pair(o) for o in (options or [])]
        # Display labels (may be "value — description"); shown in the list.
        self.options = [d for d, _ in pairs]
        # Answer values (bare value for dicts); sent back to the server on pick.
        self._answers = [a for _, a in pairs]
        self.highlighted = 0
        self.answer: str | None = None
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
