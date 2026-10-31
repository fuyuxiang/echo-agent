import { describe, expect, it } from 'vitest';
import { buildDraft } from '../automation/schedule-utils';
import { nextRunLabel, runStatusLabel, taskState, taskStateLabel } from '../automation/task-presentation';
import type { Automation } from '@/lib/types';
const task = (): Automation => ({ ...buildDraft(), createdAt: '2020-01-01T00:00:00Z' });
describe('定时任务计划状态与执行状态', () => {
  it('过期与主动暂停分开，失败不改变计划状态', () => {
    const now = new Date(2026, 9, 31, 12);
    expect(taskState({ ...task(), validUntilDate: '2026-10-30' }, [], now)).toBe('ended');
    expect(taskState({ ...task(), status: 'PAUSED' }, [], now)).toBe('paused');
    expect(taskState(task(), [{ id: 'r', automationId: '', automationName: '', startedAt: '', status: 'failed', archived: false }], now)).toBe('active');
  });
  it('错过一次性计划不会被误认为用户暂停', () => {
    expect(taskStateLabel({ ...task(), scheduleType: 'once', scheduledDate: '2020-01-01', status: 'PAUSED' })).toBe('已错过');
  });
  it('待确认、停止中和等待会话均有可理解的执行状态', () => {
    const run = { id: 'r', automationId: '', automationName: '', startedAt: '', status: 'running', archived: false };
    expect(runStatusLabel({ ...run, needsAttention: true })).toBe('待确认');
    expect(runStatusLabel({ ...run, stopRequested: true })).toBe('正在停止');
    expect(runStatusLabel({ ...run, waitingReason: '等待会话' })).toBe('等待会话');
  });
  it('下次时间显示明确日期和时间，跨年包含年份', () => {
    const now = new Date(2026, 9, 31, 12);
    expect(nextRunLabel(new Date(2026, 10, 1, 9).toISOString(), now)).toMatch(/^明天 09:00/);
    expect(nextRunLabel(new Date(2027, 0, 1, 9).toISOString(), now)).toMatch(/^2027\/1\/1 09:00/);
  });
});
