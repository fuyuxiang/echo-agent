# 内置技能目录

Echo Agent 提供 35 个内置技能，分布在 10 个类别中。本文列出所有可用技能及其功能说明。

另有用户主动安装的可选技能 `cohesivity`，存放于仓库 `optional-skills/cohesivity/`，不计入内置数量，也不被默认加载或打包。参见[安装与授权说明](using-skills.md)。

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
| image-gen | 通过 OpenAI、Stability AI 或 Pollinations 生成图片 | OpenAI 用 `OPENAI_API_KEY`；Stability 用 `STABILITY_API_KEY`；Pollinations 无需密钥 |
| meme-gen | 用 Pillow 为模板或自选图片添加文字 | 无 API Key；需要 Pillow |
| ppt-author | 创建演示文稿，自动排版、配图和动画 | — |

---

## development

软件开发辅助类技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| code-runner | 执行 Python 代码片段，并做超时与部分危险模式检查 | — |
| github-ops | 通过 `gh` CLI 执行 GitHub 操作 | 需要 `gh` 及其登录状态 |
| plan | 将复杂任务分解为可执行的步骤计划 | — |
| skill-creator | 辅助创建新的 SKILL.md 技能文件 | — |
| workflow-chain | 运行受审查的 JSON 命令阶段；技能组合由 Agent 组织 | — |

!!! warning "code-runner 不是安全沙箱"
    该技能通过主机 Python 子进程执行代码。临时工作目录和模式检查不能阻止代码读取主机文件，也不构成可靠的网络隔离；不要用它执行不可信代码。

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
| stocks | 通过免费公开接口查询股票、基金和加密货币行情 | 无 API Key |

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
| tts-voice | 使用 Edge TTS 或 OpenAI TTS 合成语音 | Edge TTS 无密钥；OpenAI 需要 `OPENAI_API_KEY` |
| voice-note | 将语音消息转录为文字并整理为笔记 | — |

---

## productivity

日常效率与办公自动化技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| calendar | 通过 CalDAV 或本地 ICS 管理日程 | CalDAV 需要服务器地址与账户凭据；本地 ICS 无密钥 |
| daily-briefing | 生成每日简报：天气、日程、待办、新闻摘要 | — |
| email-assistant | 通过 Himalaya 或 IMAP/SMTP 阅读与发送邮件 | 配置 Himalaya 账户或 `ECHO_EMAIL_HOST` 等邮件连接变量 |
| note-taking | 结构化笔记记录，支持标签、搜索和导出 | — |
| notion-sync | 读取、创建和更新 Notion 页面与数据库 | `NOTION_API_TOKEN` |
| ocr-document | 从图片或 PDF 中提取文字，支持表格识别 | — |
| reminder | 设置定时提醒，支持重复提醒和条件触发 | — |
| summarize | 对长文本、网页、文档进行智能摘要 | — |
| weather | 查询指定城市的实时天气和未来预报 | — |

!!! tip "daily-briefing 组合技能"
    `daily-briefing` 的说明指导 Agent 组合天气、日历、提醒与新闻；附带脚本本身只提供日期、天气和可读取的本地提醒摘要，不自动调用其他技能或注册定时投递。完整组合由 Agent 明确执行，周期运行另用已授权 Cron。

---

## research

信息检索与深度研究技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| arxiv | 搜索 arXiv 论文，获取摘要和 PDF 链接 | — |
| deep-research | 组合搜索、网页提取与交叉核对，生成带来源的报告 | 无固定 API Key；取决于所选搜索方式 |
| rss-watcher | 监控 RSS 源，提取更新并生成摘要 | — |
| web-extract | 从网页中提取结构化数据（文章、表格、列表） | — |
| web-search | 使用 DuckDuckGo 或自行部署的 SearXNG 检索网页 | 无 API Key；需要相应服务或依赖 |

!!! warning "deep-research 的开销显著高于其他技能"
    它会执行多轮检索与网页抓取，单次调用的 token 消耗可能是普通对话的数十倍。技能本身不做用量预估，也没有内置上限。

    需要约束时用成本侧的开关：`cost.dailyBudgetUsd` 设定每日硬上限（达到即拒绝新调用），`cost.softThresholdRatio` 在到达该比例时发出告警。事后用 `echo-agent cost` 按模型查看归因。参见[成本控制](../../guides/cost-control.md)。

---

## utility

通用工具类技能。

| 技能名 | 描述 | 所需环境变量 |
|--------|------|-------------|
| calculator | 执行数学计算、单位换算和公式求解 | — |
| file-convert | 文件格式转换：PDF↔Word、图片格式、音视频转码 | — |
| maps-poi | 使用 OpenStreetMap/Nominatim 与 OSRM 检索地点和路线 | 默认无 API Key；可选 `AMAP_API_KEY` |
| text-tools | 文本处理工具集：翻译、格式化、正则替换、编码转换 | — |

---

## 凭据与运行条件

上表区分了必需的账户凭据与可选的供应商密钥。`image-gen` 的 OpenAI 和 Stability AI 路径分别使用 `OPENAI_API_KEY` 和 `STABILITY_API_KEY`；Pollinations 路径无密钥。`tts-voice` 的 Edge TTS 路径无密钥，OpenAI 路径使用 `OPENAI_API_KEY`。`notion-sync` 使用 `NOTION_API_TOKEN`；`github-ops` 依赖已登录的 `gh` CLI。`calendar` 的 CalDAV 路径需相应账户凭据；本地 ICS 路径不需要网络密钥。具体安装命令和限制以各技能的 `SKILL.md` 为准。

## 相关链接

- [技能系统使用指南](using-skills.md)
- [插件系统](../plugins/using-plugins.md) — 如需注册新工具
