import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const script = readFileSync(new URL("../src-tauri/src/automation/native_panel/artifact_editor.js", import.meta.url), "utf8");

function select(t, source, selector) {
  const { window } = new JSDOM(source, { runScripts: "outside-only" });
  t.after(() => window.close());
  const edit = window.eval(`(${script})`);
  edit({ kind: "start" });
  const element = window.document.querySelector(selector);
  element.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  return { element, edit, token: edit({ kind: "selection" }).token };
}

test("HTML text edits preserve every byte outside the selected text range", t => {
  const source = "<!doctype html>\r\n<title>Test</title>\r\n<p class='hero'>Before</p>\r\n<pre>\n\nTwo lines start here</pre>\r\n";
  const { edit, token } = select(t, source, "p");
  assert.equal(edit({ kind: "apply", source, token, change: { text: "After & more" } }),
    source.replace("Before", "After &amp; more"));
});

test("duplicate elements and implicit table sections still select the correct source element", t => {
  const source = "<table><tr><td>Same</td><td>Same</td></tr></table>";
  const { edit, token } = select(t, source, "td:nth-child(2)");
  assert.equal(edit({ kind: "apply", source, token, change: { text: "Updated" } }),
    "<table><tr><td>Same</td><td>Updated</td></tr></table>");
});

test("appearance changes preserve the selected element's descendants and neighboring markup", t => {
  const source = "<p class='hero'><strong data-x='z'>Keep</strong></p>\n<!--keep this too-->";
  const { edit, token } = select(t, source, "p");
  assert.equal(edit({ kind: "apply", source, token, change: { color: "#123456" } }),
    '<p class="hero" style="color: rgb(18, 52, 86);"><strong data-x=\'z\'>Keep</strong></p>\n<!--keep this too-->');
});

test("changed source and ambiguous implicit closing tags leave the source untouched", t => {
  const source = "<p>First<p>Second";
  const { edit, token } = select(t, source, "p");
  assert.match(edit({ kind: "apply", source, token, change: { text: "Changed" } }).error, /源码范围/);
  assert.match(edit({ kind: "apply", source: source.replace("First", "External"), token,
    change: { color: "#123456" } }).error, /源码已变化/);
});
