// Execute the production Rust-embedded DOM snapshot in jsdom. This verifies
// label semantics; it does not certify CDP, native approvals or real Chrome.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { JSDOM } from 'jsdom';
const script = readFileSync('src-tauri/src/automation/browser-snapshot.js','utf8');
const deepQuery=readFileSync('src-tauri/src/automation/deep-query.js','utf8');
function inPage(html, run) {
  const dom = new JSDOM(html, { url: 'https://example.invalid/owned', runScripts: 'outside-only' });
  dom.window.HTMLElement.prototype.getBoundingClientRect = () => ({ x: 0, y: 0, left: 0, top: 0, right: 100, bottom: 30, width: 100, height: 30 });
  dom.window.HTMLElement.prototype.scrollIntoView = () => {};
  // jsdom supplies textContent while a rendered browser additionally has innerText.
  Object.defineProperty(dom.window.HTMLElement.prototype, 'innerText', { get() { return this.textContent; } });
  dom.window.__echoQueryAll=dom.window.eval(`(${deepQuery})`);
  try { return run(dom.window); } finally { dom.window.close(); }
}
const snapshot = html => inPage(html, window => JSON.parse(JSON.stringify(window.eval(script))));
test('associated for label names a Chinese input and remains stable after typing', () => {
  for (const value of ['', 'private entered text']) {
    const result = snapshot(`<label for="review-text">本次验收文本</label><input id="review-text" value="${value}">`);
    assert.equal(result.elements[0].name, '本次验收文本');
    assert.equal(result.elements[0].tag, 'input');
  }
});
test('wrapped labels name textarea and select controls', () => {
  const result = snapshot('<label>会议说明<textarea></textarea></label><label>保存范围<select><option>个人</option></select></label>');
  assert.equal(result.elements[0].name, '会议说明');
  assert.equal(result.elements[1].name, '保存范围个人');
});
test('aria-labelledby supports multiple ids and takes precedence over aria-label', () => {
  const result = snapshot('<span id="a">审批</span><span id="b">目标</span><input aria-labelledby="missing a b" aria-label="fallback">');
  assert.equal(result.elements[0].name, '审批 目标');
});
test('existing aria-label, button text and password protections remain intact', () => {
  const result = snapshot('<label for="x">implicit</label><input id="x" aria-label="explicit"><button>确认发送</button><label for="secret">密码</label><input id="secret" type="password" value="never-leak-password">');
  assert.equal(result.elements[0].name, 'explicit');
  assert.equal(result.elements[1].name, '确认发送');
  assert.equal(result.elements[2].type, 'password');
  assert.equal(result.elements[2].value, undefined);
  assert(!JSON.stringify(result).includes('never-leak-password'));
});
test('unrelated and hidden labels do not invent interactive elements', () => {
  const result = snapshot('<label for="missing">unused</label><input aria-label="visible"><input style="display:none" aria-label="hidden">');
  assert.equal(result.elements.length, 1);
  assert.equal(result.elements[0].name, 'visible');
});

test('shadow labels use their own root for snapshots and consequential interaction previews', () => {
  inPage('<span id="send">删除账户</span><div id="host"></div>', window => {
    const host = window.document.getElementById('host');
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = '<span id="recipient">收件人</span><input aria-labelledby="recipient"><span id="send">发送消息</span><span id="now">立即</span><button aria-labelledby="send now"></button><input aria-labelledby="outside" aria-label="本地后备">';
    const observed = window.eval(script);
    assert.deepEqual(Array.from(observed.elements, e => e.name), ['收件人', '发送消息 立即', '本地后备']);
    const button = shadow.querySelector('button');
    // jsdom does not implement CSS.escape; production refs here are simple eN.
    window.CSS = {escape: value => { assert.match(value, /^e\d+$/); return value; }};
    window.document.elementFromPoint = () => host;
    shadow.elementFromPoint = () => button;
    const interact = window.eval(`(${readFileSync('src-tauri/src/automation/interaction-target.js', 'utf8')})`);
    const preview = interact({ kind: 'click', reference: button.dataset.echoRef }, 'owned-document', false, false, null, null);
    assert.equal(preview.target.preview.name, '发送消息 立即');
    assert(!JSON.stringify(preview).includes('删除账户'));
  });
});

test('nested open shadow roots keep identical label IDs isolated', () => {
  inPage('<div id="host"></div>', window => {
    const outer = window.document.querySelector('#host').attachShadow({mode:'open'});
    outer.innerHTML = '<span id="name">外层输入</span><input aria-labelledby="name"><div id="inner"></div>';
    outer.querySelector('#inner').attachShadow({mode:'open'}).innerHTML = '<span id="name">内层输入</span><input aria-labelledby="name">';
    assert.deepEqual(Array.from(window.eval(script).elements, e => e.name), ['外层输入', '内层输入']);
  });
});
