import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";

// node-pty's ConPTY cleanup forks a Node helper when a terminal is closed.
// The Theia backend has no console, so an unhidden helper opens a separate
// Windows console even though the backend itself was started without one.
const expectedVersion = "1.2.0-beta.12";
const originalCall = "child_process_1.fork(path.join(__dirname, 'conpty_console_list_agent'), [_this._innerPid.toString()])";
const hiddenCall = "child_process_1.fork(path.join(__dirname, 'conpty_console_list_agent'), [_this._innerPid.toString()], { windowsHide: true })";
// A ConPTY shell needs its pseudo console. CREATE_NO_WINDOW on this native
// CreateProcessW call disconnects the shell from it and breaks terminal I/O.
const originalConptyFlags = "EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT, // dwCreationFlags";
const hiddenConptyFlags = "EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW, // dwCreationFlags";
const originalInputSocket = "this._inSocket.setEncoding('utf8');";
const guardedInputSocket = `${originalInputSocket}
        this._inSocket.on('error', function (err) {
            console.warn('ConPTY input pipe failed:', err);
            _this._outSocket.destroy();
        });`;
const originalTerminalSocket = `        _this._socket = _this._agent.outSocket;
        // Not available until \`ready\` event emitted.`;
const guardedTerminalSocket = `        _this._socket = _this._agent.outSocket;
        // Register before ready_datapipe: a pipe can fail during connection.
        _this._socket.on('error', function (err) {
            console.warn('ConPTY output pipe failed:', err);
            _this._close();
            _this._socket.destroy();
        });
        _this._socket.on('close', function () {
            _this.emit('exit', _this._agent.exitCode);
            _this._close();
        });
        // Not available until \`ready\` event emitted.`;
const originalLateTerminalHandlers = `            // Shutdown if \`error\` event is emitted.
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
`;
const nativePatchVersion = 2;
const prebuildArtifacts = ["conpty.node", "conpty_console_list.node", "pty.node", "spawn-helper"];

function paths(browserRoot) {
  const packageRoot = join(browserRoot, "node_modules/node-pty");
  return {
    manifest: join(packageRoot, "package.json"),
    agent: join(packageRoot, "lib/windowsPtyAgent.js"),
    terminal: join(packageRoot, "lib/windowsTerminal.js"),
    conptySource: join(packageRoot, "src/win/conpty.cc"),
    conptyBinary: join(packageRoot, "build/Release/conpty.node"),
    nativeMarker: join(packageRoot, ".echoagent-conpty-upstream.json"),
  };
}

function occurrences(source, value) {
  return source.split(value).length - 1;
}

export function isNodePtyConsoleHelperHidden(browserRoot) {
  try {
    const { manifest, agent } = paths(browserRoot);
    if (JSON.parse(readFileSync(manifest, "utf8")).version !== expectedVersion) return false;
    const source = readFileSync(agent, "utf8");
    return occurrences(source, hiddenCall) === 1 && occurrences(source, originalCall) === 0;
  } catch {
    return false;
  }
}

export function patchNodePtyConsoleHelper(browserRoot) {
  const { manifest, agent } = paths(browserRoot);
  const version = JSON.parse(readFileSync(manifest, "utf8")).version;
  if (version !== expectedVersion) {
    throw new Error(`Unsupported node-pty version ${version}; review the Windows console helper patch.`);
  }
  const source = readFileSync(agent, "utf8");
  if (occurrences(source, hiddenCall) === 1 && occurrences(source, originalCall) === 0) return;
  if (occurrences(source, originalCall) !== 1 || occurrences(source, hiddenCall) !== 0) {
    throw new Error("node-pty ConPTY helper changed; review its fork call before staging.");
  }
  writeFileSync(agent, source.replace(originalCall, hiddenCall));
  if (!isNodePtyConsoleHelperHidden(browserRoot)) {
    throw new Error("node-pty ConPTY helper patch verification failed.");
  }
}

export function isNodePtyPipeErrorsHandled(browserRoot) {
  try {
    const { manifest, agent, terminal } = paths(browserRoot);
    if (JSON.parse(readFileSync(manifest, "utf8")).version !== expectedVersion) return false;
    const agentSource = readFileSync(agent, "utf8");
    const terminalSource = readFileSync(terminal, "utf8");
    return occurrences(agentSource, guardedInputSocket) === 1
      && occurrences(terminalSource, guardedTerminalSocket) === 1
      && occurrences(terminalSource, originalLateTerminalHandlers) === 0;
  } catch {
    return false;
  }
}

export function patchNodePtyPipeErrors(browserRoot) {
  const { manifest, agent, terminal } = paths(browserRoot);
  const version = JSON.parse(readFileSync(manifest, "utf8")).version;
  if (version !== expectedVersion) {
    throw new Error(`Unsupported node-pty version ${version}; review the Windows pipe error patch.`);
  }
  if (isNodePtyPipeErrorsHandled(browserRoot)) return;
  const agentSource = readFileSync(agent, "utf8");
  const terminalSource = readFileSync(terminal, "utf8");
  if (occurrences(agentSource, originalInputSocket) !== 1
      || occurrences(agentSource, guardedInputSocket) !== 0
      || occurrences(terminalSource, originalTerminalSocket) !== 1
      || occurrences(terminalSource, guardedTerminalSocket) !== 0
      || occurrences(terminalSource, originalLateTerminalHandlers) !== 1) {
    throw new Error("node-pty Windows pipe handling changed; review before staging.");
  }
  writeFileSync(agent, agentSource.replace(originalInputSocket, guardedInputSocket));
  writeFileSync(terminal, terminalSource.replace(originalTerminalSocket, guardedTerminalSocket)
    .replace(originalLateTerminalHandlers, ""));
  if (!isNodePtyPipeErrorsHandled(browserRoot)) {
    throw new Error("node-pty Windows pipe error patch verification failed.");
  }
}

export function isNodePtyConptySourceUnmodified(browserRoot) {
  try {
    const { manifest, conptySource } = paths(browserRoot);
    if (JSON.parse(readFileSync(manifest, "utf8")).version !== expectedVersion) return false;
    const source = readFileSync(conptySource, "utf8");
    return occurrences(source, originalConptyFlags) === 1 && occurrences(source, hiddenConptyFlags) === 0;
  } catch {
    return false;
  }
}

export function ensureNodePtyConptySourceUnmodified(browserRoot) {
  const { manifest, conptySource } = paths(browserRoot);
  const version = JSON.parse(readFileSync(manifest, "utf8")).version;
  if (version !== expectedVersion) {
    throw new Error(`Unsupported node-pty version ${version}; review the ConPTY creation flags.`);
  }
  const source = readFileSync(conptySource, "utf8");
  if (occurrences(source, originalConptyFlags) === 1 && occurrences(source, hiddenConptyFlags) === 0) return;
  if (occurrences(source, hiddenConptyFlags) === 1 && occurrences(source, originalConptyFlags) === 0) {
    writeFileSync(conptySource, source.replace(hiddenConptyFlags, originalConptyFlags));
    if (isNodePtyConptySourceUnmodified(browserRoot)) return;
  }
  throw new Error("node-pty ConPTY creation flags differ from the pinned upstream source; review before staging.");
}

function binaryHash(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

export function markNodePtyConptyRebuilt(browserRoot) {
  if (!isNodePtyConptySourceUnmodified(browserRoot) || !nativeAssetsReady(browserRoot)) {
    throw new Error("node-pty ConPTY source or prebuilds are not ready for staging.");
  }
  const { conptyBinary, nativeMarker } = paths(browserRoot);
  writeFileSync(nativeMarker, JSON.stringify({ version: nativePatchVersion, binarySha256: binaryHash(conptyBinary) }));
}

function nativeAssetsReady(browserRoot) {
  const packageRoot = join(browserRoot, "node_modules/node-pty");
  const releaseDir = join(packageRoot, "build/Release");
  const prebuildDir = join(packageRoot, "prebuilds", `${process.platform}-${process.arch}`);
  for (const name of ["conpty.node", "conpty_console_list.node"]) {
    const source = join(releaseDir, name);
    const staged = join(prebuildDir, name);
    if (!existsSync(source) || !existsSync(staged) || binaryHash(source) !== binaryHash(staged)) return false;
  }
  if (process.platform === "win32") {
    for (const name of ["conpty.dll", "OpenConsole.exe"]) {
      const source = join(releaseDir, "conpty", name);
      const staged = join(prebuildDir, "conpty", name);
      if (!existsSync(source) || !existsSync(staged) || binaryHash(source) !== binaryHash(staged)) return false;
    }
  }
  return true;
}

export function isNodePtyConptyReady(browserRoot) {
  try {
    if (!isNodePtyConptySourceUnmodified(browserRoot) || !nativeAssetsReady(browserRoot)) return false;
    const { conptyBinary, nativeMarker } = paths(browserRoot);
    const marker = JSON.parse(readFileSync(nativeMarker, "utf8"));
    return marker.version === nativePatchVersion && marker.binarySha256 === binaryHash(conptyBinary);
  } catch {
    return false;
  }
}

export function syncNodePtyPrebuilds(browserRoot) {
  if (!isNodePtyConptySourceUnmodified(browserRoot)) {
    throw new Error("node-pty ConPTY creation flags must match upstream before syncing prebuilds.");
  }
  const packageRoot = join(browserRoot, "node_modules/node-pty");
  const releaseDir = join(packageRoot, "build/Release");
  const prebuildDir = join(packageRoot, "prebuilds", `${process.platform}-${process.arch}`);
  for (const name of ["conpty.node", "conpty_console_list.node"]) {
    if (!existsSync(join(releaseDir, name))) {
      throw new Error(`node-pty rebuild did not produce ${name}.`);
    }
  }
  mkdirSync(prebuildDir, { recursive: true });
  for (const name of prebuildArtifacts) {
    if (existsSync(join(releaseDir, name))) {
      copyFileSync(join(releaseDir, name), join(prebuildDir, name));
    }
  }
  if (process.platform === "win32") {
    const conptyDir = join(releaseDir, "conpty");
    for (const name of ["conpty.dll", "OpenConsole.exe"]) {
      if (!existsSync(join(conptyDir, name))) {
        throw new Error(`node-pty rebuild did not stage ${name}.`);
      }
    }
    cpSync(conptyDir, join(prebuildDir, "conpty"), { recursive: true });
  }
}
