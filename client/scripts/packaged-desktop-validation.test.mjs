import * as fs from "node:fs";
import * as path from "node:path";
import { tmpdir } from "node:os";
import { EventEmitter } from "node:events";
import { spawnSync } from "node:child_process";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { afterEach, expect, it } from "vitest";

const temporary = [];
afterEach(() => {
  for (const directory of temporary.splice(0)) fs.rmSync(directory, { recursive: true, force: true });
});

const ready = {
  webviewRendered: true, ipcReady: true, resourcesPresent: true,
  officeExported: true, officeError: null, visibleConsoleWindows: [],
};

function fixture(report, exitCode = 0) {
  const directory = fs.mkdtempSync(path.join(tmpdir(), "echo-validation-script-test-"));
  temporary.push(directory);
  const output = path.join(directory, "diagnostics");
  const executable = path.join(directory, "echoagent.exe");
  fs.writeFileSync(executable, "test executable");
  const logs = [], errors = [];
  let home;
  const imports = {
    "node:fs": fs, "node:path": path, "node:os": { tmpdir: () => directory },
    "node:timers/promises": { setTimeout: () => new Promise(() => {}) },
    "node:child_process": { spawn(_executable, _args, options) {
      home = options.env.ECHO_AGENT_HOME;
      expect(options.env.ECHO_DESKTOP_VALIDATION).toBe("1");
      expect(options.env.PATH).toBe(path.join(home, "empty-path"));
      if (report) fs.writeFileSync(path.join(home, "desktop-validation.json"), JSON.stringify(report));
      fs.writeFileSync(path.join(home, "config.toml"), "private runtime data");
      const app = new EventEmitter();
      Object.assign(app, { pid: 12345, exitCode: null, signalCode: null });
      queueMicrotask(() => { app.exitCode = exitCode; app.emit("exit", exitCode); });
      return app;
    } },
  };
  const source = fs.readFileSync(new URL("./validate-packaged-desktop.mjs", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
  } }).outputText;
  const run = () => runInNewContext(`(async () => { ${js}\n })()`, {
    exports: {}, process: {
      argv: ["node", "validate-packaged-desktop.mjs", executable], platform: "win32",
      env: { ECHO_DESKTOP_VALIDATION_OUTPUT: output },
    },
    console: { log: value => logs.push(value), error: value => errors.push(value) },
    require: id => {
      if (!(id in imports)) throw new Error(`Unexpected production import: ${id}`);
      return imports[id];
    },
  });
  return { output, logs, errors, run, home: () => home };
}

it("keeps the console failure gate and retains only the startup report", async () => {
  const report = { ...ready, visibleConsoleWindows: ["ConsoleWindowClass PID=1304 HWND=131704 phase=office-export processTree=conhost.exe[1304] <- taskkill.exe[4321]"] };
  const result = fixture(report);
  await expect(result.run()).rejects.toThrow(/Packaged desktop startup check failed/);
  expect(JSON.parse(result.logs[0])).toEqual(report);
  expect(JSON.parse(fs.readFileSync(path.join(result.output, "desktop-validation.json"), "utf8"))).toEqual(report);
  expect(fs.existsSync(path.join(result.output, "config.toml"))).toBe(false);
  expect(fs.existsSync(result.home())).toBe(true);
  expect(result.errors.join("\n")).toContain("Validation data retained for diagnosis");
  const run = JSON.parse(fs.readFileSync(path.join(result.output, "validation-run.json"), "utf8"));
  expect(run.passed).toBe(false);
  expect(run.pid).toBe(12345);
  expect(run.error).toContain("phase=office-export");
});

it("retains successful startup evidence before removing the temporary runtime home", async () => {
  const result = fixture(ready);
  await result.run();
  expect(fs.existsSync(result.home())).toBe(false);
  expect(JSON.parse(fs.readFileSync(path.join(result.output, "validation-run.json"), "utf8"))).toMatchObject({ passed: true, exitCode: 0, error: null });
  expect(fs.existsSync(path.join(result.output, "desktop-validation.json"))).toBe(true);
});

it("preserves an early failure even if the application never wrote a report", async () => {
  const result = fixture(null, 7);
  await expect(result.run()).rejects.toThrow(/ENOENT/);
  const run = JSON.parse(fs.readFileSync(path.join(result.output, "validation-run.json"), "utf8"));
  expect(run).toMatchObject({ passed: false, exitCode: 7 });
  expect(run.error).toContain("ENOENT");
});

it("fails packaging validation if the bundled Office worker cannot run", async () => {
  const result = fixture({ ...ready, officeExported: false, officeError: "Node resource failed" });
  await expect(result.run()).rejects.toThrow("Node resource failed");
});

it.each([null, "validate-native-review.mjs", "validate-packaged-desktop.mjs"])("requires both packaged journeys and restores resources (missing runner: %s)", missingRunner => {
  const root = fs.mkdtempSync(path.join(tmpdir(), "echo-package-resource-test-"));
  temporary.push(root);
  const executable = path.join(root, "EchoAgent.app", "Contents", "MacOS", "echoagent");
  const output = path.join(root, "diagnostics");
  const resourceNames = ["office/node/bin/node", "office/node/LICENSE", "office/worker.mjs", "office/fonts/NotoSansCJKsc-Regular.otf"];
  function write(file, contents) {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, contents);
  }
  write(executable, "synthetic executable");
  for (const name of resourceNames) {
    write(path.join(root, "src-tauri", "resources", name), name);
    write(path.join(root, "EchoAgent.app", "Contents", "Resources", name), name);
  }
  const scripts = path.join(root, "scripts");
  for (const file of ["validate-packaged-native-review.mjs", "lib/native-review-isolation.mjs"]) {
    write(path.join(scripts, file), fs.readFileSync(new URL(`./${file}`, import.meta.url)));
  }
  for (const runner of ["validate-native-review.mjs", "validate-packaged-desktop.mjs"]) {
    if (runner === missingRunner) continue;
    write(path.join(scripts, runner), `
import { existsSync } from "node:fs";
if (${JSON.stringify(resourceNames)}.some(name => existsSync("src-tauri/resources/" + name))) process.exit(1);
`);
  }
  const result = spawnSync(process.execPath, [path.join(scripts, "validate-packaged-native-review.mjs"), executable], {
    cwd: root, env: { ...process.env, PACKAGE_REVIEW_OUTPUT: output }, encoding: "utf8", timeout: 20000,
  });
  const proof = JSON.parse(fs.readFileSync(path.join(output, "package-resource-review.json"), "utf8"));
  expect(proof.checks.map(check => check.id)).toEqual(["native-bundled-journey", "bundled-webview-and-office"]);
  expect(proof.checks.map(check => check.passed)).toEqual([
    missingRunner !== "validate-native-review.mjs",
    missingRunner !== "validate-packaged-desktop.mjs",
  ]);
  expect(proof.stagedFiles.every(file => file.restored)).toBe(true);
  for (const name of resourceNames) {
    expect(fs.readFileSync(path.join(root, "src-tauri", "resources", name), "utf8")).toBe(name);
  }
  expect(result.status, result.error?.message || result.stdout + result.stderr).toBe(missingRunner ? 1 : 0);
  expect(proof.passed).toBe(missingRunner === null);
});
