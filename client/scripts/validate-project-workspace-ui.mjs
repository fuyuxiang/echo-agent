import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createUiReviewServer, reviewChromium } from './ui-review-runtime.mjs';

const output = mkdtempSync(join(tmpdir(), 'echo-project-workspace-ui-'));
console.log(`Screenshots: ${output}`);
const server = await createUiReviewServer(0, { fixture: '/scripts/fixtures/expert-project-review.tsx' });
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
  for (const [width, height] of [[1280, 800], [1024, 680], [420, 900]]) for (const theme of ['light', 'dark']) {
    const page = await browser.newPage({ viewport: { width, height } });
    const errors = []; page.on('pageerror', e => errors.push(e.message));
    try {
      await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workspace&theme=${theme}`);
      await page.getByRole('button', { name: '打开项目 客户洞察与产品规划' }).click();
      assert.equal(await page.getByRole('tab', { name: '概览', exact: true }).getAttribute('aria-selected'), 'true');
      assert.equal(await page.getByRole('tab').count(), 4);
      if (width === 420) {
        for (const narrowWidth of [360, 420]) {
          await page.setViewportSize({ width: narrowWidth, height });
          const tabs = await page.locator('.pd-tabs').evaluate(nav => {
            const root = document.documentElement;
            const originalFont = root.style.fontSize;
            root.style.fontSize = `${16 * 18 / 13}px`;
            const bounds = nav.getBoundingClientRect();
            const buttons = [...nav.querySelectorAll('[role="tab"]')];
            const result = {
              fits: buttons.every(button => button.getBoundingClientRect().right <= bounds.right + 1),
              singleRow: buttons.every(button => button.getBoundingClientRect().top === bounds.top),
              iconsHidden: buttons.every(button => getComputedStyle(button.querySelector('svg')).display === 'none'),
            };
            root.style.fontSize = originalFont;
            return result;
          });
          assert.deepEqual(tabs, { fits: true, singleRow: true, iconsHidden: true }, `Project tabs at ${narrowWidth}px, font 18`);
        }
      }
      await page.getByRole('button', { name: /检查成果/ }).waitFor();
      await page.screenshot({ path: join(output, `${width}-${theme}-overview.png`), fullPage: true });
      await page.getByRole('button', { name: /检查成果/ }).click();
      const task = page.getByRole('region', { name: '任务：竞品分析与产品机会点' });
      await task.getByRole('button', { name: /竞品分析报告.md/ }).click();
      const preview = page.getByRole('dialog', { name: '预览：竞品分析报告.md' });
      await preview.getByText('项目上下文复用、进度呈现和成果管理应形成完整流程。', { exact: true }).waitFor();
      await page.screenshot({ path: join(output, `${width}-${theme}-preview.png`), fullPage: true });
      await preview.getByRole('button', { name: '用于新任务' }).click();
      await page.getByRole('group', { name: '本次项目资料' }).waitFor();
      const draft = page.getByRole('textbox', { name: '输入问题或任务' });
      await draft.fill('根据这份成果，整理三项后续行动与验收要求');
      await page.getByRole('button', { name: '发送', exact: true }).click();
      await page.waitForFunction(() => window.reviewCalls.some(c => c.command === 'review_start_project_task'));
      const started = await page.evaluate(() => window.reviewCalls.find(c => c.command === 'review_start_project_task'));
      assert.equal(started.args.cwd, '/review/workspace');
      assert.equal(started.args.attachments.length, 1);
      assert.match(started.args.attachments[0], /^\/review\/project-files\//);
      await page.getByRole('tab', { name: '任务', exact: true }).click();
      await page.locator('.project-task-row').filter({ hasText: '形成下一季度产品路线图' }).click();
      await page.getByRole('button', { name: '暂停任务', exact: true }).click();
      await page.waitForFunction(() => window.reviewCalls.some(c => c.command === 'agent_cancel' && c.args.sessionId === 'roadmap-session' && c.args.cancelAction === 'pause'));
      await page.getByRole('button', { name: '继续任务', exact: true }).click();
      assert(await page.evaluate(() => window.reviewCalls.some(c => c.command === 'review_open_project_session' && c.args.sessionId === 'roadmap-session')));
      await page.getByRole('button', { name: '返回任务列表' }).click();
      await page.locator('.project-task-row').filter({ hasText: '竞品分析与产品机会点' }).click();
      await page.getByRole('button', { name: '确认验收', exact: true }).click();
      await page.getByRole('dialog', { name: '确认验收任务？' }).getByRole('button', { name: '确认验收', exact: true }).click();
      await page.getByRole('tab', { name: '动态', exact: true }).click();
      await page.getByRole('textbox', { name: '给项目留个言' }).fill('下周优先验证成果复用流程。');
      await page.getByRole('button', { name: '发布留言' }).click();
      await page.getByText('下周优先验证成果复用流程。', { exact: false }).waitFor();
      if (width < 900) await page.getByRole('button', { name: '展开项目上下文' }).click();
      await page.getByRole('button', { name: '管理项目定时工作' }).click();
      const schedules = page.getByRole('dialog', { name: '任务：项目定时工作' });
      await schedules.getByRole('button', { name: '新建定时工作' }).click();
      await schedules.getByRole('button', { name: '保存定时工作' }).click();
      await schedules.getByRole('button', { name: '立即运行' }).waitFor();
      const saved = await page.evaluate(() => window.reviewCalls.find(c => c.command === 'automations_save'));
      assert.equal(saved.args.automation.projectId, 'review-local-project');
      assert.equal(saved.args.automation.schedule.freq, 'WEEKLY');
      assert.deepEqual(saved.args.automation.skills, ['/review/skills/analysis']);
      assert(await page.evaluate(() => window.reviewCalls.some(c => c.command === 'skills_list' && c.args.cwd === '/review/workspace')));
      await schedules.getByRole('button', { name: '立即运行' }).click();
      await schedules.getByRole('button', { name: '打开任务' }).waitFor();
      await schedules.getByRole('button', { name: '关闭任务详情' }).click();
      if (width < 900) await page.getByRole('button', { name: '收起项目上下文' }).first().click();
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, `overflow at ${width}/${theme}`);
      assert.deepEqual(errors, []);
      console.log(`PASS ${width}x${height} ${theme}: preview → reference → send; pause; accept; comment; schedule`);
    } catch (e) { await page.screenshot({ path: join(output, `${width}-${theme}-failure.png`), fullPage: true }); throw e; }
    finally { await page.close(); }
  }
  console.log(`Screenshots: ${output}`);
} finally { await browser?.close(); await server.close(); }
