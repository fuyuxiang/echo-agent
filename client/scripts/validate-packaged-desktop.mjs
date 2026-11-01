import { spawn } from "node:child_process";
import { mkdtempSync, existsSync, readFileSync, rmSync, mkdirSync, copyFileSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { tmpdir } from "node:os";
import { setTimeout as delay } from "node:timers/promises";
const executable = process.argv[2];
if (!executable || !existsSync(executable)) throw new Error("Pass the packaged application executable");
const home = mkdtempSync(join(tmpdir(), "echo-desktop-validation-"));
const app = spawn(resolve(executable), [], { env: { ...process.env, ECHO_AGENT_HOME: home, ECHO_DESKTOP_VALIDATION: "1" }, stdio: "inherit" });
let error;
let validationError;
let passed = false;
app.once("error", value => { error = value; });
const exited = new Promise(resolve => { app.once("exit", resolve); app.once("error", resolve); });
try {
  const finished = await Promise.race([exited.then(() => true), delay(150_000, undefined, { ref: false }).then(() => false)]);
  if (error) throw error;
  if (!finished) throw new Error("Packaged desktop did not finish its WebView/IPC startup check within 150 seconds");
  const report = JSON.parse(readFileSync(join(home, "desktop-validation.json"), "utf8"));
  console.log(JSON.stringify(report));
  const consoleCheckPassed = process.platform !== "win32"
    || (Array.isArray(report.visibleConsoleWindows) && report.visibleConsoleWindows.length === 0);
  const cliCheckPassed = process.platform !== "win32"
    || (Object.hasOwn(report, "cliError") && report.cliError === null);
  if (!report.webviewRendered || !report.ipcReady || !report.resourcesPresent
      || (["win32", "darwin"].includes(process.platform) && !report.ideStarted)
      || !cliCheckPassed
      || !consoleCheckPassed
      || app.exitCode !== 0) {
    throw new Error(`Packaged desktop startup check failed: ${report.ideError ?? report.cliError ?? JSON.stringify(report.visibleConsoleWindows ?? [])}`);
  }
  passed = true;
} catch (error) {
  validationError = error;
  throw error;
} finally {
  if (app.exitCode === null) { app.kill(); await Promise.race([exited, delay(3000, undefined, { ref: false })]); if (app.exitCode === null) app.kill("SIGKILL"); }
  if (process.env.ECHO_DESKTOP_VALIDATION_OUTPUT) {
    try {
      const output = resolve(process.env.ECHO_DESKTOP_VALIDATION_OUTPUT);
      mkdirSync(output, { recursive: true });
      writeFileSync(join(output, "validation-run.json"), JSON.stringify({
        executable: resolve(executable), pid: app.pid, home, passed,
        exitCode: app.exitCode, signal: app.signalCode,
        error: validationError?.stack ?? null,
      }, null, 2));
      // Only retain the isolated startup report and IDE log, not runtime data.
      for (const [source, target] of [["desktop-validation.json", "desktop-validation.json"], [join("theia", "theia.log"), "theia.log"]]) {
        const path = join(home, source);
        if (existsSync(path)) copyFileSync(path, join(output, target));
      }
    } catch (error) {
      console.error(`Could not retain packaged desktop diagnostics: ${error}`);
    }
  }
  if (passed) rmSync(home, { recursive: true, force: true });
  else console.error(`Validation data retained for diagnosis: ${home}`);
}
