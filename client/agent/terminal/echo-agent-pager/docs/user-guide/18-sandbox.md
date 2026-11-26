# Sandbox Mode

Sandbox mode restricts what the agent process and its spawned commands can access on your filesystem and network using OS-level kernel primitives (Landlock on Linux, Seatbelt on macOS). The kernel enforces these limits for the process lifetime.

Sandbox mode is off by default.

---

## Quick Start

```bash
# Run with workspace sandbox (read everywhere, write to CWD + temp dirs + ~/.echo-agent/)
echoagent --sandbox workspace

# Read-only mode (read everywhere, write only to ~/.echo-agent/ + temp dirs)
echoagent --sandbox read-only

# Most restrictive profile (read CWD + system paths, write CWD + temp dirs + ~/.echo-agent/, no child network)
echoagent --sandbox strict
```

---

## Built-in Profiles

| Profile               | FS Read            | FS Write                                       | Child Network | Use Case                          |
| --------------------- | ------------------ | ---------------------------------------------- | ------------- | --------------------------------- |
| `off` (default)       | Unrestricted       | Unrestricted                                   | Unrestricted  | No sandbox                        |
| `workspace`           | Everywhere         | CWD + `~/.echo-agent/` + `/tmp` + `/var/tmp`         | Allowed       | Normal development                |
| `devbox`              | Everywhere         | All top-level dirs except `/data`              | Allowed       | Disposable dev VMs                |
| `read-only`           | Everywhere         | `~/.echo-agent/` + `/tmp` + `/var/tmp`               | Blocked¹      | Exploration, code review          |
| `strict`              | CWD + system paths | CWD + `~/.echo-agent/` + `/tmp` + `/var/tmp`         | Blocked¹      | Untrusted code                    |

¹ Child-network blocking is enforced on **Linux only** (via seccomp). On macOS it is a no-op — these profiles do not restrict child-process network there.

To block specific files (e.g. `.env` or credential paths) on top of a profile, define a [custom profile](#custom-profiles) with a `deny` list — it is kernel-enforced (read + write/rename) and supports glob patterns like `**/*.pem`.

### Profile Details

**workspace** -- The recommended profile for everyday development. The agent can read any file on the system (for understanding dependencies, system libraries, etc.) but can only write to the current working directory, `~/.echo-agent/`, and temp directories (`/tmp`, `/var/tmp`, plus the macOS temp dirs). Network access is allowed for tools like `web_search` and MCP servers.

**devbox** -- A reserved built-in profile for disposable development VMs. The agent can read everywhere and write to every top-level directory except `/data` and the virtual filesystems (`/proc`, `/sys`, `/dev`), including the home directory. Network access is allowed. `--sandbox devbox` runs the built-in profile, which shadows any `[profiles.devbox]` you define in `sandbox.toml`.

**read-only** -- Use when you want the agent to analyze code without modifying your project files. The agent can read everything but can only write to `~/.echo-agent/` (needed for session persistence) and temp directories. Child-process network access is blocked on Linux (no-op on macOS).

**strict** -- The most restrictive profile, for reviewing untrusted code. The agent can only read files within the current working directory and essential system paths. Writes are limited to CWD, `~/.echo-agent/`, and temp directories. Child-process network access is blocked on Linux (no-op on macOS).

### Direct global hook write protection

Under `workspace`, `read-only`, and `strict` (and custom profiles that extend those bases), the EchoAgent state directory remains writable for session/runtime files, but the kernel **write-denies** the EchoAgent-owned direct disk paths used as user-global hook sources (they stay readable):

- `~/.echo-agent/hooks/` (hook directory)
- `~/.echo-agent/hooks-paths` (registry file; not loaded as hook JSON — only its absolute targets are)
- Absolute targets listed in `hooks-paths` (relative lines are ignored; missing targets refuse sandbox start)

On first launch under these profiles, EchoAgent creates a real empty `hooks/` directory and empty `hooks-paths` file when they are missing (never symlinks or wrong types). Claude/Cursor global settings are **not** covered by this write-deny; discovery of those vendors remains separately gated by compatibility settings.

A symlinked `$ECHO_AGENT_HOME` or a `hooks-paths` entry with a symlink component is refused at sandbox start (prevents retargeting). Existing parent directories of protected paths are pinned so they cannot be renamed out from under the deny (siblings remain writable). On Linux, nested user namespaces are disabled inside bubblewrap so mount binds cannot be rearranged. Project hooks remain gated by folder trust. The `devbox` profile does not apply this protection (disposable VMs). Profiles that require it refuse to start if the kernel policy cannot be applied (including Linux without verified read-only mounts).

---

## Custom Profiles

Create custom sandbox profiles in `~/.echo-agent/sandbox.toml` (global) or `.echo-agent/sandbox.toml` (per-project):

```toml
[profiles.project]
# Start from a built-in profile, then add overrides
extends = "workspace"
restrict_network = true

# Paths the agent can read but NOT write/delete
read_only = ["/data"]

# Additional writable paths
read_write = ["/tmp/scratch"]

# Paths or globs to kernel-deny (read + write/rename, enforced; see notes below)
deny = ["/data/shared-secrets", "**/.env", "**/*.pem"]
```

Use the custom profile:

```bash
echoagent --sandbox project
```

A custom profile can't reuse a built-in name. `--sandbox devbox` always runs the built-in `devbox` profile, shadowing any `[profiles.devbox]` you define.

If the user and project files define the same custom profile differently, EchoAgent uses the user profile and shows a startup warning. Run `/doctor` to see both file locations and how to resolve the conflict. Identical definitions do not produce a warning.

### Custom Profile Fields

| Field              | Type     | Description                                          |
| ------------------ | -------- | ---------------------------------------------------- |
| `extends`          | String   | Base built-in profile to inherit from (`workspace`, `devbox`, `read-only`, `strict`). Defaults to `workspace` when omitted |
| `restrict_network` | Boolean  | Block network access for child processes             |
| `read_only`        | String[] | Additional read-only paths                           |
| `read_write`       | String[] | Additional read-write paths                          |
| `deny`             | String[] | Paths or globs to kernel-deny (read + write/rename; see notes). An entry with `*`, `?`, or `[` is a glob |

> **Note on `read_only` / `read_write`:** These are **literal directory grants**,
> not globs. A trailing `/**` (or `/*`) is treated as the parent directory, so
> `…/cache/**` grants `…/cache` (and a bare `/**` grants `/`). Any entry that
> still contains `*`, `?`, or `[` after that (e.g. `/home/**/cache`, or a
> directory literally named `dir[1]`) is skipped with a warning — list the
> concrete directories you need, or put globs under `deny`. Entries with
> leading or trailing whitespace are also skipped with a warning: whitespace
> is significant in literal paths, so fix the entry rather than relying on
> trimming.

> **Note on `deny`:** A non-empty `deny` list is **kernel-enforced**. Denied paths
> are **read-denied and write/rename-denied** via Seatbelt on macOS and a bwrap
> bind-over on Linux, so a denied path can neither be read (via `bash`, `grep`, or
> subagents) nor relocated out of the deny set and read elsewhere (the
> `mv secret x && cat x` bypass is closed). On **Linux**, read-deny requires
> `bubblewrap`: if it is missing (or any single deny path can't be bound), EchoAgent
> refuses to start rather than run with denied paths exposed (`devbox`, which only
> write-denies `/data`, still falls back to Landlock). Writes to paths **not** in
> `deny` are controlled by what you grant in `read_write`.

> **Globs in `deny`:** An entry is a **glob** if it contains `*`, `?`, or `[`.
> Those characters **always** mean glob — to deny a literal file whose name
> contains them, name a parent directory instead. The supported, gitignore-style
> subset is:
>
> - `*` — any run of characters within one path segment (stops at `/`)
> - `?` — exactly one character within a segment
> - `**` — spans directories (as a whole path segment, e.g. `**/`, `a/**`); `**/`
>   also matches zero directories, so `**/.env` matches `.env` and `sub/.env`
> - `[abc]` / `[a-z]` — character classes; a leading `!` **or** `^` negates
>   (`[!a]` and `[^a]` both mean "not `a`")
>
> Brace alternation (`{a,b}`), backslash-escapes, empty path segments (a
> doubled `//` or a trailing `/`), `.` or `..` segments, and the unusual class
> forms `[]…]` (literal `]` first) and POSIX `[[:…:]]` are **not** supported,
> so the two platforms
> can never interpret a glob differently. A glob using an unsupported
> metacharacter, or one that is malformed, makes EchoAgent **refuse to start** (fail
> closed) on **both** platforms — write `*.pem` and `*.key` as separate entries
> rather than `*.{pem,key}`.
>
> Relative globs are anchored at the workspace; absolute globs (e.g.
> `/home/**/.ssh`) at their literal prefix. Non-glob entries keep exact-path
> matching. A relative glob matches **only inside the workspace**. To deny
> files elsewhere, write the entry as an absolute path. Enforcement otherwise
> differs by platform:
>
> - **macOS is airtight:** each glob becomes a Seatbelt regex applied at runtime,
>   so matching files are denied **even if created after EchoAgent starts**.
> - **Linux is best-effort:** a mount namespace can't glob at runtime, so each
>   glob is expanded to the files that **exist at launch** and those are bound
>   over. Files created **later** that match a glob are **not** covered — name
>   exact paths for anything that must be airtight on Linux. A matched symlink
>   is masked together with its resolved target. A glob that matches too many
>   files, or whose tree is too deep or broad to scan, makes EchoAgent **refuse to
>   start** rather than under-enforce; the error names the globs and the
>   directory where the scan stopped. The launch scan starts at each glob's
>   literal prefix and includes gitignored and hidden files, so on very large
>   workspaces prefer anchored globs (`certs/**/*.pem` scans only `certs/`)
>   over bare `**` patterns.

---

## How It Works

The sandbox is applied to the **entire echoagent process** at startup using kernel primitives -- not per-command wrapping. This means all tool operations are covered:

- `read_file`, `search_replace`, `list_dir` -- restricted by Landlock/Seatbelt in-process
- `bash` commands, `grep` (rg) -- child processes inherit FS restrictions automatically
- Network -- on Linux, child processes can be blocked via seccomp; on macOS this is a no-op

When a non-`off` sandbox profile is **requested** (CLI, `ECHO_AGENT_SANDBOX`, config, or a managed requirement):

- The agent runs **in-process**, not through the shared leader, so tool calls stay in this process when the profile is enforced. If leader mode would otherwise have been on, a one-line note at startup says so
- If a built-in profile fails to apply, EchoAgent warns and continues without enforcement (see [Platform Support](#platform-support)), but still refuses the leader so tools are not delegated elsewhere
- `echoagent workspace start`, `restart`, and `resume` are unavailable; `pause`, `stop`, and `status` still work

Disable the profile at the source that selected it to use the refused commands.

The sandbox is **irreversible** once applied. The agent cannot relax restrictions at runtime.

---

## Resuming Sessions

The profile a session was started with is saved with the session and is **fixed
for the life of the session**. When you resume it (`echoagent --resume <id>`,
`echoagent --continue`, or `echoagent -r`), EchoAgent restores that same profile automatically —
so a session started with `--sandbox workspace` won't silently come back under a
stricter default and break commands that previously worked.

Resuming will **not** change a session's sandbox:

- Omitting `--sandbox` on resume uses the session's saved profile.
- Passing `--sandbox <profile>` that **matches** the saved profile is allowed.
- Passing `--sandbox <profile>` that **differs** from the saved profile is
  **refused with an error** — changing a resumed session's sandbox is a safety
  footgun (it could widen access the session was meant to be confined to, or
  break a session that relied on broader access). Start a new session to use a
  different profile.

Profile resolution order for a **new** session:

1. An explicit `--sandbox <profile>` flag or `ECHO_AGENT_SANDBOX` environment variable
2. The `[sandbox] profile` in your config
3. `off` (no sandbox)

---

## Platform Support

| Platform | Mechanism | Minimum Version        |
| -------- | --------- | ---------------------- |
| Linux    | Landlock  | Kernel 5.13 or later   |
| macOS    | Seatbelt  | macOS (all versions)   |

If the sandbox cannot be applied (e.g., unsupported kernel, missing entitlements), EchoAgent logs a warning and continues without enforcement. The exception is an explicitly-requested **custom profile**: on **both macOS and Linux**, if it cannot be applied (unknown profile, malformed `sandbox.toml`, or — on Linux — `bubblewrap` unavailable for a non-empty `deny`), EchoAgent refuses to start rather than run with its denied paths exposed.

---

## Network Restrictions

On Linux, profiles with `restrict_network` block network access in **child processes** (bash commands, scripts) via seccomp. On macOS, network blocking is a no-op. Built-in tools that make HTTP requests in-process (web search, LLM API calls) are never affected -- the agent needs network access to function.

In practice, on Linux this means:

- `web_search`, `web_fetch`, and the LLM API always have network access
- `bash` commands like `curl`, `wget`, and `npm install` are blocked when `restrict_network` is enabled

---

## Shell Environment Policy

The sandbox controls which files and network a subprocess can reach. The top-level `[shell_environment_policy]` table controls which environment variables it inherits, so a tool command the model runs cannot read a secret that happens to sit in your shell environment.

```toml
[shell_environment_policy]
inherit = "core"                 # all (default) | core | none
ignore_default_excludes = false  # also drop *KEY* / *SECRET* / *TOKEN*
exclude = ["ACME_*", "CI_*"]     # drop these names
include_only = ["PATH", "HOME"]  # if set, keep only these names
set = { MY_FLAG = "1" }          # force these values
```

EchoAgent builds the child environment in order: it starts from `inherit` (`all` keeps everything, `core` keeps a small platform set such as `PATH` and `HOME`, `none` starts empty); drops the built-in secret patterns `*KEY*`, `*SECRET*`, and `*TOKEN*` unless `ignore_default_excludes = true`; drops any `exclude` matches; applies `set`; and, when `include_only` is non-empty, keeps only the matching names. Patterns are case-insensitive globs (`*`, `?`).

The default (`inherit = "all"`, `ignore_default_excludes = true`) leaves the environment untouched, so nothing changes until you configure a policy. On the non-persistent backend the policy also filters variables captured from your login shell, so an `.rc` file export cannot slip a secret past `exclude` or `include_only`. The persistent shell is one exception: it applies the policy to its base environment, but variables that an `.rc` file exports during login are replayed from a snapshot and are not re-filtered, so keep secrets out of shell startup files there. Enforcement covers the bash tool and terminals on macOS, Linux, and Windows.

---

## Event Logging

Sandbox events are logged to `~/.echo-agent/sandbox-events.jsonl` for debugging. Events include:

- Profile applied (which profile, timestamp)
- Violations (attempted access to denied paths)

---

## When to Use Sandbox Mode

**Use `workspace` when:**

- Working on your own projects and you want basic write protection
- Running in shared environments where you want to limit the scope of changes

**Define a custom profile with a `deny` list when:**

- You need to block specific files (e.g. `.env` or credential paths) on top of a base profile
- You need kernel enforcement that covers `bash`, `grep`, and subagents — not just the `read_file` tool

**Use `read-only` when:**

- Reviewing code you do not trust
- Exploring a codebase without risk of accidental modification
- Running code analysis or audits

**Use `strict` when:**

- Analyzing untrusted or third-party code
- Running in security-sensitive environments
- You want maximum isolation

**Skip sandbox when:**

- The agent needs to install dependencies (`npm install`, `pip install`)
- The agent needs to modify files outside the working directory
- You are working in a trusted environment and want maximum flexibility

---

## Trade-offs

| Aspect      | Without Sandbox            | With Sandbox                    |
| ----------- | -------------------------- | ------------------------------- |
| Safety      | Agent has full system access | Agent restricted to profile rules |
| Capability  | Can do anything            | Limited by profile              |
| Performance | No overhead                | Negligible overhead             |
| Recovery    | Must trust the agent       | Kernel enforces boundaries      |

The sandbox enforces limits at the OS level -- through Landlock or a mount namespace on Linux, and Seatbelt on macOS -- not a separate VM.
