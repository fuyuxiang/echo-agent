# Local Models

Echo Agent supports local models through OpenAI-compatible API endpoints. All local model servers use the OpenAI provider with a custom `api_base`.

## Supported Local Model Servers

| Server | Default Port | Description |
|--------|-------------|-------------|
| Ollama | 11434 | Lightweight local model runner |
| LM Studio | 1234 | GUI-based model management tool |
| vLLM | 8000 | High-performance inference engine |

## Prerequisites

### Ollama

Install and pull the models you need:

```bash
# Install Ollama (macOS/Linux)
curl -fsSL https://ollama.com/install.sh | sh

# Pull models
ollama pull llama3.1
ollama pull qwen2.5
ollama pull deepseek-r1

# Start the server (listens on port 11434 by default)
ollama serve
```

### LM Studio

1. Download and install from [lmstudio.ai](https://lmstudio.ai)
2. Download your desired model through the UI
3. Start the Local Server (default port 1234)

### vLLM

```bash
pip install vllm

# Start the OpenAI-compatible server
python -m vllm.entrypoints.openai.api_server \
  --model Qwen/Qwen2.5-72B-Instruct \
  --port 8000
```

## Configuration

```yaml
models:
  providers:
    - name: "ollama"
      api_key: "ollama"  # placeholder, not validated
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

## Considerations

!!! warning "Model Capabilities Vary"
    Local model capabilities differ significantly depending on the model. Not all models support:

    - **Tool Calling**: Some smaller models lack support or have incomplete implementations
    - **Vision**: Only multimodal models support image inputs
    - **Streaming**: Supported by Ollama and vLLM, but may be unstable in certain configurations
    - **Context Window**: Local models are typically limited by available VRAM; usable context may be much smaller than advertised

    These servers are reached over the OpenAI-compatible protocol, so the tool-call format, streaming behaviour and image handling are all determined by the local server and the model it loads rather than implemented here. The same configuration can behave differently after a model swap, so verify a new model with one tool-calling conversation before relying on it.

## Performance Tips

1. **VRAM management**: Running local models requires sufficient GPU memory. 7B models need ~8GB, 70B models need ~48GB+
2. **Quantized models**: When VRAM is limited, use quantized variants (e.g., Q4_K_M) to trade minimal accuracy for lower resource usage
3. **Context length**: Reduce `max_tokens` to avoid OOM errors. Adjust based on your actual available VRAM
4. **Concurrency**: Local inference is slower than cloud APIs. Avoid high concurrency to prevent request queuing or timeouts
5. **Model preloading**: Ollama loads models into VRAM on first request; subsequent requests are much faster. Pre-warm with `ollama run <model>`
