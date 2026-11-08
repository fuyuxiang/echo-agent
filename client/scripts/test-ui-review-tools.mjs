import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const helper = new URL("./ui-review-tools.mjs", import.meta.url).href;

test("browser and PNG audits use the configured tools outside a clean checkout", () => {
  const root = mkdtempSync(join(tmpdir(), "echo-review-tools-"));
  try {
    const checkout = join(root, "checkout");
    const tools = join(root, "独立 tools", "node_modules");
    mkdirSync(checkout);
    mkdirSync(join(tools, "playwright"), { recursive: true });
    mkdirSync(join(tools, "playwright-core", "lib"), { recursive: true });
    writeFileSync(join(tools, "playwright", "index.mjs"), "export const chromium = { installation: 'configured' };");
    writeFileSync(join(tools, "playwright-core", "package.json"), '{"name":"playwright-core","version":"0.0.0"}');
    writeFileSync(join(tools, "playwright-core", "lib", "utilsBundle.js"), "exports.PNG = { installation: 'configured' };");
    const script = [
      'import assert from "node:assert/strict";',
      "import { reviewChromium, reviewPng } from " + JSON.stringify(helper) + ";",
      'assert.equal((await reviewChromium()).installation, "configured");',
      'assert.equal(reviewPng().installation, "configured");',
    ].join("\n");
    const run = spawnSync(process.execPath, ["--input-type=module", "-e", script], {
      cwd: checkout, env: { ...process.env, PLAYWRIGHT_MODULE: join(tools, "playwright", "index.mjs") }, encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stderr);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an invalid explicit tools path reports the configuration error", () => {
  const root = mkdtempSync(join(tmpdir(), "echo-review-tools-missing-"));
  try {
    const script = [
      'import assert from "node:assert/strict";',
      "import { reviewChromium, reviewPng } from " + JSON.stringify(helper) + ";",
      'assert.throws(() => reviewPng(), /PLAYWRIGHT_MODULE does not exist/);',
      'await assert.rejects(reviewChromium(), /PLAYWRIGHT_MODULE does not exist/);',
    ].join("\n");
    const run = spawnSync(process.execPath, ["--input-type=module", "-e", script], {
      cwd: root, env: { ...process.env, PLAYWRIGHT_MODULE: join(root, "missing", "index.mjs") }, encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stderr);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a root installation resolves the ESM browser entry and matching PNG decoder", () => {
  const root = mkdtempSync(join(tmpdir(), "echo-review-tools-root-"));
  try {
    const tools = join(root, "node_modules");
    mkdirSync(join(tools, "playwright"), { recursive: true });
    mkdirSync(join(tools, "playwright-core", "lib"), { recursive: true });
    writeFileSync(join(root, "ui-review-tools.mjs"), readFileSync(new URL(helper)));
    writeFileSync(join(tools, "playwright", "package.json"), JSON.stringify({
      name: "playwright", exports: { import: "./index.mjs", require: "./index.cjs" },
    }));
    writeFileSync(join(tools, "playwright", "index.mjs"), "export const chromium = { installation: 'root-esm' };");
    writeFileSync(join(tools, "playwright", "index.cjs"), "throw new Error('Browser import must use the ESM entry');");
    writeFileSync(join(tools, "playwright-core", "package.json"), '{"name":"playwright-core","version":"0.0.0"}');
    writeFileSync(join(tools, "playwright-core", "lib", "utilsBundle.js"), "exports.PNG = { installation: 'root-esm' };");
    const script = [
      'import assert from "node:assert/strict";',
      'import { reviewChromium, reviewPng } from "./ui-review-tools.mjs";',
      'assert.equal((await reviewChromium()).installation, "root-esm");',
      'assert.equal(reviewPng().installation, "root-esm");',
    ].join("\n");
    const env = { ...process.env }; delete env.PLAYWRIGHT_MODULE;
    const run = spawnSync(process.execPath, ["--input-type=module", "-e", script], { cwd: root, env, encoding: "utf8" });
    assert.equal(run.status, 0, run.stderr);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
