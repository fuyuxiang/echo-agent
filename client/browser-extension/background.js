// The popup explicitly selects the tabs for one task. Websites cannot pair.
let socket = null;
let allowed = new Set();
const attached = new Set();
const detaching = new Set();
let closingTab = null;
let serial = Promise.resolve();
let pauseBarrier = Promise.resolve();
let epoch = 0;
let pairingRevision = 0;
let paused = true;
let heartbeat;
const pairingTimeoutMs = 10_000;

const descriptor = tab => ({
  id: String(tab.id), title: tab.title || '', url: tab.url || '',
  type: 'page', webSocketDebuggerUrl: `extension:${tab.id}`,
});
const methods = new Set([
  'Page.navigate', 'Page.getFrameTree', 'Page.createIsolatedWorld',
  'Page.captureScreenshot', 'Runtime.evaluate', 'Input.dispatchMouseEvent',
  'Input.dispatchKeyEvent', 'Input.insertText', 'DOM.getFrameOwner',
  'DOM.getBoxModel', 'DOM.resolveNode', 'Runtime.callFunctionOn', 'Runtime.releaseObject', 'DOM.getDocument', 'DOM.querySelector', 'DOM.setFileInputFiles',
]);
const validUrl = value => /^(https?:|about:blank$)/.test(value || '');

async function detachDebuggers() {
  const previous = [...attached];
  attached.clear();
  for (const tabId of previous) {
    detaching.add(tabId);
    try { await chrome.debugger.detach({ tabId }); } catch { /* Already detached. */ }
    finally { detaching.delete(tabId); }
  }
}
async function pause() {
  paused = true;
  epoch++;
  // Already admitted CDP commands finish once. Queued commands see the new
  // epoch and cannot run. The host receives an acknowledgement after detach.
  pauseBarrier = Promise.all([pauseBarrier, serial]).then(detachDebuggers);
  await pauseBarrier;
  return { paused: true };
}
async function disconnect() {
  clearInterval(heartbeat);
  heartbeat = undefined;
  const previous = socket;
  socket = null;
  allowed.clear();
  const detached = pause();
  previous?.close();
  await Promise.all([detached, showConnected(false)]);
}
async function showConnected(connected) {
  try { await chrome.action.setBadgeText({ text: connected ? 'ON' : '' }); }
  catch { /* Badge feedback does not control connection lifetime. */ }
}
function waitForOpen(connection) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('Pairing timed out')), pairingTimeoutMs);
    const fail = message => {
      clearTimeout(timeout);
      reject(new Error(message));
    };
    connection.onopen = () => {
      clearTimeout(timeout);
      resolve();
    };
    connection.onerror = () => fail('Connection failed or pairing expired');
    connection.onclose = () => {
      fail('Pairing connection closed');
      if (socket === connection) void disconnect();
    };
  });
}
function guard(connection, generation, allowPaused = false) {
  if (socket !== connection || (!allowPaused && paused) || epoch !== generation) {
    throw new Error('Task paused or connection replaced; observe before retry');
  }
}
function authorizedTab(raw) {
  const id = Number(raw);
  if (!Number.isSafeInteger(id) || !allowed.has(id)) {
    throw new Error('Tab is not authorized for this task');
  }
  return id;
}
async function execute(command, connection, generation) {
  const { method, params = {}, targetId } = command;
  if (method === 'Echo.tabs') {
    guard(connection, generation, true);
    const tabs = await chrome.tabs.query({});
    guard(connection, generation, true);
    return tabs.filter(tab => allowed.has(tab.id)).map(descriptor);
  }
  if (method === 'Echo.focusTaskTab') {
    guard(connection, generation, true);
    const tab = await chrome.tabs.update(authorizedTab(targetId), { active: true });
    guard(connection, generation, true);
    await chrome.windows.update(tab.windowId, { focused: true });
    return descriptor(tab);
  }
  if (method === 'Echo.newTab') {
    guard(connection, generation);
    if (!validUrl(params.url)) throw new Error('Unsupported URL');
    const tab = await chrome.tabs.create({ url: params.url });
    guard(connection, generation);
    allowed.add(tab.id);
    return descriptor(tab);
  }
  guard(connection, generation);
  const id = authorizedTab(targetId);
  if (method === 'Echo.selectTab') {
    await chrome.tabs.update(id, { active: true });
    guard(connection, generation);
    return descriptor(await chrome.tabs.get(id));
  }
  if (method === 'Echo.closeTab') {
    closingTab = id;
    try {
      await chrome.tabs.remove(id);
      allowed.delete(id);
      return { closed: String(id) };
    } finally {
      closingTab = null;
    }
  }
  if (!methods.has(method)) throw new Error('CDP method is outside the desktop control protocol');
  if (method === 'Page.navigate' && !validUrl(params.url)) throw new Error('Unsupported navigation');
  if (!attached.has(id)) {
    await chrome.debugger.attach({ tabId: id }, '1.3');
    attached.add(id);
  }
  // attach is asynchronous: takeover during it must not dispatch the input.
  guard(connection, generation);
  return chrome.debugger.sendCommand({ tabId: id }, method, params);
}
function reply(connection, command, result, error) {
  if (socket !== connection || connection.readyState !== WebSocket.OPEN) return;
  connection.send(JSON.stringify({ id: command?.id, ...(error ? { error: String(error.message || error) } : { result }) }));
}
function receive(connection, event) {
  if (socket !== connection) return;
  let command;
  try {
    if (typeof event.data !== 'string' || event.data.length > 2 * 1024 * 1024) throw new Error('Invalid command');
    command = JSON.parse(event.data);
    if (typeof command?.method !== 'string') throw new Error('Invalid command');
  } catch (error) { reply(connection, command, null, error); return; }
  if (command.method === 'Echo.pause') {
    void pause().then(result => reply(connection, command, result), error => reply(connection, command, null, error));
    return;
  }
  const generation = epoch;
  if (command.method === 'Echo.resume') {
    void pauseBarrier.then(() => {
      try {
        guard(connection, generation, true);
        paused = false;
        reply(connection, command, { resumed: true });
      } catch (error) {
        reply(connection, command, null, error);
      }
    });
    return;
  }
  serial = serial.then(async () => {
    try { reply(connection, command, await execute(command, connection, generation)); }
    catch (error) { reply(connection, command, null, error); }
  });
}

chrome.debugger.onDetach.addListener(({ tabId }, reason) => {
  attached.delete(tabId);
  // Chrome can report the close before the pending tabs.remove resolves.
  if (closingTab === tabId && reason === 'target_closed') return;
  if (!detaching.has(tabId) && allowed.has(tabId)) void disconnect();
});
chrome.tabs.onRemoved.addListener(id => { allowed.delete(id); attached.delete(id); });
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  if (sender.id !== chrome.runtime.id || sender.url !== chrome.runtime.getURL('popup.html')) return;
  if (message?.kind === 'disconnect') {
    pairingRevision++;
    void disconnect().then(() => respond({ ok: true }));
    return true;
  }
  if (message?.kind !== 'pair') return;
  void (async () => {
    let connection;
    let switching = false;
    try {
      const endpoint = new URL(message.endpoint);
      if (endpoint.protocol !== 'ws:' || endpoint.hostname !== '127.0.0.1' || !endpoint.port ||
          endpoint.username || endpoint.password || endpoint.search || endpoint.hash ||
          !/^\/[a-f0-9]{64}$/.test(endpoint.pathname)) throw new Error('Invalid pairing endpoint');
      if (!Array.isArray(message.tabs) || !message.tabs.length || message.tabs.length > 64 ||
          !message.tabs.every(Number.isSafeInteger)) throw new Error('Choose one to 64 tabs');
      const revision = ++pairingRevision;
      const tabs = await Promise.all(message.tabs.map(async id => {
        try { return await chrome.tabs.get(id); }
        catch { throw new Error('所选标签页已关闭或不可访问，请重新选择'); }
      }));
      if (!tabs.every(tab => /^https?:/.test(tab.url || ''))) throw new Error('只能连接 HTTP 或 HTTPS 标签页，请重新选择');
      if (revision !== pairingRevision) throw new Error('Pairing request replaced or cancelled');
      switching = true;
      await disconnect();
      if (revision !== pairingRevision) throw new Error('Pairing request replaced or cancelled');
      connection = new WebSocket(endpoint);
      socket = connection;
      allowed = new Set(tabs.map(tab => tab.id));
      connection.onmessage = event => receive(connection, event);
      await waitForOpen(connection);
      if (socket !== connection) throw new Error('Pairing connection closed or replaced');
      // Chrome 116+ service workers need WebSocket traffic within their 30s
      // idle window, including while the model is thinking or a task is paused.
      // https://developer.chrome.com/docs/extensions/how-to/web-platform/websockets
      heartbeat = setInterval(() => {
        if (socket === connection && connection.readyState === WebSocket.OPEN) {
          connection.send(JSON.stringify({ heartbeat: true }));
        }
      }, 20_000);
      await showConnected(true);
      if (socket !== connection) throw new Error('Pairing connection closed or replaced');
      respond({ ok: true });
    } catch (error) {
      if (connection && socket === connection) await disconnect();
      const retained = !switching && socket?.readyState === WebSocket.OPEN;
      const detail = String(error.message || error);
      respond({ error: retained ? `${detail}。原连接仍可用` : detail });
    }
  })();
  return true;
});
