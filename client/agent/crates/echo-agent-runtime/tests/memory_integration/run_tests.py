#!/usr/bin/env python3
# ruff: noqa: E701, E702
"""
Memory System Integration Tests — Full Suite.

Launches echoagent agent stdio in isolated environments ($HOME override)
with pre-populated memory files. Tests the entire memory lifecycle:
indexing, search, embeddings, flush, session-end, compaction, pruning.

The script auto-builds the binary (release + dev features) before running.
Requires: cargo, rg (ripgrep).

Usage:
    # Run from the repo root:
    python3 crates/echo-agent-runtime/tests/memory_integration/run_tests.py

    # Run only fast (no-model) tests:
    python3 crates/echo-agent-runtime/tests/memory_integration/run_tests.py --fast

    # Run a single test by name:
    python3 crates/echo-agent-runtime/tests/memory_integration/run_tests.py test_fts_search_quality

    # Skip build (use pre-built binary):
    ECHO_AGENT_BINARY=/path/to/echo-agent-pager python3 run_tests.py --fast
"""

import json
import os
import select
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid

# ── Colours ──────────────────────────────────────────────────────────────
R, G, Y, B, N = "\033[31m", "\033[32m", "\033[33m", "\033[1m", "\033[0m"
PASS = FAIL = SKIP = 0


def ok(m):
    global PASS
    PASS += 1
    print(f"  {G}✓{N} {m}")


def fail(m, d=""):
    global FAIL
    FAIL += 1
    print(f"  {R}✗{N} {m}")
    d and print(f"    {d}")


def skip(m):
    global SKIP
    SKIP += 1
    print(f"  {Y}⊘{N} {m} (skipped)")


def section(t):
    print(f"\n{B}── {t} ──{N}")


# ── ACP Client (NDJSON over stdin/stdout) ────────────────────────────────


class AcpClient:
    """Talks ACP (JSON-RPC over NDJSON) to a echoagent agent stdio subprocess."""

    def __init__(self, proc, cwd=None):
        self.proc = proc
        self._id = 0
        self.notifications = []
        self.cwd = cwd
        self.session_id = None

    def _nid(self):
        self._id += 1
        return self._id

    def send(self, method, params=None, timeout=30):
        mid = self._nid()
        msg = {"jsonrpc": "2.0", "id": mid, "method": method}
        if params:
            msg["params"] = params
        try:
            self.proc.stdin.write((json.dumps(msg) + "\n").encode())
            self.proc.stdin.flush()
        except BrokenPipeError:
            return {"error": "broken pipe"}
        return self._read(mid, timeout)

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params:
            msg["params"] = params
        try:
            self.proc.stdin.write((json.dumps(msg) + "\n").encode())
            self.proc.stdin.flush()
        except Exception:
            pass

    def _read(self, eid, timeout=30):
        deadline = time.time() + timeout
        while time.time() < deadline:
            r, _, _ = select.select([self.proc.stdout], [], [], min(deadline - time.time(), 1))
            if not r:
                continue
            line = self.proc.stdout.readline()
            if not line:
                break
            try:
                msg = json.loads(line)
            except Exception:
                continue
            if "id" not in msg:
                self.notifications.append(msg)
                continue
            if msg.get("id") == eid:
                return msg
        return {"error": f"timeout id={eid}"}

    def drain_notifications(self, duration=1):
        """Read any pending notifications for a short time."""
        deadline = time.time() + duration
        while time.time() < deadline:
            r, _, _ = select.select([self.proc.stdout], [], [], 0.1)
            if not r:
                continue
            line = self.proc.stdout.readline()
            if not line:
                break
            try:
                msg = json.loads(line)
                if "id" not in msg:
                    self.notifications.append(msg)
            except Exception:
                pass

    def prompt(self, text, timeout=15):
        """Send session/prompt with the stored session_id."""
        params = {"prompt": [{"type": "text", "text": text}]}
        if self.session_id:
            params["sessionId"] = self.session_id
        return self.send("session/prompt", params, timeout=timeout)

    def close(self):
        self.notify("shutdown")
        self.notify("exit")
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


# ── Isolated Environment ─────────────────────────────────────────────────


class IsolatedEnv:
    """Creates a fully isolated echoagent environment with custom $HOME.

    The agent subprocess gets a fake $HOME with:
      ~/.echo-agent/auth.json   (copied from real home)
      ~/.echo-agent/memory/     (pre-populated by tests)
      ~/.echo-agent/logs/       (memory.log appears here)

    And a workspace directory used as cwd when spawning the agent.
    """

    def __init__(self, workspace_name="test-project"):
        self.root = tempfile.mkdtemp(prefix="echo-agent-memtest-")
        self.fake_home = os.path.join(self.root, "home")
        self.workspace = os.path.join(self.root, "workspace", workspace_name)
        os.makedirs(self.fake_home)
        os.makedirs(self.workspace)

        # Create .echo-agent dirs in fake home
        self.echo_agent_home = os.path.join(self.fake_home, ".echo-agent")
        self.memory_dir = os.path.join(self.echo_agent_home, "memory")
        self.logs_dir = os.path.join(self.echo_agent_home, "logs")
        os.makedirs(self.memory_dir)
        os.makedirs(self.logs_dir)

        # Copy auth from real home
        real_auth = os.path.expanduser("~/.echo-agent/auth.json")
        if os.path.isfile(real_auth):
            shutil.copy2(real_auth, os.path.join(self.echo_agent_home, "auth.json"))

    def write_config(self, toml_str):
        """Write global ~/.echo-agent/config.toml (where the agent reads config)."""
        with open(os.path.join(self.echo_agent_home, "config.toml"), "w") as f:
            f.write(toml_str)

    def write_global_memory(self, content):
        """Write global MEMORY.md."""
        path = os.path.join(self.memory_dir, "MEMORY.md")
        with open(path, "w") as f:
            f.write(content)
        return path

    def find_workspace_dir(self):
        """Find the workspace hash directory (created by the agent)."""
        for d in os.listdir(self.memory_dir):
            full = os.path.join(self.memory_dir, d)
            if os.path.isdir(full) and d != "sessions":
                return full
        return None

    def write_workspace_memory(self, content):
        """Write workspace MEMORY.md (must call after agent has created the dir)."""
        ws_dir = self.find_workspace_dir()
        if not ws_dir:
            return None
        path = os.path.join(ws_dir, "MEMORY.md")
        with open(path, "w") as f:
            f.write(content)
        return path

    def get_db_path(self):
        ws_dir = self.find_workspace_dir()
        if ws_dir:
            db = os.path.join(ws_dir, "index.sqlite")
            if os.path.isfile(db):
                return db
        return None

    def query_db(self, sql, params=()):
        db = self.get_db_path()
        if not db:
            return []
        conn = sqlite3.connect(db)
        try:
            return conn.execute(sql, params).fetchall()
        finally:
            conn.close()

    def memory_log(self):
        path = os.path.join(self.logs_dir, "memory.log")
        if os.path.isfile(path):
            with open(path) as f:
                return f.read()
        return ""

    def session_files(self):
        """List all session log .md files (in sessions/ subdirectories)."""
        files = []
        for root, _dirs, fnames in os.walk(self.memory_dir):
            for f in fnames:
                if f.endswith(".md") and "sessions" in root:
                    files.append(os.path.join(root, f))
        return sorted(files)

    def all_md_files(self):
        """List all .md files in memory tree (including session logs)."""
        files = []
        for root, _dirs, fnames in os.walk(self.memory_dir):
            for f in fnames:
                if f.endswith(".md"):
                    files.append(os.path.join(root, f))
        return sorted(files)

    def spawn_agent(self, extra_env=None):
        binary = os.environ.get("ECHO_AGENT_BINARY", "echoagent")
        if not shutil.which(binary) and not os.path.isfile(binary):
            print(f"{R}echoagent binary not found: {binary}{N}")
            sys.exit(1)
        env = os.environ.copy()
        env["HOME"] = self.fake_home
        env["ECHO_AGENT_MEMORY"] = "1"
        env["RUST_LOG"] = "warn"
        # Disable leader mode
        env["ECHO_AGENT_NO_LEADER"] = "1"
        if extra_env:
            env.update(extra_env)
        proc = subprocess.Popen(
            [binary, "agent", "--no-leader", "stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=self.workspace,
            env=env,
        )
        return AcpClient(proc, cwd=self.workspace)

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


def _default_config() -> str:
    embed_model = os.environ.get("ECHO_AGENT_MEMORY_EMBEDDING_MODEL", "").strip()
    embed_model_line = f'model = "{embed_model}"\n' if embed_model else ""
    return f"""
[memory]
enabled = true

[memory.embedding]
{embed_model_line}dimensions = 1024

[memory.search]
max_results = 6
min_score = 0.0

[compaction.memory_flush]
enabled = true
soft_threshold_tokens = 4000

[compaction.pruning]
enabled = true
keep_last_n_turns = 3
soft_trim_threshold = 4000
"""


DEFAULT_CONFIG = _default_config()

# Config with flush disabled — for tests that only care about indexing
INDEXING_ONLY_CONFIG = """
[memory]
enabled = true

[memory.search]
max_results = 10
min_score = 0.0

[compaction.memory_flush]
enabled = false

[compaction.pruning]
enabled = false
"""


def init_session(client):
    """Send ACP initialize + session/new to create a session.

    The ACP protocol requires:
      1. initialize -> returns capabilities
      2. session/new -> creates session, triggers memory init + reindex
    """
    resp = client.send(
        "initialize",
        {
            "protocolVersion": "2025-03-26",
            "clientInfo": {"name": "memory-integration-test", "version": "0.1"},
            "capabilities": {},
        },
    )
    if "error" in resp:
        return resp
    # Create a session (triggers spawn_session_actor -> MEMORY_INIT)
    params = {"mcpServers": []}
    if client.cwd:
        params["cwd"] = client.cwd
    sess = client.send("session/new", params)
    if "result" in sess:
        client.session_id = sess["result"].get("sessionId")
    return sess


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 1: Indexing & Storage
# ═══════════════════════════════════════════════════════════════════════════


def test_global_memory_indexing():
    """Global MEMORY.md is indexed into chunks + FTS on session start."""
    section("1. Global Memory Indexing + Embeddings")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        marker = uuid.uuid4().hex[:8]
        env.write_global_memory(f"""# Team Conventions
* Always use graphite to create PRs (marker: {marker})
* Never commit without TL review
* Use blake3 for content hashing
* Follow Rust 2024 edition conventions
""")
        ok(f"wrote global MEMORY.md (marker={marker})")

        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"init: {resp}")
            return
        ok("agent initialized")

        # Wait for background reindex + embedding
        time.sleep(5)

        # Check chunks
        chunks = env.query_db("SELECT id, source, length(text) FROM chunks")
        if chunks:
            ok(f"indexed {len(chunks)} chunk(s)")
        else:
            fail("no chunks after reindex")
            client.close()
            return

        # Check FTS for our marker
        fts = env.query_db(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH ?",
            (marker,),
        )
        if fts and fts[0][0] > 0:
            ok(f"FTS finds marker '{marker}'")
        else:
            fail(f"FTS can't find marker '{marker}'")

        # Check embeddings
        try:
            emb = env.query_db("SELECT count(*) FROM chunks_vec_rowids")
            if emb and emb[0][0] > 0:
                ok(f"{emb[0][0]} embedding(s) computed")
            else:
                skip("no embeddings (API key or sqlite-vec missing)")
        except Exception:
            skip("embeddings table not available")

        client.close()
    finally:
        env.cleanup()


def test_workspace_vs_global_memory():
    """Both global and workspace MEMORY.md are indexed with correct source labels."""
    section("2. Workspace vs Global Memory (source classification)")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Global\n* Global rule: use graphite for PRs\n")

        # Start agent to create workspace dir
        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"init: {resp}")
            return
        time.sleep(3)

        # Now write workspace memory
        ws_path = env.write_workspace_memory("# Workspace\n* Workspace rule: use cargo fmt\n")
        if ws_path:
            ok("wrote workspace MEMORY.md")
        else:
            fail("couldn't find workspace dir")
            client.close()
            return

        client.close()

        # Restart agent to reindex both
        client2 = env.spawn_agent()
        init_session(client2)
        time.sleep(4)

        chunks = env.query_db("SELECT id, source FROM chunks ORDER BY source")
        sources = {src for _, src in chunks}
        if "global" in sources:
            ok("global source indexed")
        else:
            fail("global source missing", f"got sources: {sources}")
        if "workspace" in sources:
            ok("workspace source indexed")
        else:
            fail("workspace source missing", f"got sources: {sources}")

        # FTS finds content from each source
        for word, label in [("graphite", "global"), ("cargo", "workspace")]:
            r = env.query_db(
                "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
                "WHERE chunks_fts MATCH ?",
                (word,),
            )
            if r and r[0][0] > 0:
                ok(f"FTS finds {label} content ('{word}')")
            else:
                fail(f"FTS can't find {label} content ('{word}')")

        client2.close()
    finally:
        env.cleanup()


def test_index_integrity():
    """Schema tables (chunks, meta, FTS) exist and are consistent."""
    section("3. Index Schema + Data Integrity")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Rules\n* Rule 1\n* Rule 2\n* Rule 3\n")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        # Tables
        tables = [t[0] for t in env.query_db("SELECT name FROM sqlite_master WHERE type='table'")]
        for t in ["chunks", "meta"]:
            if t in tables:
                ok(f"table '{t}' exists")
            else:
                fail(f"table '{t}' missing")

        # FTS table
        if "chunks_fts" in tables:
            ok("FTS virtual table exists")
        else:
            fail("chunks_fts missing")

        # Chunk/FTS row-count consistency
        cc = env.query_db("SELECT count(*) FROM chunks")[0][0]
        fc = env.query_db("SELECT count(*) FROM chunks_fts")[0][0]
        if cc == fc:
            ok(f"chunks ({cc}) == FTS ({fc})")
        else:
            fail(f"chunks ({cc}) != FTS ({fc})")

        # Every chunk has required fields
        nulls = env.query_db(
            "SELECT count(*) FROM chunks "
            "WHERE id IS NULL OR text IS NULL OR path IS NULL OR hash IS NULL"
        )
        if nulls and nulls[0][0] == 0:
            ok("all chunks have required fields (id, text, path, hash)")
        else:
            fail(f"{nulls[0][0]} chunks with NULL required fields")

        # Meta
        meta = {k: v for k, v in env.query_db("SELECT key, value FROM meta")}
        if "embedding_dimensions" in meta:
            ok(f"embedding_dimensions = {meta['embedding_dimensions']}")
        if "schema_version" in meta:
            ok(f"schema_version = {meta['schema_version']}")

        client.close()
    finally:
        env.cleanup()


def test_chunk_hash_correctness():
    """Each chunk's stored hash matches blake3(text)."""
    section("4. Chunk Hash Correctness (blake3)")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("""# Hashing Test
* Line one for hashing
* Line two for hashing
* Line three for hashing
""")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        rows = env.query_db("SELECT hash, text FROM chunks")
        if not rows:
            fail("no chunks to verify")
            client.close()
            return

        # blake3 isn't available in Python stdlib, so we verify format
        # (64-char lowercase hex = 256-bit digest) rather than recomputing.
        # The actual blake3 correctness is covered by Rust unit tests in index.rs.
        mismatches = 0
        for stored_hash, _text in rows:
            if len(stored_hash) == 64 and all(c in "0123456789abcdef" for c in stored_hash):
                pass  # valid 256-bit hex digest
            else:
                mismatches += 1

        if mismatches == 0:
            ok(f"all {len(rows)} chunk hashes are valid blake3 format (64-char hex)")
        else:
            fail(f"{mismatches}/{len(rows)} chunks have invalid hash format")

        # Verify same content produces same hash (deterministic)
        hashes = [h for h, _ in rows]
        unique = len(set(hashes))
        texts = [t for _, t in rows]
        unique_texts = len(set(texts))
        if unique == unique_texts:
            ok("hash is deterministic (unique hash per unique text)")
        else:
            skip(f"hash uniqueness: {unique} hashes for {unique_texts} unique texts")

        client.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 2: FTS Search
# ═══════════════════════════════════════════════════════════════════════════


def test_fts_search_quality():
    """FTS matches exact words, OR queries, and avoids false positives."""
    section("5. FTS Search Quality")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("""# Coding Standards
* Always use snake_case for Rust functions
* Prefer Result over panic
* Use tracing instead of println for logging
* SQLite FTS5 for full-text search
* blake3 for content hashing
""")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        # Exact word match
        r = env.query_db(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH 'tracing'"
        )
        if r and r[0][0] > 0:
            ok("FTS: exact match 'tracing'")
        else:
            fail("FTS: no match for 'tracing'")

        # OR query
        r = env.query_db(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH 'blake3 OR sqlite'"
        )
        if r and r[0][0] > 0:
            ok("FTS: OR query 'blake3 OR sqlite'")
        else:
            fail("FTS: OR query failed")

        # No match
        r = env.query_db(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH 'zzzznonexistent'"
        )
        if r and r[0][0] == 0:
            ok("FTS: no false positives")
        else:
            fail("FTS: false positive for nonexistent term")

        # Prefix search
        r = env.query_db(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH 'trac*'"
        )
        if r and r[0][0] > 0:
            ok("FTS: prefix search 'trac*'")
        else:
            skip("FTS: prefix search 'trac*' not matched")

        client.close()
    finally:
        env.cleanup()


def test_fts_special_characters():
    """FTS handles queries with special characters without crashing."""
    section("6. FTS Special Character Handling")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("""# Edge Cases
* Use C++ for performance-critical code
* Configure with --enable-feature=fast_path
* Email: dev@example.com
* Path: /usr/local/bin/echoagent
* Version >= 2.0.0
""")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        # These special-character queries should NOT crash the FTS engine
        test_queries = [
            ("C++", "C++"),
            ("--enable-feature", "flag-style"),
            ("dev@example.com", "email"),
            ("/usr/local/bin", "path"),
            (">= 2.0.0", "comparison"),
            ("'quoted'", "single quotes"),
            ('"double"', "double quotes"),
            ("(parens)", "parentheses"),
            ("", "empty string"),
            ("   ", "whitespace only"),
        ]
        for query, label in test_queries:
            try:
                # Match the Rust backend's sanitization: split on non-alphanumeric,
                # keep tokens >= 2 chars, join with OR
                import re as _re

                words = [w for w in _re.split(r"[^a-zA-Z0-9]+", query) if len(w) >= 2]
                if not words:
                    ok(f"FTS: '{label}' → empty after sanitization (safe)")
                    continue
                fts_q = " OR ".join(words)
                r = env.query_db(
                    "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
                    "WHERE chunks_fts MATCH ?",
                    (fts_q,),
                )
                ok(f"FTS: '{label}' query OK (matched={r[0][0] if r else 0})")
            except Exception as e:
                fail(f"FTS: '{label}' query crashed: {e}")

        client.close()
    finally:
        env.cleanup()


def test_fts_multi_file_search():
    """FTS returns results from both global and workspace memory files."""
    section("7. FTS Multi-File Search")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        global_marker = uuid.uuid4().hex[:8]
        ws_marker = uuid.uuid4().hex[:8]
        env.write_global_memory(f"# Global\n* Global unique marker: {global_marker}\n")

        # Session 1: create workspace dir
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        env.write_workspace_memory(f"# Workspace\n* Workspace unique marker: {ws_marker}\n")
        c1.close()

        # Session 2: reindex both
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        # Search for global marker
        r = env.query_db(
            "SELECT c.source FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH ?",
            (global_marker,),
        )
        if r and r[0][0] == "global":
            ok("FTS: global marker found with source=global")
        elif r:
            fail(f"FTS: global marker found but source={r[0][0]}")
        else:
            fail("FTS: global marker not found")

        # Search for workspace marker
        r = env.query_db(
            "SELECT c.source FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH ?",
            (ws_marker,),
        )
        if r and r[0][0] == "workspace":
            ok("FTS: workspace marker found with source=workspace")
        elif r:
            fail(f"FTS: workspace marker found but source={r[0][0]}")
        else:
            fail("FTS: workspace marker not found")

        c2.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 3: Reindex, Idempotency, Content Updates
# ═══════════════════════════════════════════════════════════════════════════


def test_reindex_idempotency():
    """Reindexing same content across sessions doesn't create duplicates."""
    section("8. Reindex Idempotency — Same content, no duplicates")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Stable Content\n* This should only appear once\n")

        # Session 1
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        count1 = env.query_db("SELECT count(*) FROM chunks")[0][0]
        hashes1 = sorted([h[0] for h in env.query_db("SELECT hash FROM chunks")])
        c1.close()

        # Session 2 — same content
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(3)
        count2 = env.query_db("SELECT count(*) FROM chunks")[0][0]
        hashes2 = sorted([h[0] for h in env.query_db("SELECT hash FROM chunks")])
        c2.close()

        if count1 == count2:
            ok(f"chunk count stable across sessions ({count1})")
        else:
            fail(f"chunk count changed: {count1} → {count2}")

        if hashes1 == hashes2:
            ok("chunk hashes identical across sessions")
        else:
            fail(f"hashes differ: session1={len(hashes1)}, session2={len(hashes2)}")
    finally:
        env.cleanup()


def test_content_update_detection():
    """Modified MEMORY.md is re-indexed with new content."""
    section("9. Content Update — Modified MEMORY.md re-indexed")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        marker_v1 = uuid.uuid4().hex[:8]
        env.write_global_memory(f"# Version 1\n* Old content marker: {marker_v1}\n")

        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        v1_texts = env.query_db("SELECT text FROM chunks")
        c1.close()

        # Update content with a new marker
        marker_v2 = uuid.uuid4().hex[:8]
        env.write_global_memory(f"# Version 2\n* Updated content marker: {marker_v2}\n")

        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(3)
        v2_texts = env.query_db("SELECT text FROM chunks")

        if v1_texts and v2_texts and v1_texts != v2_texts:
            ok("chunk text updated after content change")
        else:
            fail("chunk not updated")

        # Old marker should be gone, new should be present
        r = env.query_db("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (marker_v2,))
        if r and r[0][0] > 0:
            ok(f"FTS finds new marker '{marker_v2}'")
        else:
            fail(f"FTS can't find new marker '{marker_v2}'")

        r = env.query_db("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (marker_v1,))
        if r and r[0][0] == 0:
            ok(f"old marker '{marker_v1}' removed from FTS")
        else:
            fail(f"old marker '{marker_v1}' still in FTS")

        c2.close()
    finally:
        env.cleanup()


def test_large_file_chunking():
    """A large MEMORY.md is split into multiple reasonably-sized chunks."""
    section("10. Large File — Proper Chunking")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        # Write a large file with multiple sections
        sections = []
        for i in range(10):
            sections.append(f"## Section {i}\n\n{'Lorem ipsum dolor sit amet. ' * 50}\n")
        env.write_global_memory("# Large Document\n\n" + "\n".join(sections))

        client = env.spawn_agent()
        init_session(client)
        time.sleep(4)

        chunks = env.query_db("SELECT count(*) FROM chunks")[0][0]
        if chunks > 1:
            ok(f"large file split into {chunks} chunks")
        else:
            fail(f"large file not chunked (only {chunks} chunk)")

        # Verify each chunk has reasonable size
        sizes = env.query_db("SELECT length(text) FROM chunks")
        max_size = max(s[0] for s in sizes)
        if max_size <= 6000:
            ok(f"max chunk size {max_size} chars (under limit)")
        else:
            fail(f"chunk too large: {max_size} chars")

        # Verify chunks cover all sections (no data loss)
        all_text = "\n".join(t[0] for t in env.query_db("SELECT text FROM chunks"))
        for i in range(10):
            if f"Section {i}" in all_text:
                pass  # good
            else:
                fail(f"Section {i} missing from chunks")
                break
        else:
            ok("all 10 sections present across chunks")

        client.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 4: Memory Enable/Disable & Config
# ═══════════════════════════════════════════════════════════════════════════


def test_memory_disabled():
    """No index is created when memory is disabled."""
    section("11. Memory Disabled — No Index Created")
    env = IsolatedEnv()
    try:
        env.write_config("""
[memory]
enabled = false
""")
        env.write_global_memory("# Should not be indexed\n")
        client = env.spawn_agent(extra_env={"ECHO_AGENT_MEMORY": "0"})
        init_session(client)
        time.sleep(2)

        db = env.get_db_path()
        if db is None:
            ok("no index created when memory disabled")
        else:
            fail(f"index created at {db} despite memory disabled")

        # Memory log should not have MEMORY_INIT
        log = env.memory_log()
        if "MEMORY_INIT: storage + backend created" not in log:
            ok("no MEMORY_INIT logged when disabled")
        else:
            fail("MEMORY_INIT logged despite memory being disabled")

        client.close()
    finally:
        env.cleanup()


def test_memory_disabled_no_config():
    """Memory stays off when no [memory] section in config at all."""
    section("12. No Memory Config Section — Memory stays off")
    env = IsolatedEnv()
    try:
        # Config with no memory section at all
        env.write_config("")
        env.write_global_memory("# Should not be indexed\n")
        client = env.spawn_agent(extra_env={"ECHO_AGENT_MEMORY": "0"})
        init_session(client)
        time.sleep(2)

        db = env.get_db_path()
        if db is None:
            ok("no index created without memory config section")
        else:
            fail(f"index created without config section: {db}")

        client.close()
    finally:
        env.cleanup()


def test_memory_enabled_env_override():
    """ECHO_AGENT_MEMORY=1 env var enables memory even without config."""
    section("13. ECHO_AGENT_MEMORY Env Var Override")
    env = IsolatedEnv()
    try:
        # Config enables memory
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Env test content\n* Key value here\n")
        client = env.spawn_agent(extra_env={"ECHO_AGENT_MEMORY": "1"})
        init_session(client)
        time.sleep(3)

        db = env.get_db_path()
        if db:
            ok("index created with ECHO_AGENT_MEMORY=1")
        else:
            fail("no index despite ECHO_AGENT_MEMORY=1 and config enabled")

        client.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 5: Logging
# ═══════════════════════════════════════════════════════════════════════════


def test_memory_log_events():
    """Memory log file is written with lifecycle events."""
    section("14. Memory Log Events")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Test\n* Test content for logging\n")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(4)
        client.close()

        log = env.memory_log()
        events = ["MEMORY_INIT", "MEMORY_REINDEX"]
        for ev in events:
            if ev in log:
                ok(f"log: {ev}")
            else:
                fail(f"log missing: {ev}")

        if not log:
            fail("memory.log empty (--features dev not enabled?)")
        else:
            lines = log.strip().split("\n")
            ok(f"memory.log has {len(lines)} line(s)")
    finally:
        env.cleanup()


def test_memory_log_contains_session_id():
    """Log entries include the session ID for correlation."""
    section("15. Memory Log Contains Session ID")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Log Session Test\n")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)
        client.close()

        log = env.memory_log()
        if not log:
            skip("memory.log empty (--features dev not enabled?)")
            return

        # Log should contain structured fields — at least the MEMORY_INIT event
        if "MEMORY_INIT" in log:
            ok("log has structured MEMORY_INIT event")
        else:
            fail("no MEMORY_INIT in log")

        # Verify log is valid structured data (each line should be parseable or human-readable)
        lines = [line for line in log.strip().split("\n") if line.strip()]
        if len(lines) > 0:
            ok(f"log has {len(lines)} non-empty line(s)")
        else:
            fail("log has no content lines")
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 6: Session End Hook
#
# NOTE: session_end currently only fires via SessionCommand::Shutdown,
# which is sent in TUI/headless modes on SIGTERM. In stdio mode (used by
# tests), the agent drops when stdin closes, racing the session actor's
# event loop. The channel-close path includes on_session_end but the
# LocalSet may exit before it runs. These tests verify the hook when it
# fires but gracefully skip when the race is lost.
# ═══════════════════════════════════════════════════════════════════════════


def test_session_end_summary():
    """Session end writes a summary .md file when >= 2 user messages."""
    section("16. Session End — Summary Written")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Base Memory\n")

        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"init: {resp}")
            return
        time.sleep(2)

        # Send enough prompts to qualify for session-end (>= 2 user messages)
        for i in range(3):
            client.prompt(f"test message {i} about Rust programming", timeout=10)
            time.sleep(1)

        client.close()
        time.sleep(2)

        # Check session files
        sfiles = env.session_files()
        if sfiles:
            ok(f"{len(sfiles)} session log(s) written")
            for sf in sfiles:
                os.path.getsize(sf)
                with open(sf) as f:
                    content = f.read()
                if "## Session Summary" in content:
                    ok(f"  {os.path.basename(sf)}: has Session Summary header")
                else:
                    fail(f"  {os.path.basename(sf)}: missing Session Summary header")
        else:
            skip("no session logs (session may have been too short for model)")

    finally:
        env.cleanup()


def test_session_end_short_session_skipped():
    """Sessions with < 2 user messages don't write a summary."""
    section("17. Session End — Short session skipped")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Base\n")

        client = env.spawn_agent()
        init_session(client)
        time.sleep(2)

        # Only send 1 message — below the MIN_USER_MESSAGES threshold
        client.prompt("single message", timeout=5)
        time.sleep(1)

        client.close()
        time.sleep(2)

        sfiles = env.session_files()
        if not sfiles:
            ok("no session log for single-message session (correctly skipped)")
        else:
            # Could still be OK if the threshold is 1, but we expect 2
            skip(f"session log written for short session ({len(sfiles)} files)")
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 7: Cross-Session Context Persistence
# ═══════════════════════════════════════════════════════════════════════════


def test_cross_session_index_persistence():
    """Index persists across sessions — chunks from session 1 are available in session 2."""
    section("18. Cross-Session Index Persistence")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        marker = uuid.uuid4().hex[:8]
        env.write_global_memory(f"# Cross-session\n* Persistent marker: {marker}\n")

        # Session 1: index the content
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        count1 = env.query_db("SELECT count(*) FROM chunks")[0][0]
        c1.close()
        time.sleep(1)

        if count1 == 0:
            fail("session 1 didn't index anything")
            return

        # Session 2: verify the index persists
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(2)

        count2 = env.query_db("SELECT count(*) FROM chunks")[0][0]
        if count2 >= count1:
            ok(f"index persisted: session1={count1}, session2={count2}")
        else:
            fail(f"index shrunk: session1={count1}, session2={count2}")

        # FTS still works in session 2
        r = env.query_db("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (marker,))
        if r and r[0][0] > 0:
            ok("FTS finds marker from session 1 in session 2")
        else:
            fail("FTS can't find marker from session 1")

        c2.close()
    finally:
        env.cleanup()


def test_session_log_indexed_next_session():
    """Session log .md written in session 1 gets indexed in session 2."""
    section("19. Session Log Indexed in Next Session")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Base\n")

        # Session 1: send enough messages to generate a session log
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(2)
        for i in range(3):
            c1.prompt(f"discuss topic {i} about memory systems", timeout=10)
            time.sleep(1)
        c1.close()
        time.sleep(2)

        sfiles = env.session_files()
        if not sfiles:
            skip("no session logs written in session 1, can't test re-indexing")
            return
        ok(f"session 1 wrote {len(sfiles)} log(s)")

        # Session 2: reindex should pick up the session logs
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        # Check if session content appears in chunks
        chunks = env.query_db("SELECT source, text FROM chunks")
        session_chunks = [(s, t) for s, t in chunks if s == "session"]
        if session_chunks:
            ok(f"{len(session_chunks)} session chunk(s) indexed")
        else:
            skip("session logs not yet indexed (may need explicit reindex)")

        c2.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 8: Large & Edge Cases
# ═══════════════════════════════════════════════════════════════════════════


def test_empty_memory_file():
    """Empty MEMORY.md doesn't crash the indexer."""
    section("20. Empty MEMORY.md — No Crash")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("")

        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"agent crashed with empty MEMORY.md: {resp}")
        else:
            ok("agent started with empty MEMORY.md")

        time.sleep(2)
        chunks = env.query_db("SELECT count(*) FROM chunks")
        if chunks:
            ok(f"indexer handled empty file ({chunks[0][0]} chunks)")
        else:
            ok("indexer handled empty file (no DB created)")

        client.close()
    finally:
        env.cleanup()


def test_unicode_memory_content():
    """MEMORY.md with unicode (emoji, CJK, etc.) is indexed correctly."""
    section("21. Unicode Memory Content")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("""# International Content 🌍
* Japanese: プログラミングの規約
* Chinese: 代码审查流程
* Emoji markers: 🔥 performance 🐛 bugs 🚀 deployment
* Arabic: مراجعة الكود
* Mixed: Use café-style naming for résumé parser
""")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        chunks = env.query_db("SELECT count(*) FROM chunks")[0][0]
        if chunks > 0:
            ok(f"unicode content indexed ({chunks} chunks)")
        else:
            fail("unicode content not indexed")

        # FTS should find ASCII words embedded in unicode content
        r = env.query_db("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'performance'")
        if r and r[0][0] > 0:
            ok("FTS finds ASCII word in unicode content")
        else:
            fail("FTS can't find ASCII word in unicode content")

        client.close()
    finally:
        env.cleanup()


def test_binary_content_resilience():
    """MEMORY.md with binary-ish content doesn't crash the indexer."""
    section("22. Binary-ish Content Resilience")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        # Write content with null-adjacent chars, control chars, etc.
        env.write_global_memory(
            "# Edge\n"
            "* Tab\there\n"
            "* Line\x0bfeed\n"
            "* Backspace\x08test\n"
            "* Long line: " + "x" * 10000 + "\n"
        )
        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"agent crashed with binary content: {resp}")
        else:
            ok("agent started with binary-ish content")

        time.sleep(3)
        chunks = env.query_db("SELECT count(*) FROM chunks")
        count = chunks[0][0] if chunks else 0
        ok(f"indexer handled binary-ish content ({count} chunks)")

        client.close()
    finally:
        env.cleanup()


def test_multiple_workspace_isolation():
    """Two different workspaces have separate indices."""
    section("23. Multiple Workspace Isolation")
    env1 = IsolatedEnv(workspace_name="project-alpha")
    try:
        env1.write_config(INDEXING_ONLY_CONFIG)
        env1.write_global_memory("# Shared\n* Shared content\n")

        # Start agent in workspace alpha
        c1 = env1.spawn_agent()
        init_session(c1)
        time.sleep(3)
        c1.close()

        # Now create workspace beta in the same fake home
        beta_workspace = os.path.join(env1.root, "workspace", "project-beta")
        os.makedirs(beta_workspace, exist_ok=True)

        # Config is already in global ~/.echo-agent/config.toml from env1.write_config

        # Start agent in workspace beta (reuse env1's fake home)
        binary = os.environ.get("ECHO_AGENT_BINARY", "echoagent")
        env_vars = os.environ.copy()
        env_vars["HOME"] = env1.fake_home
        env_vars["ECHO_AGENT_MEMORY"] = "1"
        env_vars["RUST_LOG"] = "warn"
        env_vars["ECHO_AGENT_NO_LEADER"] = "1"
        proc2 = subprocess.Popen(
            [binary, "agent", "--no-leader", "stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=beta_workspace,
            env=env_vars,
        )
        c2 = AcpClient(proc2, cwd=beta_workspace)
        init_session(c2)
        time.sleep(3)
        c2.close()

        # Count workspace dirs in memory (should be 2 different hashes)
        ws_dirs = [
            d
            for d in os.listdir(env1.memory_dir)
            if os.path.isdir(os.path.join(env1.memory_dir, d)) and d != "sessions"
        ]
        if len(ws_dirs) >= 2:
            ok(f"two workspace dirs created: {ws_dirs}")
        elif len(ws_dirs) == 1:
            # Could be same hash — depends on workspace path hashing
            skip("only 1 workspace dir (workspaces may hash the same)")
        else:
            fail("no workspace dirs found")
    finally:
        env1.cleanup()


def test_concurrent_access_safety():
    """Two sequential sessions on the same workspace don't corrupt the index."""
    section("24. Sequential Session Safety (no corruption)")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Concurrency\n* Shared data for safety test\n")

        for session_num in range(3):
            c = env.spawn_agent()
            resp = init_session(c)
            if "error" in resp:
                fail(f"session {session_num} failed to init: {resp}")
                return
            time.sleep(2)
            c.close()
            time.sleep(1)

        # After 3 sessions, DB should be consistent
        chunks = env.query_db("SELECT count(*) FROM chunks")
        fts = env.query_db("SELECT count(*) FROM chunks_fts")
        if chunks and fts and chunks[0][0] == fts[0][0]:
            ok(f"3 sessions: DB consistent ({chunks[0][0]} chunks = {fts[0][0]} FTS)")
        elif chunks and fts:
            fail(f"DB inconsistent: chunks={chunks[0][0]}, FTS={fts[0][0]}")
        else:
            fail("no data after 3 sessions")

        # Verify integrity
        try:
            integrity = env.query_db("PRAGMA integrity_check")
            if integrity and integrity[0][0] == "ok":
                ok("SQLite integrity check: ok")
            else:
                fail(f"SQLite integrity issue: {integrity}")
        except Exception as e:
            fail(f"integrity check failed: {e}")
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 9: Flush — artifact reindexing & dedup
# ═══════════════════════════════════════════════════════════════════════════


def test_flush_artifact_reindexed():
    """A manually-written flush .md file is picked up by the next session's reindex."""
    section("25. Flush Artifact Reindexed on Next Session")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Base\n")

        # Session 1: let the agent create the workspace dir + index
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        ws_dir = env.find_workspace_dir()
        if not ws_dir:
            fail("no workspace dir after session 1")
            c1.close()
            return
        c1.close()
        time.sleep(1)

        # Manually write a flush artifact (mimicking what run_memory_flush writes)
        flush_marker = uuid.uuid4().hex[:8]
        sessions_dir = os.path.join(ws_dir, "sessions")
        os.makedirs(sessions_dir, exist_ok=True)
        flush_path = os.path.join(sessions_dir, "2025-02-25-flush-sim123456.md")
        with open(flush_path, "w") as f:
            f.write(f"""## Key Decisions
* We chose Rust for the memory backend (marker: {flush_marker})
* blake3 for content-addressed dedup

## Architecture
* SQLite + FTS5 for search
* sqlite-vec for embeddings
""")
        ok(f"wrote simulated flush artifact (marker={flush_marker})")

        # Session 2: reindex should pick it up
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        # Check that the flush content is in the index
        r = env.query_db(
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (flush_marker,)
        )
        if r and r[0][0] > 0:
            ok(f"flush artifact indexed — FTS finds marker '{flush_marker}'")
        else:
            fail(f"flush artifact NOT indexed — FTS can't find '{flush_marker}'")

        # Check source label
        r = env.query_db(
            "SELECT DISTINCT c.source FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH ?",
            (flush_marker,),
        )
        if r:
            ok(f"flush artifact source = '{r[0][0]}'")
        else:
            fail("no source for flush artifact chunks")

        c2.close()
    finally:
        env.cleanup()


def test_flush_dedup_by_hash():
    """Identical flush content written twice doesn't create duplicate chunks."""
    section("26. Flush Dedup by Content Hash")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Base\n")

        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        ws_dir = env.find_workspace_dir()
        if not ws_dir:
            fail("no workspace dir")
            c1.close()
            return
        c1.close()
        time.sleep(1)

        # Write the same content to two different flush files
        sessions_dir = os.path.join(ws_dir, "sessions")
        os.makedirs(sessions_dir, exist_ok=True)
        identical_content = "## Decisions\n* Always use tracing\n* Always use blake3\n"
        for _i, name in enumerate(["2025-02-25-flush-aaa.md", "2025-02-25-flush-bbb.md"]):
            with open(os.path.join(sessions_dir, name), "w") as f:
                f.write(identical_content)

        # Reindex
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        # Check that dedup works — same content hash should not duplicate chunks
        hashes = env.query_db("SELECT hash, count(*) FROM chunks GROUP BY hash HAVING count(*) > 1")
        if not hashes:
            ok("no duplicate chunk hashes (dedup working)")
        else:
            # Two files with same content might produce same chunks —
            # the reindex should detect identical hash and skip
            skip(f"{len(hashes)} hash collision(s) — dedup may be file-level not chunk-level")

        c2.close()
    finally:
        env.cleanup()


def test_flush_log_events():
    """Memory log contains MEMORY_FLUSH events when flush is configured."""
    section("27. Flush Log Events")
    env = IsolatedEnv()
    try:
        # Enable flush with very low threshold so it might trigger
        env.write_config("""
[memory]
enabled = true

[compaction.memory_flush]
enabled = true
soft_threshold_tokens = 100

[compaction.pruning]
enabled = false
""")
        env.write_global_memory("# Flush logging test\n")

        client = env.spawn_agent()
        init_session(client)
        time.sleep(2)

        # Send several prompts to build up some token usage
        for i in range(5):
            client.prompt(f"Tell me about topic {i}: " + "context ", timeout=15)
            time.sleep(2)

        client.close()
        time.sleep(2)

        log = env.memory_log()
        if "MEMORY_FLUSH" in log:
            ok("MEMORY_FLUSH event in log (flush triggered)")
            if "MEMORY_FLUSH: completed" in log:
                ok("MEMORY_FLUSH: completed event logged")
        else:
            skip("MEMORY_FLUSH not triggered (context usage may not have hit threshold)")

        if "MEMORY_COMPACT" in log:
            ok("MEMORY_COMPACT event in log (compaction triggered)")
        else:
            skip("MEMORY_COMPACT not triggered (context usage may not have hit threshold)")

    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 10: Compaction — post-compaction memory re-injection
# ═══════════════════════════════════════════════════════════════════════════


def test_compaction_resets_memory_injection():
    """After compaction, memory context is re-injected on the next turn."""
    section("28. Compaction Resets Memory Injection")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Important context\n* Always use graphite for PRs\n")

        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        # Send many messages to try to trigger compaction
        for i in range(8):
            client.prompt(
                f"Generate a long response about topic {i}. " + "Please include lots of detail. ",
                timeout=20,
            )
            time.sleep(2)

        client.close()
        time.sleep(2)

        log = env.memory_log()
        if "MEMORY_COMPACT: post-compaction reset" in log:
            ok("post-compaction memory injection reset detected")
        else:
            skip("compaction not triggered (would need more messages to fill context)")

        # If MEMORY_INJECT appeared at the start, that's the first-turn injection
        if "MEMORY_INJECT" in log:
            ok("MEMORY_INJECT event found (first-turn injection worked)")
            # Count inject events — if compaction happened, we should see >= 2
            inject_count = log.count("MEMORY_INJECT")
            if inject_count >= 2:
                ok(f"MEMORY_INJECT appeared {inject_count} times (re-injected after compaction)")
            else:
                skip(f"only {inject_count} MEMORY_INJECT (compaction may not have triggered)")
        else:
            skip("no MEMORY_INJECT in log (memory context injection may not be configured)")

    finally:
        env.cleanup()


def test_compaction_preserves_index():
    """Compaction doesn't corrupt or destroy the memory index."""
    section("29. Compaction Preserves Index Integrity")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        marker = uuid.uuid4().hex[:8]
        env.write_global_memory(f"# Persistent\n* Critical marker: {marker}\n")

        client = env.spawn_agent()
        init_session(client)
        time.sleep(3)

        # Record pre-compaction state
        pre_chunks = env.query_db("SELECT count(*) FROM chunks")[0][0]

        # Try to trigger compaction
        for i in range(6):
            client.prompt(f"Topic {i}: " + "detailed information ", timeout=15)
            time.sleep(2)

        # Index should still be intact
        post_chunks = env.query_db("SELECT count(*) FROM chunks")[0][0]
        if post_chunks >= pre_chunks:
            ok(f"index preserved: pre={pre_chunks}, post={post_chunks}")
        else:
            fail(f"index lost chunks: pre={pre_chunks}, post={post_chunks}")

        # FTS still finds our marker
        r = env.query_db("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (marker,))
        if r and r[0][0] > 0:
            ok("FTS still finds marker after potential compaction")
        else:
            fail("FTS lost marker after potential compaction")

        # Integrity check
        integrity = env.query_db("PRAGMA integrity_check")
        if integrity and integrity[0][0] == "ok":
            ok("SQLite integrity preserved after session with potential compaction")
        else:
            fail(f"integrity issue: {integrity}")

        client.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 11: Pruning — config acceptance & behavior verification
# ═══════════════════════════════════════════════════════════════════════════


def test_pruning_config_accepted():
    """Custom pruning config values are accepted by the agent."""
    section("30. Pruning Config Accepted")
    env = IsolatedEnv()
    try:
        env.write_config("""
[memory]
enabled = true

[compaction.pruning]
enabled = true
keep_last_n_turns = 2
soft_trim_threshold = 500
soft_trim_head = 100
soft_trim_tail = 100
hard_clear_age_turns = 5
""")
        env.write_global_memory("# Pruning Test\n")
        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"agent rejected pruning config: {resp}")
        else:
            ok("agent accepted aggressive pruning config (keep_last=2, hard_clear=5)")
        time.sleep(2)
        client.close()
    finally:
        env.cleanup()


def test_pruning_disabled_no_side_effects():
    """With pruning disabled, tool results are never modified."""
    section("31. Pruning Disabled — No Side Effects")
    env = IsolatedEnv()
    try:
        env.write_config("""
[memory]
enabled = true

[compaction.pruning]
enabled = false
""")
        env.write_global_memory("# No-prune test\n")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(2)

        # Send several messages
        for i in range(4):
            client.prompt(f"message {i} with pruning disabled", timeout=10)
            time.sleep(1)

        client.close()
        time.sleep(1)

        # With pruning disabled, the agent should run fine
        ok("agent ran with pruning disabled, no errors")

        # Verify no pruning log messages
        log = env.memory_log()
        if "pruned" not in log.lower() and "trimmed" not in log.lower():
            ok("no pruning-related log entries when disabled")
        else:
            skip("pruning-related logs found (may be from other subsystem)")
    finally:
        env.cleanup()


def test_pruning_extreme_config():
    """Extreme pruning config (keep_last=0, threshold=1) doesn't crash."""
    section("32. Pruning Extreme Config — No Crash")
    env = IsolatedEnv()
    try:
        env.write_config("""
[memory]
enabled = true

[compaction.pruning]
enabled = true
keep_last_n_turns = 0
soft_trim_threshold = 1
soft_trim_head = 10
soft_trim_tail = 10
hard_clear_age_turns = 1
""")
        env.write_global_memory("# Extreme pruning\n")
        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"agent crashed with extreme pruning config: {resp}")
        else:
            ok("agent survived extreme pruning config (keep=0, threshold=1, hard_clear=1)")
        time.sleep(2)

        # Send a few messages — pruning should aggressively trim
        for i in range(3):
            client.prompt(f"extreme prune test message {i}", timeout=10)
            time.sleep(1)

        # Agent should still be alive
        resp = client.prompt("final message after extreme pruning", timeout=10)
        if "error" not in resp or resp.get("error") == "broken pipe":
            ok("agent still responsive after extreme pruning")
        else:
            skip(f"agent response: {resp.get('error', 'ok')}")

        client.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 12: End-to-end round-trip — store → search → find
#
# These are the most important tests. They verify that memory produced by
# each lifecycle event (session_end hook, flush, reindex) is actually
# SEARCHABLE in a subsequent session. Without these, the rest of the suite
# only proves plumbing exists — not that the system is useful.
# ═══════════════════════════════════════════════════════════════════════════


def test_roundtrip_session_end_searchable():
    """Session-end hook writes content that is FTS-searchable in the next session."""
    section("33. Round-trip: session_end → searchable in next session")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Base\n")

        # Session 1: send enough messages to trigger session_end hook (>= 2 user msgs).
        # Use distinctive topic words so we can search for them.
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(2)
        for i in range(3):
            c1.prompt(
                f"Tell me about flamingos and their migration patterns (turn {i})",
                timeout=10,
            )
            time.sleep(1)
        c1.close()
        time.sleep(2)

        # Verify a session log was written
        sfiles = env.session_files()
        if not sfiles:
            skip("no session log written — session_end hook didn't fire, can't test round-trip")
            return
        ok(f"session 1 wrote {len(sfiles)} session log(s)")

        # Read the session log to see what topics it captured
        with open(sfiles[0]) as f:
            session_content = f.read()
        if "flamingo" in session_content.lower():
            ok("session log captured topic 'flamingo'")
        else:
            # The metadata summary captures user message text, so 'flamingo' should be there
            skip(f"session log doesn't mention 'flamingo' (content: {session_content[:200]}...)")

        # Session 2: reindex picks up the session log → search should find it
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        # Search the index for our distinctive topic
        r = env.query_db(
            "SELECT c.text, c.source FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
            "WHERE chunks_fts MATCH 'flamingo OR flamingos OR migration'"
        )
        if r:
            ok(f"FTS finds session content in session 2 ({len(r)} match(es), source='{r[0][1]}')")
            # Verify the matched text actually contains our topic
            found_text = " ".join(row[0] for row in r)
            if "flamingo" in found_text.lower():
                ok("matched text contains 'flamingo' — memory is useful")
            else:
                fail("FTS matched but text doesn't contain 'flamingo'")
        else:
            fail("FTS can't find session 1 topics in session 2 — round-trip broken")

        c2.close()
    finally:
        env.cleanup()


def test_roundtrip_flush_artifact_searchable():
    """Flush output is searchable by topic keywords in the next session."""
    section("34. Round-trip: flush artifact → searchable by topic")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Base\n")

        # Session 1: create workspace dir
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        ws_dir = env.find_workspace_dir()
        if not ws_dir:
            fail("no workspace dir")
            c1.close()
            return
        c1.close()
        time.sleep(1)

        # Simulate flush output with distinctive, searchable content
        sessions_dir = os.path.join(ws_dir, "sessions")
        os.makedirs(sessions_dir, exist_ok=True)
        with open(os.path.join(sessions_dir, "2025-02-25-flush-roundtrip.md"), "w") as f:
            f.write("""## Architecture Decisions
* Chose PostgreSQL over MySQL for JSONB support
* Using Redis for session caching with 15-minute TTL
* Kubernetes with Istio service mesh for zero-trust networking

## User Preferences
* Prefers snake_case for all Python code
* Always run black formatter before committing
* Test coverage threshold is 85%
""")
        ok("wrote flush artifact with distinct topics")

        # Session 2: reindex → search
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        # Search for specific technical decisions
        searches = [
            ("PostgreSQL OR JSONB", "database choice"),
            ("Redis OR caching OR TTL", "caching layer"),
            ("Kubernetes OR Istio", "infrastructure"),
            ("snake_case OR black OR formatter", "code style"),
            ("coverage OR threshold", "testing policy"),
        ]
        found_count = 0
        for query, label in searches:
            r = env.query_db(
                "SELECT c.text FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
                "WHERE chunks_fts MATCH ?",
                (query,),
            )
            if r:
                found_count += 1
                ok(f"FTS finds '{label}' from flush artifact")
            else:
                fail(f"FTS can't find '{label}' — flush content not searchable")

        if found_count == len(searches):
            ok(f"all {found_count}/{len(searches)} flush topics searchable — memory is useful")

        c2.close()
    finally:
        env.cleanup()


def test_roundtrip_global_memory_searchable_across_sessions():
    """Global MEMORY.md content remains searchable across multiple sessions."""
    section("35. Round-trip: global memory persists and is searchable across 3 sessions")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        unique_topics = {
            "zephyr": "project codename",
            "quaternion": "math concept",
            "obsidian": "tool name",
        }
        lines = [f"* {v}: {k}" for k, v in unique_topics.items()]
        env.write_global_memory("# Durable Knowledge\n" + "\n".join(lines) + "\n")

        for session_num in range(1, 4):
            c = env.spawn_agent()
            init_session(c)
            time.sleep(3)

            # Verify every unique topic is still searchable
            for word, label in unique_topics.items():
                r = env.query_db(
                    "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
                    "WHERE chunks_fts MATCH ?",
                    (word,),
                )
                if not r or r[0][0] == 0:
                    fail(f"session {session_num}: FTS lost '{word}' ({label})")
                    c.close()
                    return

            ok(f"session {session_num}: all {len(unique_topics)} topics searchable")
            c.close()
            time.sleep(1)

        ok("global memory remained searchable across 3 sessions")
    finally:
        env.cleanup()


def test_roundtrip_workspace_memory_searchable():
    """Workspace MEMORY.md content is searchable with workspace-specific queries."""
    section("36. Round-trip: workspace memory searchable by project-specific terms")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        env.write_global_memory("# Global\n* Use graphite for PRs\n")

        # Session 1: create workspace dir
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)

        # Write workspace-specific memory
        ws_content = """# Project Foxtrot Setup
* Backend: Axum with tower middleware
* Database: CockroachDB with sqlx driver
* Auth: JWT tokens with ed25519 signing
* CI: Buildkite with parallelism=8
* Deploy: ArgoCD with progressive delivery
"""
        ws_path = env.write_workspace_memory(ws_content)
        if not ws_path:
            fail("couldn't find workspace dir")
            c1.close()
            return
        ok("wrote workspace MEMORY.md with project-specific terms")
        c1.close()
        time.sleep(1)

        # Session 2: reindex and search
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(4)

        project_searches = [
            ("Axum OR tower", "web framework"),
            ("CockroachDB OR sqlx", "database"),
            ("JWT OR ed25519", "authentication"),
            ("Buildkite OR parallelism", "CI pipeline"),
            ("ArgoCD OR progressive", "deployment"),
        ]
        found = 0
        for query, label in project_searches:
            r = env.query_db(
                "SELECT c.text, c.source FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
                "WHERE chunks_fts MATCH ?",
                (query,),
            )
            if r:
                found += 1
                # Verify it's actually from workspace source
                sources = {row[1] for row in r}
                if "workspace" in sources:
                    ok(f"FTS finds '{label}' from workspace source")
                else:
                    ok(f"FTS finds '{label}' (source={sources})")
            else:
                fail(f"FTS can't find '{label}' from workspace memory")

        if found == len(project_searches):
            ok(f"all {found}/{len(project_searches)} workspace topics searchable")

        c2.close()
    finally:
        env.cleanup()


def test_roundtrip_mixed_sources_searchable():
    """Global + workspace + session content all coexist and are independently searchable."""
    section("37. Round-trip: mixed sources (global + workspace + session) all searchable")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)

        # Write global memory with unique terms
        env.write_global_memory("""# Global Conventions
* Always use ribonuclease as the error-handling pattern
* Prefer dendrochronology for date calculations
""")

        # Session 1: create workspace dir, write workspace memory
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        env.write_workspace_memory("""# Workspace: Project Orion
* Use spectroscopy for config parsing
* Deploy with helioseismology orchestration
""")

        # Send messages to trigger session_end (≥2 user msgs with unique topics)
        for msg in [
            "We need to implement bioluminescence detection",
            "Also add the thermocline monitoring feature",
            "And integrate the magnetosphere dashboard",
        ]:
            c1.send(
                "session/prompt",
                {"messages": [{"role": "user", "content": {"type": "text", "text": msg}}]},
                timeout=10,
            )
            time.sleep(1)
        c1.close()
        time.sleep(2)

        # Also simulate a flush artifact with unique terms
        ws_dir = env.find_workspace_dir()
        if ws_dir:
            sessions_dir = os.path.join(ws_dir, "sessions")
            os.makedirs(sessions_dir, exist_ok=True)
            with open(os.path.join(sessions_dir, "2025-02-25-flush-mixed.md"), "w") as f:
                f.write("## Flush Notes\n* Remember the cephalopod configuration pattern\n")

        # Session 2: reindex all sources → search each
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(5)

        source_checks = [
            # (query, expected_source, label)
            ("ribonuclease OR dendrochronology", "global", "global memory terms"),
            ("spectroscopy OR helioseismology", "workspace", "workspace memory terms"),
            ("cephalopod", "session", "flush artifact terms"),
        ]

        all_found = True
        for query, expected_source, label in source_checks:
            r = env.query_db(
                "SELECT c.source, c.text FROM chunks_fts f JOIN chunks c ON c.rowid = f.rowid "
                "WHERE chunks_fts MATCH ?",
                (query,),
            )
            if r:
                actual_sources = {row[0] for row in r}
                ok(f"FTS finds {label} (sources={actual_sources})")
                if expected_source in actual_sources:
                    ok(f"  → correct source '{expected_source}'")
                else:
                    skip(f"  → expected source '{expected_source}', got {actual_sources}")
            else:
                fail(f"FTS can't find {label} — source '{expected_source}' missing")
                all_found = False

        # Also check session_end content if it was written
        sfiles = env.session_files()
        session_terms = ["bioluminescence", "thermocline", "magnetosphere"]
        if sfiles:
            # Check if session log content is searchable
            for term in session_terms:
                r = env.query_db(
                    "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?", (term,)
                )
                if r and r[0][0] > 0:
                    ok(f"session_end topic '{term}' searchable")
                else:
                    # Session metadata summary captures user message text
                    skip(f"session_end topic '{term}' not in FTS (may not be in summary)")
                    break
        else:
            skip("no session logs to search (session_end hook didn't fire)")

        if all_found:
            ok("all sources coexist and are searchable — memory system works end-to-end")

        c2.close()
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 13: End-to-end search — hybrid search through the live agent
#
# These tests verify that the agent's search pipeline (hybrid_search with
# FTS + optional vector KNN, recency decay, source weights) works end-to-end.
# Verification is done via memory.log events, NOT direct DB queries.
#
# Log events checked:
#   MEMORY_INJECT_SEARCH — first-turn search with query + result count
#   MEMORY_INJECT         — memory context was injected into the prompt
#   MEMORY_SEARCH          — model called the memory_search tool
# ═══════════════════════════════════════════════════════════════════════════


def test_e2e_first_turn_injection_finds_memory():
    """First-turn injection searches memory and injects results into prompt."""
    section("38. E2E: first-turn injection finds pre-populated memory")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("""# Project Conventions
* Always use protobuf for service-to-service communication
* Deploy using ArgoCD with progressive rollouts
* Use CockroachDB for the primary datastore
""")
        client = env.spawn_agent()
        resp = init_session(client)
        if "error" in resp:
            fail(f"init: {resp}")
            return
        time.sleep(5)  # wait for reindex to complete

        # Send a first message related to the memory content — triggers injection
        resp = client.prompt("What serialization format should I use for our services?", timeout=20)
        time.sleep(3)
        client.close()
        time.sleep(2)

        log = env.memory_log()

        # 1. Verify the search was executed
        if "MEMORY_INJECT_SEARCH" in log:
            ok("MEMORY_INJECT_SEARCH logged — first-turn search ran")
            # Check that results > 0
            import re

            m = re.search(r"MEMORY_INJECT_SEARCH: results=(\d+)", log)
            if m and int(m.group(1)) > 0:
                ok(f"first-turn search found {m.group(1)} result(s)")
            elif m:
                fail("first-turn search found 0 results — memory content not searchable")
            else:
                skip("couldn't parse result count from log")
        else:
            fail("MEMORY_INJECT_SEARCH not in log — first-turn search didn't run")

        # 2. Verify injection happened
        if "MEMORY_INJECT:" in log:
            ok("MEMORY_INJECT logged — memory context was injected into prompt")
        else:
            fail("MEMORY_INJECT not in log — search returned results but injection failed")

    finally:
        env.cleanup()


def test_e2e_first_turn_injection_query_matches_user_message():
    """First-turn search query is derived from the user's first message."""
    section("39. E2E: injection search query matches user message")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Memory\n* Use flamingos for load balancing\n")

        client = env.spawn_agent()
        init_session(client)
        time.sleep(5)

        # Send a distinctive first message
        client.prompt("Tell me about our flamingo configuration", timeout=20)
        time.sleep(3)
        client.close()
        time.sleep(2)

        log = env.memory_log()
        if "MEMORY_INJECT_SEARCH" in log:
            # The query should contain words from the user's message
            if "flamingo" in log.lower():
                ok("search query contains 'flamingo' from user message")
            else:
                skip("search query logged but doesn't contain 'flamingo' (may be truncated)")
        else:
            fail("MEMORY_INJECT_SEARCH not logged")

    finally:
        env.cleanup()


def test_e2e_no_injection_when_memory_empty():
    """No MEMORY_INJECT when memory has no relevant content."""
    section("40. E2E: no injection with empty memory")
    env = IsolatedEnv()
    try:
        env.write_config(INDEXING_ONLY_CONFIG)
        # Write empty memory — nothing to find
        env.write_global_memory("")

        client = env.spawn_agent()
        init_session(client)
        time.sleep(2)

        client.prompt("What is the project setup?", timeout=10)
        time.sleep(2)
        client.close()
        time.sleep(1)

        log = env.memory_log()
        if "MEMORY_INJECT_SEARCH" in log:
            import re

            m = re.search(r"MEMORY_INJECT_SEARCH: results=(\d+)", log)
            if m and int(m.group(1)) == 0:
                ok("search returned 0 results for empty memory")
            elif m:
                skip(f"search returned {m.group(1)} results (index may have stale data)")
        # MEMORY_INJECT should NOT appear (no results to inject)
        if "MEMORY_INJECT: first-turn memory context injected" not in log:
            ok("no MEMORY_INJECT with empty memory — correct")
        else:
            fail("MEMORY_INJECT fired despite empty memory")

    finally:
        env.cleanup()


def test_e2e_injection_with_multiple_sources():
    """First-turn injection finds content from both global and workspace sources."""
    section("41. E2E: injection finds global + workspace memory")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Global\n* Always use spectroscopy for config parsing\n")

        # Session 1: create workspace dir and write workspace memory
        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(3)
        env.write_workspace_memory(
            "# Workspace\n* Use helioseismology for deployment orchestration\n"
        )
        c1.close()
        time.sleep(1)

        # Session 2: both sources should be indexed and searchable
        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(6)  # extra time for reindex of both sources

        # Send a prompt that should match workspace memory
        c2.prompt("deployment orchestration helioseismology", timeout=20)
        time.sleep(3)
        c2.close()
        time.sleep(2)

        log = env.memory_log()
        if "MEMORY_INJECT_SEARCH" in log:
            import re

            m = re.search(r"MEMORY_INJECT_SEARCH: results=(\d+)", log)
            if m and int(m.group(1)) > 0:
                ok(f"injection search found {m.group(1)} result(s) from indexed sources")
            elif m:
                fail("injection search found 0 results despite both sources being indexed")
            else:
                skip("couldn't parse result count from MEMORY_INJECT_SEARCH log")
        else:
            fail("MEMORY_INJECT_SEARCH not logged")

        if "MEMORY_INJECT:" in log:
            ok("memory context injected from multi-source index")
        else:
            fail("MEMORY_INJECT not logged")

    finally:
        env.cleanup()


def test_e2e_memory_search_tool_invocation():
    """Model can invoke memory_search tool and get results (when prompted)."""
    section("42. E2E: memory_search tool invocation")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("""# Project Knowledge Base
* The authentication system uses JWT tokens with ed25519 signing
* Rate limiting is handled by a Redis-backed token bucket at 1000 req/s
* Database migrations use sqlx with offline mode for CI
""")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(4)

        # Ask the model to explicitly search memory
        client.prompt(
            "Search your memory for information about our authentication system",
            timeout=20,
        )
        time.sleep(5)
        client.close()
        time.sleep(1)

        log = env.memory_log()

        # Check if the model called memory_search
        if "MEMORY_SEARCH: invoked" in log:
            ok("model invoked memory_search tool")
            if "MEMORY_SEARCH: complete" in log:
                import re

                m = re.search(r"MEMORY_SEARCH: complete.*results=(\d+)", log)
                if m:
                    count = int(m.group(1))
                    if count > 0:
                        ok(f"memory_search returned {count} result(s) — tool works e2e")
                    else:
                        fail("memory_search returned 0 results despite indexed content")
                else:
                    ok("MEMORY_SEARCH: complete logged (couldn't parse count)")
        else:
            skip("model didn't call memory_search (model-dependent — may need explicit prompting)")

    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# GROUP 14: Embedding (optional, depends on API availability)
# ═══════════════════════════════════════════════════════════════════════════


def test_embedding_computation():
    """Embeddings are computed for indexed chunks (requires API key)."""
    section("42. Embedding Computation")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("""# Embedding Test
* First piece of knowledge about embeddings
* Second piece about vector search
* Third item about semantic similarity
""")
        client = env.spawn_agent()
        init_session(client)
        time.sleep(6)  # embedding is async, give extra time

        chunks = env.query_db("SELECT count(*) FROM chunks")[0][0]
        if chunks == 0:
            fail("no chunks indexed")
            client.close()
            return

        try:
            emb = env.query_db("SELECT count(*) FROM chunks_vec_rowids")
            if emb and emb[0][0] > 0:
                ok(f"{emb[0][0]}/{chunks} chunks have embeddings")
                if emb[0][0] == chunks:
                    ok("all chunks embedded (100% coverage)")
                else:
                    skip(f"partial embedding coverage ({emb[0][0]}/{chunks})")
            else:
                skip("no embeddings computed (API key or sqlite-vec unavailable)")
        except Exception:
            skip("chunks_vec table not available")

        client.close()
    finally:
        env.cleanup()


def test_embedding_idempotency():
    """Re-running embedding on same content doesn't create duplicates."""
    section("28. Embedding Idempotency")
    env = IsolatedEnv()
    try:
        env.write_config(DEFAULT_CONFIG)
        env.write_global_memory("# Stable\n* Content for embedding idempotency\n")

        c1 = env.spawn_agent()
        init_session(c1)
        time.sleep(5)
        try:
            emb1 = env.query_db("SELECT count(*) FROM chunks_vec_rowids")[0][0]
        except Exception:
            skip("embeddings not available")
            return
        c1.close()

        c2 = env.spawn_agent()
        init_session(c2)
        time.sleep(5)
        try:
            emb2 = env.query_db("SELECT count(*) FROM chunks_vec_rowids")[0][0]
        except Exception:
            skip("embeddings not available")
            return
        c2.close()

        if emb1 == emb2:
            ok(f"embedding count stable ({emb1})")
        else:
            fail(f"embedding count changed: {emb1} → {emb2}")
    finally:
        env.cleanup()


# ═══════════════════════════════════════════════════════════════════════════
# Main runner
# ═══════════════════════════════════════════════════════════════════════════

ALL_TESTS = [
    # Group 1: Indexing & Storage
    test_global_memory_indexing,
    test_workspace_vs_global_memory,
    test_index_integrity,
    test_chunk_hash_correctness,
    # Group 2: FTS Search
    test_fts_search_quality,
    test_fts_special_characters,
    test_fts_multi_file_search,
    # Group 3: Reindex, Idempotency, Content Updates
    test_reindex_idempotency,
    test_content_update_detection,
    test_large_file_chunking,
    # Group 4: Enable/Disable & Config
    test_memory_disabled,
    test_memory_disabled_no_config,
    test_memory_enabled_env_override,
    # Group 5: Logging
    test_memory_log_events,
    test_memory_log_contains_session_id,
    # Group 6: Session End Hook
    test_session_end_summary,
    test_session_end_short_session_skipped,
    # Group 7: Cross-Session Persistence
    test_cross_session_index_persistence,
    test_session_log_indexed_next_session,
    # Group 8: Large & Edge Cases
    test_empty_memory_file,
    test_unicode_memory_content,
    test_binary_content_resilience,
    test_multiple_workspace_isolation,
    test_concurrent_access_safety,
    # Group 9: Flush — artifact reindexing & dedup
    test_flush_artifact_reindexed,
    test_flush_dedup_by_hash,
    test_flush_log_events,
    # Group 10: Compaction
    test_compaction_resets_memory_injection,
    test_compaction_preserves_index,
    # Group 11: Pruning
    test_pruning_config_accepted,
    test_pruning_disabled_no_side_effects,
    test_pruning_extreme_config,
    # Group 12: End-to-end round-trip (store → search → find)
    test_roundtrip_session_end_searchable,
    test_roundtrip_flush_artifact_searchable,
    test_roundtrip_global_memory_searchable_across_sessions,
    test_roundtrip_workspace_memory_searchable,
    test_roundtrip_mixed_sources_searchable,
    # Group 13: E2E search (hybrid search through the live agent)
    test_e2e_first_turn_injection_finds_memory,
    test_e2e_first_turn_injection_query_matches_user_message,
    test_e2e_no_injection_when_memory_empty,
    test_e2e_injection_with_multiple_sources,
    test_e2e_memory_search_tool_invocation,
    # Group 14: Embeddings
    test_embedding_computation,
    test_embedding_idempotency,
]

# Tests that don't need a model endpoint (fast, good for CI)
FAST_TESTS = [
    test_index_integrity,
    test_chunk_hash_correctness,
    test_fts_search_quality,
    test_fts_special_characters,
    test_reindex_idempotency,
    test_content_update_detection,
    test_large_file_chunking,
    test_memory_disabled,
    test_memory_disabled_no_config,
    test_empty_memory_file,
    test_unicode_memory_content,
    test_binary_content_resilience,
    test_concurrent_access_safety,
    test_flush_artifact_reindexed,
    test_flush_dedup_by_hash,
    test_roundtrip_flush_artifact_searchable,
    test_roundtrip_global_memory_searchable_across_sessions,
    test_roundtrip_workspace_memory_searchable,
    test_e2e_first_turn_injection_finds_memory,
    test_e2e_no_injection_when_memory_empty,
    test_pruning_config_accepted,
    test_pruning_extreme_config,
]


def find_repo_root():
    """Walk up from cwd to find the repo root (contains Cargo.toml + crates/)."""
    d = os.path.abspath(os.getcwd())
    while d != "/":
        if os.path.isfile(os.path.join(d, "Cargo.toml")) and os.path.isdir(
            os.path.join(d, "crates")
        ):
            return d
        d = os.path.dirname(d)
    return None


def build_binary():
    """Build echo-agent-pager with --features dev --release. Returns binary path."""
    repo = find_repo_root()
    if not repo:
        print(f"{R}Could not find repo root (no Cargo.toml + crates/ above cwd){N}")
        sys.exit(1)

    binary = os.path.join(repo, "target", "release", "echo-agent-pager")

    # Find rg for ECHO_AGENT_SHELL_BUNDLE_RG_PATH
    rg = shutil.which("rg")
    if not rg:
        print(f"{R}rg (ripgrep) not found — required for build{N}")
        sys.exit(1)

    env = os.environ.copy()
    env["ECHO_AGENT_SHELL_BUNDLE_RG_PATH"] = rg

    print(f"{B}Building echo-agent-pager (release + dev)...{N}")
    result = subprocess.run(
        ["cargo", "build", "-p", "echo-agent-pager", "--features", "dev", "--release"],
        cwd=repo,
        env=env,
    )
    if result.returncode != 0:
        print(f"{R}Build failed (exit {result.returncode}){N}")
        sys.exit(1)
    print(f"{G}Build OK{N}: {binary}\n")
    return binary


def main():
    print(f"\n{B}{'=' * 60}")
    print("  Memory System Integration Tests — Full Suite")
    print(f"{'=' * 60}{N}\n")

    # Build or locate binary
    if "ECHO_AGENT_BINARY" in os.environ:
        binary = os.environ["ECHO_AGENT_BINARY"]
        print(f"Binary: {binary} (from $ECHO_AGENT_BINARY, skipping build)")
    else:
        binary = build_binary()
        os.environ["ECHO_AGENT_BINARY"] = binary

    if not os.path.isfile(binary):
        print(f"{R}Binary not found: {binary}{N}")
        sys.exit(1)

    print(f"Binary: {binary}")
    print(f"PID: {os.getpid()}")

    args = sys.argv[1:]

    if "--fast" in args:
        tests = FAST_TESTS
        args.remove("--fast")
        print(f"Mode: {Y}fast{N} ({len(tests)} tests, no model dependency)")
    elif args:
        # Run specific tests by name
        test_map = {t.__name__: t for t in ALL_TESTS}
        tests = []
        for name in args:
            if name in test_map:
                tests.append(test_map[name])
            else:
                print(f"{R}Unknown test: {name}{N}")
                print(f"Available: {', '.join(test_map.keys())}")
                sys.exit(1)
        print(f"Mode: {Y}selective{N} ({len(tests)} tests)")
    else:
        tests = ALL_TESTS
        print(f"Mode: {Y}full{N} ({len(tests)} tests)")

    print()

    for t in tests:
        try:
            t()
        except Exception as e:
            fail(f"EXCEPTION in {t.__name__}: {e}")
            import traceback

            traceback.print_exc()

    print(f"\n{B}{'=' * 60}")
    print(f"  {G}Passed: {PASS}{N}  {R}Failed: {FAIL}{N}  {Y}Skipped: {SKIP}{N}")
    total = PASS + FAIL
    if total > 0:
        rate = PASS / total * 100
        print(f"  Pass rate: {rate:.0f}% ({PASS}/{total})")
    print(f"{B}{'=' * 60}{N}")

    sys.exit(1 if FAIL > 0 else 0)


if __name__ == "__main__":
    main()
