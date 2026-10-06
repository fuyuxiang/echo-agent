import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";
import { isTheiaWindowlessRuntime, patchTheiaWindowlessHelpers } from "./patch-theia-windowless-helpers.mjs";

const sourceFiles = [
  "packages/core/src/node/messaging/ipc-connection-provider.ts",
  "packages/plugin-ext/src/hosted/node/hosted-plugin-process.ts",
];
const runtimeFiles = [
  "packages/core/lib/node/messaging/ipc-connection-provider.js",
  "packages/plugin-ext/lib/hosted/node/hosted-plugin-process.js",
];
const directories = [];

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "echo-windowless-theia-"));
  directories.push(root);
  for (const file of sourceFiles) {
    const path = join(root, file);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, "const options = {\n            windowsHide: process.platform === 'win32',\n};\n");
  }
  return root;
}

afterEach(() => {
  for (const root of directories.splice(0)) rmSync(root, { recursive: true, force: true });
});

it("patches both background forks once and rejects changed upstream options", () => {
  const root = fixture();
  patchTheiaWindowlessHelpers(root);
  for (const file of sourceFiles) {
    const source = readFileSync(join(root, file), "utf8");
    expect(source).toContain("detached: process.platform === 'win32'");
    patchTheiaWindowlessHelpers(root);
    expect(readFileSync(join(root, file), "utf8")).toBe(source);
  }
  writeFileSync(join(root, sourceFiles[1]), "const options = { windowsHide: false };\n");
  expect(() => patchTheiaWindowlessHelpers(root)).toThrow(/fork changed/);
});

it("rejects a staged runtime missing either worker's windowless option", () => {
  const root = fixture();
  for (const file of runtimeFiles) {
    const path = join(root, file);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, "const options = { windowsHide: process.platform === 'win32', detached: process.platform === 'win32' }; cp.fork('worker', [], options);\n");
  }
  expect(isTheiaWindowlessRuntime(root)).toBe(true);
  writeFileSync(join(root, runtimeFiles[1]), "cp.fork('worker', [], { windowsHide: true });\n");
  expect(isTheiaWindowlessRuntime(root)).toBe(false);
});
