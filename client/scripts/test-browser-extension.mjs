// Exercise the shipped service worker with a fake Chrome API. No user tabs,
// browser profile or debugger connection is touched by these tests.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const source = readFileSync('browser-extension/background.js', 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
function fixture() {
  const handlers = {}, commands = [], detached = [], sockets = [];
  const timers = new Map();
  let timerId = 0;
  let pendingAttach;
  class Socket {
    static OPEN = 1;
    constructor() { this.readyState = 1; this.sent = []; sockets.push(this); queueMicrotask(() => this.onopen?.()); }
    send(raw) { this.sent.push(JSON.parse(raw)); }
    close() { this.onclose?.(); }
    message(value) { this.onmessage({ data: JSON.stringify(value) }); }
  }
  const chrome = {
    runtime: { id: 'owned', getURL: path => `chrome-extension://owned/${path}`, onMessage: { addListener: fn => handlers.message = fn } },
    tabs: {
      get: async id => ({ id, url: 'https://example.invalid/', title: `tab ${id}` }),
      query: async () => [1, 2].map(id => ({ id, url: 'https://example.invalid/' })),
      update: async () => {}, remove: async () => { throw new Error('Unexpected tab close'); },
      onRemoved: { addListener: fn => handlers.removed = fn },
    },
    debugger: {
      attach: async () => { if (pendingAttach) await pendingAttach; },
      detach: async ({ tabId }) => { detached.push(tabId); handlers.detach({ tabId }); },
      sendCommand: async ({ tabId }, method, params) => { commands.push({ tabId, method, params }); return { ok: true }; },
      onDetach: { addListener: fn => handlers.detach = fn },
    },
    action: { setBadgeText: async () => {} },
  };
  vm.runInNewContext(source, { chrome, WebSocket: Socket, URL, queueMicrotask,
    setInterval: fn => { const id = ++timerId; timers.set(id, fn); return id; },
    clearInterval: id => timers.delete(id),
  });
  const popup = { id: 'owned', url: 'chrome-extension://owned/popup.html' };
  async function message(value, sender = popup) {
    return new Promise(resolve => {
      if (!handlers.message(value, sender, result => resolve(JSON.parse(JSON.stringify(result))))) resolve(undefined);
    });
  }
  async function pair() {
    assert.deepEqual(await message({ kind: 'pair', endpoint: `ws://127.0.0.1:12345/${'a'.repeat(64)}`, tabs: [1] }), { ok: true });
    const socket = sockets.at(-1);
    socket.message({ id: 'resume', method: 'Echo.resume' });
    await tick();
    return socket;
  }
  return { handlers, commands, detached, sockets, pair, message, timers, deferAttach: value => pendingAttach = value };
}

test('only the popup can pair; non-loopback endpoints and empty selection fail', async () => {
  const f = fixture();
  assert.equal(await f.message({ kind: 'pair' }, { id: 'owned', url: 'https://example.invalid/' }), undefined);
  assert.match((await f.message({ kind: 'pair', endpoint: `ws://remote.invalid:12345/${'a'.repeat(64)}`, tabs: [1] })).error, /endpoint/);
  assert.match((await f.message({ kind: 'pair', endpoint: `ws://127.0.0.1:12345/${'a'.repeat(64)}`, tabs: [] })).error, /tabs/);
  assert.equal(f.sockets.length, 0);
});

test('only selected tabs and allowlisted CDP methods execute', async () => {
  const f = fixture(), s = await f.pair();
  s.message({ id: 'other', method: 'Input.insertText', targetId: '2', params: { text: 'no' } });
  s.message({ id: 'method', method: 'Browser.grantPermissions', targetId: '1' });
  s.message({ id: 'good', method: 'Input.insertText', targetId: '1', params: { text: '中文' } });
  await tick();
  assert.equal(f.commands.length, 1);
  assert.equal(f.commands[0].params.text, '中文');
  assert.match(s.sent.find(v => v.id === 'other').error, /authorized/);
  assert.match(s.sent.find(v => v.id === 'method').error, /protocol/);
});

test('pause during debugger attachment invalidates both active and queued input, then acknowledges detach', async () => {
  const f = fixture(), s = await f.pair();
  let resolveAttach;
  f.deferAttach(new Promise(resolve => resolveAttach = resolve));
  s.message({ id: 'first', method: 'Input.insertText', targetId: '1', params: { text: 'first' } });
  s.message({ id: 'queued', method: 'Input.insertText', targetId: '1', params: { text: 'second' } });
  await tick();
  s.message({ id: 'pause', method: 'Echo.pause' });
  resolveAttach();
  await tick();
  assert.equal(f.commands.length, 0);
  assert.deepEqual(f.detached, [1]);
  assert.equal(s.sent.find(v => v.id === 'pause').result.paused, true);
  s.message({ id: 'paused-status', method: 'Echo.tabs' });
  await tick();
  assert.equal(s.sent.find(v => v.id === 'paused-status').result.length, 1);
  assert.match(s.sent.find(v => v.id === 'queued').error, /paused/);
  s.message({ id: 'still-paused', method: 'Input.insertText', targetId: '1' });
  await tick();
  assert.equal(f.commands.length, 0);
  s.message({ id: 'again', method: 'Echo.resume' });
  await tick();
  s.message({ id: 'after-resume', method: 'Input.insertText', targetId: '1', params: { text: 'approved' } });
  await tick();
  assert.equal(f.commands.length, 1);
});

test('replacement pairing revokes old commands and disconnect preserves user tabs', async () => {
  const f = fixture(), old = await f.pair();
  await f.pair();
  old.message({ id: 'old-task', method: 'Input.insertText', targetId: '1' });
  await tick();
  assert.equal(f.commands.length, 0);
  assert.deepEqual(await f.message({ kind: 'disconnect' }), { ok: true });
});

test('idle pairing keeps its worker alive and stops heartbeat on disconnect', async () => {
  const f = fixture(), s = await f.pair();
  assert.equal(f.timers.size, 1);
  for (const callback of f.timers.values()) callback();
  assert.deepEqual(s.sent.at(-1), { heartbeat: true });
  await f.message({ kind: 'disconnect' });
  assert.equal(f.timers.size, 0);
});
