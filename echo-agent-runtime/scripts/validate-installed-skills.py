"""Validate installed skill resources and execution outside the source tree.

Run with the installed environment's Python using -I (no inherited PYTHONPATH).
No public network, model keys or dependency installations are used by this probe.
"""
from __future__ import annotations

import argparse
import asyncio
from importlib import metadata
import importlib.util
from importlib.resources import files
import json
from pathlib import Path
import tempfile


async def validate(extra: str, output: Path) -> None:
    from echo_agent.agent.tools.skill_run import SkillRunTool
    from echo_agent.agent.tools.skills import SkillViewTool
    from echo_agent.config.loader import resolve_config_data
    from echo_agent.runtime_paths import bundled_skills_dir
    from echo_agent.security.tool_policy import tool_policy_decision
    from echo_agent.skills.store import SkillStore

    assert files('echo_agent').joinpath('config/default.yaml').is_file()
    assert files('echo_agent').joinpath('skills/web_search.py').is_file()
    builtin = bundled_skills_dir()
    assert builtin is not None
    with tempfile.TemporaryDirectory(prefix='echo-installed-skills-') as directory:
        store = SkillStore(user_dir=Path(directory) / 'data/skills', builtin_dir=builtin)
        checked = []
        for name, script in [('web-search', 'scripts/web_search.py'),
                             ('web-extract', 'scripts/extract_url.py'),
                             ('code-runner', 'scripts/safe_exec.py')]:
            for path in ('SKILL.md', script):
                result = await SkillViewTool(store).execute({'name': name, 'file_path': path})
                assert result.success and result.output, (name, path, result.error)
                checked.append(f'{name}/{path}')
        assert not list(store.user_dir.iterdir())
        run = await SkillRunTool(store).execute({'name': 'code-runner', 'script': 'scripts/safe_exec.py',
                                                'args': ['print(6 * 7)']})
        assert run.success and run.output.strip() == '42', (run.error, run.output)
        for tools, execution, name, expected in [
            ({'profile': 'coding'}, {}, 'skill_run', False),
            ({'profile': 'coding', 'also_allow': ['skill_run', 'web_fetch']}, {}, 'skill_run', True),
            ({'profile': 'coding', 'also_allow': ['skill_run', 'web_fetch']}, {}, 'web_fetch', True),
            ({'allow': ['skill_view'], 'also_allow': ['skill_run']}, {}, 'skill_run', False),
            ({'also_allow': ['skill_run'], 'deny': ['skill_run']}, {}, 'skill_run', False),
            ({'also_allow': ['web_fetch']}, {'network_policy': 'deny'}, 'web_fetch', False),
        ]:
            cfg = resolve_config_data({'security': {'profile': 'daemon'}, 'tools': tools, 'execution': execution})
            assert tool_policy_decision(cfg, name)[0] is expected
        from echo_agent.skills import web_search
        assert callable(web_search.search_text)
        versions = {}
        if extra in {'skills', 'all'}:
            from ddgs import DDGS
            from ddgs.ddgs import DDGS as DDGSImplementation
            import trafilatura
            from lxml import html
            assert DDGS and trafilatura and html
            for distribution in ('ddgs', 'trafilatura', 'lxml'):
                versions[distribution] = metadata.version(distribution)
            # Execute both callers against the new package with a controlled
            # result, proving neither relies on an old package in the venv.
            from unittest.mock import patch

            path = store.find_skill_dir('deep-research') / 'scripts/research_report.py'
            spec = importlib.util.spec_from_file_location('installed_research_report', path)
            research = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(research)
            assert research.search_text is web_search.search_text
            expected = [{'title': 'source', 'href': 'https://source.example/', 'body': 'controlled evidence'}]
            with patch.object(web_search, '_search_bing_rss', return_value=[]), \
                    patch.object(DDGSImplementation, 'text', return_value=expected):
                assert web_search.search_text('test query') == expected
                assert research.search_multi('test query') == expected
        # An old package must not accidentally mask an unmigrated caller.
        try:
            metadata.version('duckduckgo-search')
        except metadata.PackageNotFoundError:
            pass
        else:
            raise AssertionError('old duckduckgo-search is installed in this acceptance environment')
        report = {'extra': extra, 'package': str(files('echo_agent')), 'builtin': str(builtin),
                  'resources': checked, 'skill_run': run.output.strip(), 'policy_checks': 6,
                  'dependencies': versions, 'result': 'passed'}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps(report, ensure_ascii=False))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--extra', choices=['base', 'skills', 'all'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    asyncio.run(validate(args.extra, args.output.resolve()))
