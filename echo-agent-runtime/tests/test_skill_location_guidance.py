import json
from pathlib import Path

import pytest

from echo_agent.agent.tools.skill_run import SkillRunTool
from echo_agent.agent.tools.skills import SkillViewTool
from echo_agent.config.loader import resolve_config_data
from echo_agent.skills.store import SkillStore


@pytest.fixture
def bundled_store(tmp_path):
    return SkillStore(user_dir=tmp_path / 'empty-workspace' / 'data' / 'skills',
                      builtin_dir=Path(__file__).resolve().parents[1] / 'skills')


@pytest.mark.asyncio
async def test_bundled_script_readable_with_empty_workspace(bundled_store):
    for name, script in [('web-search', 'scripts/web_search.py'),
                         ('web-extract', 'scripts/extract_url.py'),
                         ('code-runner', 'scripts/safe_exec.py')]:
        result = await SkillViewTool(bundled_store).execute({'name': name, 'file_path': script})
        assert result.success and 'def ' in result.output
    assert list(bundled_store.user_dir.iterdir()) == []


@pytest.mark.asyncio
async def test_skill_view_identifies_policy_restriction(bundled_store):
    cfg = resolve_config_data({'security': {'profile': 'daemon'}, 'tools': {'profile': 'coding'}})
    result = await SkillViewTool(bundled_store, config=cfg).execute({'name': 'web-search'})
    assert 'scripts/web_search.py' in result.output
    assert 'not necessarily in the workspace' in result.output
    assert 'not included by tools.profile=coding' in result.output
    assert 'not a missing script or execution backend' in result.output


@pytest.mark.asyncio
async def test_skill_view_explains_pinned_runner_when_enabled(bundled_store):
    cfg = resolve_config_data({'security': {'profile': 'daemon'},
                               'tools': {'profile': 'coding', 'also_allow': ['skill_run', 'web_fetch']}})
    result = await SkillViewTool(bundled_store, config=cfg).execute({'name': 'web-search'})
    assert "skill_run(name='web-search'" in result.output
    assert "skill_view(name='web-search'" in result.output
    assert 'Execution unavailable' not in result.output


@pytest.mark.asyncio
async def test_existing_runner_executes_script_from_builtin_directory(tmp_path):
    builtin = tmp_path / 'bundled' / 'probe'
    (builtin / 'scripts').mkdir(parents=True)
    (builtin / 'SKILL.md').write_text('---\nname: probe\ndescription: probe\n---\n')
    (builtin / 'asset.txt').write_text('builtin-asset')
    (builtin / 'scripts' / 'probe.py').write_text(
        'import json,sys\nfrom pathlib import Path\n'
        'print(json.dumps({"cwd":str(Path.cwd()),"python":sys.executable,'
        '"asset":Path("asset.txt").read_text()}))\n')
    store = SkillStore(user_dir=tmp_path / 'empty-workspace', builtin_dir=builtin.parent)
    result = await SkillRunTool(store).execute({'name': 'probe', 'script': 'scripts/probe.py'})
    assert result.success, result.error
    output = json.loads(result.output)
    assert output['cwd'] == str(builtin)
    assert output['asset'] == 'builtin-asset'
    import sys
    assert output['python'] == sys.executable


@pytest.mark.parametrize('tools,execution,reason', [
    ({'profile': 'coding', 'allow': ['skill_view'], 'also_allow': ['skill_run']}, {}, 'exclusive tools.allow'),
    ({'profile': 'full', 'also_allow': ['skill_run'], 'deny': ['skill_run']}, {}, 'tools.deny'),
    ({'profile': 'full'}, {}, 'security.profile=daemon'),
])
@pytest.mark.asyncio
async def test_skill_view_reports_actual_exclusive_and_denied_policy(bundled_store, tools, execution, reason):
    cfg = resolve_config_data({'security': {'profile': 'daemon'}, 'tools': tools, 'execution': execution})
    result = await SkillViewTool(bundled_store, config=cfg).execute({'name': 'web-search'})
    assert 'Execution unavailable' in result.output
    assert reason in result.output


@pytest.mark.parametrize('settings,tool,reason', [
    ({'tools': {'profile': 'coding'}}, 'skill_run', 'tools.profile=coding'),
    ({'tools': {'profile': 'full', 'deny': ['skill_run']}}, 'skill_run', 'tools.deny'),
    ({'tools': {'profile': 'full', 'web': {'enabled': True}}, 'execution': {'network_policy': 'deny'}},
     'web_fetch', 'network_policy=deny'),
    ({'tools': {'profile': 'full', 'web': {'enabled': True}}}, 'web_search', 'API key not configured'),
    ({'tools': {'profile': 'full', 'web': {'enabled': True, 'search_provider': 'searxng'}}},
     'web_search', 'api_base not configured'),
])
def test_status_distinguishes_policy_and_search_configuration(settings, tool, reason):
    from echo_agent.cli.status import _capability_report
    cfg = resolve_config_data({'security': {'profile': 'daemon'}, **settings})
    items = {item['name']: item for item in _capability_report(cfg, entrypoint='gateway')['tools']}
    assert items[tool]['available'] is False
    assert reason in items[tool]['reason']
