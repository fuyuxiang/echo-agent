# EchoAgent Vendored Runtime

This directory is a source snapshot tracked directly by the
`echo-agent-desktop` repository. It is not a Git submodule.

- Source: <https://github.com/fuyuxiang/echo-agent.git>
- Source revision: `c2ad97f87aea4303b6000a2c22128bc91ee76c9b`
- Source license: Apache License 2.0
- Imported: 2026-09-01
- Runtime name: `echo-agent`
- Rust entry crate: `echo-agent-runtime`

The checked-in snapshot is maintained as the `echo-agent` Agent Runtime. Its crate
names, Rust modules, environment variables, configuration keys, protocol
identifiers, and user-facing terminology use the EchoAgent namespace. The
protocol migration helper lives in `scripts/rename-runtime-namespace.*`.
Modified files are maintained as part of EchoAgent.

Local build correction (2026-10-28): `echo-agent-proto-build` resolves the actual
executable selected from PATH before emitting Cargo file watches, and tracks
PATH/PROTOC environment changes. This prevents a missing relative `protoc` watch
from rebuilding the Runtime on every Cargo invocation. Locator regressions run
with `cargo test --locked --manifest-path vendor/echo-agent-build/Cargo.toml -p echo-agent-proto-build --lib`.

Local Windows build correction (2026-10-30): `echo-agent-proto-build` writes both
the dependency manifest and the disposable descriptor set inside a temporary
directory. Passing `NUL` as the descriptor output could create a real reserved-name
file in the crate directory on Windows. Dependency parsing matches the complete
descriptor target so drive letters and spaces in temporary paths remain valid.
The library tests above cover Windows manifest paths and invoke real `protoc` in
an isolated workspace to check for leftover files on success and failure.

When importing a newer source revision, use a temporary checkout, compare it
with the maintained source and integrate required compatibility changes there,
run the complete namespace migration, then replace only the tracked source snapshot
(never `target/` or nested Git metadata). Update `ECHOAGENT_VENDOR.json`, this
file, `THIRD_PARTY_NOTICES.md`, and the changelog, then run the complete frontend
and Rust test suites before committing.
