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
| image-gen | Generate images from text descriptions, multiple styles and sizes | `OPENAI_API_KEY` |
| meme-gen | Generate memes based on topics or situations | `OPENAI_API_KEY` |
| ppt-author | Create presentations with auto layout, images, and animations | — |

!!! tip "Image generation tip"
    Both `image-gen` and `meme-gen` automatically translate Chinese descriptions to English prompts for optimal results. Just describe what you want in any language.

---

## development

Software development assistance skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| code-runner | Execute Python/JS/Shell code snippets in a sandbox and return results | — |
| github-ops | Perform GitHub operations: create issues, PRs, view repo status | `GITHUB_TOKEN` |
| plan | Break down complex tasks into executable step-by-step plans | — |
| skill-creator | Assist in creating new SKILL.md skill files | — |
| workflow-chain | Chain multiple skills into automated workflows | — |

!!! warning "code-runner security restrictions"
    `code-runner` executes code in a restricted sandbox with no network access and limited filesystem access (designated temp directory only). For full execution environments, extend via the plugin system.

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
| stocks | Query real-time stock quotes, historical data, and technical indicators | `ALPHA_VANTAGE_KEY` |

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
| tts-voice | Convert text to natural speech, multiple voices and languages | `ELEVENLABS_API_KEY` |
| voice-note | Transcribe voice messages to text and organize as notes | — |

---

## productivity

Daily efficiency and office automation skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| calendar | Manage calendar events: create, query, modify, and delete schedules | `GOOGLE_CREDENTIALS_JSON` |
| daily-briefing | Generate daily briefings: weather, schedule, to-dos, news digest | — |
| email-assistant | Compose, reply to, and manage emails with templates and batch ops | `EMAIL_CREDENTIALS` |
| note-taking | Structured note-taking with tags, search, and export | — |
| notion-sync | Bi-directional sync with Notion databases and pages | `NOTION_TOKEN` |
| ocr-document | Extract text from images or PDFs, including table recognition | — |
| reminder | Set timed reminders with recurring and conditional triggers | — |
| summarize | Intelligent summarization of long text, web pages, and documents | — |
| weather | Query real-time weather and forecasts for specified cities | — |

!!! tip "daily-briefing is a composite skill"
    `daily-briefing` automatically calls `calendar`, `weather`, `reminder`, and other skills to aggregate information. Ensure related skills have their environment variables configured for the most complete daily briefing.

---

## research

Information retrieval and deep research skills.

| Skill | Description | Required Env Vars |
|-------|-------------|-------------------|
| arxiv | Search arXiv papers, get abstracts and PDF links | — |
| deep-research | Conduct multi-round deep research on complex topics, produce reports | `TAVILY_API_KEY` |
| rss-watcher | Monitor RSS feeds, extract updates, and generate summaries | — |
| web-extract | Extract structured data from web pages (articles, tables, lists) | — |
| web-search | Perform web searches and return structured results | `SERPAPI_KEY` or `TAVILY_API_KEY` |

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
| maps-poi | Location search, route planning, and POI queries | `AMAP_API_KEY` or `GOOGLE_MAPS_KEY` |
| text-tools | Text processing toolkit: translation, formatting, regex replace, encoding | — |

---

## Environment Variables Summary

All skills requiring environment variables at a glance:

| Env Var | Used By | How to Obtain |
|---------|---------|---------------|
| `OPENAI_API_KEY` | image-gen, meme-gen | [OpenAI Platform](https://platform.openai.com/) |
| `GITHUB_TOKEN` | github-ops | [GitHub Settings → Tokens](https://github.com/settings/tokens) |
| `ALPHA_VANTAGE_KEY` | stocks | [Alpha Vantage](https://www.alphavantage.co/support/) |
| `ELEVENLABS_API_KEY` | tts-voice | [ElevenLabs](https://elevenlabs.io/) |
| `GOOGLE_CREDENTIALS_JSON` | calendar | [Google Cloud Console](https://console.cloud.google.com/) |
| `EMAIL_CREDENTIALS` | email-assistant | From your email provider |
| `NOTION_TOKEN` | notion-sync | [Notion Integrations](https://www.notion.so/my-integrations) |
| `TAVILY_API_KEY` | deep-research, web-search | [Tavily](https://tavily.com/) |
| `SERPAPI_KEY` | web-search | [SerpAPI](https://serpapi.com/) |
| `AMAP_API_KEY` | maps-poi | [Amap Open Platform](https://lbs.amap.com/) |
| `GOOGLE_MAPS_KEY` | maps-poi | [Google Maps Platform](https://developers.google.com/maps) |

## Related Links

- [Skills System Guide](using-skills.en.md)
- [Plugin System](../plugins/using-plugins.en.md) — For registering new tools
