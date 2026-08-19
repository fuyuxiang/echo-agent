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
      input_per_1k: 0.0
      output_per_1k: 0.0
    custom-gpt4:
      input_per_1k: 0.03
      output_per_1k: 0.06
```

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

!!! question "Needs maintainer confirmation"
    When the daily budget is exhausted, does the system hard-stop (reject new requests) or degrade gracefully (fall back to cheaper models)? This behavior is pending confirmation.

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
      input_per_1k: 0.0
      output_per_1k: 0.0
    # Custom pricing
    azure-gpt4o:
      input_per_1k: 0.005
      output_per_1k: 0.015
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

The router supports cost-aware decisions — simple tasks are automatically routed to lower-cost models:

```yaml
router:
  strategy: cost_aware
  rules:
    - condition: "complexity == 'simple'"
      prefer: cheap_model
    - condition: "complexity == 'complex'"
      prefer: capable_model
```

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
      input_per_1k: 0.0
      output_per_1k: 0.0
    deepseek-chat:
      input_per_1k: 0.001
      output_per_1k: 0.002

router:
  strategy: cost_aware
```
