#!/usr/bin/env python3
"""Safe Python code execution with resource limits."""

import argparse
import ast
import os
import platform
import subprocess
import sys
import tempfile
from pathlib import Path

MAX_MEMORY_MB = 256
MAX_OUTPUT_BYTES = 10000

BLOCKED_PATTERNS = [
    "os.system", "os.exec", "os.popen", "os.remove", "os.rmdir",
    "subprocess", "shutil.rmtree", "__import__",
    "importlib", "ctypes", "open('/etc", "open('/root",
]

ALLOWED_MODULES = {
    "math", "statistics", "decimal", "fractions", "random",
    "json", "csv", "re", "datetime", "collections", "itertools",
    "functools", "operator", "string", "textwrap", "hashlib",
    "base64", "urllib.parse", "pathlib",
}


def check_safety(code: str) -> list[str]:
    violations = []
    for pattern in BLOCKED_PATTERNS:
        if pattern in code:
            violations.append(f"Blocked pattern: {pattern}")
    try:
        tree = ast.parse(code)
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name.split(".")[0] in ("subprocess", "ctypes", "importlib"):
                        violations.append(f"Blocked import: {alias.name}")
            elif isinstance(node, ast.ImportFrom):
                if node.module and node.module.split(".")[0] in ("subprocess", "ctypes", "importlib"):
                    violations.append(f"Blocked import from: {node.module}")
    except SyntaxError as e:
        violations.append(f"Syntax error: {e}")
    return violations


def _build_sandbox_env():
    env = {"PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ.get("PATH", "")}
    for key in ("HOME", "LANG", "LC_ALL"):
        if key in os.environ:
            env[key] = os.environ[key]
    return env


def _resource_preexec(memory_mb):
    def _set_limits():
        if platform.system() != "Darwin":
            import resource
            mem_bytes = memory_mb * 1024 * 1024
            resource.setrlimit(resource.RLIMIT_AS, (mem_bytes, mem_bytes))
            resource.setrlimit(resource.RLIMIT_NPROC, (32, 32))
    return _set_limits


def execute(code: str, timeout: int = 30) -> dict:
    violations = check_safety(code)
    if violations:
        return {"success": False, "error": "Safety check failed: " + "; ".join(violations), "stdout": "", "stderr": ""}

    sandbox_dir = tempfile.mkdtemp(prefix="echo_sandbox_")
    script_path = os.path.join(sandbox_dir, "script.py")
    with open(script_path, "w") as f:
        f.write(code)

    try:
        result = subprocess.run(
            [sys.executable, script_path],
            capture_output=True, text=True, timeout=timeout,
            env=_build_sandbox_env(),
            cwd=sandbox_dir,
            preexec_fn=_resource_preexec(MAX_MEMORY_MB),
        )
        return {
            "success": result.returncode == 0,
            "stdout": result.stdout[:MAX_OUTPUT_BYTES],
            "stderr": result.stderr[:5000],
            "returncode": result.returncode,
        }
    except subprocess.TimeoutExpired:
        return {"success": False, "error": f"Timeout after {timeout}s", "stdout": "", "stderr": ""}
    finally:
        import shutil
        shutil.rmtree(sandbox_dir, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description="Safe Python code executor")
    parser.add_argument("code", nargs="?", help="Code string to execute")
    parser.add_argument("--file", "-f", help="Execute from file")
    parser.add_argument("--timeout", "-t", type=int, default=30)
    parser.add_argument("--check-only", action="store_true", help="Only check safety")
    args = parser.parse_args()

    if args.file:
        code = Path(args.file).read_text()
    elif args.code:
        code = args.code
    else:
        code = sys.stdin.read()

    if args.check_only:
        violations = check_safety(code)
        if violations:
            print("UNSAFE:", "; ".join(violations))
            sys.exit(1)
        print("SAFE")
        sys.exit(0)

    result = execute(code, timeout=args.timeout)
    if result["success"]:
        print(result["stdout"])
    else:
        print(f"Error: {result.get('error', result.get('stderr', 'Unknown'))}", file=sys.stderr)
        if result.get("stdout"):
            print(result["stdout"])
        sys.exit(1)


if __name__ == "__main__":
    main()
