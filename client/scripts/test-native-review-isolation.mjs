import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';

test('native acceptance strips inherited credentials, discovery overrides, and review switches without changing HOME', () => {
  const inherited = { HOME: '/synthetic/foreign-home', PATH: '/synthetic/bin',
    OpenAI_API_KEY: 'mixed-case-marker', Echo_Agent_Config_Path: '/synthetic/config', Node_Options: '--require synthetic',
    ECHO_AGENT_HOME: '/synthetic/foreign-data', ECHO_AGENT_AUTH_PATH: '/synthetic/private-auth',
    ECHO_AGENT_AGENT_ID: 'foreign-device-identity',
    ECHO_AGENT_AUTH: 'private-marker', ECHO_AGENT_AUTH_PROVIDER_COMMAND: 'foreign-command',
    ECHO_AGENT_CONFIG_PATH: '/synthetic/private-config', OTEL_EXPORTER_OTLP_ENDPOINT: 'https://foreign.invalid',
    CLAUDE_CONFIG_DIR: '/synthetic/claude', OPENAI_API_KEY: 'private-marker', GROK_AUTH: 'private-marker', CONFIG_PATH: '/synthetic/private-config', NODE_OPTIONS: '--require foreign', PYTHONPATH: '/synthetic/python', CODEX_HOME: '/synthetic/codex',
    ECHO_DESKTOP_REVIEW_CODE_AGENT: '1', ECHO_LOCAL_REVIEW_CONFIG: 'foreign', ECHO_WEBDAV_REVIEW_SECRET: 'foreign' };
  const clean = cleanNativeReviewEnvironment(inherited);
  assert.deepEqual(clean, { HOME: inherited.HOME, PATH: inherited.PATH, ECHO_AGENT_REVIEW_ISOLATED: '1',
    ECHO_AGENT_AGENT_ID: '00000000-0000-4000-8000-000000000001',
    ECHO_AGENT_TELEMETRY_ENABLED: 'false', ECHO_AGENT_TELEMETRY_TRACE_UPLOAD: 'false' });
  assert.equal(inherited.ECHO_AGENT_AUTH, 'private-marker');
});

test('Runtime production compatibility discovery consistently uses controlled getter', () => {
  const base = 'vendor/echo-agent-build/crates/codegen';
  const walk = directory => readdirSync(directory, { withFileTypes: true }).flatMap(item => {
    const path = join(directory, item.name);
    return item.isDirectory() && item.name !== 'tests' ? walk(path)
      : item.isFile() && item.name.endsWith('.rs') && !item.name.includes('test') ? [path] : [];
  });
  for (const crate of ['echo-agent-core', 'echo-agent-runtime', 'echo-agent-tools', 'echo-agent-workspace', 'echo-agent-plugin-marketplace']) {
    for (const path of walk(join(base, crate, 'src'))) {
      const production = readFileSync(path, 'utf8').split('#[cfg(test)]')[0];
      assert(!/dirs::home_dir\s*\(/.test(production), `Uncontrolled user discovery: ${path}`);
      assert(!/var_os\("(?:HOME|USERPROFILE)"\)/.test(production), `Uncontrolled compatibility HOME: ${path}`);
      assert(!/shellexpand::tilde\(/.test(production), `Uncontrolled tilde resolver: ${path}`);
    }
  }
  const paths = readFileSync(join(base, 'echo-agent-config/src/paths.rs'), 'utf8');
  assert(paths.includes('resolve_discovery_home_with(controlled_discovery_home(), echo_agent_home::os_home_dir)'));
  assert(paths.includes('home.join("system/claude/managed-settings.json")'));
  assert(paths.includes('Some(PathBuf::from("/etc/echoagent"))'), 'system policy must stay authoritative');
});
