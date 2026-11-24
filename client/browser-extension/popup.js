const list = document.querySelector('#tabs');
const endpoint = document.querySelector('#endpoint');
const status = document.querySelector('#status');
let statusRevision = 0;

for (const tab of await chrome.tabs.query({ currentWindow: true })) {
  if (!/^https?:/.test(tab.url || '')) continue;
  const label = document.createElement('label');
  const input = document.createElement('input');
  input.type = 'checkbox';
  input.value = String(tab.id);
  input.checked = !!tab.active;
  label.append(input, document.createTextNode((tab.title || tab.url).slice(0, 100)));
  list.append(label);
}

async function updateStatus(message, success) {
  const revision = ++statusRevision;
  status.textContent = message.kind === 'pair' ? '正在连接所选标签页…' : '正在断开连接…';
  let text;
  try {
    const result = await chrome.runtime.sendMessage(message);
    text = result?.ok ? success : (result?.error || '浏览器扩展未返回确认');
  } catch {
    text = '浏览器扩展未响应，请重试';
  }
  if (revision === statusRevision) status.textContent = text;
}

document.querySelector('#connect').onclick = () => updateStatus({
  kind: 'pair',
  endpoint: endpoint.value,
  tabs: [...list.querySelectorAll('input:checked')].map(input => Number(input.value)),
}, '已连接；仅本次任务可控制所选标签页');

document.querySelector('#disconnect').onclick = () =>
  updateStatus({ kind: 'disconnect' }, '已断开，控制权已交回');
