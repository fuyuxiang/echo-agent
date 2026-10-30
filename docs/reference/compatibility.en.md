# Compatibility Reference

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

This page follows `pyproject.toml` and the current CI configuration. The project is in the `0.x` Beta series; read the [Changelog](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/CHANGELOG.md) and back up the workspace before upgrading.

## Python and platforms

`requires-python = ">=3.11"`. CI tests Python 3.11 and 3.12. Newer versions meet the installation constraint but are absent from the current CI matrix and package classifiers. Package metadata does not restrict operating systems. Verify channels, external commands, browsers, and optional dependencies in the target deployment environment; an untested platform or version has no documented support guarantee.

The Gateway uses `aiohttp` and the CLI uses `argparse`. Storage uses SQLite; there is no PostgreSQL backend. The default Gateway address is `127.0.0.1:58123` with the `/api/v1` API prefix. No separate Prometheus HTTP port is enabled by default.

## Required dependencies

This list is generated from `project.dependencies` in `pyproject.toml`. The installer resolves transitive dependencies.

| Package | Version constraint |
|---|---|
| `pydantic` | `pydantic>=2.0` |
| `pydantic-settings` | `pydantic-settings>=2.0` |
| `cryptography` | `cryptography>=41.0` |
| `regex` | `regex>=2024.11.6` |
| `pyyaml` | `pyyaml>=6.0` |
| `loguru` | `loguru>=0.7.2,<0.8` |
| `aiohttp` | `aiohttp>=3.9` |
| `aiosqlite` | `aiosqlite>=0.20` |
| `croniter` | `croniter>=1.4` |
| `numpy` | `numpy>=1.24` |
| `Pillow` | `Pillow>=13.0.0,<14` |
| `huggingface-hub` | `huggingface-hub>=0.20,<2.0` |
| `mmh3` | `mmh3>=4.1.0,<6.0.0` |
| `numpy` | `numpy>=1.26; python_version == '3.12'` |
| `numpy` | `numpy>=2.1.0; python_version == '3.13'` |
| `numpy` | `numpy>=2.3.0; python_version >= '3.14'` |
| `onnxruntime` | `onnxruntime>=1.17.0,!=1.20.0,!=1.24.0,!=1.24.1; python_version < '3.13'` |
| `onnxruntime` | `onnxruntime>1.21.0,!=1.24.0,!=1.24.1; python_version == '3.13'` |
| `onnxruntime` | `onnxruntime>=1.24.2; python_version >= '3.14'` |
| `py-rust-stemmers` | `py-rust-stemmers>=0.1.0,<0.2` |
| `requests` | `requests>=2.31,<3` |
| `tokenizers` | `tokenizers>=0.15,<1.0` |
| `tqdm` | `tqdm>=4.66,<5` |
| `questionary` | `questionary>=2.0` |
| `prompt-toolkit` | `prompt-toolkit>=3.0` |
| `httpx` | `httpx>=0.23` |
| `rich` | `rich>=13.5` |

## Optional dependency groups

These names come from `project.optional-dependencies` in `pyproject.toml`. Install one with, for example, `pip install "echo-agent[browser]"`. `all` is an explicitly listed set of common optional packages; do not assume it includes every group added in the future.

| Group | Installation |
|---|---|
| `openai` | `pip install "echo-agent[openai]"` |
| `anthropic` | `pip install "echo-agent[anthropic]"` |
| `bedrock` | `pip install "echo-agent[bedrock]"` |
| `gemini` | `pip install "echo-agent[gemini]"` |
| `allproviders` | `pip install "echo-agent[allproviders]"` |
| `vector` | `pip install "echo-agent[vector]"` |
| `container` | `pip install "echo-agent[container]"` |
| `process` | `pip install "echo-agent[process]"` |
| `fal` | `pip install "echo-agent[fal]"` |
| `weixin` | `pip install "echo-agent[weixin]"` |
| `browser` | `pip install "echo-agent[browser]"` |
| `otel` | `pip install "echo-agent[otel]"` |
| `tokenizers` | `pip install "echo-agent[tokenizers]"` |
| `documents` | `pip install "echo-agent[documents]"` |
| `tui` | `pip install "echo-agent[tui]"` |
| `all` | `pip install "echo-agent[all]"` |
| `dev` | `pip install "echo-agent[dev]"` |
| `docs` | `pip install "echo-agent[docs]"` |
| `skills` | `pip install "echo-agent[skills]"` |

## Upgrades and data

SQLite schema migrations run automatically during connection initialization. `echo-agent migrate` only handles USER memory ownership and legacy `MEMORY.*.md` shards; it is not a database schema migration command. `checkpoint restore` does not restore databases, sessions, memory, or logs. For a cross-version rollback, stop the service and restore a full backup matching the target version. See [upgrade and data migration](../operations/upgrade-migrations.en.md).

## Server qualification as of 2026-10-21

Production wheels contain the Dashboard, fallback page, bundled skills, and default config. Running an installed wheel needs no Node/pnpm, Rust/Tauri, or root client directory. Dashboard source builds use Node (24 LTS recommended) and pinned pnpm 10.34.5; CI covers pnpm 10/11. Existing Node 20 build compatibility does not imply upstream maintenance.

The Gemini extra now uses `google-genai>=2.32,<3`, verified with the real SDK and a controlled HTTP edge. Provider names, key/env precedence, and model configuration remain compatible. Native async streams close on cancellation; signed content survives tool turns; SDK automatic function execution is disabled. Inline data URLs are supported for images; arbitrary remote image URLs are not fetched. No real Google API call was performed.

FastEmbed is the bundled `0.9.0+echo.1` compatibility copy, with source/hash/license recorded in its VENDOR.md; the existing Pillow 13 compatibility patch is preserved. PyMuPDF is temporarily `<2` after reproducing the 2.0 release metadata mismatch. cryptography is a core dependency for default credential encryption; regex supplies bounded matching.

MCP supports the implemented legacy revisions 2024-11-05, 2025-03-26, and 2025-06-18; the stateless 2026-07-28 model is not implemented. A2A remains an inbound legacy tasks/send/get/cancel subset, without a claim of A2A 1.x conformance. See the [server review](../reviews/server-review-and-remediation.md) for evidence and upgrade decisions.
