import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { test } from "node:test";

test("dependency audit covers every tracked Rust lockfile", () => {
  const result = spawnSync("git", ["ls-files", "-z", "--", "*Cargo.lock"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.error?.message || result.stderr);
  const tracked = result.stdout.split("\0").filter(Boolean).sort();
  assert.ok(tracked.length > 0);
  const source = readFileSync(new URL("./audit-dependencies.py", import.meta.url), "utf8");
  const audited = [...source.matchAll(/"([^"\n]+\/Cargo\.lock)"/g)].map(match => match[1]).sort();
  assert.deepEqual(audited, tracked);
});
