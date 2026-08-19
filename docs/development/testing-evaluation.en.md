# Testing & Evaluation

Testing strategy and evaluation framework for Echo Agent.

---

## Test Framework

- **Backend**: pytest + pytest-asyncio + pytest-cov
- **Frontend**: Vitest + @testing-library/react

## Running Tests

```bash
# All backend tests
python -m pytest tests/ -v --cov

# Specific module
python -m pytest tests/test_memory*.py -v

# Frontend
cd web && pnpm test --run
```

## Coverage Requirement

`fail_under = 75` in pyproject.toml. CI fails if coverage drops below threshold.

## Evaluation Framework

Built-in behavioral evaluation:

```bash
echo-agent eval
```

### Dataset Format (YAML)

```yaml
- id: test_web_search
  input: "Search today's news"
  expected_tools: [web]
  expected_contains: ["news"]
  forbidden_tools: [shell]
  max_iterations: 5
```

### Metrics

- `contains_all` — Output contains expected content
- `tool_usage_correctness` — Correct tools used
- `iteration_efficiency` — Iterations within bounds
- `response_quality` — Response quality score
- `forbidden_tools_check` — Forbidden tools not invoked
- `semantic_quality` — Semantic quality assessment
