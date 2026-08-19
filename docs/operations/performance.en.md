# Performance

Performance tuning guidelines for Echo Agent.

---

## Key Bottlenecks

1. **Model inference latency** — typically the dominant factor
2. **Context window / compression** — larger histories increase latency
3. **Memory retrieval** — vector search + reranking adds overhead
4. **Tool concurrency** — parallel tool calls vs sequential
5. **Channel streaming** — flush intervals affect perceived responsiveness

## Tuning Parameters

### Context & Compression

```yaml
compression:
  triggerRatio: 0.7      # compress when context reaches 70% of window
  targetRatio: 0.5       # compress down to 50%
```

### Memory Retrieval

```yaml
memory:
  retrievalTopK: 10      # reduce for faster retrieval
  rerankEnabled: true     # disable if latency matters more than relevance
```

### Tool Execution

```yaml
tools:
  maxConcurrent: 3       # parallel tool calls
  timeout: 30            # per-tool timeout (seconds)
```

### Spill Threshold

```yaml
spill:
  threshold: 8000        # chars before spill triggers
```

## Measurement

- Use `echo-agent cost --days 1` for token usage
- Check Gateway health endpoint for latency stats
- Dashboard Analytics for per-request timing
- OpenTelemetry traces for detailed breakdown

## General Advice

- Model latency dominates: choose faster models for interactive use
- Reduce `retrievalTopK` if memory retrieval feels slow
- Use `compression.triggerRatio` to balance context richness vs speed
- FAISS indexing is CPU-bound; keep knowledge base size reasonable
- Rate limiters protect against upstream throttling, not local performance

!!! note
    Do not provide absolute QPS guarantees — performance depends entirely on model provider latency and rate limits.
