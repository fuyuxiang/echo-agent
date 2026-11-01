import * as fs from "node:fs";
import * as path from "node:path";
import { tmpdir } from "node:os";
import { EventEmitter } from "node:events";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { afterEach, expect, it } from "vitest";

const temporary = [];
afterEach(() => {
  for (const directory of temporary.splice(0)) fs.rmSync(directory, { recursive: true, force: true });
});

const ready = {
  webviewRendered: true, ipcReady: true, resourcesPresent: true,
  ideStarted: true, ideError: null, cliError: null, visibleConsoleWindows: [],
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
      if (report) fs.writeFileSync(path.join(home, "desktop-validation.json"), JSON.stringify(report));
      fs.mkdirSync(path.join(home, "theia"));
      fs.writeFileSync(path.join(home, "theia", "theia.log"), "isolated IDE shutdown log");
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

it("keeps the console failure gate and retains only the report and isolated IDE log", async () => {
  const report = { ...ready, visibleConsoleWindows: ["ConsoleWindowClass PID=1304 HWND=131704 phase=ide-shutdown processTree=conhost.exe[1304] <- taskkill.exe[4321]"] };
  const result = fixture(report);
  await expect(result.run()).rejects.toThrow(/Packaged desktop startup check failed/);
  expect(JSON.parse(result.logs[0])).toEqual(report);
  expect(JSON.parse(fs.readFileSync(path.join(result.output, "desktop-validation.json"), "utf8"))).toEqual(report);
  expect(fs.readFileSync(path.join(result.output, "theia.log"), "utf8")).toBe("isolated IDE shutdown log");
  expect(fs.existsSync(path.join(result.output, "config.toml"))).toBe(false);
  expect(fs.existsSync(result.home())).toBe(true);
  expect(result.errors.join("\n")).toContain("Validation data retained for diagnosis");
  const run = JSON.parse(fs.readFileSync(path.join(result.output, "validation-run.json"), "utf8"));
  expect(run.passed).toBe(false);
  expect(run.pid).toBe(12345);
  expect(run.error).toContain("phase=ide-shutdown");
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
  expect(fs.existsSync(path.join(result.output, "theia.log"))).toBe(true);
});
