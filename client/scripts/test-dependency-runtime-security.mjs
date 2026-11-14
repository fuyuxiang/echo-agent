import assert from "node:assert/strict";
import * as fs from "node:fs";
import { createServer, request as httpRequest } from "node:http";
import { once } from "node:events";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { runInNewContext } from "node:vm";
import test from "node:test";

const theiaRequire = createRequire(resolve("vendor/theia-platform/package.json"));
const cleanupRequire = createRequire(theiaRequire.resolve("rimraf"));

test("Hono mounts preserve authentication boundaries for encoded path separators", async () => {
  const { Hono } = theiaRequire("hono");
  const child = new Hono();
  child.get("/public", context => context.text("public"));
  child.get("/admin/secret", context => context.text("private"));
  const app = new Hono();
  app.use("/app/admin/*", context => context.text("denied", 403));
  app.mount("/app", request => child.fetch(request));
  assert.equal(await (await app.request("/app/public")).text(), "public");
  assert.equal((await app.request("/app/admin/secret")).status, 403);
  assert.equal((await app.request("/app/public%5C..%5Cadmin/secret")).status, 404);
});

test("the Node adapter rejects raw backslashes without dispatching a rewritten route", async () => {
  const { Hono } = theiaRequire("hono");
  const { serve } = theiaRequire("@hono/node-server");
  const app = new Hono();
  let privateRequests = 0;
  app.get("/public", context => context.text("public"));
  app.get("/admin/secret", context => { privateRequests++; return context.text("private"); });
  const server = serve({ fetch: app.fetch, hostname: "127.0.0.1", port: 0 });
  if (!server.listening) await once(server, "listening");
  const read = path => new Promise((resolve, reject) => {
    const request = httpRequest({ hostname: "127.0.0.1", port: server.address().port, path }, response => {
      response.resume();
      response.on("end", () => resolve(response.statusCode));
      response.on("error", reject);
    });
    request.on("error", reject);
    request.setTimeout(5000, () => request.destroy(new Error("adapter request timed out")));
    request.end();
  });
  try {
    assert.equal(await read("/public\\..\\admin/secret"), 400);
    assert.equal(privateRequests, 0);
    assert.equal(await read("/public"), 200, "a rejected request must not break subsequent requests");
  } finally {
    server.closeAllConnections?.();
    await new Promise(resolve => server.close(resolve));
  }
});

test("FTP listings ignore inherited fact names while preserving valid file metadata", () => {
  const { parseLine } = theiaRequire("basic-ftp/dist/parseListMLSD.js");
  for (const fact of ["__proto__", "constructor", "toString"]) {
    const file = parseLine(`type=file;size=12;${fact}=untrusted; 中文.txt`);
    assert.equal(file.name, "中文.txt");
    assert.equal(file.size, 12);
    assert.equal(file.isFile, true);
  }
});

test("legacy glob consumers retain the callable minimatch API without globparts preprocessing", () => {
  const cliRequire = createRequire(resolve("vendor/theia-platform/dev-packages/cli/package.json"));
  for (const [require, expected] of [[theiaRequire, "3.1.5"], [cliRequire, "5.1.9"]]) {
    const globRequire = createRequire(require.resolve("glob"));
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

test("modern IDE matching bounds adversarial preprocessing and preserves normal matching", () => {
  const entry = theiaRequire.resolve("minimatch");
  const result = spawnSync(process.execPath, ["--max-old-space-size=128", "-e", `
    const assert = require('node:assert/strict');
    const { minimatch } = require(process.argv[1]);
    assert.equal(minimatch('src/中文.ts', '**/*.ts', { optimizationLevel: 2 }), true);
    assert.equal(typeof minimatch('main.ts', '**/../a/b/'.repeat(20) + '*.ts', { optimizationLevel: 2 }), 'boolean');
    console.log('bounded');
  `, entry], { cwd: tmpdir(), encoding: "utf8", timeout: 10000 });
  assert.equal(result.status, 0, result.error?.message || result.stderr || `matcher failed: ${result.signal}`);
  assert.equal(result.stdout.trim(), "bounded");
});

test("IDE glob keeps async, sync and file-type search APIs and stops I/O on abort", async () => {
  const glob = cleanupRequire("glob");
  const directory = fs.mkdtempSync(join(tmpdir(), "echo-secure-glob-"));
  try {
    fs.mkdirSync(join(directory, "中文/deep"), { recursive: true });
    fs.writeFileSync(join(directory, "root.ts"), "");
    fs.writeFileSync(join(directory, "中文/main.ts"), "");
    fs.writeFileSync(join(directory, "中文/deep/other.js"), "");
    const normalize = paths => paths.map(path => path.replaceAll("\\", "/")).sort();
    const options = { cwd: directory, nodir: true };
    assert.deepEqual(normalize(await glob.glob("**/*.ts", options)), ["root.ts", "中文/main.ts"]);
    assert.deepEqual(normalize(glob.globSync("**/*.ts", options)), ["root.ts", "中文/main.ts"]);
    const files = await glob.glob("**/*.ts", { ...options, withFileTypes: true });
    assert.equal(files.length, 2);
    assert.ok(files.every(file => file.isFile() && file.fullpath().startsWith(directory)));
    const controller = new AbortController();
    let reads = 0;
    const instrumentedFs = { ...fs, readdir(...args) {
      reads++;
      const callback = args.pop();
      fs.readdir(...args, (error, entries) => {
        controller.abort(new Error("fixture traversal budget"));
        callback(error, entries);
      });
    } };
    await assert.rejects(glob.glob("**/*", { cwd: directory, signal: controller.signal, fs: instrumentedFs }), /fixture traversal budget/);
    await delay(80);
    assert.equal(reads, 1, "cancelled search continued traversing directories");
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
});

test("redirects strip custom sensitive headers from same-realm and cross-realm arrays", async () => {
  const redirects = theiaRequire("follow-redirects");
  const target = createServer((request, response) => response.end(JSON.stringify(request.headers)));
  let origin;
  const listen = server => new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const close = server => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); });
  try {
    await listen(target);
    origin = createServer((_request, response) => {
      response.writeHead(302, { Location: `http://127.0.0.1:${target.address().port}/target` }).end();
    });
    await listen(origin);
    for (const sensitiveHeaders of [["X-API-Key"], runInNewContext('["X-API-Key"]')]) {
      const headers = await new Promise((resolve, reject) => {
        const request = redirects.http.get(`http://127.0.0.1:${origin.address().port}/`, {
          sensitiveHeaders, headers: { "X-API-Key": "fixture-only", "X-Harmless": "retained" },
        }, response => {
          let body = "";
          response.setEncoding("utf8");
          response.on("data", chunk => { body += chunk; });
          response.on("end", () => { try { resolve(JSON.parse(body)); } catch (error) { reject(error); } });
          response.on("error", reject);
        });
        request.on("error", reject);
        request.setTimeout(5000, () => request.destroy(new Error("redirect fixture timed out")));
      });
      assert.equal(headers["x-api-key"], undefined);
      assert.equal(headers["x-harmless"], "retained");
    }
  } finally {
    if (origin) await close(origin);
    await close(target);
  }
});
