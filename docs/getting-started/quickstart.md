# 快速上手

5 分钟完成从安装到发送第一条消息的全流程。

---

## 第一步：安装

```bash
pip install echo-agent[all]
```

验证安装成功：

```bash
echo-agent --version
# echo-agent 0.3.8
```

!!! tip "虚拟环境"
    建议在虚拟环境中安装，避免依赖冲突：

    ```bash
    python -m venv ~/.echo-agent/venv
    source ~/.echo-agent/venv/bin/activate
    pip install echo-agent[all]
    ```

---

## 第二步：运行 Setup 向导

```bash
echo-agent setup
```

向导会引导你完成以下配置：

1. **选择模型提供商** — OpenAI、Anthropic、Gemini、Bedrock、OpenRouter 或 OpenAI 兼容端点
2. **输入 API Key** — 对应提供商的密钥
3. **选择模型** — 如 `gpt-4o`、`claude-sonnet-4-20250514`、`gemini-2.0-flash` 等
4. **基本参数** — Agent 名称、语言偏好等

配置文件保存在 `~/.echo-agent/config.yaml`。

!!! note "也可手动配置"
    跳过向导直接编辑配置文件：

    ```bash
    mkdir -p ~/.echo-agent
    cat > ~/.echo-agent/config.yaml << 'EOF'
    model:
      provider: openai
      model: gpt-4o
      api_key: sk-your-key-here
    EOF
    ```

---

## 第三步：启动 Agent

```bash
echo-agent run
```

启动成功后你会看到类似输出：

```
[INFO] Echo Agent v0.3.8 starting...
[INFO] Model: openai/gpt-4o
[INFO] Memory: loaded (42 entries)
[INFO] Skills: 12 active
[INFO] Channels: cli
[INFO] Ready. Type your message below.
```

---

## 第四步：发送第一条消息

在终端中直接输入消息并回车：

```
You: 你好，请介绍一下你自己
```

Agent 会回复并记住这次对话。你可以继续对话，Agent 具备上下文记忆能力。

---

## 第五步：验证成功

确认以下功能正常工作：

```bash
# 查看运行状态
echo-agent status

# 查看费用统计
echo-agent cost

# 查看已加载的技能
echo-agent skill list
```

!!! tip "验证记忆"
    重启 Agent 后再次对话，询问上次聊了什么——如果它能回忆起来，说明记忆系统工作正常。

---

## 下一步

你已经成功运行了 Echo Agent。接下来可以：

- **接入更多平台** — 将 Agent 接入钉钉、微信、Slack 等通道，见 [通道配置](../integrations/channels/index.md)
- **后台运行** — 配置为系统服务，保持 7×24 在线，见 [部署指南](../operations/deployment.md)
- **打开 Dashboard** — 通过 Web 面板管理 Agent：
  ```bash
  echo-agent gateway
  # 浏览器访问 http://localhost:8080
  ```
- **探索技能** — 查看和管理 Agent 的技能库：
  ```bash
  echo-agent skill list
  echo-agent evolution status
  ```
- **定时任务** — 让 Agent 定时执行任务：
  ```bash
  echo-agent cron add "每天早上9点汇报天气" --schedule "0 9 * * *"
  ```
