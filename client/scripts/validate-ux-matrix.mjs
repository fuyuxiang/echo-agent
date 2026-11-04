// Production components with isolated fixture IPC. This does not claim a real
// Agent, native window geometry, or operating-system scaling.
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium, reviewOutput } from './ui-review-runtime.mjs';

const baseline = process.argv.includes('--baseline');
const font = process.argv.includes('--large') ? 18 : 13;
const output = reviewOutput(`/private/tmp/echo-ux-matrix-${baseline ? 'before' : 'after'}-${font}`);
const maximum = process.env.UI_REVIEW_MAX_VIEWPORT?.split('x').map(Number);
if (maximum) assert(maximum.length === 2 && maximum.every(n => Number.isInteger(n) && n >= 680));
const sizes = process.argv.includes('--stress') ? [[1024, 640]]
  : font === 18 ? [[1024, 680], [1280, 720]]
  : [...(maximum ? [maximum] : []), [1440, 900], [1280, 720], [1024, 680]];
const routes = [
  ['home', 'ui-surfaces', { surface: 'home' }, '.home'],
  ['conversation', 'ui-surfaces', { surface: 'workbench' }, '.chatview'],
  ['settings-general', 'ui-surfaces', { surface: 'general' }, '.settings-modal'],
  ['settings-model', 'ui-surfaces', { surface: 'model' }, '.settings-modal'],
  ['projects', 'ui-surfaces', { surface: 'projects' }, '.page-toolbar'],
  ['project-details', 'enterprise-theme-review', { surface: 'project' }, '.pd-page'],
  ['organization', 'ui-surfaces', { surface: 'organization-shell', orgState: 'many' }, '.org-memory'],
  ['experts', 'ui-surfaces', { surface: 'management-experts', extensionState: 'populated' }, '.page-toolbar'],
  ['skills', 'ui-surfaces', { surface: 'page', label: '技能', extensionState: 'populated' }, '.page-toolbar'],
  ['connectors', 'ui-surfaces', { surface: 'page', label: '连接器', extensionState: 'populated' }, '.page-toolbar'],
  ['plugins', 'ui-surfaces', { surface: 'page', label: '插件', extensionState: 'populated' }, '.page-toolbar'],
  ['coding-host', 'ui-surfaces', { surface: 'coding' }, '.coding-workbench'],
  ['document', 'document-workspace-review', {}, '.document-workbench'],
  ['groups', 'group-review', {}, '.group-workspace'],
];
const selected = routes.filter(([name]) => !process.env.UI_REVIEW_MATRIX_FILTER || new RegExp(process.env.UI_REVIEW_MATRIX_FILTER).test(name));
const report = { baseline, font, evidenceLevel: 'browser production components / isolated IPC',
  maximumViewportSource: maximum ? 'caller-supplied display visibleFrame; browser viewport only' : 'not supplied',
  systemScale150: 'not tested by this browser matrix', animation: 'frozen for static captures', cases: [], passed: false };
const server = await createUiReviewServer(0, { fixture: '/scripts/fixtures/theme-review-router.ts', baseline });
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' });
  for (const [width, height] of sizes) for (const theme of ['light', 'dark']) for (const [name, route, params, ready] of selected) {
    const context = await browser.newContext({ viewport: { width, height }, colorScheme: theme, deviceScaleFactor: 1 });
    const page = await context.newPage();
    page.setDefaultTimeout(15000); page.setDefaultNavigationTimeout(90000);
    const entry = { name, width, height, theme, font, errors: [], passed: false,
      screenshot: `${name}-${width}x${height}-${theme}-${font}.png` };
    page.on('pageerror', error => entry.errors.push(error.message));
    try {
      await page.addInitScript(({ theme, font }) => {
        localStorage.setItem('echoagent.theme', theme);
        localStorage.setItem('echoagent.fontSize', String(font));
      }, { theme, font });
      const query = new URLSearchParams({ ...params, theme, platform: 'mac' });
      await page.goto(`${origin}/__ui-review/${route}?${query}`, { waitUntil: 'networkidle' });
      // Some older fixtures do not use the production main entry's font init.
      // Apply the same exported function; this remains application font size.
      await page.evaluate(async font => { const { applyFontSize } = await import('/src/lib/font-size.ts'); applyFontSize(font); }, font);
      if (name === 'groups') {
        await page.getByRole('button', { name: '创建专家团', exact: true }).click();
        await page.getByRole('textbox', { name: '专家团名称', exact: true }).fill('交付检查组');
        await page.getByRole('checkbox', { name: /代码专家/ }).check();
        await page.getByRole('checkbox', { name: /文档助手/ }).check();
        await page.getByRole('button', { name: '创建并进入专家团', exact: true }).click();
        await page.locator('.group-chat__heading').waitFor();
      } else await page.locator(ready).first().waitFor();
      await page.evaluate(() => document.fonts.ready);
      entry.geometry = await page.evaluate(() => {
        const bounds = e => { const r = e.getBoundingClientRect(); return { x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom }; };
        const visible = e => e.checkVisibility() && e.getBoundingClientRect().height > 0;
        return { viewport: [innerWidth, innerHeight], dpr: devicePixelRatio,
          rootFont: getComputedStyle(document.documentElement).fontSize,
          theme: document.documentElement.dataset.theme,
          rootOverflow: document.documentElement.scrollWidth > innerWidth + 1,
          containers: [...document.querySelectorAll('.app__main,.settings-modal__panel,.group-chat')].filter(visible)
            .map(e => ({ className: e.className, overflow: e.scrollWidth > e.clientWidth + 1 })),
          dialogs: [...document.querySelectorAll('[aria-modal=true]')].filter(visible).map(bounds),
          controls: [...document.querySelectorAll('button[aria-label="发送"],button[aria-label="关闭设置"]')].filter(visible).map(bounds),
          sidebar: [...document.querySelectorAll('.sidebar')].some(visible),
        };
      });
      assert.equal(entry.geometry.theme, theme, `${name}: incorrect resolved theme`);
      assert.equal(entry.geometry.rootOverflow, false, `${name}: document horizontal overflow`);
      assert(entry.geometry.containers.every(c => !c.overflow), `${name}: workspace horizontal overflow ${JSON.stringify(entry.geometry.containers)}`);
      for (const box of [...entry.geometry.dialogs, ...entry.geometry.controls]) {
        assert(box.x >= -1 && box.y >= -1 && box.right <= width + 1 && box.bottom <= height + 1,
          `${name}: dialog or primary control outside viewport ${JSON.stringify(box)}`);
      }
      assert.deepEqual(entry.errors, [], `${name}: browser rendering errors`);
      entry.passed = true;
    } catch (error) { entry.failure = String(error); }
    finally {
      await page.screenshot({ path: join(output, entry.screenshot) }).catch(error => { entry.passed = false; entry.captureError = String(error); });
      report.cases.push(entry);
      writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2));
      console.log(`${entry.passed ? 'PASS' : 'FAIL'} ${entry.screenshot}${entry.failure ? ': '+entry.failure : ''}`);
      await context.close();
    }
  }
  report.passed = report.cases.length > 0 && report.cases.every(c => c.passed);
  if (!report.passed) process.exitCode = 1;
} finally {
  await browser?.close(); await server.close();
  writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2));
}
