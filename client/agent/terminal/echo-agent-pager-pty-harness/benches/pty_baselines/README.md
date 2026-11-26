# PTY benchmark baselines

Baselines are per-platform (macOS arm64 has very different timing from an
Linux arm64 CI runner) and per-scenario. CI compares the current run
against the matching platform file and fails if any scenario's p99 frame
time grows by more than 15% (default; `--threshold` overrides).

File naming: `<platform>.json` where `<platform>` matches the CI artifact
arch name — `linux-x86_64`, `linux-aarch64`, `macos-aarch64`.

## Producing a baseline

Run the full bench suite on a quiet machine:

```bash
cargo run -p echo-agent-pager --release --bin pty-bench -- \
  --all \
  --write-baseline terminal/echo-agent-pager-pty-harness/benches/pty_baselines/<platform>.json
```

## Overwriting after an intentional perf change

A PR that intentionally shifts frame timing (either direction) must update
the affected baselines. Include the `pty-bench` output from a clean run in
the PR body so reviewers can sanity-check the new numbers.

## First run

Platform files are seeded on first CI run (see `pager-bench` job). Until
then, `--baseline <missing-file>` will fail loudly with a clear error.
