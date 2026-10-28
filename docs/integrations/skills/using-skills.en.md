# Skills System Guide

Echo Agent bundles 35 SKILL.md files in 10 categories. Skills instruct the model and may contain scripts. Their presence does not configure an external account, API or local executable.

## Discovery, selection and execution

1. Enabled skills enter a character-budgeted list of name, category and description. Omission is explicit, and `skills_list` provides discovery beyond the prompt budget.
2. The model selects a skill. There is no separate relevance-ranking, mutual-exclusion or trigger engine.
3. `skill_view` reads SKILL.md/supporting files and reports missing declared dependencies/environment variables without installing them.
4. `skill_run` runs a script in its skill directory. Declared, permitted dependencies may be installed through the existing approval gate; execution switches, tool policy, timeouts and process cleanup still apply.

`tools.exec.enabled: false` disables skill scripts too. `daemon` and `public_gateway` deny `skill_run` by default; an operator must explicitly allow it when needed. Skills run host code rather than in an OS sandbox. Disabling a skill affects listing, reading and execution consistently.

## Frontmatter that is actually consumed

````markdown
---
name: my-report
version: 1.0.0
description: "Generate a report using an explicitly configured API account."
metadata:
  echo:
    tags: [Report]
    requires:
      pip: [httpx]
      env: [MY_REPORT_API_KEY]
---

# My Report

Describe real script arguments, output files and failure handling here.
````

Category comes from the directory. Tags use `metadata.echo.tags`; dependencies use `metadata.echo.requires.pip`/`requires.bins`, and credential names use `metadata.echo.requires.env`. The plugin manifest's top-level `requires_env` is not equivalent. Installation may also use `metadata.echo.install`, aligned with the real dependencies.

Scripts inherit safe process infrastructure and explicitly declared credential keys, not the full Runtime environment. Set variables on the service process or in a protected systemd EnvironmentFile. There is no generic top-level `env` configuration section. Keep credentials out of command-line arguments.

## Built-in requirements

| Skill | Credentials and boundary |
|---|---|
| image-gen | `OPENAI_API_KEY` for OpenAI; `STABILITY_API_KEY` for Stability; other paths follow the script |
| web-search | DuckDuckGo or configured SearXNG; no promised SERPAPI/Tavily adapter |
| tts-voice | No key for Edge TTS; `OPENAI_API_KEY` for OpenAI |
| stocks | Public endpoints, no mandatory ALPHA_VANTAGE_KEY; availability depends on the remote service |
| notion-sync | `NOTION_API_TOKEN` |
| calendar | CalDAV URL/account arguments; the local ICS example is a separate recipe, not Google OAuth integration |

YAML in briefing, monitoring or market-alert instructions may be an Agent/operator recipe. It takes effect automatically only if the script explicitly reads it. Use core `cronjob` creation/authorization for scheduling and the existing delivery pipeline for confirmed output.

## Installation and management

Dashboard Skills exposes content, files, dependency state, toggles and declared dependency installation. External import and mutation use admin authorization and admission. See [evolution and evaluation](../../concepts/evolution-evaluation.en.md) for candidates, review, promotion and rollback. Plugins register tools; skills compose existing tools, with different trust and lifecycle rules.

## Cohesivity (optional skill)

Cohesivity lives in the repository's `optional-skills/cohesivity/`, outside the 35 built-in skills. It is excluded from Python wheels/sdists and the default skill directory. Normal installation, startup, and generic app/database/deployment requests do not load it. A user must explicitly choose Cohesivity and import the directory from a reviewed repository revision.

With a source checkout available, explicitly requesting local installation uses these `skill_install` parameters (not runtime YAML):

```json
{
  "source": "local",
  "location": "/absolute/path/to/echo-agent/optional-skills/cohesivity",
  "run_install": false
}
```

This imports instructions through the existing scan/admission path. It does not execute npm, contact Cohesivity, install a global skill, or create a tenant. Obtain a reviewed revision first if source is not available locally; do not import the whole repository root as a skill.

Backend bootstrap needs separate authorization. The skill pins `@cohesivity/init@0.9.4` and requires disclosure of third-party execution, global `~/.agents/skills` writes, project-guidance edits, a machine identifier, and the tenant's 72-hour expiry before execution. Declining these effects stops initialization. Deployment, paid operations, claiming, MCP configuration, and feedback each require the appropriate explicit authorization. Installing the skill does not authorize the upstream installer.

- [Skill catalog](catalog.en.md)
- [Skill authoring](../../development/skill-authoring.en.md)
- [Tools and permissions](../../guides/tools-permissions.en.md)
