import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";
import {
  isNodePtyConsoleHelperHidden, isNodePtyPipeErrorsHandled, isNodePtyConptyReady, isNodePtyConptySourceUnmodified,
  markNodePtyConptyRebuilt, patchNodePtyConsoleHelper, patchNodePtyPipeErrors, ensureNodePtyConptySourceUnmodified,
  syncNodePtyPrebuilds,
} from "./patch-theia-node-pty.mjs";

const temporaryDirectories = [];
const originalCall = "child_process_1.fork(path.join(__dirname, 'conpty_console_list_agent'), [_this._innerPid.toString()])";
const originalFlags = "EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT, // dwCreationFlags";

function fixture(version = "1.2.0-beta.12", call = originalCall) {
  const browserRoot = mkdtempSync(join(tmpdir(), "echo-node-pty-patch-"));
  temporaryDirectories.push(browserRoot);
  const packageRoot = join(browserRoot, "node_modules/node-pty");
  mkdirSync(join(packageRoot, "lib"), { recursive: true });
  mkdirSync(join(packageRoot, "src/win"), { recursive: true });
  mkdirSync(join(packageRoot, "build/Release"), { recursive: true });
  writeFileSync(join(packageRoot, "package.json"), JSON.stringify({ version }));
  writeFileSync(join(packageRoot, "lib/windowsPtyAgent.js"), `var agent = ${call};\n`);
  writeFileSync(join(packageRoot, "lib/windowsTerminal.js"), "");
  writeFileSync(join(packageRoot, "src/win/conpty.cc"), `CreateProcessW(..., ${originalFlags});\n`);
  writeFileSync(join(packageRoot, "build/Release/conpty.node"), "test binary");
  writeFileSync(join(packageRoot, "build/Release/conpty_console_list.node"), "test helper binary");
  if (process.platform === "win32") {
    const conptyDir = join(packageRoot, "build/Release/conpty");
    mkdirSync(conptyDir, { recursive: true });
    writeFileSync(join(conptyDir, "conpty.dll"), "dll");
    writeFileSync(join(conptyDir, "OpenConsole.exe"), "console");
  }
  return browserRoot;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

it("handles early ConPTY pipe errors without terminating the backend", () => {
  const browserRoot = fixture();
  const packageRoot = join(browserRoot, "node_modules/node-pty/lib");
  const agentPath = join(packageRoot, "windowsPtyAgent.js");
  const terminalPath = join(packageRoot, "windowsTerminal.js");
  writeFileSync(agentPath, `this._inSocket.setEncoding('utf8');\n`);
  writeFileSync(terminalPath, `        _this._socket = _this._agent.outSocket;
        // Not available until \`ready\` event emitted.
        _this._socket.on('ready_datapipe', function () {
            // Shutdown if \`error\` event is emitted.
            _this._socket.on('error', function (err) {
                // Close terminal session.
                _this._close();
                // EIO, happens when someone closes our child process: the only process
                // in the terminal.
                // node < 0.6.14: errno 5
                // node >= 0.6.14: read EIO
                if (err.code) {
                    if (~err.code.indexOf('errno 5') || ~err.code.indexOf('EIO'))
                        return;
                }
                // Throw anything else.
                if (_this.listeners('error').length < 2) {
                    throw err;
                }
            });
            // Cleanup after the socket is closed.
            _this._socket.on('close', function () {
                _this.emit('exit', _this._agent.exitCode);
                _this._close();
            });
        });
`);
  expect(isNodePtyPipeErrorsHandled(browserRoot)).toBe(false);
  patchNodePtyPipeErrors(browserRoot);
  expect(isNodePtyPipeErrorsHandled(browserRoot)).toBe(true);
  const terminal = readFileSync(terminalPath, "utf8");
  expect(terminal).not.toContain("throw err;");
  expect(terminal.indexOf("_socket.on('error'")).toBeLessThan(terminal.indexOf("_socket.on('ready_datapipe'"));
  patchNodePtyPipeErrors(browserRoot);
  expect(readFileSync(terminalPath, "utf8")).toBe(terminal);
});

it("hides the node-pty console helper and preserves the IPC fork call", () => {
  const browserRoot = fixture();
  expect(isNodePtyConsoleHelperHidden(browserRoot)).toBe(false);
  patchNodePtyConsoleHelper(browserRoot);
  const agent = readFileSync(join(browserRoot, "node_modules/node-pty/lib/windowsPtyAgent.js"), "utf8");
  expect(agent).toContain("[_this._innerPid.toString()], { windowsHide: true })");
  expect(isNodePtyConsoleHelperHidden(browserRoot)).toBe(true);
  patchNodePtyConsoleHelper(browserRoot);
  expect(readFileSync(join(browserRoot, "node_modules/node-pty/lib/windowsPtyAgent.js"), "utf8")).toBe(agent);
});

it("fails staging when the installed node-pty version or helper changes", () => {
  expect(() => patchNodePtyConsoleHelper(fixture("1.2.0"))).toThrow(/Unsupported node-pty version/);
  expect(() => patchNodePtyConsoleHelper(fixture("1.2.0-beta.12", "someOtherFork()"))).toThrow(/helper changed/);
});

it("keeps the ConPTY shell spawn unmodified and rejects a stale native binary", () => {
  const browserRoot = fixture();
  expect(isNodePtyConptySourceUnmodified(browserRoot)).toBe(true);
  const sourcePath = join(browserRoot, "node_modules/node-pty/src/win/conpty.cc");
  const source = readFileSync(sourcePath, "utf8");
  expect(source).toContain("EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,");
  expect(source).not.toContain("CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW");
  expect(isNodePtyConptyReady(browserRoot)).toBe(false);
  expect(() => markNodePtyConptyRebuilt(browserRoot)).toThrow(/not ready/);
  syncNodePtyPrebuilds(browserRoot);
  markNodePtyConptyRebuilt(browserRoot);
  expect(isNodePtyConptyReady(browserRoot)).toBe(true);
  writeFileSync(join(browserRoot, "node_modules/node-pty/build/Release/conpty.node"), "different binary");
  expect(isNodePtyConptyReady(browserRoot)).toBe(false);
});

it("fails when node-pty changes its ConPTY creation flags", () => {
  const browserRoot = fixture();
  writeFileSync(join(browserRoot, "node_modules/node-pty/src/win/conpty.cc"), "other CreateProcessW flags");
  expect(() => ensureNodePtyConptySourceUnmodified(browserRoot)).toThrow(/flags differ/);
});

it("migrates the old no-window ConPTY patch", () => {
  const browserRoot = fixture();
  const sourcePath = join(browserRoot, "node_modules/node-pty/src/win/conpty.cc");
  writeFileSync(sourcePath, readFileSync(sourcePath, "utf8").replace(
    "CREATE_UNICODE_ENVIRONMENT,", "CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,"));
  expect(isNodePtyConptySourceUnmodified(browserRoot)).toBe(false);
  ensureNodePtyConptySourceUnmodified(browserRoot);
  expect(isNodePtyConptySourceUnmodified(browserRoot)).toBe(true);
});

it("syncs rebuilt native files into prebuilds for Theia bundling", () => {
  const browserRoot = fixture();
  syncNodePtyPrebuilds(browserRoot);
  const prebuildDir = join(browserRoot, "node_modules/node-pty/prebuilds", `${process.platform}-${process.arch}`);
  expect(readFileSync(join(prebuildDir, "conpty.node"), "utf8")).toBe("test binary");
  expect(readFileSync(join(prebuildDir, "conpty_console_list.node"), "utf8")).toBe("test helper binary");
  if (process.platform === "win32") {
    expect(readFileSync(join(prebuildDir, "conpty/conpty.dll"), "utf8")).toBe("dll");
  }
  markNodePtyConptyRebuilt(browserRoot);
  writeFileSync(join(prebuildDir, "conpty.node"), "stale binary");
  expect(isNodePtyConptyReady(browserRoot)).toBe(false);
});
