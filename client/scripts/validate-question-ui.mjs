import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-question-ui-")));
console.log(`Question UI evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&state=question`;
const evidence = [];
const maxFont = 16 * 18 / 13;
let browser;

async function inspect(page, name, coding = false) {
  const geometry = await page.locator(".question-inline").evaluate((card, coding) => {
    const rect = element => {
      const { x, y, right, bottom, width, height } = element.getBoundingClientRect();
      return { x, y, right, bottom, width, height };
    };
    const body = card.querySelector(".question-inline__body");
    return {
      card: rect(card), body: rect(body), header: rect(card.querySelector(".question-inline__head")),
      footer: rect(card.querySelector(".question-inline__footer")),
      composer: rect(document.querySelector(coding ? ".echo-theia-agent__composer" : ".echo-composer")),
      sessionHeader: rect(document.querySelector(coding ? ".coding-workbench__topbar" : ".session-header")),
      interaction: coding ? rect(card.closest(".coding-agent__interaction")) : null,
      overflow: document.documentElement.scrollWidth > innerWidth + 1 || card.scrollWidth > card.clientWidth + 1 || body.scrollWidth > body.clientWidth + 1,
      options: [...card.querySelectorAll(".question-inline__option")].map(rect),
      actions: [...card.querySelectorAll(".question-inline__actions button")].map(rect),
      headerActions: [...card.querySelectorAll(".question-inline__head button")].map(element => ({ label: element.getAttribute("aria-label"), ...rect(element) })),
      progress: [...card.querySelectorAll(".question-inline__timeout, .question-inline__navigation")].map(rect),
    };
  }, coding);
  const { card, body, header, footer, composer, sessionHeader, options, actions } = geometry;
  const context = `${name}: ${JSON.stringify(geometry)}`;
  assert.ok(!geometry.overflow, `horizontal overflow: ${context}`);
  if (coding) {
    const { interaction } = geometry;
    assert.ok(card.y >= interaction.y && card.bottom <= interaction.bottom + 1, `card clipped by the coding interaction: ${context}`);
    assert.ok(body.height >= 120, `coding question must leave readable body space: ${context}`);
  } else {
    assert.ok(Math.abs(card.x - composer.x) < 1 && Math.abs(card.right - composer.right) < 1, `card must align with composer: ${context}`);
    assert.ok(composer.y - card.bottom >= 8 && composer.y - card.bottom <= 16, `inconsistent card/composer gap: ${context}`);
  }
  assert.ok(card.y >= sessionHeader.bottom - 1 && card.bottom < composer.y, `card clips the session header or composer: ${context}`);
  assert.ok(header.bottom <= body.y + 1 && body.bottom <= footer.y + 1, `body overlaps fixed header/footer: ${context}`);
  assert.ok(actions.every(action => action.x >= card.x && action.right <= card.right && action.bottom <= card.bottom && action.height >= 32), `actions clipped or too small: ${context}`);
  assert.ok(geometry.headerActions.every(action => action.x >= card.x && action.right <= card.right && action.y >= header.y && action.bottom <= header.bottom), `header actions clipped: ${context}`);
  if (geometry.progress.length === 2) assert.ok(geometry.progress[0].right + 4 <= geometry.progress[1].x, `countdown overlaps navigation: ${context}`);
  assert.ok(options.every((option, index) => Math.abs(option.width - options[0].width) < 1 && (index === 0 || option.y >= options[index - 1].bottom)), `options must form full-width rows: ${context}`);
  evidence.push({ name, ...geometry });
  await page.screenshot({ path: join(output, `${name}.png`) });
}

try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  page.setDefaultNavigationTimeout(60000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  for (const [width, height] of [[1920, 1080], [1280, 800], [1024, 680], [768, 680]]) {
    for (const theme of ["light", "dark"]) for (const font of [16, maxFont]) {
      await page.setViewportSize({ width, height });
      await page.goto(`${base}&theme=${theme}`);
      const card = page.locator(".question-inline");
      await card.waitFor();
      await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      assert.equal(await card.locator("h3").count(), 1, "only the current question should be rendered");
      assert.equal(await card.locator(".question-inline__option").count(), 3);
      await card.locator(".question-inline__option").first().click();
      await inspect(page, `question-${width}-${height}-${theme}-${font}`);
      if (width === 1280 && theme === "light" && font === 16) {
        await page.locator(".chatview__footer").screenshot({ path: join(output, "question-card-preview.png") });
        await card.getByRole("button", { name: "下一步", exact: true }).click();
        assert.equal(await card.getByRole("status").getAttribute("aria-label"), "第 2 题，共 3 题");
        assert.equal(await page.evaluate(() => window.workbenchCalls.filter(call => call.command === "agent_resolve_question").length), 0, "next must not submit the request");
        await card.locator(".question-inline__option").nth(1).click();
        await card.getByRole("button", { name: "上一题", exact: true }).click();
        assert.equal(await card.locator(".question-inline__option").first().getAttribute("aria-pressed"), "true");
        await card.getByRole("button", { name: "下一题", exact: true }).click();
        assert.equal(await card.locator(".question-inline__option").nth(1).getAttribute("aria-pressed"), "true");
        await card.getByRole("button", { name: "下一步", exact: true }).click();
        await card.getByRole("textbox", { name: "自定义回答" }).fill("15 页，适合 20 分钟分享");
        await card.getByRole("textbox", { name: "自定义回答" }).press("Enter");
        await card.waitFor({ state: "hidden" });
        const calls = await page.evaluate(() => window.workbenchCalls.filter(call => call.command === "agent_resolve_question"));
        assert.equal(calls.length, 1, "the entire interview must submit once");
        const { args } = calls[0];
        assert.equal(args.outcome, "accepted");
        assert.deepEqual(Object.values(args.answers), [["Claude Opus 5 品牌视觉（推荐）"], ["纯扎心吐槽"], ["Other"]]);
        assert.equal(args.annotations["篇幅和交付形式？"].notes, "15 页，适合 20 分钟分享");
      }
    }
  }

  await page.setViewportSize({ width: 1024, height: 680 });
  for (const variant of ["long", "multi", "freeform", "single", "expired"]) {
    await page.goto(`${base}&theme=dark&questionVariant=${variant}&questionMode=plan`);
    const card = page.locator(".question-inline");
    await card.waitFor();
    await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
    if (variant === "long") {
      const scroll = await card.locator(".question-inline__body").evaluate(element => {
        element.scrollTop = element.scrollHeight;
        return element.scrollTop;
      });
      assert.ok(scroll > 0, "long question content must remain scrollable");
      await card.getByRole("button", { name: "下一题", exact: true }).click();
      assert.equal(await card.locator(".question-inline__body").evaluate(element => element.scrollTop), 0, "new question must start at the top");
    }
    if (variant === "multi") {
      await card.locator(".question-inline__option").nth(0).click();
      await card.locator(".question-inline__option").nth(1).click();
      assert.equal(await card.locator('[aria-pressed="true"]').count(), 2);
    }
    if (variant === "expired") {
      await card.getByText(/已超时/).waitFor();
      assert.equal(await card.getByRole("button", { name: "取消", exact: true }).isDisabled(), true);
    }
    await inspect(page, `question-${variant}-1024-dark-large`);
  }

  await page.goto(`${base}&questionVariant=single&questionFailure=1`);
  const card = page.locator(".question-inline");
  await card.waitFor();
  await card.locator(".question-inline__option").first().click();
  await card.getByRole("button", { name: "提交", exact: true }).click();
  await card.getByRole("alert").waitFor();
  assert.equal(await card.locator(".question-inline__option").first().getAttribute("aria-pressed"), "true", "submission failure must preserve the answer");
  assert.equal(await card.getByRole("button", { name: "提交", exact: true }).isEnabled(), true);
  await inspect(page, "question-submit-error");

  // The shared card must also work inside the real resizable coding pane,
  // including the wider countdown text in its final minute and plan actions.
  const codingBase = base.replace("surface=workbench", "surface=coding-question");
  const heldDraft = "确认后继续使用的开发草稿\n".repeat(12);
  await page.evaluate(text => localStorage.setItem(`echo-coding-followup-v1:${encodeURIComponent("/review/EchoAgent")}:question-review`, text), heldDraft);
  await page.setViewportSize({ width: 1024, height: 680 });
  for (const width of [300, 350, 410]) for (const theme of ["light", "dark"]) for (const font of [16, maxFont]) {
    await page.goto(`${codingBase}&panelWidth=${width}&theme=${theme}&questionVariant=ending&questionMode=plan`);
    const card = page.locator(".question-inline");
    await card.waitFor();
    await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    await inspect(page, `coding-question-${width}-${theme}-${font.toFixed(2)}`, true);
    const composerInput = page.getByRole("textbox", { name: "补充开发要求", exact: true });
    assert.equal(await composerInput.isDisabled(), true);
    assert.equal(await composerInput.inputValue(), heldDraft, "a pending question must retain the existing coding draft");
    await card.locator(".question-inline__option").first().click();
    await card.getByRole("button", { name: "下一步", exact: true }).click();
    await card.getByRole("button", { name: "上一题", exact: true }).click();
    assert.equal(await card.locator(".question-inline__option").first().getAttribute("aria-pressed"), "true");
    const cancel = card.getByRole("button", { name: "取消", exact: true });
    assert.equal(await cancel.evaluate(element => {
      const box = element.getBoundingClientRect();
      return element.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    }), true, "cancel must be directly clickable without scrolling the card horizontally");
    await cancel.click();
    await card.waitFor({ state: "hidden" });
    const calls = await page.evaluate(() => window.workbenchCalls.filter(call => call.command === "agent_resolve_question"));
    assert.equal(calls.length, 1);
    assert.equal(calls[0].args.outcome, "cancelled");
    assert.equal(await composerInput.isEnabled(), true);
    assert.equal(await composerInput.inputValue(), heldDraft, "resolving the question must restore the held draft");
    assert.ok(await composerInput.evaluate(element => element.getBoundingClientRect().height >= 88), "the draft must regain its normal reading height after resolution");
  }
  assert.deepEqual(errors, [], "question surfaces must not have page errors");
  writeFileSync(join(output, "report.json"), JSON.stringify({ passed: true, evidence }, null, 2));
  console.log(`Question UI passed: ${evidence.length} layouts, navigation, answer retention, submission, expiry and retry.`);
} finally {
  await browser?.close();
  await server.close();
}
