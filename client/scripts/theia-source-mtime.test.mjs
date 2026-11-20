import { mkdirSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createHash } from "node:crypto";
import { expect, it, vi } from "vitest";
import { latestTheiaSourceMtime } from "./theia-source-mtime.mjs";

it("invalidates the staged IDE when a shared compiler input changes", () => {
  const root = mkdtempSync(join(tmpdir(), "echo-theia-build-inputs-"));
  const source = join(root, "vendor/theia-platform");
  const old = 1_700_000_000;
  const put = (path) => {
    const file = join(root, path);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, "fixture");
    utimesSync(file, old, old);
    return file;
  };
  try {
    for (const area of ["packages", "dev-packages", "security", "examples/browser", "examples/echo-coding-bridge"]) {
      put(`vendor/theia-platform/${area}/input`);
    }
    for (const name of ["package.json", "package-lock.json", "lerna.json"]) put(`vendor/theia-platform/${name}`);
    put("vendor/theia-language-plugins/input");
    put("vendor/nodejs/input");
    for (const name of [
      "build-theia.mjs", "prepare-theia.mjs", "stage-theia-runtime.mjs", "theia-runtime-manifest.mjs",
      "node-runtime-version.mjs", "patch-theia-scanoss.mjs", "patch-theia-node-pty.mjs",
      "patch-theia-windowless-helpers.mjs", "patch-theia-git-windowless.mjs", "manage-theia-vsix.mjs",
      "theia-plugin-validation.mjs", "theia-runtime-package-lock.json", "theia-source-mtime.mjs",
    ]) put(`scripts/${name}`);
    const config = put("vendor/theia-platform/configs/base.tsconfig.json");
    const compileReferences = put("vendor/theia-platform/scripts/compile-references.js");
    const stagedAt = latestTheiaSourceMtime(source);
    utimesSync(config, old + 1, old + 1);
    expect(latestTheiaSourceMtime(source)).toBeGreaterThan(stagedAt);
    utimesSync(compileReferences, old + 2, old + 2);
    expect(latestTheiaSourceMtime(source)).toBe((old + 2) * 1000);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

it("regenerates workspace references before compiling with warm dependencies", async () => {
  const manifest = "{}";
  const fingerprint = createHash("sha256").update(manifest).update(manifest)
    .update(`${process.platform}/${process.arch}/${process.version}/${process.versions.modules}`).digest("hex");
  const npmCalls = [];
  const mock = {
    "node:fs": {
      readFileSync: path => path.endsWith(".echoagent-install-fingerprint") ? fingerprint : manifest,
      existsSync: () => true, copyFileSync: () => {}, writeFileSync: () => {},
    },
    "node:child_process": { spawnSync: (_command, args) => {
      npmCalls.push(args);
      return { status: 0, stdout: "11.21.1" };
    } },
    "./node-runtime-version.mjs": { assertSupportedNodeVersion: () => {}, assertSupportedNpmVersion: () => {} },
    "./patch-theia-node-pty.mjs": {
      patchNodePtyConsoleHelper: () => {}, patchNodePtyPipeErrors: () => {},
      ensureNodePtyConptySourceUnmodified: () => {}, syncNodePtyPrebuilds: () => {}, markNodePtyConptyRebuilt: () => {},
    },
    "./patch-theia-windowless-helpers.mjs": { patchTheiaWindowlessHelpers: () => {}, isTheiaWindowlessRuntime: () => true },
    "./patch-theia-scanoss.mjs": { patchTheiaScanoss: () => {} },
  };
  try {
    for (const [id, exports] of Object.entries(mock)) vi.doMock(id, () => exports);
    await import("./build-theia.mjs");
    expect(npmCalls).not.toContainEqual(["ci"]);
    const compile = npmCalls.findIndex(args => args[0] === "run" && args[1] === "compile");
    expect(compile).toBeGreaterThan(0);
    expect(npmCalls[compile - 1]).toEqual(["run", "compute-references"]);
  } finally {
    for (const id of Object.keys(mock)) vi.doUnmock(id);
  }
});
