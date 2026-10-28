#!/usr/bin/env python3
"""Workflow engine: chain multiple skills in sequence/parallel."""

import argparse
import json
import shlex
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path


def run_step(step):
    cmd = step["command"]
    print(f"  [RUN] {cmd}")
    # No shell=True. This engine takes commands from a JSON file or argv, and a
    # shell here turned every caller into arbitrary command execution: one
    # "command" string with `;` or `&&` chained anything it liked. Parsing with
    # shlex and passing a list means a step runs exactly one program with
    # exactly the arguments written, and shell metacharacters are inert data.
    # Steps that genuinely need a pipeline should invoke `sh -c` explicitly, so
    # the intent is visible in the workflow file.
    try:
        argv = shlex.split(cmd)
    except ValueError as e:
        raise RuntimeError(f"Cannot parse command {cmd!r}: {e}") from e
    if not argv:
        raise RuntimeError("Empty command")
    try:
        result = subprocess.run(
            argv, capture_output=True, text=True, timeout=step.get("timeout", 60)
        )
    except FileNotFoundError as e:
        # Most often a command written for a shell, e.g. "a; b" or "a && b".
        # Say so, rather than surfacing a bare FileNotFoundError on 'a;'.
        raise RuntimeError(
            f"Program {argv[0]!r} not found. Shell syntax (;, &&, |, >) is not "
            f"interpreted; wrap it explicitly as: sh -c '<your command>'"
        ) from e
    output = result.stdout.strip()
    if result.returncode != 0:
        err = result.stderr.strip()
        if step.get("ignore_error"):
            print(f"  [WARN] {err[:200]}")
        else:
            raise RuntimeError(f"Step failed: {err[:500]}")
    return {"command": cmd, "output": output, "returncode": result.returncode}


def run_workflow(workflow_def):
    results = []
    for stage in workflow_def.get("stages", []):
        print(f"\n== Stage: {stage.get('name', 'unnamed')} ==")
        steps = stage.get("steps", [])
        mode = stage.get("mode", "sequential")

        if mode == "parallel":
            with ThreadPoolExecutor(max_workers=min(len(steps), 4)) as pool:
                futures = {pool.submit(run_step, s): s for s in steps}
                for f in as_completed(futures):
                    try:
                        results.append(f.result())
                    except Exception as e:
                        if not stage.get("ignore_error"):
                            raise
                        print(f"  [ERROR] {e}")
        else:
            for step in steps:
                results.append(run_step(step))

    return results


def run_from_file(filepath):
    data = json.loads(Path(filepath).read_text())
    print(f"Workflow: {data.get('name', 'unnamed')}")
    results = run_workflow(data)
    print(f"\nCompleted: {len(results)} steps")
    return results


def run_inline(commands):
    workflow = {
        "name": "inline",
        "stages": [{"name": "main", "mode": "sequential",
                    "steps": [{"command": c} for c in commands]}]
    }
    return run_workflow(workflow)


def main():
    parser = argparse.ArgumentParser(description="Workflow engine")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("run")
    p.add_argument("file", help="Workflow JSON file")
    p = sub.add_parser("inline")
    p.add_argument("commands", nargs="+", help="Commands to chain")
    p = sub.add_parser("template")
    p.add_argument("--output", "-o", default="workflow.json")
    args = parser.parse_args()

    if args.cmd == "run":
        run_from_file(args.file)
    elif args.cmd == "inline":
        run_inline(args.commands)
    elif args.cmd == "template":
        template = {
            "name": "my-workflow",
            "stages": [
                {"name": "build", "mode": "sequential",
                 "steps": [{"command": "echo hello"}]},
                {"name": "test", "mode": "parallel",
                 "steps": [{"command": "echo test1"}, {"command": "echo test2"}]},
            ]
        }
        Path(args.output).write_text(json.dumps(template, indent=2))
        print(f"Template saved: {args.output}")
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
