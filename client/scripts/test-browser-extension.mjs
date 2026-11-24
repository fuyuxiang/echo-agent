// Exercise the shipped service worker with a fake Chrome API. No user tabs,
// browser profile or debugger connection is touched by these tests.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const source = readFileSync('browser-extension/background.js', 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
function fixture() {
  const handlers = {}, commands = [], detached = [], sockets = [], focused = [];
  const timers = new Map();
  let timerId = 0;
  let pendingAttach, pendingDetach;
  let autoOpen = true;
  const tabIds = new Set([1, 2]);
  class Socket {
    static OPEN = 1;
    constructor() {
      this.readyState = 0;
      this.sent = [];
      sockets.push(this);
      if (autoOpen) queueMicrotask(() => this.open());
    }
    open() { this.readyState = 1; this.onopen?.(); }
    send(raw) { this.sent.push(JSON.parse(raw)); }
    close() { this.readyState = 3; this.onclose?.(); }
    message(value) { this.onmessage({ data: JSON.stringify(value) }); }
  }
  const chrome = {
    runtime: { id: 'owned', getURL: path => `chrome-extension://owned/${path}`, onMessage: { addListener: fn => handlers.message = fn } },
    tabs: {
      get: async id => {
        if (!tabIds.has(id)) throw new Error(`No tab with id: ${id}`);
        return { id, url: 'https://example.invalid/', title: `tab ${id}` };
      },
      query: async () => [...tabIds].map(id => ({ id, url: 'https://example.invalid/' })),
      update: async id => ({ id, windowId: 7, url: 'https://example.invalid/' }),
      remove: async id => {
        handlers.detach({ tabId: id }, 'target_closed');
        tabIds.delete(id);
        handlers.removed(id);
      },
      onRemoved: { addListener: fn => handlers.removed = fn },
    },
    debugger: {
      attach: async () => { if (pendingAttach) await pendingAttach; },
      detach: async ({ tabId }) => {
        if (pendingDetach) await pendingDetach;
        detached.push(tabId);
        handlers.detach({ tabId });
      },
      sendCommand: async ({ tabId }, method, params) => { commands.push({ tabId, method, params }); return { ok: true }; },
      onDetach: { addListener: fn => handlers.detach = fn },
    },
    action: { setBadgeText: async () => {} },
    windows: { update: async (id, options) => { focused.push([id, options.focused]); } },
  };
  vm.runInNewContext(source, { chrome, WebSocket: Socket, URL, queueMicrotask,
    setInterval: fn => { const id = ++timerId; timers.set(id, fn); return id; },
    clearInterval: id => timers.delete(id), setTimeout, clearTimeout,
  });
  const popup = { id: 'owned', url: 'chrome-extension://owned/popup.html' };
  async function message(value, sender = popup) {
    return new Promise(resolve => {
      if (!handlers.message(value, sender, result => resolve(JSON.parse(JSON.stringify(result))))) resolve(undefined);
    });
  }
  const requestPair = (tabs = [1]) => message({ kind: 'pair', endpoint: `ws://127.0.0.1:12345/${'a'.repeat(64)}`, tabs });
  async function pair(tabs) {
    assert.deepEqual(await requestPair(tabs), { ok: true });
    const socket = sockets.at(-1);
    socket.message({ id: 'resume', method: 'Echo.resume' });
    await tick();
    return socket;
  }
  return { handlers, commands, detached, sockets, pair, requestPair, message, timers, focused,
    deferAttach: value => pendingAttach = value,
    deferDetach: value => pendingDetach = value,
    deferOpen: () => { autoOpen = false; },
  };
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

test('a paused task can reveal only its authorized tab without attaching a debugger', async () => {
  const f = fixture(), s = await f.pair();
  s.message({ id: 'pause', method: 'Echo.pause' });
  await tick();
  s.message({ id: 'wrong', method: 'Echo.focusTaskTab', targetId: '2' });
  s.message({ id: 'reveal', method: 'Echo.focusTaskTab', targetId: '1' });
  s.message({ id: 'type', method: 'Input.insertText', targetId: '1', params: { text: 'blocked' } });
  await tick();
  assert.match(s.sent.find(value => value.id === 'wrong').error, /authorized/);
  assert.equal(s.sent.find(value => value.id === 'reveal').result.id, '1');
  assert.match(s.sent.find(value => value.id === 'type').error, /paused/);
  assert.deepEqual(f.focused, [[7, true]]);
  assert.equal(f.commands.length, 0);
});

test('replacement pairing revokes old commands and disconnect preserves user tabs', async () => {
  const f = fixture(), old = await f.pair();
  await f.pair();
  old.message({ id: 'old-task', method: 'Input.insertText', targetId: '1' });
  await tick();
  assert.equal(f.commands.length, 0);
  assert.deepEqual(await f.message({ kind: 'disconnect' }), { ok: true });
});

test('closing a paired debugged tab acknowledges the close and keeps other tabs paired', async () => {
  const f = fixture(), s = await f.pair([1, 2]);
  s.message({ id: 'attach', method: 'Runtime.evaluate', targetId: '1' });
  s.message({ id: 'close', method: 'Echo.closeTab', targetId: '1' });
  s.message({ id: 'tabs', method: 'Echo.tabs' });
  s.message({ id: 'remaining', method: 'Runtime.evaluate', targetId: '2' });
  await tick();
  assert.deepEqual(s.sent.find(value => value.id === 'close')?.result, { closed: '1' });
  assert.deepEqual(s.sent.find(value => value.id === 'tabs')?.result.map(tab => tab.id), ['2']);
  assert.equal(s.sent.find(value => value.id === 'remaining')?.result.ok, true);
  assert.equal(s.readyState, 1);
});

test('unsolicited debugger detach still invalidates the pairing', async () => {
  for (const reason of ['canceled_by_user', 'target_closed']) {
    const f = fixture(), s = await f.pair([1, 2]);
    for (const targetId of ['1', '2']) s.message({ id: targetId, method: 'Runtime.evaluate', targetId });
    await tick();
    f.handlers.detach({ tabId: 1 }, reason);
    await tick();
    assert.equal(s.readyState, 3);
    assert.deepEqual(f.detached, [2]);
  }
});

test('a closed tab in the replacement leaves the current pairing connected', async () => {
  const f = fixture(), current = await f.pair();
  assert.match((await f.requestPair([1, 3])).error, /标签页已关闭/);
  assert.equal(f.sockets.length, 1);
  assert.equal(current.readyState, 1);
  assert.equal(f.timers.size, 1);
  current.message({ id: 'tabs-after-failed-repair', method: 'Echo.tabs' });
  await tick();
  assert.deepEqual(current.sent.at(-1).result.map(tab => tab.id), ['1']);
});

test('idle pairing keeps its worker alive and stops heartbeat on disconnect', async () => {
  const f = fixture(), s = await f.pair();
  assert.equal(f.timers.size, 1);
  for (const callback of f.timers.values()) callback();
  assert.deepEqual(s.sent.at(-1), { heartbeat: true });
  await f.message({ kind: 'disconnect' });
  assert.equal(f.timers.size, 0);
});

test('latest pause wins over an earlier resume and acknowledges completed detach', async () => {
  const f = fixture(), s = await f.pair();
  s.message({ id: 'attach', method: 'Runtime.evaluate', targetId: '1' });
  await tick();
  let resolveDetach;
  f.deferDetach(new Promise(resolve => resolveDetach = resolve));
  s.message({ id: 'pause-first', method: 'Echo.pause' });
  s.message({ id: 'resume-stale', method: 'Echo.resume' });
  s.message({ id: 'pause-latest', method: 'Echo.pause' });
  await tick();
  assert.equal(s.sent.some(value => value.id === 'pause-latest'), false, 'pause acknowledged before detach');
  resolveDetach();
  await tick();
  assert.deepEqual(f.detached, [1]);
  assert.equal(s.sent.find(value => value.id === 'pause-latest').result.paused, true);
  assert.match(s.sent.find(value => value.id === 'resume-stale').error, /paused/);
  s.message({ id: 'while-paused', method: 'Runtime.evaluate', targetId: '1' });
  await tick();
  assert.equal(f.commands.length, 1);
});

test('a stale connection cannot pause its replacement', async () => {
  const f = fixture(), old = await f.pair(), current = await f.pair();
  old.message({ id: 'old-pause', method: 'Echo.pause' });
  current.message({ id: 'current-observation', method: 'Runtime.evaluate', targetId: '1' });
  await tick();
  assert.equal(current.sent.find(value => value.id === 'current-observation').result?.ok, true);
  assert.equal(f.commands.length, 1);
});

test('closing during pairing resolves the old popup and preserves the replacement', async () => {
  const f = fixture();
  f.deferOpen();
  let firstResult;
  void f.requestPair().then(result => { firstResult = result; });
  await tick();
  assert.equal(f.sockets.length, 1);
  const second = f.requestPair();
  await tick();
  assert.equal(f.sockets.length, 2);
  assert.equal(f.sockets[0].readyState, 3);
  assert.match(firstResult?.error || '', /closed|replaced/i);
  const current = f.sockets[1];
  current.open();
  assert.deepEqual(await second, { ok: true });
  assert.equal(f.timers.size, 1);
});

test('the popup keeps the newest pairing result when an older request resolves late', async () => {
  const popupSource = readFileSync('browser-extension/popup.js', 'utf8');
  const buttons = { '#connect': {}, '#disconnect': {} };
  const pending = [];
  const status = { textContent: '' };
  const elements = {
    ...buttons,
    '#tabs': { querySelectorAll: () => [{ value: '1' }] },
    '#endpoint': { value: 'ws://127.0.0.1:12345/test-fixture' },
    '#status': status,
  };
  await vm.runInNewContext(`(async () => { ${popupSource} })()`, {
    document: { querySelector: selector => elements[selector] },
    chrome: { tabs: { query: async () => [] }, runtime: {
      sendMessage: () => new Promise(resolve => pending.push(resolve)),
    } },
  });
  const pairing = buttons['#connect'].onclick();
  const disconnect = buttons['#disconnect'].onclick();
  pending[1]({ ok: true });
  await disconnect;
  pending[0]({ error: 'old pairing was replaced' });
  await pairing;
  assert.equal(status.textContent, '已断开，控制权已交回');
});
