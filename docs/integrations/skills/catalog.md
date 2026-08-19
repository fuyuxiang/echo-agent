# 内置技能目录

Echo Agent 提供 35 个内置技能，分布在 10 个类别中。本文列出所有可用技能及其功能说明。

## 概览

| 类别 | 技能数量 | 说明 |
|------|---------|------|
| [创意 (creative)](#creative) | 4 | 图片、PPT、表格等内容创作 |
| [开发 (development)](#development) | 5 | 代码执行、GitHub 操作、工作流编排 |
| [运维 (devops)](#devops) | 2 | Docker 管理、系统监控 |
| [财务 (finance)](#finance) | 2 | 财务跟踪、股票行情 |
| [健康 (health)](#health) | 1 | 健身与营养建议 |
| [学习 (learning)](#learning) | 1 | 闪卡记忆 |
| [媒体 (media)](#media) | 2 | 语音合成、语音笔记 |
| [效率 (productivity)](#productivity) | 9 | 日历、邮件、笔记、提醒等 |
| [研究 (research)](#research) | 5 | 论文检索、深度研究、网页提取 |
| [工具 (utility)](#utility) | 4 | 计算器、文件转换、地图、文本处理 |

---

## creative

创意内容生成类技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| excel-author | 根据需求创建和编辑 Excel 表格，支持公式、图表和样式 | — |
| image-gen | 根据文字描述生成图片，支持多种风格和尺寸 | `OPENAI_API_KEY` |
| meme-gen | 根据话题或情境生成表情包/梗图 | `OPENAI_API_KEY` |
| ppt-author | 创建演示文稿，自动排版、配图和动画 | — |

!!! tip "图片生成提示"
    `image-gen` 和 `meme-gen` 都会自动将中文描述翻译为英文 prompt，以获得最佳生成效果。你只需用中文描述想要的画面即可。

---

## development

软件开发辅助类技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| code-runner | 在沙箱中执行 Python/JS/Shell 代码片段并返回结果 | — |
| github-ops | 执行 GitHub 操作：创建 issue、PR、查看仓库状态 | `GITHUB_TOKEN` |
| plan | 将复杂任务分解为可执行的步骤计划 | — |
| skill-creator | 辅助创建新的 SKILL.md 技能文件 | — |
| workflow-chain | 将多个技能串联为自动化工作流 | — |

!!! warning "code-runner 安全限制"
    `code-runner` 在受限沙箱中执行代码，无法访问网络和文件系统（除指定临时目录）。如需完整执行环境，请使用插件方式扩展。

---

## devops

系统运维与容器管理技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| docker-manage | 管理 Docker 容器：启动、停止、查看日志、构建镜像 | — |
| system-monitor | 监控系统资源：CPU、内存、磁盘、进程状态 | — |

---

## finance

财务与投资相关技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| finance-tracker | 记录和分析个人收支，生成财务报表 | — |
| stocks | 查询实时股票行情、历史数据和技术指标 | `ALPHA_VANTAGE_KEY` |

---

## health

健康管理技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| fitness-nutrition | 提供健身计划和营养建议，跟踪运动记录 | — |

!!! warning "健康建议免责声明"
    `fitness-nutrition` 提供的建议仅供参考，不构成医疗意见。如有健康问题，请咨询专业医生。

---

## learning

学习辅助技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| flashcards | 创建和复习闪卡，支持间隔重复算法 (SRS) | — |

---

## media

音频与媒体处理技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| tts-voice | 将文本转换为自然语音，支持多种声音和语言 | `ELEVENLABS_API_KEY` |
| voice-note | 将语音消息转录为文字并整理为笔记 | — |

---

## productivity

日常效率与办公自动化技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| calendar | 管理日历事件：创建、查询、修改和删除日程 | `GOOGLE_CREDENTIALS_JSON` |
| daily-briefing | 生成每日简报：天气、日程、待办、新闻摘要 | — |
| email-assistant | 撰写、回复和管理邮件，支持模板和批量操作 | `EMAIL_CREDENTIALS` |
| note-taking | 结构化笔记记录，支持标签、搜索和导出 | — |
| notion-sync | 双向同步 Notion 数据库和页面内容 | `NOTION_TOKEN` |
| ocr-document | 从图片或 PDF 中提取文字，支持表格识别 | — |
| reminder | 设置定时提醒，支持重复提醒和条件触发 | — |
| summarize | 对长文本、网页、文档进行智能摘要 | — |
| weather | 查询指定城市的实时天气和未来预报 | — |

!!! tip "daily-briefing 组合技能"
    `daily-briefing` 会自动调用 `calendar`、`weather`、`reminder` 等技能来汇总信息。确保相关技能的环境变量已配置，可获得最完整的每日简报。

---

## research

信息检索与深度研究技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| arxiv | 搜索 arXiv 论文，获取摘要和 PDF 链接 | — |
| deep-research | 对复杂话题进行多轮深度研究并生成报告 | `TAVILY_API_KEY` |
| rss-watcher | 监控 RSS 源，提取更新并生成摘要 | — |
| web-extract | 从网页中提取结构化数据（文章、表格、列表） | — |
| web-search | 执行网页搜索并返回结构化结果 | `SERPAPI_KEY` 或 `TAVILY_API_KEY` |

!!! question "需维护者确认"
    `deep-research` 单次调用可能消耗大量 token。是否应增加 token 用量预估提示或设置上限？

---

## utility

通用工具类技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| calculator | 执行数学计算、单位换算和公式求解 | — |
| file-convert | 文件格式转换：PDF↔Word、图片格式、音视频转码 | — |
| maps-poi | 地点搜索、路线规划和 POI 兴趣点查询 | `AMAP_API_KEY` 或 `GOOGLE_MAPS_KEY` |
| text-tools | 文本处理工具集：翻译、格式化、正则替换、编码转换 | — |

---

## 环境变量汇总

以下是所有需要环境变量的技能汇总：

| 环境变量 | 使用技能 | 获取方式 |
|----------|---------|---------|
| `OPENAI_API_KEY` | image-gen, meme-gen | [OpenAI Platform](https://platform.openai.com/) |
| `GITHUB_TOKEN` | github-ops | [GitHub Settings → Tokens](https://github.com/settings/tokens) |
| `ALPHA_VANTAGE_KEY` | stocks | [Alpha Vantage](https://www.alphavantage.co/support/) |
| `ELEVENLABS_API_KEY` | tts-voice | [ElevenLabs](https://elevenlabs.io/) |
| `GOOGLE_CREDENTIALS_JSON` | calendar | [Google Cloud Console](https://console.cloud.google.com/) |
| `EMAIL_CREDENTIALS` | email-assistant | 邮箱服务商提供 |
| `NOTION_TOKEN` | notion-sync | [Notion Integrations](https://www.notion.so/my-integrations) |
| `TAVILY_API_KEY` | deep-research, web-search | [Tavily](https://tavily.com/) |
| `SERPAPI_KEY` | web-search | [SerpAPI](https://serpapi.com/) |
| `AMAP_API_KEY` | maps-poi | [高德开放平台](https://lbs.amap.com/) |
| `GOOGLE_MAPS_KEY` | maps-poi | [Google Maps Platform](https://developers.google.com/maps) |

## 相关链接

- [技能系统使用指南](using-skills.md)
- [插件系统](../plugins/using-plugins.md) — 如需注册新工具
