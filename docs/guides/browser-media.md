# 浏览器与媒体能力

Echo Agent 提供浏览器自动化、网页检索、图像理解与生成、语音合成等多媒体能力，覆盖从信息获取到内容创作的完整链路。

---

## 工具总览

| 工具 | 风险等级 | 说明 |
|------|----------|------|
| `browser` | `exec` | 驱动真实 Chromium 浏览器，执行多步网页交互 |
| `web_fetch` | `read_only` | 抓取指定 URL 的内容 |
| `web_search` | `read_only` | 搜索引擎查询 |
| `vision_analyze` | `read_only` | 使用视觉模型分析图像 |
| `image_generate` | — | 根据文本提示生成图像 |
| `text_to_speech` | — | 文本转语音 |
| `send_file` | `read_only` | 向指定会话发送文件或图片 |

---

## Browser 工具

### 能力概述

Browser 工具基于 Playwright 驱动一个真实的 Chromium 实例，支持完整的网页交互操作：

**导航**：`open`（创建会话）、`navigate`（跳转 URL）、`back`、`forward`、`reload`

**交互**：`click`（点击元素）、`type`（输入文本，可设 `press_enter` 提交）、`press`（按键）、`scroll`（滚动）、`hover`（悬停）、`select`（下拉选择）、`upload`（上传文件）

**检查**：`snapshot`（页面快照）、`screenshot`（截图为 PNG）、`get_images`（获取页面图片列表）、`console`（JS 错误日志）、`evaluate`（执行 JS 表达式）、`wait`（等待文本或加载状态）

**会话**：`open`（开启，返回 session_id）、`close`（关闭会话）

### 元素引用

每次快照后返回的元素引用格式为 `@e1`、`@e2` 等。引用在每次快照后重新编号，**必须使用最新快照中的引用**进行交互。

### 使用示例

```yaml
# 打开浏览器并导航
- action: open
# 返回 session_id: "abc123"

- action: navigate
  session_id: "abc123"
  url: "https://example.com"

# 点击搜索框并输入
- action: click
  session_id: "abc123"
  ref: "@e5"

- action: type
  session_id: "abc123"
  ref: "@e5"
  text: "Echo Agent"
  press_enter: true

# 截图并用视觉模型分析
- action: screenshot
  session_id: "abc123"
  full_page: true
```

### 会话管理

Browser 采用 per-owner 隔离模型：每个会话绑定到创建者（通过 `session_key` / `user_id` 识别），其他用户无法操纵已打开的会话。

- **会话限制**：支持 per-owner 和全局两级上限，防止单用户耗尽资源
- **空闲回收**：长时间无操作的会话会被自动清理
- **SSRF 防护**：每个请求（包括页面内子资源加载和重定向跳转）都经过 SSRF 检测，阻止访问内网地址
- **Dialog 处理**：自动处理 `alert`/`confirm`/`prompt` 弹窗，并记录内容供模型查看
- **Console 记录**：捕获页面 `error`/`warning` 级别的控制台输出

!!! question "需维护者确认"
    会话存储状态（`storage_state_path`）的持久化策略、`allow_private` 白名单配置方式、以及 `dialog_policy` 在 config.yaml 中的暴露路径，需维护者补充具体配置示例。

!!! warning "安全限制"
    - `evaluate` 操作会拦截读取 cookie、localStorage、sessionStorage 等敏感数据的表达式
    - 禁止通过 JS 执行导航跳转（`location.href`）、打开新窗口（`window.open`）等操作
    - 所有导航目标都必须通过 SSRF 校验

---

## Web 工具

### web_fetch — 网页抓取

从指定 URL 获取页面内容，自动处理重定向（每一跳都进行 SSRF 校验）。

```yaml
tools:
  web:
    proxy: ""              # 可选 HTTP 代理
    allow_private: false   # 是否允许访问内网地址
```

**参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `url` | string | 目标 URL |
| `max_chars` | integer | 内容截取上限（默认 2MB） |

!!! note "限制"
    - 最多跟踪 5 次重定向
    - 超时 30 秒
    - 禁止访问内网 IP（除非 `allow_private: true`）

### web_search — 网页搜索

通过搜索引擎 API 执行查询，返回结构化结果。

**支持的搜索 Provider**：

| Provider | 认证 | 说明 |
|----------|------|------|
| Brave | API Key | 默认 Provider |
| Tavily | API Key | AI 优化搜索 |
| SerpAPI | API Key | Google 搜索结果 |
| SearXNG | 无需 Key | 自建实例，需配置 `api_base` |
| Serply | API Key | 通用搜索 API |

```yaml
tools:
  web_search:
    provider: "brave"
    api_key: "BSA-xxx"
```

---

## Vision 工具

### 图像分析

使用视觉模型（如 GPT-4o、Claude 等支持多模态的模型）对图像进行理解和分析。

**参数**：

| 参数 | 必填 | 说明 |
|------|------|------|
| `image` | 是 | 本地图片路径或图片 URL |
| `prompt` | 是 | 关于图像的问题或指令 |
| `model` | 否 | 指定视觉模型（默认使用系统配置） |

**典型用法**：

- 分析 Browser 截图内容
- 识别图片中的文字（OCR）
- 描述图片内容
- 对比多张图片

```yaml
# 分析浏览器截图
- tool: vision_analyze
  image: "/workspace/screenshots/shot_1234.png"
  prompt: "页面上显示了哪些商品？价格分别是多少？"
```

---

## 图像生成

Echo Agent 支持两种图像生成后端：

### OpenAI DALL-E

```yaml
tools:
  image_gen:
    provider: "openai"
    api_key: "sk-xxx"
    model: "dall-e-3"        # 默认模型
```

**参数**：

| 参数 | 说明 |
|------|------|
| `prompt` | 图像描述 |
| `size` | `256x256` / `512x512` / `1024x1024` / `1792x1024` / `1024x1792` |
| `quality` | `standard` / `hd` |

### FAL.ai

支持多种先进模型：

| 模型 | 标识 |
|------|------|
| FLUX Schnell | `fal-ai/flux/schnell`（默认） |
| FLUX 2 Pro | `fal-ai/flux-2-pro` |
| Ideogram V3 | `fal-ai/ideogram/v3` |
| Recraft V4 Pro | `fal-ai/recraft/v4/pro/text-to-image` |
| Qwen Image | `fal-ai/qwen-image` |

```yaml
tools:
  image_gen:
    provider: "fal"
    fal_key: "fal-xxx"
    model: "fal-ai/flux/schnell"
```

**参数**：

| 参数 | 说明 |
|------|------|
| `prompt` | 图像描述 |
| `aspect_ratio` | `landscape` / `square` / `portrait` |

---

## 语音合成（TTS）

将文本转换为语音音频，支持两种后端。

### Edge TTS（默认，免费）

使用微软 Edge 在线语音服务，无需 API Key。

```yaml
tools:
  tts:
    backend: "edge"
    default_voice: "zh-CN-XiaoxiaoNeural"
```

### OpenAI TTS

```yaml
tools:
  tts:
    backend: "openai"
    openai_api_key: "sk-xxx"
    tts_model: "tts-1"        # 或 tts-1-hd
    default_voice: "alloy"    # alloy/echo/fable/onyx/nova/shimmer
```

**参数**：

| 参数 | 必填 | 说明 |
|------|------|------|
| `text` | 是 | 要转换的文本 |
| `voice` | 否 | 语音名称 |
| `backend` | 否 | `edge` 或 `openai` |
| `output_path` | 否 | 输出路径（自动生成） |
| `deliver` | 否 | 设为 `true` 直接发送到聊天 |
| `caption` | 否 | 随音频附带的文字说明 |

!!! tip "自动投递"
    设置 `deliver: true` 后，生成的音频会自动发送到当前对话，适合定时任务等无人值守场景。

---

## 文件发送（send_file）

将本地文件或图片发送到指定通道和会话。

**参数**：

| 参数 | 必填 | 说明 |
|------|------|------|
| `channel` | 是 | 目标通道（如 `weixin`） |
| `chat_id` | 是 | 目标会话 ID |
| `file_path` | 是 | 本地文件路径 |
| `caption` | 否 | 附带文字 |
| `as_image` | 否 | 强制以图片渲染（默认根据 MIME 类型推断） |

!!! warning "通道支持"
    并非所有通道都支持文件发送。不支持的通道会返回错误提示，需改用文本方式传递内容或切换到支持文件的通道。

---

## 典型场景

### 网页信息提取

```
用户: 帮我查一下 example.com 上的产品价格

Agent 流程:
1. browser.open → 获得 session_id
2. browser.navigate → 打开目标页面
3. browser.snapshot → 获取页面结构
4. browser.screenshot → 截图
5. vision_analyze → 分析截图中的价格信息
6. browser.close → 关闭会话
```

### 生成图片并发送

```
用户: 画一张日落海滩的图片发给我

Agent 流程:
1. image_generate → 生成图片（返回 URL）
2. web_fetch → 下载图片到本地
3. send_file → 发送到用户会话
```

### 语音播报

```
用户: 把今天的新闻摘要读给我听

Agent 流程:
1. web_search → 搜索今日新闻
2. 整理摘要文本
3. text_to_speech(deliver=true) → 生成并发送语音
```
