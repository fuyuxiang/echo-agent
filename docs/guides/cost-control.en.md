# Cost Control

Echo Agent includes a built-in cost tracking and budget control system that helps you understand per-model spending and prevent unexpected overruns.

## Overview

The cost system provides:

- Per-model cost attribution
- Daily budget caps with soft threshold warnings
- Custom pricing tables (for local/self-hosted models)
- CLI and Dashboard query interfaces
- Router integration for cost-aware routing decisions

---

## Configuration

Enable cost tracking in your project configuration:

```yaml
cost:
  enabled: true
  daily_budget_usd: 5.0
  soft_threshold_ratio: 0.8
  pricing_overrides:
    my-local-llama:
      input_per_1m: 0
      output_per_1m: 0
    custom-gpt4:
      input_per_1m: 30
      output_per_1m: 60
```

!!! warning "Prices are per million tokens"
    The override keys are `input_per_1m`, `output_per_1m`, `cache_read_per_1m` and `cache_write_per_1m` — per **1M** tokens, not per 1K. `_resolve_price()` reads them with `ov.get("input_per_1m", 0.0)`, so a key like `input_per_1k` raises no error and instead falls back to `0.0`, leaving that model's cost permanently at zero.

### Field Reference

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Enable cost tracking and budget control |
| `daily_budget_usd` | float | `0.0` | Daily budget in USD; 0 means unlimited |
| `soft_threshold_ratio` | float | `0.8` | Ratio at which a soft warning is raised |
| `pricing_overrides` | dict | `{}` | Model pricing override table |

---

## Daily Budget and Soft Warnings

When `daily_budget_usd` is set to a positive value, the system accumulates spending within each UTC calendar day.

- **Soft warning**: Triggered when daily spending reaches `daily_budget_usd * soft_threshold_ratio`. For example, with a $5 budget and 0.8 threshold, a warning fires at $4.
- **Hard limit**: Triggered when daily spending reaches `daily_budget_usd` (see behavior below).

The hard cap is a **hard stop**: once `daily_budget_usd` is reached, further model calls are refused with a `BudgetExceeded` error carrying the amount spent and a note that the budget resets the next day. There is no automatic fallback to cheaper models — an exhausted budget stops work rather than silently changing output quality.

The soft threshold only emits a warning log, once per budget period, and does not affect calls. The counter rolls over on the UTC calendar day.

`daily_budget_usd` defaults to `0`, which disables the limit.

---

## Per-Model Cost Attribution

Token usage for each LLM call is tracked via `LLMResponse.usage`:

- `input_tokens` — number of input tokens
- `output_tokens` — number of output tokens
- `cache_read_input_tokens` — input tokens served from cache

The system calculates per-call cost using built-in or overridden pricing tables and aggregates by model.

---

## Pricing Overrides

For self-hosted models or custom endpoints, use `pricing_overrides` to set actual unit prices:

```yaml
cost:
  pricing_overrides:
    # Local models at zero cost
    ollama-llama3:
      input_per_1m: 0
      output_per_1m: 0
    # Custom pricing
    azure-gpt4o:
      input_per_1m: 5
      output_per_1m: 15
```

Models not listed in the override table use built-in pricing data.

---

## CLI Usage

Use `echo-agent cost` to view cost reports:

```bash
# View cost for the last 7 days
echo-agent cost --days 7

# Output as JSON (suitable for scripting)
echo-agent cost --days 30 --json
```

Output includes:

- Daily total spending
- Cost breakdown grouped by model
- Budget utilization percentage
- Cache hit rate statistics

---

## Dashboard Analytics

The Dashboard Analytics page provides visual cost insights:

- Daily/weekly/monthly cost trend charts
- Model cost proportion breakdown
- Cache hit rate trends
- Budget consumption progress bar

---

## Cost Optimization Strategies

### 1. Model Routing

Route different kinds of work to different models so cheaper models handle the simple tasks. Routing rules live in `models.routes`, and each rule matches on `task_types`:

```yaml
models:
  default_model: claude-sonnet-4-5
  routes:
    - task_types: [simple]
      provider: openai
      model: gpt-4o-mini
      max_tokens: 4096
    - task_types: [coding]
      provider: anthropic
      model: claude-sonnet-4-5
      fallback_models: [gpt-4o]
```

Each route accepts `provider`, `model`, `task_types`, `fallback_models`, `max_tokens`, `temperature` and `context_window`.

!!! note "There is no router section"
    Configuration has no top-level `router` section, and no `strategy: cost_aware`, `rules`, `condition` or `prefer` fields. Routing is always expressed through `task_types` on `models.routes`.

### 2. Prompt Caching

Leverage prompt caching to reduce billing for repeated input tokens:

- Static content (system prompts, tool definitions) is cached
- Monitor cache effectiveness via the `cache_read_input_tokens` metric
- Cached tokens are typically billed at a significant discount

### 3. Credential Pool Load Balancing

When multiple API keys are configured, the system automatically load-balances across them, avoiding retry overhead caused by single-key rate limiting.

### 4. Context Window Management

- Control conversation history length to avoid unnecessary token accumulation
- Use summaries instead of full history for long-running conversations
- Set appropriate `max_tokens` to limit output length

---

## Budget Alert Behavior

| State | Behavior |
|-------|----------|
| Spending < soft threshold | Normal operation |
| Spending >= soft threshold | WARNING log, yellow indicator on Dashboard |
| Spending >= hard limit | See note below |

!!! warning "Budget Exhausted"
    When daily spending reaches `daily_budget_usd`, the system blocks new LLM calls. Ensure your budget allows adequate headroom, or use `0` (unlimited) for non-critical scenarios.

---

## Full Configuration Example

```yaml
cost:
  enabled: true
  daily_budget_usd: 10.0
  soft_threshold_ratio: 0.75
  pricing_overrides:
    local-llama3-70b:
      input_per_1m: 0
      output_per_1m: 0
    deepseek-chat:
      input_per_1m: 1
      output_per_1m: 2

models:
  default_model: claude-sonnet-4-5
  routes:
    - task_types: [simple]
      provider: openai
      model: gpt-4o-mini
```
