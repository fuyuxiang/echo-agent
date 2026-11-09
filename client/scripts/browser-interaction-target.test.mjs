import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { afterEach, describe, expect, it } from 'vitest';

const script = readFileSync(new URL('../src-tauri/src/automation/interaction-target.js', import.meta.url), 'utf8');
let dom;

function fixture(markup) {
  dom = new JSDOM(markup, { url: 'https://portal.example/forms/current', runScripts: 'outside-only' });
  const { window } = dom;
  const { document } = window;
  const sample = window.eval(`(${script})`);
  const control = document.querySelector('[data-echo-ref="send"]');
  window.CSS = { escape: value => value };
  control.getBoundingClientRect = () => ({ left: 10, top: 20, width: 40, height: 20 });
  document.elementFromPoint = () => control;
  return { document, control, sample: (input, point, guard) => sample(input, 'doc', false, false, point, guard) };
}

afterEach(() => dom?.window.close());

const click = { kind: 'click', reference: 'send' };
const enter = { kind: 'key', key: 'Enter' };

describe('approved browser interaction target', () => {
  it('checks a relative action including its query against the same submit control', () => {
    const { document, sample } = fixture('<form action="/approved?token=first"><button data-echo-ref="send">Submit</button></form>');
    const before = sample(click);
    const form = document.querySelector('form');
    expect(before.submission).toMatchObject({ action: 'https://portal.example/approved?token=first', method: 'get' });
    form.setAttribute('action', '/approved?token=second');
    expect(sample(click).submission).not.toEqual(before.submission);
    form.setAttribute('action', 'https://other.example/send');
    const after = sample(click);
    expect(after.target).toEqual(before.target);
    expect(after.submission.action).toBe('https://other.example/send');
  });

  it('binds Enter to the default submitter, including form ownership and overrides', () => {
    const { document, control, sample } = fixture(`
      <button form="profile" data-echo-ref="send" formaction="/override" formmethod="post">Submit</button>
      <form id="profile" action="/form-action"><input aria-label="Email"><button>Second</button></form>`);
    document.querySelector('input').focus();
    const before = sample(enter);
    expect(before.submission).toMatchObject({ action: 'https://portal.example/override', method: 'post' });
    control.removeAttribute('form');
    expect(sample(enter).submission).toMatchObject({ action: 'https://portal.example/form-action', method: 'get' });
    document.querySelector('#profile button').remove();
    expect(sample(enter).submission).toMatchObject({ action: 'https://portal.example/form-action', submitter: null });
    expect(sample({ kind: 'key', key: 'Tab' }).submission).toBeNull();
  });

  it('leaves unsafe network actions unapprovable and handles non-network dialog submits', () => {
    const { document, sample } = fixture('<form action="javascript:doSomething()"><button data-echo-ref="send">Submit</button></form>');
    expect(sample(click).submission).toMatchObject({ action: null, method: 'get' });
    document.querySelector('form').setAttribute('method', 'dialog');
    expect(sample(click).submission).toMatchObject({ action: null, method: 'dialog' });
  });

  it('checks native submit events if an input handler changes the destination after the last sample', () => {
    const { document, control, sample } = fixture('<form action="/approved"><button data-echo-ref="send">Submit</button></form>');
    const form = document.querySelector('form');
    const dispatch = () => form.dispatchEvent(new dom.window.SubmitEvent('submit', {
      bubbles: true, cancelable: true, submitter: control,
    }));
    const { submission } = sample(click);
    sample(click, null, submission);
    expect(dispatch()).toBe(true);
    form.addEventListener('submit', () => form.setAttribute('action', '/changed-by-handler'), { once: true });
    expect(dispatch()).toBe(false);
    expect(sample(click, null, 'finish')).toBe(true);
    expect(dispatch()).toBe(true);
  });

  it('validates the actual release point across a pressed-button layout change', () => {
    const { document, control, sample } = fixture('<button data-echo-ref="send">Press</button><button id="overlay">Other</button>');
    const before = sample(click);
    const point = { x: before.x, y: before.y };
    control.getBoundingClientRect = () => ({ left: 10, top: 21, width: 40, height: 20 });
    expect(sample(click).y).toBe(point.y + 1);
    expect(sample(click, point)).toEqual(before);
    document.elementFromPoint = () => document.getElementById('overlay');
    expect(sample(click, point)).toBeNull();
  });
});
