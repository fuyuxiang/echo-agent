import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectRenderedColors } from "./ui-color-audit.mjs";
import { inspectEditorPaint } from "./editor-paint-audit.mjs";

const baseline = process.argv.includes("--baseline");
const auditOnly = baseline || process.argv.includes("--audit-only");
const quick = process.argv.includes("--quick");
const output = reviewOutput(join(tmpdir(), "echo-enterprise-theme", baseline ? "before" : "after"));
const evidence = [], screenshots = [], errors = [];
writeFileSync(join(output, "run.json"), JSON.stringify({
  startedAt: new Date().toISOString(), baseline, currentFixtures: true,
  revision: execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  workingDiffSha256: createHash("sha256").update(execFileSync("git", ["diff", "HEAD"])).digest("hex"),
  fontScale: "root = 16 * preference / 13", quick,
}, null, 2));
const servers = [];
let browser, page, activeScenario;
let pageErrors = [];
async function server(fixture) {
  if (!servers.length) servers.push(await createUiReviewServer(0, { fixture: "/scripts/fixtures/theme-review-router.ts", baseline, currentFixtures: true }));
  const instance = servers[0];
  return `http://127.0.0.1:${instance.httpServer.address().port}/__ui-review/${fixture.split("/").at(-1).replace(/\.tsx$/, "")}`;
}
function checkpoint() {
  writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
  writeFileSync(join(output, "screenshots.json"), JSON.stringify(screenshots, null, 2));
  writeFileSync(join(output, "failures.json"), JSON.stringify(evidence.flatMap(record => [...record.failures, ...record.controlFailures].map(failure => ({ context: record.context, ...failure }))), null, 2));
}
checkpoint();
try {
  const targeted = await server("/scripts/fixtures/enterprise-theme-review.tsx");
  const shared = await server("/scripts/fixtures/ui-surfaces.tsx");
  const meeting = await server("/scripts/fixtures/meeting-workbench-review.tsx");
  const groups = await server("/scripts/fixtures/group-review.tsx");
  const schedules = await server("/scripts/fixtures/scheduled-tasks-review.tsx");
  const documents = await server("/scripts/fixtures/document-workspace-review.tsx");
  browser = await (await reviewChromium()).launch({ headless: true,
    ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
  const layouts = quick ? [[1280, 800, 13]] : [[1280, 800, 13], [1280, 800, 18], [1024, 680, 13], [1024, 680, 18]];
  for (const [width, height, font] of layouts) for (const theme of ["light", "dark"]) {
    page = undefined;
    pageErrors = [];
    const go = async (base, params) => {
      activeScenario = { width, height, font, theme, fixture: base, params };
      if (page) {
        assert.deepEqual(pageErrors, [], "The preceding fixture must complete without browser errors");
        await page.close();
        errors.push(...pageErrors);
      }
      // Dispose the completed scenario before starting another fixture module.
      // This also releases editor workers and prevents aborted lazy imports in
      // an obsolete document from racing the next scenario's event handlers.
      page = await browser.newPage({ viewport: { width, height }, colorScheme: theme });
      page.setDefaultNavigationTimeout(90000);
      page.setDefaultTimeout(30000);
      const navigationErrors = [];
      pageErrors = navigationErrors;
      page.on("pageerror", error => navigationErrors.push(error.message));
      await page.goto(`${base}?${new URLSearchParams({ theme, extensionState: "populated", settingsDesign: "1", ...params })}`, { waitUntil: "networkidle" });
      try { await page.locator("#root > *").first().waitFor(); }
      catch (error) {
        writeFileSync(join(output, "browser-errors.json"), JSON.stringify({ params, pageErrors }, null, 2));
        throw error;
      }
      await page.evaluate(font => { document.documentElement.style.fontSize = `${16 * font / 13}px`; }, font);
      await page.mouse.move(0, 0);
    };
    const capture = async (name, { requiredMarks = false, requiredText = [] } = {}) => {
      activeScenario = { ...activeScenario, capture: name };
      const colors = await page.evaluate(inspectRenderedColors);
      const geometry = await page.evaluate(() => ({
        overflow: document.documentElement.scrollWidth > innerWidth + 1,
        theme: document.documentElement.dataset.theme,
        scheme: getComputedStyle(document.documentElement).colorScheme,
        rootFont: getComputedStyle(document.documentElement).fontSize,
      }));
      const context = { name, width, height, font, theme: geometry.theme };
      assert.equal(geometry.overflow, false, `${JSON.stringify(context)}: viewport overflow`);
      if (requiredMarks) assert(colors.controls.some(control => control.role === "selection-mark"), `${name}: no selection glyph was measured`);
      for (const text of requiredText) assert([...colors.text, ...colors.manual].some(item => item.text.includes(text)), `${name}: expected visible text was not measured: ${text}`);
      assert.deepEqual(pageErrors, [], `${JSON.stringify(context)}: browser errors`);
      const file = `${name}-${geometry.theme}-${width}-${font}.png`;
      const editorPaint = await page.locator(".monaco-diff-editor").count()
        ? await inspectEditorPaint(page, page, join(output, file.replace(".png", "-backdrop.png")), { diffHighlights: true }) : null;
      if (editorPaint) assert.deepEqual(editorPaint.highlightFailures, [], `${name}: character ranges must remain distinct from changed lines`);
      await page.screenshot({ path: join(output, file), timeout: 60000 });
      if (editorPaint) colors.failures.push(...editorPaint.failures.map(sample => ({ ...sample, ratio: sample.minimum, source: "editor-raster" })));
      evidence.push({ context, geometry, ...colors, ...(editorPaint ? { editorPaint } : {}) });
      screenshots.push({ ...context, file });
      checkpoint();
    };

    await go(targeted, { surface: "skill-import" });
    await page.getByRole("button", { name: "选择 Markdown 或 ZIP 技能文件", exact: true }).click();
    await page.locator(".sk-risk--medium").first().waitFor();
    for (const detail of await page.locator(".sk-batch-row-detail > summary").all()) await detail.click();
    await page.locator(".sk-cap-badge--missing_dependencies").waitFor();
    await page.locator(".sk-cap-badge--configuration_required").waitFor();
    await page.locator(".sk-batch-row").first().locator(".sk-batch-row-name").scrollIntoViewIfNeeded();
    await capture("skill-import-expanded", { requiredText: ["中风险", "缺少依赖"] });
    await page.locator(".sk-cap-badge--configuration_required").scrollIntoViewIfNeeded();
    await capture("skill-import-configuration", { requiredText: ["待连接/授权"] });
    // A mounted portal keeps its data and updates with the same ThemeProvider.
    await page.evaluate(() => window.enterpriseThemeReview.setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark"));
    await page.waitForFunction(previous => document.documentElement.dataset.theme !== previous, theme);
    assert.equal(await page.locator(".sk-risk--medium").count(), 2);
    assert.equal(await page.evaluate(() => document.documentElement.style.colorScheme), theme === "dark" ? "light" : "dark");
    await capture("skill-import-live-switch");

    for (const kind of ["专家", "技能", "连接器"]) {
      await go(targeted, { surface: "references", kind });
      await capture(`references-${kind}`, { requiredMarks: true });
    }
    await go(targeted, { surface: "knowledge" });
    await page.locator(".knowledge-picker__trigger").click();
    await page.locator(".knowledge-picker__source-option.is-selected").waitFor();
    await capture("knowledge-selected", { requiredMarks: true });

    await go(targeted, { surface: "connector" });
    const config = page.getByRole("textbox", { name: "MCP 配置内容", exact: true });
    await config.fill('{\n  "mcpServers": {}\n}');
    await page.locator(".mcp-editor-unsaved").waitFor();
    await capture("connector-unsaved");
    await config.fill("{invalid");
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await page.locator(".mcp-editor-error").waitFor();
    await capture("connector-error");

    for (const state of ["success", "invalid", "error"]) {
      await go(targeted, { surface: "agent-settings", state });
      const developer = page.locator('[data-setting-id="developer-options"]');
      assert.equal(await developer.evaluate(element => element.open), false, "Developer options must start collapsed");
      await developer.locator(":scope > summary").click();
      const subagents = developer.locator('[data-setting-id="subagents"]');
      await subagents.getByRole("spinbutton", { name: "子代理最大层级", exact: true }).fill(state === "invalid" ? "0" : "3");
      await subagents.getByRole("button", { name: "保存", exact: true }).click();
      await page.locator(".settings-msg").filter({ hasText: state === "success" ? "已保存" : state === "invalid" ? "须为" : "保存失败" }).waitFor();
      const saved = await page.evaluate(() => window.enterpriseThemeReview.calls.filter(call => call.command === "subagents_config_save"));
      assert.deepEqual(saved, state === "invalid" ? [] : [{ command: "subagents_config_save", args: { maxDepth: 3 } }], "Validate actual persistence calls as well as visible feedback");
      await capture(`settings-feedback-${state}`);
    }

    await go(targeted, { surface: "project" });
    for (const tab of ["动态", "工作项", "项目资料"]) {
      await page.getByRole("button", { name: tab, exact: true }).click();
      await capture(`project-${tab}`);
    }
    await page.locator(".project-default-preferences > summary").click();
    for (const name of ["模型", "专家", "指令", "连接器", "技能", "定时任务"]) {
      await page.getByRole("button", { name: `配置项目默认${name}`, exact: true }).click();
      const dialog = page.getByRole("dialog", { name: `项目配置：${name}`, exact: true });
      await dialog.waitFor();
      await capture(`project-config-${name}`);
      if (name === "模型") {
        await dialog.getByRole("button", { name: /选择项目默认模型/ }).click();
        await page.getByRole("listbox").waitFor();
        await capture("project-model-menu");
        await page.keyboard.press("Escape");
      }
      await dialog.getByRole("button", { name: "关闭", exact: true }).click();
    }

    await go(shared, { surface: "organization-shell", orgState: "many" });
    for (const tab of ["概览", "经验", "文档", "Skills"]) {
      await page.locator(".org-memory__nav button").filter({ hasText: tab }).click();
      await capture(`organization-${tab}`);
      if (tab !== "概览") {
        await page.getByRole("button", { name: /我的提交/ }).click();
        await capture(`organization-${tab}-submissions`);
        await page.getByRole("button", { name: /已发布/ }).click();
      }
    }
    await go(targeted, { surface: "project", state: "unavailable" });
    await page.locator(".project-default-preferences > summary").click();
    await page.getByRole("button", { name: "配置项目默认模型", exact: true }).click();
    await page.locator(".proj-model-setting__warning").waitFor();
    await capture("project-model-unavailable", { requiredText: ["已被删除或停用"] });
    await go(shared, { surface: "workbench", state: "plan" });
    await capture("conversation-completed-plan");
    await page.getByRole("button", { name: "筛选任务", exact: true }).click();
    await page.getByRole("menu", { name: "任务筛选", exact: true }).waitFor();
    await capture("sidebar-selected-filter", { requiredMarks: true });
    await page.keyboard.press("Escape");
    await go(shared, { surface: "conversation", planReview: "clean", pausedQueueReview: "1" });
    await capture("conversation-approval");
    await page.getByRole("button", { name: "查看并确认计划", exact: true }).click();
    await page.getByRole("group", { name: "计划审批", exact: true }).waitFor();
    await capture("conversation-plan");

    for (const scenario of ["completed", "recording-error"]) {
      await go(meeting, { scenario });
      await page.getByRole("heading", { name: "产品评审周会", exact: true }).waitFor();
      if (scenario === "completed") await page.waitForFunction(() => document.querySelector("audio")?.readyState >= 1);
      await capture(`meeting-${scenario}`);
    }
    await go(groups, { groupShell: "1" });
    await page.locator(".groups-page").waitFor();
    await page.getByRole("button", { name: "创建专家团", exact: true }).click();
    await page.getByRole("textbox", { name: "专家团名称", exact: true }).fill("需求交付协作组");
    await page.getByRole("textbox", { name: "协作目标", exact: true }).fill("核对审批边界并整理可复核的交付资料");
    await page.getByRole("checkbox", { name: /代码专家/ }).check();
    await page.getByRole("checkbox", { name: /文档助手/ }).check();
    await capture("groups-create");
    await page.getByRole("button", { name: "创建并进入专家团", exact: true }).click();
    await page.getByRole("heading", { name: "需求交付协作组", exact: true }).waitFor();
    await page.locator(".echo-composer__input").fill("@代码专家 请核对需求和权限边界");
    await page.getByRole("button", { name: "发送", exact: true }).click();
    await capture("groups-populated");
    await page.evaluate(() => { window.groupReview.permission(); });
    await page.getByRole("button", { name: "允许一次", exact: true }).waitFor();
    await capture("groups-permission");
    await page.getByRole("button", { name: "允许一次", exact: true }).click();
    await page.evaluate(() => window.groupReview.failExecution());
    await page.getByText("隔离验收：模型暂时不可用，已有记录保留", { exact: false }).first().waitFor();
    await capture("groups-failed");
    await go(schedules, {});
    await page.getByText("每日 AI 新闻简报", { exact: true }).first().waitFor();
    await capture("schedules-populated");
    await page.getByRole("button", { name: "每日 AI 新闻简报", exact: true }).click();
    await capture("schedules-run-records");
    await go(documents, {});
    await page.locator(".tool-side-panel").waitFor();
    await capture("document-workspace");

    if (!baseline) {
      const waitEditorTheme = async expectedTheme => page.waitForFunction(expected => {
        if (document.documentElement.dataset.theme !== expected) return false;
        // Monaco also gives its zero-size gutter the monaco-editor class.
        const editors = [...document.querySelectorAll(".monaco-diff-editor .monaco-editor")].filter(element =>
          element.getBoundingClientRect().width > 0 && element.getBoundingClientRect().height > 0);
        if (!editors.length || !editors.every(element => element.classList.contains(expected === "dark" ? "vs-dark" : "vs"))) return false;
        const tokens = [...document.querySelectorAll(".view-lines span[class*=mtk]")].filter(element =>
          !element.children.length && element.textContent.trim() && element.getBoundingClientRect().width > 0 && element.getBoundingClientRect().height > 0);
        const comment = tokens.find(element => element.textContent.trim().startsWith("//"));
        if (tokens.length < 8 || !comment) return false;
        const probe = document.createElement("span");
        probe.style.cssText = "position:absolute;visibility:hidden;color:var(--echo-code-comment)";
        document.body.append(probe);
        const expectedColor = getComputedStyle(probe).color;
        probe.remove();
        return getComputedStyle(comment).color === expectedColor;
      }, expectedTheme);
      await go(targeted, { surface: "monaco" });
      await page.locator(".monaco-diff-editor").waitFor({ timeout: 90000 });
      await waitEditorTheme(theme);
      await capture("monaco-diff");
      await page.evaluate(() => window.enterpriseThemeReview.setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark"));
      await page.waitForFunction(previous => document.documentElement.dataset.theme !== previous, theme);
      // The root preference changes before Monaco retokenizes its live model.
      // Observe the actual editor paint before taking syntax/raster samples.
      await waitEditorTheme(theme === "dark" ? "light" : "dark");
      await capture("monaco-live-switch");
    }
    errors.push(...pageErrors);
    await page.close();
    console.log(`${baseline ? "Before" : "After"}: ${width}×${height}, ${font}px, ${theme}; ${evidence.length} states saved`);
  }
  const failures = evidence.flatMap(record => [...record.failures, ...record.controlFailures].map(failure => ({ context: record.context, ...failure })));
  console.log(JSON.stringify({ baseline, states: evidence.length, text: evidence.reduce((sum, record) => sum + record.text.length, 0), controls: evidence.reduce((sum, record) => sum + record.controls.length, 0), failures: failures.length, errors, output }));
  if (!auditOnly) assert.equal(failures.length, 0, `Rendered contrast failures: ${output}/failures.json; ${JSON.stringify(failures.slice(0, 3))}`);
} catch (error) {
  const failure = { error: String(error), stack: error.stack, scenario: activeScenario,
    url: page && !page.isClosed() ? page.url() : null, pageErrors, completedStates: evidence.length };
  writeFileSync(join(output, "failure.json"), JSON.stringify(failure, null, 2));
  if (page && !page.isClosed()) await page.screenshot({ path: join(output, "failure.png"), timeout: 5000 }).catch(() => {});
  console.error("Theme review failed:", JSON.stringify(failure));
  if (process.env.GITHUB_ACTIONS === "true") {
    const message = `${JSON.stringify(activeScenario)}: ${String(error)}`.slice(0, 3000).replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
    console.error(`::error title=Theme review failed::${message}`);
  }
  throw error;
} finally {
  checkpoint();
  await browser?.close();
  for (const instance of servers) await instance.close();
}
