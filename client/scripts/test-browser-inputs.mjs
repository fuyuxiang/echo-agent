// Exercise the production DOM programs in real Chromium. Native approval
// admission and target revalidation remain covered by Rust/Native tests.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { reviewChromium } from "./ui-review-runtime.mjs";

const source = await readFile("src-tauri/src/automation/browser.rs", "utf8");
const editableText = await readFile("src-tauri/src/automation/editable-text.js", "utf8");
const interactionTarget = await readFile("src-tauri/src/automation/interaction-target.js", "utf8");
function programs(method, next) {
  const body = source.slice(source.indexOf("pub async fn " + method), source.indexOf("pub async fn " + next));
  return [...body.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map(m => {
    try { return JSON.parse('"' + m[1] + '"'); } catch { return ""; }
  }).filter(s => s.startsWith("(() =>"));
}
function program(source, values, selection = "") {
  let result = source.replace(" {} return ", " " + selection + " return ");
  result = result.replaceAll("{EDITABLE_TEXT_JS}", editableText);
  for (const [key, value] of Object.entries(values)) result = result.replaceAll("{" + key + "}", value);
  return result.replaceAll("{{", "{").replaceAll("}}", "}");
}
const [focus, native, readback] = programs("type_text", "select");
const [select] = programs("select", "upload");
assert(focus && native && readback && select, "Production DOM programs must be available");
const browser = await (await reviewChromium()).launch({
  headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
});
try {
  const page = await browser.newPage();
  const errors = []; page.on("pageerror", e => errors.push(e.message));
  await page.setContent('<input data-echo-ref="date" type="date" min="2026-01-01" max="2026-12-31"><input data-echo-ref="time" type="time"><input data-echo-ref="number" type="number" min="0" step="1"><input data-echo-ref="text"><input data-echo-ref="secret" type="password" value="never-leak"><input data-echo-ref="readonly" readonly value="original"><select data-echo-ref="choice"><option value="a">甲</option><option value="b">乙</option><option disabled value="c">不可用</option></select>');
  const variables = ref => ({ reference: JSON.stringify(ref) });
  for (const [ref, type, value] of [["date", "date", "2026-10-23"], ["time", "time", "09:30"], ["number", "number", "42"]]) {
    const state = await page.evaluate(program(focus, variables(ref), "e.select();"));
    assert.equal(state.type, type);
    await page.evaluate(program(native, { ...variables(ref), value: JSON.stringify(value), "": JSON.stringify(type) }));
    const actual = await page.evaluate(program(readback, variables(ref)));
    assert.deepEqual(actual, { value, valid: true });
  }
  for (const value of ["2026-02-30", "2027-01-01", "10/23/2026"]) {
    await assert.rejects(page.evaluate(program(native, { ...variables("date"), value: JSON.stringify(value), "": '"date"' })));
    assert.equal((await page.evaluate(program(readback, variables("date")))).value, "2026-10-23");
  }
  assert.deepEqual(await page.evaluate(program(focus, variables("secret"))), { type: "password" });
  assert.deepEqual(await page.evaluate(program(focus, variables("readonly"))), { disabled: true });
  assert.equal(await page.locator('[data-echo-ref="readonly"]').inputValue(), "original");
  await page.evaluate(program(focus, variables("text"), "e.select();"));
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.insertText", { text: "实际输入" });
  assert.deepEqual(await page.evaluate(program(readback, variables("text"))), { value: "实际输入", valid: true });
  const selectContents = "const s=getSelection(),r=document.createRange();r.selectNodeContents(e);s.removeAllRanges();s.addRange(r);";
  for (const value of ["第一行\n第二行", "第一行\n", "第一行\n\n第二行", "\n", "a\r\nb", " leading  spaces ", " 第一行  \n 第二行 ", "a\u00a0b"]) {
    await page.setContent('<div data-echo-ref="editor" contenteditable></div>');
    await page.evaluate(program(focus, variables("editor"), selectContents));
    await cdp.send("Input.insertText", { text: value });
    assert.deepEqual(await page.evaluate(program(readback, variables("editor"))),
      { value: value.replace(/\r\n?/g, "\n").replace(/\u00a0/g, " "), valid: true }, "rich editor: " + JSON.stringify(value));
    await page.evaluate(program(focus, variables("editor"), selectContents));
    await page.keyboard.press("Backspace");
    assert.deepEqual(await page.evaluate(program(readback, variables("editor"))), { value: "", valid: true });
  }
  await page.setContent('<div data-echo-ref="editor" contenteditable><p>第一行<br>第二行</p><p><br></p><p>第三行</p></div>');
  assert.equal((await page.evaluate(program(readback, variables("editor")))).value, "第一行\n第二行\n\n第三行");
  // Restore the select fixture after the rich-editor cases.
  await page.setContent('<select data-echo-ref="choice"><option value="a">甲</option><option value="b">乙</option><option disabled value="c">不可用</option></select>');
  const picked = await page.evaluate(program(select, { ...variables("choice"), values_json: '["乙"]' }));
  assert.equal(picked.verified, true); assert.equal(picked.selected[0].value, "b");
  for (const values of ['["missing"]', '["c"]', '["a","b"]']) {
    assert((await page.evaluate(program(select, { ...variables("choice"), values_json: values }))).error);
    assert.equal(await page.locator('[data-echo-ref="choice"]').inputValue(), "b");
  }
  await page.setContent('<form><input name="name" data-echo-ref="name" required><input name="date" data-echo-ref="date" type="date" required><select name="room" data-echo-ref="room" required><option value="">请选择</option><option value="b">乙会议室</option></select><label><input type="checkbox" name="simulation" required>模拟确认</label><button type="submit">保存</button></form>');
  await page.evaluate(() => {
    window.submissions = 0;
    document.querySelector("form").addEventListener("submit", event => {
      event.preventDefault(); window.submissions++;
      const result = Object.fromEntries(new FormData(event.target));
      const link = document.createElement("a");
      link.href = URL.createObjectURL(new Blob([JSON.stringify(result)], { type: "application/json" }));
      link.download = "form_result.json"; link.click();
    });
  });
  await page.getByRole("button", { name: "保存" }).click();
  assert.equal(await page.evaluate(() => window.submissions), 0, "HTML validity must block incomplete submission");
  await page.evaluate(program(focus, variables("name"), "e.select();"));
  await cdp.send("Input.insertText", { text: "张三" });
  await page.evaluate(program(native, { ...variables("date"), value: '"2026-10-23"', "": '"date"' }));
  assert.equal((await page.evaluate(program(select, { ...variables("room"), values_json: '["b"]' }))).verified, true);
  await page.getByLabel("模拟确认").click();
  const saved = page.waitForEvent("download");
  await page.getByRole("button", { name: "保存" }).click();
  const download = await saved;
  assert.equal(download.suggestedFilename(), "form_result.json");
  assert.deepEqual(JSON.parse(await readFile(await download.path(), "utf8")), { name: "张三", date: "2026-10-23", room: "b", simulation: "on" });
  assert.equal(await page.evaluate(() => window.submissions), 1);

  await page.setContent(`<style>button { width:120px;height:40px } button:active { transform:translateY(1px) }</style>
    <form action="https://portal.example/approved"><input><button data-echo-ref="submit">提交</button></form>`);
  const sample = (input, point = null, guard = null) => page.evaluate(`(${interactionTarget})(${JSON.stringify(input)},"test-document",false,false,${JSON.stringify(point)},${JSON.stringify(guard)})`);
  const clickTarget = { kind: "click", reference: "submit", button: "left" };
  const before = await sample(clickTarget);
  await page.evaluate(() => {
    const form = document.querySelector("form");
    form.addEventListener("submit", event => { event.preventDefault(); form.dataset.submitted = "yes"; });
    form.setAttribute("action", "https://different.example/submit");
    form.querySelector("input").focus();
  });
  assert.notDeepEqual((await sample(clickTarget)).submission, before.submission);
  assert.equal((await sample({ kind: "key", key: "Enter" })).submission.action, "https://different.example/submit");
  await page.locator("form").evaluate(form => form.setAttribute("action", "https://portal.example/approved"));
  const point = { x: before.x, y: before.y };
  await page.mouse.move(point.x, point.y);
  await page.mouse.down();
  assert.equal((await sample(clickTarget)).y, point.y + 1);
  assert.deepEqual(await sample(clickTarget, point), before);
  await page.mouse.up();
  assert.equal(await page.locator("form").getAttribute("data-submitted"), "yes");
  await page.locator("button").evaluate(button => {
    button.addEventListener("click", () => button.form.setAttribute("action", "https://different.example/submit"), { once: true });
  });
  await sample(clickTarget, point, before.submission);
  await page.mouse.click(point.x, point.y);
  assert.equal(await sample(clickTarget, point, "finish"), true, "submission changed inside the click handler must be blocked");
  assert.deepEqual(errors, []);
  console.log("Real Chromium passed: native inputs, verified selection, validated form download, changed submission destination and pressed-button hit testing.");
} finally { await browser.close(); }
