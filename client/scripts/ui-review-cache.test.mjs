import { afterEach, expect, it, vi } from "vitest";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewCache, pruneAbandonedUiReviewCaches } from "./ui-review-cache.mjs";

const roots = [];
const temporaryRoot = () => { const root = mkdtempSync(join(tmpdir(), "echo-review-cache-test-")); roots.push(root); return root; };
afterEach(() => { vi.restoreAllMocks(); for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

it("isolates simultaneous fixtures and releases only the closed server's cache", () => {
  const root = temporaryRoot();
  const first = createUiReviewCache("/same-fixture.tsx", root);
  const second = createUiReviewCache("/same-fixture.tsx", root);
  expect(first.directory).not.toBe(second.directory);
  writeFileSync(join(first.directory, "optimizer"), "first");
  writeFileSync(join(second.directory, "optimizer"), "second");
  first.close(); first.close();
  expect(existsSync(first.directory)).toBe(false);
  expect(existsSync(join(second.directory, "optimizer"))).toBe(true);
  second.close();
  expect(existsSync(second.directory)).toBe(false);
});

it("reclaims crashed and legacy process-owned caches while preserving live and unknown directories", () => {
  const root = temporaryRoot();
  const child = spawnSync(process.execPath, ["-e", "process.exit(0)"]);
  expect(child.status).toBe(0);
  const abandoned = [`${child.pid}-1-0123456789ab`, `${child.pid}-0123456789ab-ABC123`];
  const preserved = [`${process.pid}-1-0123456789ab`, "0123456789ab", "user-files", "999999999999999999-1-0123456789ab"];
  for (const name of [...abandoned, ...preserved]) { mkdirSync(join(root, name)); writeFileSync(join(root, name, "marker"), "keep"); }
  const external = temporaryRoot();
  writeFileSync(join(external, "marker"), "outside cache");
  const link = join(root, `${child.pid}-2-0123456789ab`);
  symlinkSync(external, link, process.platform === "win32" ? "junction" : "dir");
  pruneAbandonedUiReviewCaches(root);
  for (const name of abandoned) expect(existsSync(join(root, name))).toBe(false);
  for (const name of preserved) expect(existsSync(join(root, name, "marker"))).toBe(true);
  expect(existsSync(link)).toBe(true);
  expect(existsSync(join(external, "marker"))).toBe(true);
});

it("preserves caches when the operating system denies probing their owner", () => {
  const root = temporaryRoot();
  const directory = join(root, "12345-1-0123456789ab");
  mkdirSync(directory);
  vi.spyOn(process, "kill").mockImplementation(() => { throw Object.assign(new Error("Denied"), { code: "EPERM" }); });
  pruneAbandonedUiReviewCaches(root);
  expect(existsSync(directory)).toBe(true);
});
