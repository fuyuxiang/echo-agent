#!/usr/bin/env python3
"""PreToolUse hook that blocks recursive ``grep``.

Recursive grep (``grep -r``/``-R``/``--recursive``/``-d recurse``/``rgrep`` ...)
walks an entire directory tree into memory and can OOM-kill the agent process on
large repos. The system prompt only *asks* the model to avoid it; this hook
turns that into a hard, deterministic block.

Protocol: read the PreToolUse envelope as JSON on stdin and signal the decision
to the runner:

    recursive grep -> deny:  exit 2 + a deny JSON on stdout (+ reason on stderr)
    anything else  -> allow: exit 0, nothing on stdout

Any unexpected condition falls through to "allow" (fail-open), matching the
runner's contract -- only an explicit deny blocks the tool call.

Detection is a pure function (``command_is_recursive``) with no I/O, so it is
trivially unit-testable -- run ``no-recursive-grep-guard.py --self-test``.
Parsing is a single quote-aware lexer (``lex``) shared by every stage:

  * a quoted span is one operand and is never read as a flag
    (``grep "rm -rf" log`` and ``grep "-r" file`` are not recursive);
  * shell ``#`` line-comments are dropped;
  * live command substitutions ``$(...)`` / backticks (unquoted or inside
    double quotes -- single quotes suppress them) are recursed into;
  * pipeline/compound operators (``| & ; ( ) { }`` + newlines) split segments,
    so a recursive flag must belong to grep, not e.g. ``ls -R | grep``;
  * ``sh``/``bash -c "<script>"`` inner scripts are recursed into;
  * transparent wrappers (``sudo``/``env``/``xargs``/...) are peeled, looking
    past their own flags/args for grep.
"""

from __future__ import annotations

import json
import re
import sys
from typing import NamedTuple

SHELLS = {"sh", "bash", "dash", "zsh", "ash", "ksh", "mksh"}
# Wrappers whose real command may sit behind their own flags/args.
WRAPPERS = {
    "sudo",
    "doas",
    "command",
    "env",
    "time",
    "nice",
    "nohup",
    "stdbuf",
    "exec",
    "xargs",
    "setsid",
}
GREPS = {"grep", "egrep", "fgrep"}
# grep options whose value is the following token (skip it so it is not a flag).
ARG_OPTS = {"-e", "-f", "-m", "-A", "-B", "-C", "-D"}
# Operators that separate pipeline segments / compounds (brace groups included).
OPERATOR_CHARS = set("|&;(){}\n\r")
ASSIGNMENT = re.compile(r"[A-Za-z_]\w*=")
# A short-flag cluster containing r/R -- grep's only r/R short flags both recurse.
SHORT_RECURSIVE = re.compile(r"-[A-Za-z]*[rR]")
MAX_DEPTH = 5

DENY_REASON = (
    "Blocked: recursive grep (grep -r/-R/--recursive/rgrep) can read an entire "
    "directory tree into memory and OOM-kill the agent process on large repos. Use the "
    "dedicated search tool instead, which streams ripgrep results safely."
)


class Tok(NamedTuple):
    """One lexed token."""

    text: str  # token text, with surrounding quotes removed
    quoted: bool  # any part came from inside quotes -> operand, never a flag
    op: bool  # True if this is a shell operator that separates segments


def _read_paren_subst(s: str, i: int) -> tuple[int, str]:
    """Read a ``$(...)`` body starting at the ``$`` (index ``i``). Parens are
    matched quote-aware so a ``)`` inside a string does not close it early.
    Returns ``(index_after_closing_paren, body)``."""
    n = len(s)
    i += 2  # skip "$("
    start = i
    depth = 1
    quote = None
    while i < n and depth > 0:
        c = s[i]
        if quote:
            if c == quote:
                quote = None
        elif c in ("'", '"'):
            quote = c
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                break
        i += 1
    return i + 1, s[start:i]


def _read_backtick_subst(s: str, i: int) -> tuple[int, str]:
    """Read a backtick substitution body starting at the opening backtick."""
    n = len(s)
    i += 1
    start = i
    while i < n and s[i] != "`":
        i += 1
    return i + 1, s[start:i]


def lex(s: str) -> tuple[list[Tok], list[str]]:
    """Single quote-aware pass over a command string. Returns the list of tokens
    plus the bodies of any *live* command substitutions (to be recursed into)."""
    tokens: list[Tok] = []
    subst: list[str] = []
    buf: list[str] = []
    quoted = False
    quote = None
    i, n = 0, len(s)

    def flush() -> None:
        nonlocal buf, quoted
        if buf or quoted:
            tokens.append(Tok("".join(buf), quoted, False))
        buf, quoted = [], False

    while i < n:
        c = s[i]
        if quote == "'":  # single quotes: everything literal, no substitution
            if c == "'":
                quote = None
            else:
                buf.append(c)
            i += 1
            continue
        if quote == '"':  # double quotes: literal text, but substitutions live
            if c == '"':
                quote = None
                i += 1
            elif c == "$" and i + 1 < n and s[i + 1] == "(":
                i, body = _read_paren_subst(s, i)
                subst.append(body)
            elif c == "`":
                i, body = _read_backtick_subst(s, i)
                subst.append(body)
            else:
                buf.append(c)
                i += 1
            continue
        # unquoted
        if c in ("'", '"'):
            quote = c
            quoted = True
            i += 1
        elif c == "#" and not buf and not quoted:
            break  # comment to end of line
        elif c == "$" and i + 1 < n and s[i + 1] == "(":
            i, body = _read_paren_subst(s, i)
            subst.append(body)
        elif c == "`":
            i, body = _read_backtick_subst(s, i)
            subst.append(body)
        elif c in " \t":
            flush()
            i += 1
        elif c in OPERATOR_CHARS:
            flush()
            tokens.append(Tok(c, False, True))
            i += 1
        else:
            buf.append(c)
            i += 1
    flush()
    return tokens, subst


def _split_segments(tokens: list[Tok]) -> list[list[Tok]]:
    """Cut a token list into pipeline segments on operator tokens."""
    segments: list[list[Tok]] = []
    current: list[Tok] = []
    for tok in tokens:
        if tok.op:
            if current:
                segments.append(current)
                current = []
        else:
            current.append(tok)
    if current:
        segments.append(current)
    return segments


def _basename(text: str) -> str:
    return text.rsplit("/", 1)[-1]


def _grep_args_recursive(args: list[Tok]) -> bool:
    """True if grep's argument list requests recursion."""
    skip_arg = False  # previous option consumes this token as its value
    dir_arg = False  # that consumed value belongs to -d / --directories
    for arg in args:
        if skip_arg:
            if dir_arg and arg.text == "recurse":
                return True
            skip_arg = dir_arg = False
            continue
        if arg.quoted:
            continue  # a quoted operand is literal text, never a flag
        t = arg.text
        if t == "--":
            return False  # end of options; nothing after it is a flag
        if t in ("-d", "--directories"):
            skip_arg = dir_arg = True
        elif t.startswith("--directories="):
            if t.endswith("=recurse"):
                return True
        elif t in ARG_OPTS:
            skip_arg = True
        elif t in ("--recursive", "--dereference-recursive") or SHORT_RECURSIVE.match(t):
            return True
    return False


def _shell_script_recursive(args: list[Tok], depth: int) -> bool:
    """Re-inspect a shell interpreter's ``-c`` script (and trailing words)."""
    i = 0
    while i < len(args) and not args[i].quoted and args[i].text.startswith("-"):
        i += 1  # skip the leading run of shell flags (-c, -lc, ...)
    script = " ".join(a.text for a in args[i:])
    return bool(script) and command_is_recursive(script, depth + 1)


def _segment_recursive(words: list[Tok], depth: int) -> bool:
    """True if one pipeline segment runs grep recursively."""
    idx = 0
    n = len(words)
    while idx < n and not words[idx].quoted and ASSIGNMENT.match(words[idx].text):
        idx += 1  # skip leading VAR=value assignments
    if idx >= n:
        return False
    base = _basename(words[idx].text)
    # Peel transparent wrappers: the real command may sit behind the wrapper's
    # own flags/args, so seek the next grep/shell among the following words.
    while base in WRAPPERS:
        idx += 1
        while idx < n:
            b = _basename(words[idx].text)
            if b == "rgrep" or b in GREPS or b in SHELLS:
                break
            idx += 1
        if idx >= n:
            return False
        base = _basename(words[idx].text)
    if base == "rgrep":
        return True
    if base in SHELLS:
        return _shell_script_recursive(words[idx + 1 :], depth)
    if base in GREPS:
        return _grep_args_recursive(words[idx + 1 :])
    return False


def command_is_recursive(cmd: str, depth: int = 0) -> bool:
    """Pure predicate: True if the shell command runs grep recursively."""
    if depth >= MAX_DEPTH:
        return False
    tokens, substitutions = lex(cmd)
    for body in substitutions:
        if command_is_recursive(body, depth + 1):
            return True
    return any(_segment_recursive(seg, depth) for seg in _split_segments(tokens))


def extract_command(envelope: object) -> str | None:
    """Pull toolInput.command out of the PreToolUse envelope, or None."""
    if not isinstance(envelope, dict):
        return None
    # Accept both the camelCase (toolInput) and snake_case (tool_input) shapes.
    tool_input = envelope.get("toolInput") or envelope.get("tool_input")
    if not isinstance(tool_input, dict):
        return None
    command = tool_input.get("command")
    return command if isinstance(command, str) and command else None


# Allow/deny cases exercised by --self-test; keep in sync with the README.
SELF_TEST_CASES: list[tuple[str, bool]] = [
    # --- recursive (deny) ---
    ("grep -r foo .", True),
    ("grep -R foo .", True),
    ("grep --recursive foo .", True),
    ("grep -rn TODO src", True),
    ("rgrep foo .", True),
    ("/usr/bin/grep -r x", True),
    ("egrep -r x .", True),
    ("FOO=bar grep -r x", True),
    ("cat x | grep -r y", True),
    ("sudo grep -r x", True),
    ("sudo -u root grep -r x", True),
    ("xargs -0 grep -r .", True),
    ("nice -n 10 grep -r x", True),
    ("grep -d recurse x", True),
    ('grep -d "recurse" x', True),
    ("grep --directories=recurse x", True),
    ('grep --include="*.rs" -r .', True),
    ('grep -e "p" -r .', True),
    ('bash -c "grep -r x"', True),
    ("bash -c grep -r x", True),
    ('echo "$(grep -r x)"', True),
    ("foo=$(grep -r x)", True),
    ("cat <(grep -r x)", True),
    ("{ grep -r x; }", True),
    ("grep -r x . # note", True),
    # --- not recursive (allow) ---
    ("grep foo file", False),
    ("grep -n foo file", False),
    ("ls -R | grep foo", False),
    ("grep -e -r file", False),
    ("grep -- -r file", False),
    ("grep -A 3 foo file", False),
    ("grep -d skip foo file", False),
    ('grep "rm -rf" log', False),
    ('grep "-r" file', False),
    ('grep --include="*.rs" foo file', False),
    ("grep foo file # uses -r", False),
    ("echo '$(grep -r x)'", False),
    ('echo "$(ls)"', False),
    ("sudo ls -R", False),
    ("echo grep -r as text", False),
    ("xargs grep foo", False),
    ("{ echo hi; }", False),
    ("", False),
]


def self_test() -> int:
    failures = 0
    for cmd, want in SELF_TEST_CASES:
        got = command_is_recursive(cmd)
        if got != want:
            failures += 1
            print(f"FAIL: {cmd!r} -> {got} (want {want})")
    total = len(SELF_TEST_CASES)
    print(f"{total - failures}/{total} passed")
    return 1 if failures else 0


def main() -> None:
    if "--self-test" in sys.argv[1:]:
        sys.exit(self_test())
    try:
        envelope = json.load(sys.stdin)
    except (ValueError, OSError):
        sys.exit(0)  # unparseable input -> fail open (allow)
    command = extract_command(envelope)
    if command is None or not command_is_recursive(command):
        sys.exit(0)  # nothing to block -> silent allow
    # Deny. Emit the echo-agent-native decision (read by this repo's runner) and the
    # Claude-style hookSpecificOutput for forward-compatibility, put the reason
    # on stderr for runners that surface it there, and exit 2 so any exit-code
    # based runner blocks too.
    print(
        json.dumps(
            {
                "decision": "deny",
                "reason": DENY_REASON,
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": DENY_REASON,
                },
            }
        )
    )
    print(DENY_REASON, file=sys.stderr)
    sys.exit(2)


if __name__ == "__main__":
    main()
