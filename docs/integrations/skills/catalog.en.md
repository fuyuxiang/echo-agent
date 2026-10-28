# Built-in Skill Catalog

Echo Agent provides 35 built-in skills across 10 categories. This document lists all available skills and their descriptions.

## Overview

| Category | Count | Description |
|----------|-------|-------------|
| [creative](#creative) | 4 | Image, PPT, spreadsheet content creation |
| [development](#development) | 5 | Code execution, GitHub ops, workflow orchestration |
| [devops](#devops) | 2 | Docker management, system monitoring |
| [finance](#finance) | 2 | Finance tracking, stock quotes |
| [health](#health) | 1 | Fitness and nutrition advice |
| [learning](#learning) | 1 | Flashcard memorization |
| [media](#media) | 2 | Text-to-speech, voice notes |
| [productivity](#productivity) | 9 | Calendar, email, notes, reminders, etc. |
| [research](#research) | 5 | Paper search, deep research, web extraction |
| [utility](#utility) | 4 | Calculator, file conversion, maps, text tools |

---

## creative

Creative content generation skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| excel-author | Create and edit Excel spreadsheets with formulas, charts, and styles | — |
| image-gen | Generate images with OpenAI, Stability AI, or Pollinations | `OPENAI_API_KEY` for OpenAI; `STABILITY_API_KEY` for Stability; no key for Pollinations |
| meme-gen | Overlay text on templates or user-provided images with Pillow | No API key; Pillow required |
| ppt-author | Create presentations with auto layout, images, and animations | — |

---

## development

Software development assistance skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| code-runner | Run Python snippets with timeouts and limited pattern checks | — |
| github-ops | Perform GitHub operations through the `gh` CLI | Requires `gh` with an authenticated account |
| plan | Break down complex tasks into executable step-by-step plans | — |
| skill-creator | Assist in creating new SKILL.md skill files | — |
| workflow-chain | Run reviewed JSON command stages; the Agent composes skills | — |

!!! warning "code-runner is not a security sandbox"
    This skill runs a Python subprocess on the host. A temporary working directory and pattern checks do not prevent access to host files or provide reliable network isolation. Do not run untrusted code with it.

---

## devops

System operations and container management skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| docker-manage | Manage Docker containers: start, stop, view logs, build images | — |
| system-monitor | Monitor system resources: CPU, memory, disk, process status | — |

---

## finance

Finance and investment skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| finance-tracker | Record and analyze personal income/expenses, generate reports | — |
| stocks | Query stocks, funds, and crypto through public endpoints | No API key |

---

## health

Health management skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| fitness-nutrition | Provide fitness plans and nutrition advice, track exercise records | — |

!!! warning "Health advice disclaimer"
    Advice from `fitness-nutrition` is for reference only and does not constitute medical advice. Consult a professional physician for health concerns.

---

## learning

Learning assistance skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| flashcards | Create and review flashcards with spaced repetition (SRS) support | — |

---

## media

Audio and media processing skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| tts-voice | Synthesize speech through Edge TTS or OpenAI TTS | Edge TTS needs no key; OpenAI uses `OPENAI_API_KEY` |
| voice-note | Transcribe voice messages to text and organize as notes | — |

---

## productivity

Daily efficiency and office automation skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| calendar | Manage events through CalDAV or local ICS | CalDAV account credentials; no key for local ICS |
| daily-briefing | Generate daily briefings: weather, schedule, to-dos, news digest | — |
| email-assistant | Read and send email through Himalaya or IMAP/SMTP | Configured Himalaya account or email connection variables such as `ECHO_EMAIL_HOST` |
| note-taking | Structured note-taking with tags, search, and export | — |
| notion-sync | Read, create, and update Notion pages and databases | `NOTION_API_TOKEN` |
| ocr-document | Extract text from images or PDFs, including table recognition | — |
| reminder | Set timed reminders with recurring and conditional triggers | — |
| summarize | Intelligent summarization of long text, web pages, and documents | — |
| weather | Query real-time weather and forecasts for specified cities | — |

!!! tip "daily-briefing is a composite skill"
    The instructions guide the Agent in composing weather, calendar, reminders and news. The bundled script itself reports date, weather and available local reminders; it neither invokes other skills nor registers scheduled delivery. The Agent must compose the full workflow explicitly; periodic execution uses authorized Cron.

---

## research

Information retrieval and deep research skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| arxiv | Search arXiv papers, get abstracts and PDF links | — |
| deep-research | Combine search, extraction, and cross-checking into a cited report | No fixed API key; depends on the selected search method |
| rss-watcher | Monitor RSS feeds, extract updates, and generate summaries | — |
| web-extract | Extract structured data from web pages (articles, tables, lists) | — |
| web-search | Search with DuckDuckGo or a self-hosted SearXNG instance | No API key; relevant service or dependency required |

!!! warning "deep-research costs markedly more than the other skills"
    It runs several rounds of search and page fetching, so a single invocation can consume tens of times the tokens of an ordinary exchange. The skill itself neither estimates usage nor imposes a cap.

    Constrain it from the cost side instead: `cost.dailyBudgetUsd` sets a hard daily ceiling (reaching it refuses further calls) and `cost.softThresholdRatio` warns at a fraction of it. After the fact, `echo-agent cost` breaks spending down by model. See [cost control](../../guides/cost-control.en.md).

---

## utility

General-purpose tool skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| calculator | Math calculations, unit conversions, and formula solving | — |
| file-convert | File format conversion: PDF↔Word, image formats, audio/video transcoding | — |
| maps-poi | Query places and routes with OpenStreetMap/Nominatim and OSRM | No key by default; optional `AMAP_API_KEY` |
| text-tools | Text processing toolkit: translation, formatting, regex replace, encoding | — |

---

## Credentials and runtime requirements

The table distinguishes required account credentials from optional provider keys. `image-gen` uses `OPENAI_API_KEY` or `STABILITY_API_KEY` for those respective paths; Pollinations needs no key. `tts-voice` needs no key for Edge TTS and uses `OPENAI_API_KEY` for its OpenAI path. `notion-sync` uses `NOTION_API_TOKEN`; `github-ops` requires an authenticated `gh` CLI. CalDAV calendar access needs account credentials, while local ICS needs no network key. See each skill's `SKILL.md` for installation commands and limits.

## Related Links

- [Skills System Guide](using-skills.en.md)
- [Plugin System](../plugins/using-plugins.en.md) — For registering new tools
