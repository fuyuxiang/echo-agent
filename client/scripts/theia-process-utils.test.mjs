import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { expect, it } from "vitest";

// Execute the production cleanup method; only OS process creation and logging
// are replaced so its Windows options and error handling run on every CI host.
function fixture(result, platform = "win32") {
  const source = readFileSync(new URL("../vendor/theia-platform/packages/core/src/node/process-utils.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
    experimentalDecorators: true, emitDecoratorMetadata: false,
  } }).outputText;
  const calls = [], warnings = [];
  const decorator = () => () => undefined;
  const imports = {
    child_process: { spawnSync(...args) { calls.push(args); return result; } },
    inversify: { injectable: () => value => value, inject: decorator, named: decorator },
    "../common/logger": { ILogger: Symbol() },
  };
  const exports = {};
  runInNewContext(js, { exports, process: { platform }, require: id => {
    if (!(id in imports)) throw new Error(`Unexpected production import: ${id}`);
    return imports[id];
  } });
  const utils = new exports.ProcessUtils();
  utils.logger = { warn: message => warnings.push(message) };
  return { utils, calls, warnings };
}

it("hides the real Windows process-tree cleanup without changing its target or arguments", () => {
  const { utils, calls, warnings } = fixture({ status: 0 });
  utils.terminateProcessTree(12345);
  expect(calls).toEqual([["taskkill.exe", ["/f", "/t", "/pid", "12345"], { encoding: "utf8", windowsHide: true }]]);
  expect(warnings).toEqual([]);
});

it("still tolerates descendants that exited before taskkill", () => {
  const { utils, calls, warnings } = fixture({ status: 128, output: [null, "already exited", ""] });
  expect(() => utils.terminateProcessTree(12345)).not.toThrow();
  expect(calls[0][2].windowsHide).toBe(true);
  expect(warnings).toHaveLength(1);
  expect(warnings[0]).toContain("exited with 128 for PID 12345");
});

it("still propagates an OS error starting taskkill", () => {
  const error = new Error("spawn EACCES");
  const { utils } = fixture({ error });
  expect(() => utils.terminateProcessTree(12345)).toThrow(error);
});
