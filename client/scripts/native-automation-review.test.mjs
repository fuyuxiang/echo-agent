import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { expect, it } from 'vitest';

const source = readFileSync(new URL('./fixtures/native-automation-review.js', import.meta.url), 'utf8');
const home = '/private/tmp/echo-native-review-automation-test';
const cwd = `${home}/workspaces/default`;
const other = `${home}/workspaces/second`;

async function runFirstLaunch(receipts) {
  let saved;
  const commands = [];
  const invoke = async (command, args) => {
    commands.push(command);
    switch (command) {
      case 'desktop_review_context': return { phase: 'first-launch', home };
      case 'path_stat': return { exists: true, kind: 'directory', absolute: cwd };
      case 'automations_snapshot':
        return { automations: saved ? [receipts.readback(saved)] : [], records: [] };
      case 'automations_save':
        saved = { ...structuredClone(args.automation), id: 'test-automation' };
        return receipts.save(structuredClone(saved));
      case 'agent_auth_status':
        return { ready: false, runtimeReady: false, reason: 'Synthetic test stops before running an automation' };
      default: throw new Error(`Unexpected fixture command: ${command}`);
    }
  };
  const window = { __TAURI_INTERNALS__: { invoke } };
  runInNewContext(source, { window, setTimeout, clearTimeout, crypto: { randomUUID: () => 'synthetic' } });
  const report = await window.__echoNativeAutomationReview({ invoke, home, cwd, phase: 'first-launch' });
  return { commands, saveCheck: report.checks.find(check => check.id === 'native-automation-paused-once') };
}

it('accepts equal workspaces in separate saved and readback IPC responses', async () => {
  const { commands, saveCheck } = await runFirstLaunch({ save: structuredClone, readback: structuredClone });
  expect(saveCheck.result).toBe('passed');
  expect(commands).toContain('agent_auth_status');
  expect(commands).not.toContain('automations_run');
});

it.each([
  { label: 'empty list', paths: [] },
  { label: 'different workspace', paths: [other] },
  { label: 'extra workspace at the end', paths: [cwd, other] },
  { label: 'extra workspace at the start', paths: [other, cwd] },
])('rejects a readback with $label', async ({ paths }) => {
  const { commands, saveCheck } = await runFirstLaunch({
    save: structuredClone,
    readback: saved => ({ ...structuredClone(saved), cwds: paths }),
  });
  expect(saveCheck.result).toBe('failed');
  expect(commands).not.toContain('agent_auth_status');
});

it('requires the save receipt to name only the owned workspace', async () => {
  const { commands, saveCheck } = await runFirstLaunch({
    save: saved => ({ ...saved, cwds: [cwd, other] }),
    readback: structuredClone,
  });
  expect(saveCheck.evidence.error).toMatch(/changed task input, isolation or paused status/);
  expect(commands.filter(command => command === 'automations_snapshot')).toHaveLength(1);
});
