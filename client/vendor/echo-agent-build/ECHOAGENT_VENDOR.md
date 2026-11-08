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

Local continuation-title correction (2026-11-07): a fresh fork persists the first
local ACP prompt index separately from the compaction boundary. Opening a fresh
continuation keeps its temporary title; its first local prompt supplies its own
title. Recovery after compaction uses that durable title index; inherited and
unscoped chunks cannot supply a continuation title. The standard-library-only production scope module
has direct Rust unit tests (`rustc --edition=2021 --test
crates/codegen/echo-agent-runtime/src/session/title_recovery_scope.rs`), and the
desktop native Core journey checks that a read-only fork and restart do not
generate a title from the parent history.

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

The desktop memory follow-up also keeps configured idle extraction and dream
intervals polling while memory is disabled. Each tick checks the live setting
before spawning work; re-enabling memory therefore resumes background processing
without requiring a new actor command. Native acceptance covers no writes while
disabled, automatic resume, pending-job recovery, and background consolidation.

Automatic memory log names now retain a filename-safe complete source ID under
an explicit `--sid-` suffix. Dream exclusion, forgotten-source checks and source
deletion share identity matching, with conservative compatibility for old eight-character
suffixes. This prevents rapid UUIDv7 conversations from overwriting each other's
logs or being mistaken for the current/forgotten source. Native acceptance uses
two real conversations in the same UUIDv7 timestamp window.

Source deletion recognizes full-ID history snapshots and archived logs without
falling back to UUID prefixes. Archive name collisions now use an explicit
`.archive-` timestamp suffix; existing UUID/hash timestamp archives remain
readable. Opaque source IDs containing the source delimiter are hashed to avoid
ambiguous identities. Tests also preserve the neighboring source's search index.

Derived memory titles use the same punctuation/case/whitespace normalization as
the recall policy. Manual edits repair retained derived keys at the ledger
boundary, while custom topics and revision checks remain intact. Reads repair
previously retained old titles in the current view without rewriting facts,
sources or revisions; subsequent saves/index refreshes persist the corrected
projection. New titles count Unicode scalar values, with compatibility for old
UTF-16 prefixes. `pnpm test:memory:rust` runs this dependency's own full unit suite
in an isolated profile and is a separate CI gate from host tests.

Bounded core memory context includes active, unpinned facts for the current
workspace project after existing preferences and pinned rules. This keeps
small durable project facts available when vector retrieval has no result;
global reference documents still use retrieval, and retired records stay out.

User message ACP updates now drain the replay log immediately instead of waiting
in the model text merge buffer. Prior buffered chunks and failed writes retain
FIFO ordering. This preserves the visible request after a host crash during model
response latency; recovery does not resubmit the request or repeat tools.

Legacy `/remember` titles truncated at 80 UTF-16 units are recognized alongside
60-unit import titles, so manual corrections also replace those old fact titles.
The ledger's Unicode/punctuation checks and custom-topic preservation remain in
force. `pnpm test:runtime:rust` executes the complete Runtime library suite,
including authentication and interrupted-request persistence, with private homes
and an explicit rejection of a missing or ignored interrupted-request regression.
Normal unit-mode credentials/home behavior is preserved; native review isolation
is tested explicitly rather than forced on every unit test. Discovery verifies
that the default-stack library group and the production 8 MiB SessionActor group
are disjoint and cover the entire harness. Each group owns a separate profile.
The Actor child also inherits that stack for unspecified worker threads; native
host tests and the rest of the library retain their ordinary worker environment.

Runtime configuration fixtures retain matching model keys after branding changes
and expect the current 64-character chunk overlap. The test-only prompt adapter
heap-allocates its turn future to avoid embedding it in every caller; production
stack sizes remain unchanged. The real TCP 401 retry-budget
test uses a real-clock watchdog; virtual-clock coverage stays in the retry
schedule unit tests so pending peer I/O cannot cause a false timeout.
Queue/Steer actor fixtures now read actual preferences in separate processes,
with a receipt rejecting zero-test child success. Disk changes and removal also
exercise cache invalidation. Retired chat-mode metadata, branded tool tokens,
and canonical workflow paths have current behavior assertions.

Global configuration watching canonicalizes the data home and compares canonical
parents consistently with home-level MCP routing. This fixes FSEvents events
using `/private/var` when the configured directory uses `/var`, including auth,
config and model cache changes. Scope/deleted-file and symlink routing regressions
run by default, and the three native global-change tests are re-enabled on macOS
with bounded event waiting instead of a fixed sleep.

Content-derived memory titles are display prefixes rather than unique topics.
Repeated saves match the exact full fact across legacy UTF-16 and current Unicode
titles, retaining the original ID. Unchanged retries retain the revision; newly
attributed source sessions advance it once. Independent facts sharing a title
prefix can be saved and corrected separately. Active title-format duplicates
with the same scope, kind, origin, pinning, validity and evidence are coalesced in
the read view, preserving the first ID/revision and all source sessions; the next
locked mutation persists the repaired ledger. Custom topics and distinct record
attributes remain separate. Dependency and host persistence regressions cover
upgrade/repeat/correction, stale-fact exclusion and reopening the saved ledger.
New IDs include the full fact to avoid reusing legacy ordinal-based IDs after
coalescing; existing record IDs remain stable.
Task topic keys remain unique because automatic task completion addresses them
by key; shared-prefix task saves cannot silently overwrite or complete another
task.
