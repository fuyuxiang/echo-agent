import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
import { assertSupportedNodeVersion } from "./node-runtime-version.mjs";

test("bundled Node versions must meet the Office runtime minimum", () => {
  for (const version of ["22.23.4", "22.24.0", "24.22.1"]) assert.doesNotThrow(() => assertSupportedNodeVersion(version));
  for (const version of ["20.19.0", "22.14.0", "22.23.3", "24.17.0", "24.22.0", "25.0.0"]) assert.throws(() => assertSupportedNodeVersion(version), /requires Node.js/);
});

test("legacy glob consumers retain the callable minimatch API without globparts preprocessing", () => {
  const pnpmRoot = new URL("../node_modules/.pnpm/", import.meta.url);
  const versions = ["3.1.5", "5.1.9"];
  for (const expected of versions) {
    const globRequire = createRequire(new URL(`minimatch@${expected}/node_modules/minimatch/package.json`, pnpmRoot));
    const minimatch = globRequire("minimatch");
    assert.equal(globRequire("minimatch/package.json").version, expected);
    assert.equal(typeof minimatch, "function");
    const matcher = new minimatch.Minimatch("**/*.ts", { optimizationLevel: 2 });
    assert.equal(matcher.match("中文/main.ts"), true);
    assert.equal(matcher.match("中文/main.js"), false);
    assert.equal(matcher.firstPhasePreProcess, undefined);
    assert.equal(matcher.secondPhasePreProcess, undefined);
  }
});
