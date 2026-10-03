import { createRequire } from "node:module";
import { resolve, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { isNodePtyConsoleHelperHidden, isNodePtyPipeErrorsHandled, isNodePtyConptyReady } from "./patch-theia-node-pty.mjs";

if (process.platform !== "win32") throw new Error("This ConPTY smoke test requires Windows.");
if (process.argv.length !== 3) throw new Error("Usage: node scripts/validate-theia-pty.mjs [theia-browser-directory]");
const browserRoot = resolve(process.argv[2]);
if (!isNodePtyConsoleHelperHidden(browserRoot)) throw new Error("Staged node-pty helper is not hidden.");
if (!isNodePtyPipeErrorsHandled(browserRoot)) throw new Error("Staged node-pty ConPTY pipe errors can terminate the IDE backend.");
if (!isNodePtyConptyReady(browserRoot)) throw new Error("Staged node-pty ConPTY binary or upstream creation flags failed validation.");

const require = createRequire(join(browserRoot, "package.json"));
const childProcess = require("node:child_process");
const pty = require("node-pty");
const originalFork = childProcess.fork;
let helperForkCount = 0;
let terminal;
// Intercept only to verify the real node-pty kill path; the helper still runs.
childProcess.fork = function (modulePath, args, options) {
  if (String(modulePath).includes("conpty_console_list_agent")) {
    helperForkCount++;
    if (options?.windowsHide !== true) {
      throw new Error("node-pty started a visible console-list helper.");
    }
  }
  return originalFork.apply(this, arguments);
};

try {
  terminal = pty.spawn(process.env.ComSpec || "cmd.exe", ["/Q"], {
    cwd: browserRoot,
    cols: 80,
    rows: 24,
    env: process.env,
  });
  let output = "";
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("Theia ConPTY shell did not respond.")), 10_000);
    terminal.onData(data => {
      output += data;
      if (output.includes("ECHO_PTY_SMOKE_READY")) {
        clearTimeout(timeout);
        resolve();
      }
    });
    terminal.write("echo ECHO_PTY_SMOKE_READY\r");
  });
  if (terminal.pid <= 0) throw new Error("Theia ConPTY shell has no process ID.");
  // Wait for node-pty's initial data callback to mark the terminal ready.
  await delay(100);
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("Theia ConPTY shell did not close.")), 10_000);
    terminal.onExit(() => {
      clearTimeout(timeout);
      resolve();
    });
    try {
      terminal.kill();
      terminal = undefined;
    } catch (error) {
      clearTimeout(timeout);
      reject(error);
    }
  });
  if (helperForkCount !== 1) throw new Error(`Expected one hidden console helper, got ${helperForkCount}.`);
  terminal = pty.spawn(process.env.ComSpec || "cmd.exe", ["/Q"], {
    cwd: browserRoot,
    cols: 80,
    rows: 24,
    env: process.env,
  });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("Second ConPTY shell did not respond.")), 10_000);
    let output = "";
    terminal.onData(data => {
      output += data;
      if (output.includes("ECHO_PTY_PIPE_READY")) {
        clearTimeout(timeout);
        resolve();
      }
    });
    terminal.write("echo ECHO_PTY_PIPE_READY\r");
  });
  const brokenPipeExited = new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("Broken ConPTY pipe did not close its terminal.")), 10_000);
    terminal.onExit(() => {
      clearTimeout(timeout);
      resolve();
    });
  });
  // Simulate the named pipe error seen when the user closes the shell window.
  // The script process is the host: an uncaught error here fails the build.
  terminal._agent.inSocket.emit("error", Object.assign(new Error("simulated broken ConPTY pipe"), { code: "UNKNOWN" }));
  await brokenPipeExited;
  terminal.kill();
  terminal = undefined;
  console.log("Theia ConPTY shell, hidden cleanup helper, and broken-pipe recovery are working.");
} finally {
  if (terminal) {
    try { terminal.kill(); } catch { /* Preserve the smoke-test failure. */ }
  }
  childProcess.fork = originalFork;
}
