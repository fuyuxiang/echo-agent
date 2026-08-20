# 本地模型接入

Echo Agent 支持通过 OpenAI 兼容 API 接入本地模型服务。所有本地模型服务均使用 OpenAI provider，只需配置自定义 `api_base` 即可。

## 支持的本地模型服务

| 服务 | 默认端口 | 说明 |
|------|----------|------|
| Ollama | 11434 | 轻量级本地模型运行器 |
| LM Studio | 1234 | 图形界面模型管理工具 |
| vLLM | 8000 | 高性能推理引擎 |

## 前置准备

### Ollama

安装并拉取所需模型：

```bash
# 安装 Ollama（macOS/Linux）
curl -fsSL https://ollama.com/install.sh | sh

# 拉取模型
ollama pull llama3.1
ollama pull qwen2.5
ollama pull deepseek-r1

# 启动服务（默认监听 11434）
ollama serve
```

### LM Studio

1. 从 [lmstudio.ai](https://lmstudio.ai) 下载安装
2. 在界面中下载所需模型
3. 启动本地服务器（Local Server），默认端口 1234

### vLLM

```bash
pip install vllm

# 启动 OpenAI 兼容服务
python -m vllm.entrypoints.openai.api_server \
  --model Qwen/Qwen2.5-72B-Instruct \
  --port 8000
```

## 配置示例

```yaml
models:
  providers:
    - name: "ollama"
      api_key: "ollama"  # 占位符，不会被验证
      api_base: "http://localhost:11434/v1"
      models: ["llama3.1", "qwen2.5", "deepseek-r1"]
    - name: "lm-studio"
      api_key: "lm-studio"
      api_base: "http://localhost:1234/v1"
      models: ["loaded-model"]
    - name: "vllm"
      api_key: "token-xxx"
      api_base: "http://localhost:8000/v1"
      models: ["Qwen/Qwen2.5-72B-Instruct"]
```

## 注意事项

!!! warning "模型能力差异"
    本地模型的能力因模型而异。并非所有模型都支持以下功能：

    - **工具调用（Tool Calling）**：部分小型模型不支持或支持不完整
    - **视觉理解（Vision）**：仅多模态模型支持
    - **流式输出（Streaming）**：Ollama 和 vLLM 支持，部分配置下可能不稳定
    - **上下文窗口**：本地模型通常受显存限制，实际可用上下文可能远小于标称值

    本项目通过 OpenAI 兼容协议访问这些服务，因此工具调用格式、流式行为与图片传递方式都由本地服务端及所载模型决定，不由本项目实现。同一份配置换个模型就可能表现不同，接入新模型后建议先用一次带工具调用的对话验证。

## 性能建议

1. **显存管理**：本地模型运行需要充足的 GPU 显存。7B 模型约需 8GB，70B 模型约需 48GB+
2. **量化模型**：显存不足时可使用量化版本（如 Q4_K_M），牺牲少量精度换取更低的资源占用
3. **上下文长度**：适当减小 `max_tokens` 配置以避免 OOM。建议根据实际显存调整
4. **并发控制**：本地推理速度有限，避免高并发请求导致排队或超时
5. **模型预加载**：Ollama 首次请求会加载模型到显存，后续请求会快很多。可通过 `ollama run <model>` 预热
