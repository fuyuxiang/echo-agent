import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { EventEmitter } from "node:events";
import { existsSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "vite";
import { createUiReviewServer } from "./ui-review-runtime.mjs";

vi.mock("vite", () => ({ createServer: vi.fn() }));
let root, fail, instances;
beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "echo-review-runtime-test-"));
  fail = {}; instances = [];
  createServer.mockImplementation(async config => {
    writeFileSync(join(config.cacheDir, "optimizer"), "cached");
    if (fail.create) throw new Error("create failed");
    const http = Object.assign(new EventEmitter(), {
      listening: false, closeAllConnections: vi.fn(),
      listen(_port, _host, ready) {
        queueMicrotask(() => { if (fail.listen) http.emit("error", new Error("listen failed")); else { http.listening = true; ready(); } });
      },
    });
    const server = {
      config, httpServer: http,
      environments: { client: { depsOptimizer: {
        init: async () => { if (fail.optimize) throw new Error("optimizer failed"); },
        metadata: { discovered: {} },
      } } },
      closedTimes: 0,
      close: vi.fn(async () => { server.closedTimes++; http.listening = false; if (fail.close) throw new Error("close failed"); }),
    };
    instances.push(server);
    return server;
  });
});
afterEach(() => { vi.clearAllMocks(); rmSync(root, { recursive: true, force: true }); });
const start = () => createUiReviewServer(0, { baseline: false, cacheRoot: root });

it("keeps the cache until shutdown completes and makes repeated shutdown idempotent", async () => {
  const server = await start();
  expect(existsSync(server.config.cacheDir)).toBe(true);
  const first = server.close(), second = server.close();
  expect(first).toBe(second);
  await first;
  expect(existsSync(server.config.cacheDir)).toBe(false);
  expect(server.httpServer.closeAllConnections).toHaveBeenCalledOnce();
  expect(server.closedTimes).toBe(1);
});

for (const stage of ["create", "optimize", "listen"]) {
  it(`reclaims partial caches when ${stage} fails`, async () => {
    fail[stage] = true;
    await expect(start()).rejects.toThrow(/failed/);
    expect(readdirSync(root)).toEqual([]);
    if (instances.length) expect(instances[0].httpServer.listening).toBe(false);
  });
}

it("reclaims the cache even when shutdown rejects", async () => {
  const server = await start();
  fail.close = true;
  await expect(server.close()).rejects.toThrow("close failed");
  expect(existsSync(server.config.cacheDir)).toBe(false);
});

it("preserves startup and shutdown errors while still reclaiming a failed server", async () => {
  fail.listen = true; fail.close = true;
  const error = await start().catch(error => error);
  expect(error).toBeInstanceOf(AggregateError);
  expect(error.errors.map(error => error.message)).toEqual(["listen failed", "close failed"]);
  expect(readdirSync(root)).toEqual([]);
});
