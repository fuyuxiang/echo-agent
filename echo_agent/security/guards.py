"""Runtime guards for shell and code execution."""

from __future__ import annotations

import re
import shlex
from dataclasses import dataclass
from typing import Any, Literal
from urllib.parse import urlparse

from echo_agent.security.normalizer import normalize_command
from echo_agent.security.tokenizer import ShellTokenizer


GuardAction = Literal["allow", "ask", "deny"]


@dataclass(frozen=True)
class GuardDecision:
    action: GuardAction = "allow"
    reason: str = ""
    pattern_key: str = ""
    approval_action: str = ""

    @property
    def needs_approval(self) -> bool:
        return self.action == "ask"

    @property
    def denied(self) -> bool:
        return self.action == "deny"


@dataclass(frozen=True)
class GuardFinding:
    key: str
    reason: str
    hard_block: bool = False


# Top-level directories whose recursive deletion has no recovery path. The list
# is deliberately small (mirrors hermes-agent's hardline set): only roots that
# brick the host. Recoverable mounts such as /tmp, /app, /opt are intentionally
# absent — they fall through to the recursive_delete soft-ask, where approval
# can still pass them, instead of being dead-blocked with no human override.
_ROOT_RM_DIRS = "home|root|etc|usr|var|bin|sbin|boot|lib|sys|proc|dev"

# Matches a command-start position: string start, a shell separator, or a
# transparent wrapper (sudo/env/exec/...). Used to anchor the shutdown family so
# it fires on `sudo reboot` but not on data like echo "we will REBOOT at noon".
_CMD_START = r"(?:^|[;&|\n`]|\$\(|\b(?:sudo|env|exec|nohup|setsid|time)\s+)\s*"


SHELL_HARD_PATTERNS: tuple[tuple[str, re.Pattern[str], str], ...] = (
    # Command-name patterns use re.I so case variants (RM, DD, MKFS) and
    # absolute-path invocations (/bin/RM) cannot dodge the block on a
    # case-insensitive filesystem (e.g. macOS, the dev platform here).
    #
    # root_rm enumerates the unrecoverable top-level targets (root itself plus
    # _ROOT_RM_DIRS) rather than matching any /<segment>: recoverable dirs like
    # /tmp or /app must stay out of the hardline and degrade to recursive_delete.
    # The trailing (?:\s|$) anchor excludes quotes, so a data string like
    # echo "rm -rf /" is not blocked and a quoted operand rm -rf "/" degrades to
    # soft-ask rather than a hard block.
    (
        "root_rm",
        re.compile(
            r"\brm\s+-[^\n;|&]*[rf][^\n;|&]*\s+/(?:" + _ROOT_RM_DIRS + r")?/?\*?(?:\s|$)",
            re.I,
        ),
        "destructive root removal",
    ),
    ("block_device_write", re.compile(r"\bdd\s+[^\n;|&]*\bof=/dev/", re.I), "destructive block device write"),
    ("mkfs", re.compile(r"\bmkfs(?:\.\w+)?\b", re.I), "filesystem formatting"),
    # The shutdown family is anchored to a command-start position so that common
    # English words (REBOOT, halt) inside quoted data do not trip a hard block.
    ("shutdown", re.compile(_CMD_START + r"(?:shutdown|reboot|halt|poweroff)\b", re.I), "system shutdown"),
    # Sensitive-path patterns also use re.I: these guard read access where
    # over-matching costs ~nothing, but on a case-insensitive filesystem a case
    # variant like /etc/PASSWD would otherwise slip past a case-sensitive guard.
    ("sensitive_account_file", re.compile(r"/(?:private/)?etc/(passwd|shadow|sudoers|gshadow)\b", re.I), "sensitive system account file"),
    ("root_secret_path", re.compile(r"(/root/\.ssh|/root/\.gnupg|/etc/ssh/)", re.I), "sensitive credential path"),
)

SHELL_APPROVAL_PATTERNS: tuple[tuple[str, re.Pattern[str], str], ...] = (
    ("recursive_delete", re.compile(r"\brm\s+-[^\n;|&]*r"), "recursive file deletion"),
    ("chmod_777", re.compile(r"\bchmod\s+777\b"), "world-writable permissions"),
    ("chown_root", re.compile(r"\bchown\s+[^;\n]*\broot\b"), "root ownership change"),
    ("service_control", re.compile(r"\b(systemctl|service)\s+(stop|restart|disable|mask)\b"), "service control"),
    ("kill_many", re.compile(r"\b(kill\s+-9\s+-1|pkill|killall)\b"), "broad process termination"),
    ("curl_pipe_shell", re.compile(r"\b(curl|wget)\b[^;\n|]*\|\s*(sh|bash|zsh)\b"), "download piped to shell"),
    ("inline_interpreter", re.compile(r"\b(python|python3|perl|ruby|node|sh|bash|zsh)\s+(-c|-e)\b"), "inline interpreter execution"),
    ("sensitive_project_write", re.compile(r">\s*[^\n;|&]*(\.env|id_rsa|credentials|secrets?)(?:\b|/)"), "write to credential-looking file"),
    ("sql_destructive", re.compile(r"\b(drop\s+table|truncate\s+table|delete\s+from\s+\w+\s*(?:;|$))", re.I), "destructive SQL"),
)

NETWORK_COMMANDS = frozenset({
    "curl",
    "dig",
    "ftp",
    "host",
    "nc",
    "netcat",
    "nslookup",
    "ping",
    "rsync",
    "scp",
    "sftp",
    "ssh",
    "telnet",
    "wget",
})

NETWORK_TEXT_RE = re.compile(r"\b(?:https?|ftp|ssh)://", re.I)


_INTERNAL_HOST_RE = re.compile(
    r"^(localhost|127\.\d+\.\d+\.\d+|10\.\d+\.\d+\.\d+|"
    r"172\.(1[6-9]|2\d|3[01])\.\d+\.\d+|192\.168\.\d+\.\d+|"
    r"\[?::1\]?|0\.0\.0\.0|169\.254\.\d+\.\d+|"
    r".*\.local|.*\.internal|.*\.corp|.*\.lan)$",
    re.I,
)


def _is_internal_url(url: str) -> bool:
    """Detect URLs targeting internal/private network addresses."""
    try:
        parsed = urlparse(url)
        host = (parsed.hostname or "").strip(".")
        if not host:
            return False
        return bool(_INTERNAL_HOST_RE.match(host))
    except Exception:
        return False
NETWORK_CODE_RE = re.compile(
    r"\b(import\s+(socket|requests|aiohttp|urllib|http\.client)|"
    r"from\s+(socket|requests|aiohttp|urllib|http)\s+import|"
    r"(requests|aiohttp|urllib|http\.client)\.)",
    re.I,
)
CODE_DANGEROUS_RE = re.compile(
    r"\b(os\.system|subprocess\.|eval\s*\(|exec\s*\(|__import__\s*\(|"
    r"child_process|ProcessBuilder|Runtime\.getRuntime)\b",
    re.I,
)
SENSITIVE_CODE_RE = re.compile(r"/(?:private/)?etc/(passwd|shadow|sudoers|gshadow)\b|/root/(\.ssh|\.gnupg)|/etc/ssh/")


def split_command(command: str) -> list[str]:
    try:
        return shlex.split(command, posix=True)
    except ValueError:
        return command.strip().split()


def command_name(command: str) -> str:
    parts = split_command(command)
    if not parts:
        return ""
    name = parts[0].rsplit("/", 1)[-1]
    if name in {"env", "command", "time", "nice"} and len(parts) > 1:
        return parts[1].rsplit("/", 1)[-1]
    return name


def command_uses_network(command: str) -> bool:
    if NETWORK_TEXT_RE.search(command):
        return True
    if NETWORK_CODE_RE.search(command):
        return True
    names = [part.rsplit("/", 1)[-1] for part in split_command(command)]
    return any(name in NETWORK_COMMANDS for name in names)


def scan_shell_command(command: str) -> list[GuardFinding]:
    findings: list[GuardFinding] = []
    seen_keys: set[str] = set()

    normalized = normalize_command(command)
    sub_commands = ShellTokenizer().tokenize(normalized)

    targets = [normalized] + [sc for sc in sub_commands if sc != normalized]

    for target in targets:
        for key, pattern, reason in SHELL_HARD_PATTERNS:
            if key not in seen_keys and pattern.search(target):
                findings.append(GuardFinding(key, reason, hard_block=True))
                seen_keys.add(key)
        for key, pattern, reason in SHELL_APPROVAL_PATTERNS:
            if key not in seen_keys and pattern.search(target):
                findings.append(GuardFinding(key, reason, hard_block=False))
                seen_keys.add(key)

    if command != normalized:
        for key, pattern, reason in SHELL_HARD_PATTERNS:
            if key not in seen_keys and pattern.search(command):
                findings.append(GuardFinding(key, reason, hard_block=True))
                seen_keys.add(key)
        for key, pattern, reason in SHELL_APPROVAL_PATTERNS:
            if key not in seen_keys and pattern.search(command):
                findings.append(GuardFinding(key, reason, hard_block=False))
                seen_keys.add(key)

    return findings


def scan_code(code: str, *, network_policy: str = "deny") -> list[GuardFinding]:
    findings: list[GuardFinding] = []
    if SENSITIVE_CODE_RE.search(code):
        findings.append(GuardFinding("sensitive_file", "sensitive system file access", hard_block=True))
    if network_policy == "deny" and (NETWORK_TEXT_RE.search(code) or NETWORK_CODE_RE.search(code)):
        findings.append(GuardFinding("network_denied", "network access is denied by execution policy", hard_block=True))
    elif NETWORK_TEXT_RE.search(code) or NETWORK_CODE_RE.search(code):
        findings.append(GuardFinding("network_access", "network access from code"))
    if CODE_DANGEROUS_RE.search(code):
        findings.append(GuardFinding("dynamic_execution", "dynamic process or interpreter execution"))
    return findings


def _ask_or_deny(ask: str, reason: str, pattern_key: str, approval_action: str) -> GuardDecision:
    if ask == "off":
        return GuardDecision("deny", reason=reason, pattern_key=pattern_key, approval_action=approval_action)
    return GuardDecision("ask", reason=reason, pattern_key=pattern_key, approval_action=approval_action)


def evaluate_shell_command(
    command: str,
    *,
    exec_policy: Any,
    network_policy: str,
    approval_action: str = "exec",
) -> GuardDecision:
    if not command.strip():
        return GuardDecision("deny", reason="empty command", pattern_key="empty", approval_action=approval_action)

    security = getattr(exec_policy, "security", "allowlist")
    ask = getattr(exec_policy, "ask", "on_miss")
    if security == "deny":
        return GuardDecision("deny", reason="exec security policy is deny", pattern_key="exec_denied", approval_action=approval_action)

    blocked = list(getattr(exec_policy, "blocked_commands", []) or [])
    normalized = normalize_command(command)
    for pattern in blocked:
        if pattern and (pattern in command or pattern in normalized):
            return GuardDecision("deny", reason=f"command contains blocked pattern '{pattern}'", pattern_key="blocked_command", approval_action=approval_action)

    if network_policy == "deny" and (command_uses_network(command) or command_uses_network(normalized)):
        return GuardDecision("deny", reason="network access is denied by execution policy", pattern_key="network_denied", approval_action=approval_action)

    findings = scan_shell_command(command)
    hard = next((finding for finding in findings if finding.hard_block), None)
    if hard:
        return GuardDecision("deny", reason=hard.reason, pattern_key=hard.key, approval_action=approval_action)

    if ask == "always":
        return GuardDecision("ask", reason="exec approval policy is always", pattern_key="ask_always", approval_action=approval_action)

    approvable = next((finding for finding in findings if not finding.hard_block), None)
    if approvable:
        return _ask_or_deny(ask, approvable.reason, approvable.key, approval_action)

    cmd = command_name(command)
    allowed = set(getattr(exec_policy, "allowed_commands", []) or [])
    safe_bins = set(getattr(exec_policy, "safe_bins", []) or [])
    if security == "allowlist" and cmd not in allowed and cmd not in safe_bins:
        return _ask_or_deny(ask, f"command '{cmd}' is not in the exec allowlist", "allowlist_miss", approval_action)

    return GuardDecision("allow", approval_action=approval_action)


def evaluate_code_execution(
    code: str,
    *,
    language: str,
    exec_policy: Any,
    network_policy: str,
    approval_action: str = "execute_code",
) -> GuardDecision:
    security = getattr(exec_policy, "security", "allowlist")
    ask = getattr(exec_policy, "ask", "on_miss")
    if security == "deny":
        return GuardDecision("deny", reason="exec security policy is deny", pattern_key="exec_denied", approval_action=approval_action)

    findings = scan_code(code, network_policy=network_policy)
    hard = next((finding for finding in findings if finding.hard_block), None)
    if hard:
        return GuardDecision("deny", reason=hard.reason, pattern_key=hard.key, approval_action=approval_action)

    if ask == "always":
        return GuardDecision("ask", reason="code execution approval policy is always", pattern_key="ask_always", approval_action=approval_action)

    approvable = next((finding for finding in findings if not finding.hard_block), None)
    if approvable:
        return _ask_or_deny(ask, approvable.reason, approvable.key, approval_action)

    if security == "allowlist" and ask != "off":
        return GuardDecision(
            "ask",
            reason=f"{language} code execution requires approval under allowlist security",
            pattern_key="code_execution",
            approval_action=approval_action,
        )

    return GuardDecision("allow", approval_action=approval_action)


def evaluate_tool_call(config: Any, tool_name: str, arguments: dict[str, Any]) -> GuardDecision:
    """Evaluate runtime safety before a tool call is executed."""
    exec_policy = config.tools.exec
    network_policy = config.execution.network_policy

    if tool_name == "exec":
        if not config.tools.exec.enabled:
            return GuardDecision("deny", reason="exec tool is disabled", pattern_key="tool_disabled", approval_action="exec")
        return evaluate_shell_command(
            str(arguments.get("command") or ""),
            exec_policy=exec_policy,
            network_policy=network_policy,
            approval_action="exec",
        )

    if tool_name == "execute_code":
        if not config.tools.exec.enabled or not config.tools.code_exec.enabled:
            return GuardDecision("deny", reason="code execution tool is disabled", pattern_key="tool_disabled", approval_action="execute_code")
        return evaluate_code_execution(
            str(arguments.get("code") or ""),
            language=str(arguments.get("language") or ""),
            exec_policy=exec_policy,
            network_policy=network_policy,
            approval_action="execute_code",
        )

    if tool_name == "process":
        if not config.tools.exec.enabled:
            return GuardDecision("deny", reason="process tool is disabled", pattern_key="tool_disabled", approval_action="process")
        action = arguments.get("action", "start")
        if action == "start":
            return evaluate_shell_command(
                str(arguments.get("command") or ""),
                exec_policy=exec_policy,
                network_policy=network_policy,
                approval_action="process",
            )
        if action == "signal":
            signal = str(arguments.get("signal") or "").upper()
            dangerous_signals = {"SIGKILL", "KILL", "9", "SIGTERM", "TERM", "15"}
            if signal in dangerous_signals:
                ask = getattr(exec_policy, "ask", "on_miss")
                return _ask_or_deny(ask, f"sending {signal} to process", "process_signal", "process")
            return GuardDecision("allow", approval_action="process")
        if action == "stdin":
            stdin_data = str(arguments.get("data") or arguments.get("input") or "")
            if stdin_data.strip():
                findings = scan_shell_command(stdin_data)
                hard = next((f for f in findings if f.hard_block), None)
                if hard:
                    return GuardDecision("deny", reason=f"stdin contains: {hard.reason}", pattern_key=hard.key, approval_action="process")
            return GuardDecision("allow", approval_action="process")
        return GuardDecision("allow", approval_action="process")

    if network_policy == "deny" and tool_name in {"web_fetch", "web_search"}:
        return GuardDecision("deny", reason="network access is denied by execution policy", pattern_key="network_denied", approval_action=tool_name)

    if tool_name == "web_fetch":
        url = str(arguments.get("url") or "")
        if _is_internal_url(url):
            ask = getattr(exec_policy, "ask", "on_miss")
            return _ask_or_deny(ask, f"web_fetch targeting internal/private URL: {url[:100]}", "ssrf_internal", "web_fetch")

    return GuardDecision("allow", approval_action=tool_name)
