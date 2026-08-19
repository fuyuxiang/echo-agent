# Quickstart

Go from installation to your first message in 5 minutes.

---

## Step 1: Install

```bash
pip install echo-agent[all]
```

Verify the installation:

```bash
echo-agent --version
# echo-agent 0.3.7
```

!!! tip "Virtual Environment"
    It's recommended to install in a virtual environment to avoid dependency conflicts:

    ```bash
    python -m venv ~/.echo-agent/venv
    source ~/.echo-agent/venv/bin/activate
    pip install echo-agent[all]
    ```

---

## Step 2: Run the Setup Wizard

```bash
echo-agent setup
```

The wizard guides you through:

1. **Choose a model provider** — OpenAI, Anthropic, Gemini, Bedrock, OpenRouter, or OpenAI-compatible endpoints
2. **Enter your API key** — the key for your chosen provider
3. **Select a model** — e.g., `gpt-4o`, `claude-sonnet-4-20250514`, `gemini-2.0-flash`
4. **Basic settings** — Agent name, language preference, etc.

Configuration is saved to `~/.echo-agent/config.yaml`.

!!! note "Manual Configuration"
    Skip the wizard and edit the config file directly:

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

## Step 3: Start the Agent

```bash
echo-agent run
```

On successful startup, you'll see output like:

```
[INFO] Echo Agent v0.3.7 starting...
[INFO] Model: openai/gpt-4o
[INFO] Memory: loaded (42 entries)
[INFO] Skills: 12 active
[INFO] Channels: cli
[INFO] Ready. Type your message below.
```

---

## Step 4: Send Your First Message

Type a message directly in the terminal and press Enter:

```
You: Hello, tell me about yourself
```

The Agent will respond and remember the conversation. You can continue chatting — it maintains context across turns.

---

## Step 5: Verify Success

Confirm that everything is working:

```bash
# Check running status
echo-agent status

# View cost statistics
echo-agent cost

# List loaded skills
echo-agent skill list
```

!!! tip "Verify Memory"
    Restart the Agent and ask what you talked about last time. If it recalls, the memory system is working correctly.

---

## Next Steps

You've successfully run Echo Agent. Here's where to go next:

- **Connect more platforms** — Integrate with DingTalk, WeChat, Slack, and more. See [Channel Configuration](../integrations/channels/index.md)
- **Run in background** — Configure as a system service for 24/7 availability. See [Deployment Guide](../operations/deployment.md)
- **Open the Dashboard** — Manage your Agent via the web panel:
  ```bash
  echo-agent gateway
  # Open http://localhost:8080 in your browser
  ```
- **Explore skills** — View and manage the Agent's skill library:
  ```bash
  echo-agent skill list
  echo-agent evolution status
  ```
- **Scheduled tasks** — Have the Agent run tasks on a schedule:
  ```bash
  echo-agent cron add "Report weather every morning at 9am" --schedule "0 9 * * *"
  ```
