# Skills System Guide

Echo Agent ships with 35 built-in skills across 10 categories. Skills are pre-written instruction sets (SKILL.md files) that teach the agent how to perform specific tasks.

## What Is a Skill

A skill is a structured set of operational instructions containing:

- **Name & description** — Tells the agent what this skill does
- **Version** — For compatibility management
- **Metadata** — Tags, category, required environment variables
- **Instruction body** — Markdown-formatted steps with code examples

```
skills/
├── creative/
│   ├── image-gen/SKILL.md
│   ├── meme-gen/SKILL.md
│   └── ...
├── development/
│   ├── code-runner/SKILL.md
│   └── ...
└── productivity/
    ├── calendar/SKILL.md
    └── ...
```

## How to Use Skills

Simply describe what you want in natural language. No need to remember skill names:

```
User: Generate a picture of a cat
Agent: [automatically selects image-gen skill, invokes image generation tool]

User: What's the weather tomorrow?
Agent: [automatically selects weather skill, fetches weather data]

User: Summarize the content of this webpage
Agent: [automatically selects summarize skill, extracts and summarizes content]
```

!!! tip "Skill selection is automatic"
    The agent matches the most appropriate skill based on your intent. Just describe what you want to do — no need to specify which skill to use.

## How Skill Selection Works

The agent follows this process to select skills:

1. **Intent recognition** — Analyzes user input to extract core intent
2. **Skill matching** — Computes relevance based on skill descriptions and tags
3. **Environment check** — Verifies that required environment variables are configured
4. **Skill execution** — Follows instructions in SKILL.md step by step

!!! warning "When environment variables are missing"
    If a skill's required environment variable is not configured, the agent will prompt you to set it. For example, `image-gen` requires `OPENAI_API_KEY` — if unset, the agent will tell you to configure it first.

## Environment Variable Requirements

Some skills require API keys for external services:

| Skill | Required Env Var | Purpose |
|-------|-----------------|---------|
| image-gen | `OPENAI_API_KEY` | Call DALL-E for image generation |
| web-search | `SERPAPI_KEY` or `TAVILY_API_KEY` | Perform web searches |
| tts-voice | `ELEVENLABS_API_KEY` | Text-to-speech conversion |
| stocks | `ALPHA_VANTAGE_KEY` | Fetch stock market data |
| notion-sync | `NOTION_TOKEN` | Sync with Notion |
| calendar | `GOOGLE_CREDENTIALS_JSON` | Access Google Calendar |

Configure environment variables in `config.yaml`:

```yaml
env:
  OPENAI_API_KEY: "sk-..."
  SERPAPI_KEY: "..."
```

Or set them as system environment variables.

## SKILL.md Format

Each skill is defined by a `SKILL.md` file:

```markdown
---
name: image-gen
version: "1.0.0"
description: "Generate images from text descriptions"
category: creative
tags: [image, generation, dall-e, art]
requires_env:
  - OPENAI_API_KEY
---

# Image Generation Skill

## Trigger Conditions

Activate when the user requests generating, creating, or drawing an image.

## Execution Steps

1. Parse the user's description, extract visual elements
2. Build an optimized English prompt
3. Call DALL-E API to generate the image
4. Return the image with a brief description

## Code Example

` ` `python
result = await ctx.tool_registry.invoke("openai_image", {
    "prompt": optimized_prompt,
    "size": "1024x1024",
    "quality": "standard"
})
` ` `

## Notes

- Always translate user descriptions to English prompts for best results
- Default to 1024x1024 if user doesn't specify a size
```

!!! warning "Skill frontmatter has no requires_env field"
    `requires_env` belongs to the **plugin** manifest; skills do not have it. Skill frontmatter recognises exactly five keys — `name`, `description`, `category`, `version` and `tags` — and any other key is ignored.

    Skills therefore perform no environment check before loading: a skill missing its credentials still appears in the available list, and the failure surfaces when the script actually runs. State such dependencies in the `description`, and validate them inside the script with a clear error message.

## Skills vs Plugins

| Feature | Skill | Plugin |
|---------|-------|--------|
| Format | Markdown instruction file | Python code module |
| Purpose | Guide agent behavior | Extend system capabilities |
| Development cost | Low — just write Markdown | Medium — requires programming |
| Capability scope | Compose existing tools | Register new tools and hooks |
| Discovery | Auto-scan from skills directory | Multi-source discovery mechanism |

## Next Steps

- See the [complete skill catalog](catalog.en.md) for all available skills
- For extending capabilities further, see the [plugin system](../plugins/using-plugins.en.md)
