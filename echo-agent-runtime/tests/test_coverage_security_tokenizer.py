"""表征测试 — security/tokenizer.py ShellTokenizer.tokenize()

覆盖目标：行 85-111 子命令拆分核心循环
  - 管道 |
  - 分号 ;
  - 逻辑与 &&
  - 逻辑或 ||
  - 反引号 `cmd`
  - 子shell $(cmd)
  - 嵌套子shell $(echo $(whoami))
  - 进程替换 <(cmd)
"""

from __future__ import annotations

import pytest

from echo_agent.security.tokenizer import ShellTokenizer


@pytest.fixture
def tok() -> ShellTokenizer:
    return ShellTokenizer()


class TestTokenizePipe:
    def test_single_pipe(self, tok):
        result = tok.tokenize("ls -la | grep foo")
        assert "ls -la" in result
        assert "grep foo" in result

    def test_multiple_pipes(self, tok):
        result = tok.tokenize("cat /etc/passwd | sort | uniq")
        assert "cat /etc/passwd" in result
        assert "sort" in result
        assert "uniq" in result


class TestTokenizeSemicolon:
    def test_single_semicolon(self, tok):
        result = tok.tokenize("echo hello; echo world")
        assert "echo hello" in result
        assert "echo world" in result

    def test_multiple_semicolons(self, tok):
        result = tok.tokenize("cd /tmp; ls; pwd")
        assert "cd /tmp" in result
        assert "ls" in result
        assert "pwd" in result


class TestTokenizeLogicalAnd:
    def test_logical_and(self, tok):
        result = tok.tokenize("mkdir foo && cd foo")
        assert "mkdir foo" in result
        assert "cd foo" in result

    def test_logical_and_chain(self, tok):
        result = tok.tokenize("git fetch && git merge && echo done")
        assert "git fetch" in result
        assert "git merge" in result
        assert "echo done" in result


class TestTokenizeLogicalOr:
    def test_logical_or(self, tok):
        result = tok.tokenize("test -f file.txt || echo missing")
        assert "test -f file.txt" in result
        assert "echo missing" in result


class TestTokenizeBacktick:
    def test_backtick_simple(self, tok):
        result = tok.tokenize("echo `whoami`")
        # backtick 内命令应被提取
        assert "whoami" in result

    def test_backtick_in_assignment(self, tok):
        result = tok.tokenize("X=`id -u`")
        assert "id -u" in result


class TestTokenizeSubshell:
    def test_subshell_dollar_paren(self, tok):
        result = tok.tokenize("echo $(whoami)")
        assert "whoami" in result

    def test_nested_subshell(self, tok):
        result = tok.tokenize("echo $(echo $(id -un))")
        # 最内层命令应被提取
        assert "id -un" in result

    def test_subshell_with_pipe(self, tok):
        result = tok.tokenize("X=$(cat /etc/hosts | grep local)")
        assert "cat /etc/hosts | grep local" in result or "grep local" in result


class TestTokenizeProcessSubstitution:
    def test_process_subst_read(self, tok):
        result = tok.tokenize("diff <(ls dir1) <(ls dir2)")
        assert "ls dir1" in result
        assert "ls dir2" in result


class TestTokenizeMixed:
    def test_pipe_and_semicolon(self, tok):
        result = tok.tokenize("ls | grep foo; echo done")
        assert any("ls" in r for r in result)
        assert "echo done" in result

    def test_subshell_and_pipe(self, tok):
        result = tok.tokenize("kill $(pgrep nginx) | tee log")
        assert "pgrep nginx" in result

    def test_empty_command_returns_stripped(self, tok):
        # 空命令兜底
        result = tok.tokenize("")
        assert result == [""]

    def test_plain_command_returns_itself(self, tok):
        result = tok.tokenize("ls -la")
        assert "ls -la" in result
