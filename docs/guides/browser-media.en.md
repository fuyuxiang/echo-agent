# Browser & Media Capabilities

Echo Agent provides browser automation, web retrieval, image understanding and generation, and text-to-speech capabilities — covering the full pipeline from information gathering to content creation.

---

## Tool Overview

| Tool | Risk Level | Description |
|------|-----------|-------------|
| `browser` | `exec` | Drive a real Chromium browser for multi-step web interaction |
| `web_fetch` | `read_only` | Fetch content from a URL |
| `web_search` | `read_only` | Search engine queries |
| `vision_analyze` | `read_only` | Analyze images using a vision-capable LLM |
| `image_generate` | — | Generate images from text prompts |
| `text_to_speech` | — | Convert text to speech audio |
| `send_file` | `read_only` | Send files or images to a specific chat |

---

## Browser Tool

### Capabilities

The Browser tool drives a real Chromium instance via Playwright, supporting full web interaction:

**Navigation**: `open` (create session), `navigate` (go to URL), `back`, `forward`, `reload`

**Interaction**: `click` (click element), `type` (input text, set `press_enter` to submit), `press` (keystroke), `scroll` (directional scrolling), `hover`, `select` (dropdown), `upload` (file upload)

**Inspection**: `snapshot` (page structure), `screenshot` (save PNG), `get_images` (list page images), `console` (JS error log), `evaluate` (run JS expression), `wait` (wait for text or load state)

**Session**: `open` (start, returns session_id), `close` (end session)

### Element References

Each snapshot returns element references in the format `@e1`, `@e2`, etc. References are renumbered after every snapshot — **always use refs from the latest snapshot**.

### Usage Example

```yaml
# Open browser and navigate
- action: open
# Returns session_id: "abc123"

- action: navigate
  session_id: "abc123"
  url: "https://example.com"

# Click search box and type
- action: click
  session_id: "abc123"
  ref: "@e5"

- action: type
  session_id: "abc123"
  ref: "@e5"
  text: "Echo Agent"
  press_enter: true

# Screenshot and analyze with vision
- action: screenshot
  session_id: "abc123"
  full_page: true
```

### Session Management

The Browser uses a per-owner isolation model: each session is bound to its creator (identified by `session_key` / `user_id`), preventing other users from controlling open sessions.

- **Session limits**: Two-level ceiling (per-owner and global) prevents a single user from exhausting resources
- **Idle reaping**: Sessions inactive for too long are automatically cleaned up
- **SSRF protection**: Every request (including sub-resource loads and redirect hops) is validated against SSRF rules, blocking access to private/internal addresses
- **Dialog handling**: Automatically handles `alert`/`confirm`/`prompt` dialogs, recording their content for the model to see
- **Console capture**: Records page `error`/`warning` level console output

!!! note "Related settings"
    Login-state persistence is controlled by `tools.browser.persist_login_state` (default false; there is no `storage_state_path` field). Private-address access is governed separately by `tools.web.allow_private_addresses` and `tools.browser.allow_private_addresses`, each scoped to its own tool. The dialog setting is `tools.browser.dialog_policy`.

!!! warning "Security Restrictions"
    - `evaluate` blocks expressions that read cookies, localStorage, sessionStorage, and other sensitive data
    - Prevents JS-driven navigation (`location.href`), new windows (`window.open`), and similar operations
    - All navigation targets must pass SSRF validation

---

## Web Tools

### web_fetch — Content Fetching

Fetches page content from a URL, automatically following redirects (each hop is SSRF-validated).

```yaml
tools:
  web:
    proxy: ""              # Optional HTTP proxy
    allow_private_addresses: false   # Whether to allow private/internal addresses
```

**Parameters**:

| Parameter | Type | Description |
|-----------|------|-------------|
| `url` | string | Target URL |
| `max_chars` | integer | Content truncation limit (default 2MB) |

!!! note "Limitations"
    - Maximum 5 redirect hops
    - 30-second timeout
    - Private IPs blocked unless `tools.web.allow_private_addresses: true`

### web_search — Web Search

Executes queries via search engine APIs and returns structured results.

**Supported Search Providers**:

| Provider | Auth | Notes |
|----------|------|-------|
| Brave | API Key | Default provider |
| Tavily | API Key | AI-optimized search |
| SerpAPI | API Key | Google search results |
| SearXNG | No key needed | Self-hosted, requires `api_base` |
| Serply | API Key | General search API |

```yaml
tools:
  web:
    enabled: true
    search_provider: "brave"   # brave | tavily | serpapi | searxng | serply
    search_api_key: "BSA-xxx"
```

---

## Vision Tool

### Image Analysis

Uses a vision-capable model (e.g., GPT-4o, Claude, or other multimodal models) to understand and analyze images.

**Parameters**:

| Parameter | Required | Description |
|-----------|----------|-------------|
| `image` | Yes | Local image path or image URL |
| `prompt` | Yes | Question or instruction about the image |
| `model` | No | Specify vision model (uses system default) |

**Typical uses**:

- Analyze browser screenshots
- Extract text from images (OCR)
- Describe image content
- Compare multiple images

```yaml
# Analyze a browser screenshot
- tool: vision_analyze
  image: "/workspace/screenshots/shot_1234.png"
  prompt: "What products are shown on the page? What are their prices?"
```

---

## Image Generation

Echo Agent supports two image generation backends:

### OpenAI DALL-E

```yaml
tools:
  image_gen:
    backend: "openai"
    api_key: "sk-xxx"
    model: "dall-e-3"        # Default model
```

**Parameters**:

| Parameter | Description |
|-----------|-------------|
| `prompt` | Image description |
| `size` | `256x256` / `512x512` / `1024x1024` / `1792x1024` / `1024x1792` |
| `quality` | `standard` / `hd` |

### FAL.ai

Supports multiple advanced models:

| Model | Identifier |
|-------|-----------|
| FLUX Schnell | `fal-ai/flux/schnell` (default) |
| FLUX 2 Pro | `fal-ai/flux-2-pro` |
| Ideogram V3 | `fal-ai/ideogram/v3` |
| Recraft V4 Pro | `fal-ai/recraft/v4/pro/text-to-image` |
| Qwen Image | `fal-ai/qwen-image` |

```yaml
tools:
  image_gen:
    backend: "fal"
    fal_key: "fal-xxx"
    fal_model: "fal-ai/flux/schnell"
```

**Parameters**:

| Parameter | Description |
|-----------|-------------|
| `prompt` | Image description |
| `aspect_ratio` | `landscape` / `square` / `portrait` |

---

## Text-to-Speech (TTS)

Converts text to speech audio with two backend options.

### Edge TTS (default, free)

Uses Microsoft Edge online speech service, no API key required.

```yaml
tools:
  tts:
    default_backend: "edge"
    default_voice: "en-US-AriaNeural"
```

### OpenAI TTS

```yaml
tools:
  tts:
    default_backend: "openai"
    openai_api_key: "sk-xxx"
    model: "tts-1"        # or tts-1-hd
    default_voice: "alloy"    # alloy/echo/fable/onyx/nova/shimmer
```

**Parameters**:

| Parameter | Required | Description |
|-----------|----------|-------------|
| `text` | Yes | Text to convert |
| `voice` | No | Voice name |
| `backend` | No | `edge` or `openai` |
| `output_path` | No | Output path (auto-generated if omitted) |
| `deliver` | No | Set `true` to send directly to chat |
| `caption` | No | Text message sent alongside the audio |

!!! tip "Auto-delivery"
    Setting `deliver: true` automatically sends the generated audio to the current conversation — ideal for scheduled or unattended tasks.

---

## File Sending (send_file)

Send a local file or image to a specific channel and chat.

**Parameters**:

| Parameter | Required | Description |
|-----------|----------|-------------|
| `channel` | Yes | Target channel (e.g., `weixin`) |
| `chat_id` | Yes | Target chat ID |
| `file_path` | Yes | Local file path |
| `caption` | No | Accompanying text |
| `as_image` | No | Force image rendering (defaults to MIME type inference) |

!!! warning "Channel Support"
    Not all channels support file delivery. Unsupported channels return an error — use text-based content delivery or switch to a file-capable channel instead.

---

## Common Scenarios

### Web Information Extraction

```
User: Check the product prices on example.com

Agent flow:
1. browser.open → get session_id
2. browser.navigate → open target page
3. browser.snapshot → get page structure
4. browser.screenshot → capture screenshot
5. vision_analyze → analyze prices in the screenshot
6. browser.close → end session
```

### Generate and Send an Image

```
User: Draw a sunset beach image and send it to me

Agent flow:
1. image_generate → generate image (returns URL)
2. web_fetch → download image locally
3. send_file → send to user's chat
```

### Voice Broadcast

```
User: Read me today's news summary

Agent flow:
1. web_search → search today's news
2. Compile summary text
3. text_to_speech(deliver=true) → generate and send audio
```
