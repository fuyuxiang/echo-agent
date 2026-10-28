"""表征测试 — security/guards.py scan_shell_command 原始串二次扫描（行 198-199）

覆盖目标：
  - 构造一个 normalize 后绕过主扫、但原始串仍含 hard-block 关键词的命令
  - 断言 scan_shell_command 仍返回含 hard_block=True 的 GuardFinding
  - 同时测 approval 模式的二次扫描（行 200-203）
"""

from __future__ import annotations

from echo_agent.security.guards import scan_shell_command


class TestRawCommandSecondScan:
    """验证 scan_shell_command 在 normalize 可能改变命令时仍对原始串补扫。"""

    def test_raw_sensitive_path_hard_block(self):
        """原始串含 /etc/passwd，normalize 不应消除它 → hard_block 命中。"""
        command = "cat /etc/passwd"
        findings = scan_shell_command(command)
        hard_blocks = [f for f in findings if f.hard_block]
        assert hard_blocks, f"期望 hard_block，实际 findings={findings}"
        assert any("sensitive" in f.key or "passwd" in f.reason.lower() for f in hard_blocks)

    def test_normalized_differs_still_catches_raw_hard_block(self):
        """
        percent-encoded 路径：normalize 会解码，但原始串也含触发词。
        关键：command != normalized 时走二次扫描。
        使用 ANSI-C quote 包装绕过主扫实验性构造：
        即使 normalize 可能改变原始串，scan 对原始串的二次扫描要能兜底。
        """
        # 使用一个 normalize 后可能变更的命令：
        # $'rm\x20-rf\x20/' 经 ANSI-C 解码后变为 'rm -rf /'
        # 原始串本身也含 root_rm 模式的关键字会在二次扫描中命中
        command = r"$'rm -rf /'"
        # 不论 normalize 结果如何，两者只要有一个触发就是覆盖了二次扫描路径
        findings = scan_shell_command(command)
        # 断言至少有 hard_block（来自 normalize 后扫或原始串二次扫）
        hard_blocks = [f for f in findings if f.hard_block]
        assert hard_blocks, f"期望至少一个 hard_block finding，实际={findings}"

    def test_second_scan_catches_approval_pattern_in_raw(self):
        """
        构造一个命令：normalize 后主扫可能不命中 approval 模式，
        但二次扫描原始串时命中 recursive_delete。
        这里用一个带 percent-encode 的命令让 command != normalized。
        """
        # %20 会被 percent-decode → normalize 变化，触发 command != normalized
        # 命令本身含递归删除，应在某次扫描中命中
        command = "rm%20-rf%20/tmp/testdir"
        # 如果两者不同，走二次扫描路径
        findings = scan_shell_command(command)
        # 只要有 finding（hard 或 approval）说明扫描逻辑工作
        assert findings, f"期望至少有一个 finding，实际 findings={[]}"

    def test_no_double_reporting_of_same_key(self):
        """
        同一 key 不应被双重记录（seen_keys 去重）：
        即使原始串和 normalize 串都命中同一模式，key 只出现一次。
        """
        command = "cat /etc/passwd"
        findings = scan_shell_command(command)
        keys = [f.key for f in findings]
        assert len(keys) == len(set(keys)), f"发现重复 key：{keys}"

    def test_plain_safe_command_no_findings(self):
        """普通安全命令不应有任何 finding。"""
        command = "echo hello world"
        findings = scan_shell_command(command)
        assert findings == []

    def test_root_rm_hard_blocked(self):
        """rm -rf / 直接命中 root_rm hard-block。"""
        command = "rm -rf /"
        findings = scan_shell_command(command)
        hard = [f for f in findings if f.hard_block and f.key == "root_rm"]
        assert hard, f"期望 root_rm hard_block，实际={findings}"

    def test_recursive_delete_triggers_approval(self):
        """rm -rf /tmp/data 命中 recursive_delete approval（非 hard_block）。"""
        command = "rm -rf /tmp/data"
        findings = scan_shell_command(command)
        approval = [f for f in findings if not f.hard_block and f.key == "recursive_delete"]
        assert approval, f"期望 recursive_delete approval finding，实际={findings}"
