#!/usr/bin/env python3
"""Run Dashboard review against a disposable source Runtime and local model.

Needs the Runtime's Python dependencies and an external Playwright installation.
Set PLAYWRIGHT_MODULE and optionally ECHO_UI_BROWSER_PATH. No external provider,
channel account, user workspace or existing service is used. All children stop
in finally; evidence and the disposable workspace remain under --output.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
import uuid

from aiohttp import web
import yaml


async def main(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    root = Path(__file__).resolve().parents[1]
    workspace = Path(tempfile.mkdtemp(prefix="workspace-", dir=output))
    model_calls = 0

    async def completion(request: web.Request) -> web.Response:
        nonlocal model_calls
        model_calls += 1
        body = await request.json()
        user = next((m.get("content", "") for m in reversed(body["messages"]) if m["role"] == "user"), "")
        reply = "UI_CONTROLLED_OK " + (user if isinstance(user, str) else "")
        common = {"id": "ui-local", "created": 1, "model": "ui-model"}
        marker = next((value for value in ("UI_MEMORY_OLD_681", "UI_MEMORY_CURRENT_927", "UI_MEMORY_AUTOMATIC_426")
                       if isinstance(user, str) and value in user), "")
        tool_call = None
        if marker and body["messages"][-1]["role"] != "tool":
            tool_call = {"id": "memory-" + uuid.uuid4().hex, "type": "function", "function": {
                "name": "memory", "arguments": json.dumps({
                    "action": "add", "target": "user", "key": "ui-review-preference",
                    "content": "UI_MEMORY_OLD_681" if marker == "UI_MEMORY_OLD_681" else "UI_MEMORY_CURRENT_927",
                    "source": "model_inferred" if marker == "UI_MEMORY_AUTOMATIC_426" else "user_stated",
                })}}
        if body.get("stream"):
            delta = {"tool_calls": [{"index": 0, **tool_call}]} if tool_call else {"content": reply}
            chunk = {**common, "object": "chat.completion.chunk", "choices": [
                {"index": 0, "delta": delta, "finish_reason": "tool_calls" if tool_call else "stop"}]}
            return web.Response(text="data: " + json.dumps(chunk) + "\n\ndata: [DONE]\n\n", content_type="text/event-stream")
        return web.json_response({**common, "object": "chat.completion", "choices": [
            {"index": 0, "message": {"role": "assistant", "content": "" if tool_call else reply,
                                     **({"tool_calls": [tool_call]} if tool_call else {})},
             "finish_reason": "tool_calls" if tool_call else "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}})

    model_app = web.Application()
    model_app.router.add_post("/v1/chat/completions", completion)
    model_runner = web.AppRunner(model_app)
    await model_runner.setup()
    model_site = web.TCPSite(model_runner, "127.0.0.1", 0)
    await model_site.start()
    model_port = model_site._server.sockets[0].getsockname()[1]
    admin, reader = "ui-admin-" + uuid.uuid4().hex, "ui-reader-" + uuid.uuid4().hex
    settings = {
        "workspace": str(workspace), "security": {"profile": "personal_cli"},
        "gateway": {"host": "127.0.0.1", "port": 0, "api_prefix": "/review-api", "ws_path": "/review-ws",
                    "auth": {"admin_tokens": [admin], "api_tokens": [reader]}},
        "models": {"default_model": "ui-model", "providers": [{"name": "ui-compatible", "models": ["ui-model"],
            "api_key": "local-only", "api_base": f"http://127.0.0.1:{model_port}/v1", "max_retries": 0}]},
        "memory": {"enabled": True, "vector_enabled": False, "rerank_enabled": False,
                   "memory_nudge_interval": 0, "consolidation_threshold": 10000},
        "knowledge": {"enabled": True, "auto_index": False},
        "checkpoint": {"enabled": False}, "planning": {"enabled": False}, "multi_agent": {"enabled": False},
        "scheduler": {"enabled": False}, "agent": {"heartbeat": {"enabled": False}},
        "skills": {"enabled": True, "allow_lazy_installs": False}, "mcp": {"enabled": False},
        "evolution": {"enabled": False}, "observability": {"otel_enabled": False},
    }
    config = workspace / "echo-agent.yaml"
    config.write_text(yaml.safe_dump(settings))
    config.chmod(0o600)
    environment = {k: v for k, v in os.environ.items() if not k.startswith((
        "ECHO_AGENT_", "OPENAI_", "ANTHROPIC_", "GOOGLE_", "GEMINI_", "AWS_"))}
    environment.update({"_ECHO_AGENT_SUPERVISED": "1", "ECHO_AGENT_DISABLE_LAZY_INSTALLS": "1",
                        "HF_HUB_OFFLINE": "1", "NO_PROXY": "127.0.0.1,localhost", "PYTHONUNBUFFERED": "1"})
    process = None
    browser_process = None
    logfile = (workspace / "gateway.log").open("ab")
    try:
        process = await asyncio.create_subprocess_exec(sys.executable, "-m", "echo_agent", "gateway", "--config", str(config),
            "--workspace", str(workspace), cwd=root, env=environment, stdin=asyncio.subprocess.DEVNULL,
            stdout=logfile, stderr=logfile)
        endpoint = workspace / ".echo-agent" / "gateway.json"
        deadline = time.monotonic() + 60
        while not endpoint.exists():
            if process.returncode is not None:
                raise RuntimeError(f"Gateway exited {process.returncode}; see {workspace / 'gateway.log'}")
            if time.monotonic() > deadline:
                raise TimeoutError(f"Gateway startup; see {workspace / 'gateway.log'}")
            await asyncio.sleep(.1)
        base = f"http://127.0.0.1:{json.loads(endpoint.read_text())['port']}"
        browser_env = {**environment, "ECHO_UI_BASE_URL": base, "ECHO_UI_TOKEN": admin,
                       "ECHO_UI_READER_TOKEN": reader, "ECHO_UI_OUTPUT": str(output)}
        for script in ("validate-gateway-web.mjs", "validate-dashboard-review.mjs"):
            browser_process = await asyncio.create_subprocess_exec("node", str(root / "scripts" / script),
                cwd=root, env=browser_env)
            code = await browser_process.wait()
            if code:
                raise AssertionError(f"{script} exited {code}")
        evidence = {"result": "passed", "source": str(root), "workspace": str(workspace),
                    "python": sys.version.split()[0], "model": "controlled loopback OpenAI-compatible endpoint",
                    "external_model": False, "model_requests": model_calls}
        (output / "source-runtime.json").write_text(json.dumps(evidence, indent=2))
        print(json.dumps(evidence))
    finally:
        if browser_process and browser_process.returncode is None:
            browser_process.terminate()
            await browser_process.wait()
        if process and process.returncode is None:
            process.send_signal(signal.SIGTERM)
            try:
                await asyncio.wait_for(process.wait(), 15)
            except asyncio.TimeoutError:
                process.kill()
                await process.wait()
        logfile.close()
        await model_runner.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    asyncio.run(main(parser.parse_args().output.resolve()))
