#!/usr/bin/env python3
"""Exercise a server interpreter outside its checkout, using a local model endpoint.

Run with an installed wheel's Python (with the openai extra):
  /path/to/venv/bin/python scripts/validate-server-process.py --output /tmp/echo-smoke
No real credentials, desktop, Node, or external model are used. Workspaces and
logs remain under --output. All spawned Gateway processes are stopped in finally.
"""
from __future__ import annotations

import argparse
import asyncio
import json
from importlib.util import find_spec
import os
from pathlib import Path
import re
import signal
import sys
import tempfile
import time

from aiohttp import ClientSession, ClientTimeout, web
import yaml


async def main(output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    model_requests = []
    from echo_agent.runtime_paths import bundled_skills_dir
    from echo_agent.skills.store import SkillStore

    builtin = bundled_skills_dir()
    assert builtin is not None, 'installed Runtime has no bundled skills'
    skills = SkillStore(user_dir=output / 'skill-check', builtin_dir=builtin).list_all()
    assert any(skill.name == 'calendar' for skill in skills), 'bundled calendar skill cannot be loaded'

    # These executables would fail and leave evidence if Runtime tried to
    # compile frontend/desktop sources on the server.
    blockers = output / 'blocked-toolchain'
    blockers.mkdir(exist_ok=True)
    toolchain_calls = output / 'unexpected-toolchain-calls.log'
    for command in ('node', 'npm', 'pnpm', 'npx', 'corepack', 'cargo', 'rustc'):
        executable = blockers / command
        executable.write_text('#!/bin/sh\nprintf "%s\\n" "$0" >> "$ECHO_SMOKE_TOOLCHAIN_CALLS"\nexit 97\n')
        executable.chmod(0o755)

    async def completion(request):
        body = await request.json()
        model_requests.append(body)
        messages = body['messages']
        last = messages[-1]
        if last.get('role') != 'tool':
            call = ('write_file', {'path': 'review-result.txt', 'content': 'installed-runtime-ok'})
        elif last.get('name') == 'write_file':
            call = ('read_file', {'path': 'review-result.txt'})
        elif last.get('name') == 'read_file':
            call = ('artifact_create', {'filename': 'review-report.txt'})
        elif last.get('name') == 'artifact_create':
            call = ('artifact_append', {'artifact_id': json.loads(last['content'])['artifact_id'],
                                        'sequence': 0, 'content': 'installed-runtime-ok'})
        elif last.get('name') in ('artifact_append', 'artifact_finalize'):
            created = next(m for m in reversed(messages) if m.get('role') == 'tool'
                           and m.get('name') == 'artifact_create')
            artifact_id = json.loads(created['content'])['artifact_id']
            call = ('artifact_finalize' if last['name'] == 'artifact_append' else 'artifact_deliver',
                    {'artifact_id': artifact_id})
        else:
            call = None
        message = {'role': 'assistant', 'content': None if call else 'installed-runtime-ok'}
        if call:
            message['tool_calls'] = [{'id': f'call_{len(model_requests)}', 'type': 'function',
                'function': {'name': call[0], 'arguments': json.dumps(call[1])}}]
        finish = 'tool_calls' if call else 'stop'
        if body.get('stream'):
            delta = {k: v for k, v in message.items() if k != 'role'}
            if call:
                delta['tool_calls'][0]['index'] = 0
            chunk = {'id': 'local-smoke', 'object': 'chat.completion.chunk', 'created': 1, 'model': 'smoke-model',
                     'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}]}
            return web.Response(text='data: ' + json.dumps(chunk) + '\n\ndata: [DONE]\n\n',
                                content_type='text/event-stream')
        return web.json_response({'id': 'local-smoke', 'object': 'chat.completion', 'created': 1,
            'model': 'smoke-model', 'choices': [{'index': 0, 'message': message, 'finish_reason': finish}],
            'usage': {'prompt_tokens': 10, 'completion_tokens': 5, 'total_tokens': 15}})

    app = web.Application()
    app.router.add_post('/v1/chat/completions', completion)
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, '127.0.0.1', 0)
    await site.start()
    provider_port = site._server.sockets[0].getsockname()[1]
    environment = {k: v for k, v in os.environ.items() if not k.startswith(('ECHO_AGENT_', 'GOOGLE_', 'GEMINI_'))}
    for inherited in ('PYTHONPATH', 'PYTHONHOME', 'DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS'):
        environment.pop(inherited, None)
    environment['PATH'] = str(blockers) + os.pathsep + str(Path(sys.executable).parent) + os.pathsep + os.defpath
    environment['ECHO_SMOKE_TOOLCHAIN_CALLS'] = str(toolchain_calls)
    environment.update({'_ECHO_AGENT_SUPERVISED': '1', 'ECHO_AGENT_DISABLE_LAZY_INSTALLS': '1',
                        'HF_HUB_OFFLINE': '1', 'NO_PROXY': '127.0.0.1,localhost', 'PYTHONUNBUFFERED': '1'})
    clean_home = output / 'home'
    clean_home.mkdir(exist_ok=True)
    environment.update({'HOME': str(clean_home), 'HF_HOME': str(clean_home / 'hf'),
                        'XDG_CACHE_HOME': str(clean_home / 'cache')})
    checks = []
    try:
        for prefix, ws_path, configured in [('/api/v1', '/ws', True), ('/review-api', '/review-ws', True),
                                             ('/empty-api', '/empty-ws', False)]:
            workspace = Path(tempfile.mkdtemp(prefix='workspace-', dir=output)).resolve()
            config = workspace / 'echo-agent.yaml'
            settings = {
                'workspace': str(workspace), 'security': {'profile': 'personal_cli'},
                'gateway': {'host': '127.0.0.1', 'port': 0, 'api_prefix': prefix, 'ws_path': ws_path,
                            'auth': {'admin_tokens': ['local-test-admin'], 'api_tokens': ['local-test-api']}},
                'models': {'default_model': 'smoke-model', 'providers': ([{
                    'name': 'openai', 'api_base': f'http://127.0.0.1:{provider_port}/v1',
                    'api_key': 'local-test-key', 'models': ['smoke-model'], 'max_retries': 0}] if configured else [])},
                'checkpoint': {'enabled': False}, 'memory': {'enabled': False}, 'knowledge': {'enabled': False}, 'skills': {'enabled': False},
                'planning': {'enabled': False}, 'multi_agent': {'enabled': False},
                'scheduler': {'enabled': False}, 'agent': {'heartbeat': {'enabled': False}},
                'tools': {'allow': ['read_file', 'write_file', 'artifact_create', 'artifact_append',
                                    'artifact_finalize', 'artifact_deliver'], 'restrict_to_workspace': True},
                'observability': {'otel_enabled': False},
            }
            config.write_text(yaml.safe_dump(settings))
            base_args = [sys.executable, '-I', '-m', 'echo_agent', 'gateway', '--config', str(config), '--workspace', str(workspace)]
            proc = None
            logfile = (workspace / 'gateway.log').open('ab')
            async def start():
                process = await asyncio.create_subprocess_exec(*base_args, cwd=output, env=environment,
                    stdin=asyncio.subprocess.DEVNULL, stdout=logfile, stderr=logfile)
                endpoint = workspace / '.echo-agent' / 'gateway.json'
                deadline = time.monotonic() + 45
                while time.monotonic() < deadline:
                    if process.returncode is not None:
                        raise AssertionError(f'Gateway exited {process.returncode}: {workspace / "gateway.log"}')
                    if endpoint.exists():
                        data = json.loads(endpoint.read_text())
                        return process, f'http://127.0.0.1:{data["port"]}'
                    await asyncio.sleep(.1)
                process.terminate()
                await process.wait()
                raise TimeoutError(f'Gateway startup: {workspace}')
            async def stop(process):
                if process and process.returncode is None:
                    process.send_signal(signal.SIGTERM)
                    try:
                        await asyncio.wait_for(process.wait(), 15)
                    except asyncio.TimeoutError:
                        process.kill()
                        await process.wait()
                        raise AssertionError('Gateway did not stop after SIGTERM')
            try:
                proc, base = await start()
                async with ClientSession(timeout=ClientTimeout(total=30)) as client:
                    async with client.get(base + prefix + '/health') as response:
                        assert response.status == 200
                    async with client.get(base + prefix + '/ready') as response:
                        assert response.status == (200 if configured else 503), await response.text()
                    async with client.get(base + '/') as response:
                        assert response.status == 200
                        page = await response.text()
                        assert '<html' in page.lower()
                        scripts = re.findall(r'<script[^>]+src="([^"]+)"', page)
                        assets = [path for path in scripts if path.startswith('/assets/') and path.endswith('.js')]
                        assert assets, 'Gateway served a fallback page instead of the compiled Dashboard'
                    for asset in assets:
                        async with client.get(base + asset) as response:
                            assert response.status == 200, asset
                            assert 'javascript' in response.headers.get('Content-Type', ''), asset
                            assert (await response.read()), asset
                    body = {'platform': 'api', 'user_id': 'smoke', 'chat_id': 'smoke',
                            'session_key': 'cli:installed-smoke', 'text': 'Create and read a harmless result file',
                            'wait': True, 'timeout_seconds': 25}
                    headers = {'Authorization': 'Bearer local-test-api', 'Idempotency-Key': 'installed-smoke'}
                    async with client.post(base + prefix + '/message', json=body) as response:
                        assert response.status == 401
                    async with client.post(base + prefix + '/message', headers=headers, json=body) as response:
                        data = await response.json()
                        assert data['status'] == ('completed' if configured else 'incomplete'), data
                    if not configured:
                        assert not (workspace / 'review-result.txt').exists()
                        checks.append({'prefix': prefix, 'checks': ['health', 'ready rejects unconfigured model',
                                                                  'error is incomplete'], 'workspace': str(workspace)})
                        continue
                    assert (workspace / 'review-result.txt').read_text() == 'installed-runtime-ok'
                    assert 'installed-runtime-ok' in json.dumps(data.get('reply', {}).get('tool_deliveries', [])), data
                    assert list((workspace / 'data' / 'artifacts').rglob('manifest.json'))
                    count = len(model_requests)
                    async with client.post(base + prefix + '/message', headers=headers, json=body) as response:
                        repeated = await response.json()
                        assert repeated['event_id'] == data['event_id'] and repeated['status'] == 'completed'
                    assert len(model_requests) == count, 'duplicate request re-executed the model/tool chain'
                    async with client.ws_connect(base + ws_path) as websocket:
                        await websocket.send_json({'type': 'auth', 'token': 'local-test-api', 'platform': 'ws',
                            'user_id': 'smoke', 'chat_id': 'ws-smoke', 'session_key': 'cli:installed-ws'})
                        while True:
                            frame = await asyncio.wait_for(websocket.receive_json(), 30)
                            if frame.get('type') in ('auth_ok', 'error'):
                                assert frame['type'] == 'auth_ok', frame
                                break
                        await websocket.send_json({'type': 'message', 'text': 'Create and read a harmless file',
                                                   'client_request_id': 'smoke-ws'})
                        while True:
                            frame = await asyncio.wait_for(websocket.receive_json(), 30)
                            if (frame.get('type') == 'message' and frame.get('is_final')
                                    and frame.get('metadata', {}).get('_turn_status')):
                                assert frame.get('metadata', {}).get('_turn_status') == 'completed', frame
                                break
                    async with client.ws_connect(base + '/ws/dashboard') as dashboard:
                        await dashboard.send_json({'type': 'auth', 'token': 'local-test-admin'})
                        frame = await asyncio.wait_for(dashboard.receive_json(), 10)
                        assert frame.get('type') == 'auth_ok', frame
                    # Same workspace with another ephemeral port must hit the instance lock.
                    duplicate = await asyncio.create_subprocess_exec(*base_args, cwd=output, env=environment,
                        stdin=asyncio.subprocess.DEVNULL, stdout=logfile, stderr=logfile)
                    assert await asyncio.wait_for(duplicate.wait(), 15) != 0
                    # An occupied port must fail before opening a second Runtime.
                    conflict = await asyncio.create_subprocess_exec(*base_args, '--port', base.rsplit(':', 1)[1],
                        cwd=output, env=environment, stdin=asyncio.subprocess.DEVNULL, stdout=logfile, stderr=logfile)
                    assert await asyncio.wait_for(conflict.wait(), 15) != 0
                    await stop(proc)
                    count = len(model_requests)
                    proc, base = await start()
                    async with client.post(base + prefix + '/message', headers=headers, json=body) as response:
                        recovered = await response.json()
                        assert recovered['event_id'] == data['event_id'] and recovered['status'] == 'completed', recovered
                    assert len(model_requests) == count, 'restart replayed already completed side effects'
                    checks.append({'prefix': prefix, 'checks': ['auth', 'HTTP SDK/tools/files/artifact delivery', 'idempotency',
                        'interactive WS', 'Dashboard WS', 'duplicate Runtime', 'port conflict', 'SIGTERM',
                        'restart durable result'], 'workspace': str(workspace)})
            finally:
                await stop(proc)
                logfile.close()
        assert not toolchain_calls.exists(), 'installed Runtime invoked a frontend/desktop toolchain'
        (output / 'process-results.json').write_text(json.dumps({'python': sys.executable,
            'platform': sys.platform, 'package_origin': find_spec('echo_agent').origin, 'model': 'local controlled OpenAI-compatible endpoint',
            'real_external_model': False, 'bundled_skills': len(skills), 'frontend_toolchain_invoked': False,
            'provider_requests': len(model_requests), 'cases': checks}, indent=2))
        print(json.dumps({'result': 'passed', 'cases': len(checks), 'evidence': str(output)}, ensure_ascii=False))
    finally:
        await runner.cleanup()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    asyncio.run(main(args.output.resolve()))
