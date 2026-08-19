# 技能系统使用指南

Echo Agent 内置 35 个技能（Skills），覆盖 10 个类别。技能是预编写的指令集，以 SKILL.md 文件形式存在，教会 Agent 如何执行特定任务。

## 什么是技能

技能是一组结构化的操作指令，包含：

- **名称与描述** — 告诉 Agent 这个技能做什么
- **版本号** — 用于兼容性管理
- **元数据** — 标签、分类、所需环境变量
- **指令正文** — Markdown 格式的步骤说明与代码示例

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

## 如何使用技能

直接用自然语言向 Agent 提出需求即可，无需记住技能名称：

```
用户: 帮我生成一张猫咪的图片
Agent: [自动选择 image-gen 技能，调用图片生成工具]

用户: 查一下明天的天气
Agent: [自动选择 weather 技能，获取天气信息]

用户: 把这个网页的内容总结一下
Agent: [自动选择 summarize 技能，提取并总结内容]
```

!!! tip "技能选择是自动的"
    Agent 根据你的意图自动匹配最合适的技能。你只需要描述想做什么，不需要指定使用哪个技能。

## 技能选择机制

Agent 通过以下流程选择技能：

1. **意图识别** — 分析用户输入，提取核心意图
2. **技能匹配** — 根据技能描述和标签，计算匹配度
3. **环境检查** — 验证所需环境变量是否已配置
4. **技能执行** — 按 SKILL.md 中的指令逐步执行

!!! warning "环境变量缺失时"
    如果技能依赖的环境变量未配置，Agent 会提示你设置对应变量。例如 `image-gen` 需要 `OPENAI_API_KEY`，未设置时 Agent 会告知你需要先配置该密钥。

## 环境变量需求

部分技能需要外部服务的 API 密钥：

| 技能 | 所需环境变量 | 用途 |
|------|-------------|------|
| image-gen | `OPENAI_API_KEY` | 调用 DALL-E 生成图片 |
| web-search | `SERPAPI_KEY` 或 `TAVILY_API_KEY` | 执行网页搜索 |
| tts-voice | `ELEVENLABS_API_KEY` | 文本转语音 |
| stocks | `ALPHA_VANTAGE_KEY` | 获取股票数据 |
| notion-sync | `NOTION_TOKEN` | 同步 Notion 数据 |
| calendar | `GOOGLE_CREDENTIALS_JSON` | 访问 Google 日历 |

在 `config.yaml` 中配置环境变量：

```yaml
env:
  OPENAI_API_KEY: "sk-..."
  SERPAPI_KEY: "..."
```

或通过系统环境变量设置。

## SKILL.md 格式

每个技能由一个 `SKILL.md` 文件定义：

```markdown
---
name: image-gen
version: "1.0.0"
description: "根据文字描述生成图片"
category: creative
tags: [image, generation, dall-e, art]
requires_env:
  - OPENAI_API_KEY
---

# 图片生成技能

## 触发条件

当用户要求生成、创建、画一张图片时触发本技能。

## 执行步骤

1. 解析用户描述，提取画面要素
2. 构建优化后的英文 prompt
3. 调用 DALL-E API 生成图片
4. 返回图片并附带简短描述

## 代码示例

` ` `python
result = await ctx.tool_registry.invoke("openai_image", {
    "prompt": optimized_prompt,
    "size": "1024x1024",
    "quality": "standard"
})
` ` `

## 注意事项

- 始终将用户描述翻译为英文 prompt 以获得最佳效果
- 如果用户未指定尺寸，默认使用 1024x1024
```

!!! question "需维护者确认"
    SKILL.md 中的 `requires_env` 字段是否支持设置默认值或备选变量名？当前实现中若变量缺失会直接跳过该技能。

## 技能与插件的区别

| 特性 | 技能 (Skill) | 插件 (Plugin) |
|------|-------------|---------------|
| 形式 | Markdown 指令文件 | Python 代码模块 |
| 作用 | 指导 Agent 行为 | 扩展系统能力 |
| 开发成本 | 低，只需写 Markdown | 中等，需要编程 |
| 能力范围 | 组合现有工具 | 注册新工具、钩子 |
| 发现方式 | skills 目录自动扫描 | 多来源发现机制 |

## 下一步

- 查看 [完整技能目录](catalog.md) 了解所有可用技能
- 如需扩展更多能力，参见 [插件系统](../plugins/using-plugins.md)
