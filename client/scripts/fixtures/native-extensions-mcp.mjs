// Genuine stdio MCP fixture: serves two tools and writes only fixed synthetic artifacts.
import assert from 'node:assert/strict';
import { appendFileSync, existsSync, lstatSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
const [home, nonce] = process.argv.slice(2);
assert(/^\/private\/tmp\/echo-native-review-extensions-[A-Za-z0-9_-]+$/.test(home));
assert(/^[a-f0-9]{32}$/.test(nonce)); assert.equal(realpathSync(home), home);
const cwd = join(home, 'workspaces/default');
for (const p of [home, cwd]) { const s = lstatSync(p); assert(s.isDirectory() && !s.isSymbolicLink() && !(s.mode & 0o077) && s.uid === process.getuid()); }
assert.equal(realpathSync(process.argv[1]), join(cwd, 'extension-fixtures/server.mjs'));
const audit = (event, fields = {}) => appendFileSync(join(cwd, 'extension-mcp-audit.jsonl'), JSON.stringify({ event, nonce, pid: process.pid,
  ppid: process.ppid, at: new Date().toISOString(), ...fields }) + '\n', { mode: 0o600 });
audit('started');
const schema = { type: 'object', properties: { nonce: { type: 'string', const: nonce }, phase: { type: 'string', enum: ['first', 'restart'] } }, required: ['nonce', 'phase'], additionalProperties: false };
const tools = [
  { name: 'echo_probe', description: 'Read-only probe of this synthetic acceptance server.', inputSchema: schema,
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false } },
  { name: 'echo_deliver', description: 'Write the fixed synthetic acceptance result into the isolated task workspace. Requires user approval.', inputSchema: schema,
    annotations: { readOnlyHint: false, destructiveHint: false, openWorldHint: false } },
];
const respond = (id, result) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n');
createInterface({ input: process.stdin }).on('line', line => {
  let request;
  try {
    assert(line.length <= 65536); request = JSON.parse(line);
    const { id, method, params } = request;
    if (method === 'initialize') {
      audit('initialize', { protocolVersion: params?.protocolVersion, client: params?.clientInfo });
      respond(id, { protocolVersion: params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'native-extensions', version: '1.0.0' } });
    } else if (method === 'tools/list') { audit('tools/list'); respond(id, { tools }); }
    else if (method === 'ping') respond(id, {});
    else if (method === 'tools/call') {
      const args = params.arguments;
      assert(tools.some(t => t.name === params.name)); assert.equal(args?.nonce, nonce); assert(['first', 'restart'].includes(args.phase));
      assert.deepEqual(Object.keys(args).sort(), ['nonce', 'phase']);
      const path = join(cwd, `extensions-result-${args.phase}.txt`), content = `MCP-${args.phase}-${nonce}\n真实 stdio MCP 交付。\n`;
      if (params.name === 'echo_deliver') {
        if (existsSync(path)) assert.equal(readFileSync(path, 'utf8'), content);
        else writeFileSync(path, content, { mode: 0o600, flag: 'wx' });
      }
      const output = params.name === 'echo_probe' ? `PROBE-${nonce}` : `DELIVERED-${nonce}\n${path}\n${content}`;
      audit('tools/call', { requestId: id, tool: params.name, phase: args.phase, artifactExists: existsSync(path) });
      respond(id, { content: [{ type: 'text', text: output }], isError: false });
    } else if (id !== undefined) process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code: -32601, message: 'Unsupported method' } }) + '\n');
  } catch (error) {
    audit('rejected', { method: request?.method, reason: String(error?.message || error).slice(0, 1000) });
    if (request?.id !== undefined) respond(request.id, { content: [{ type: 'text', text: 'Refused outside the fixed acceptance scope' }], isError: true });
  }
}).on('close', () => { audit('closed'); });
process.once('SIGTERM', () => { audit('terminated'); process.exit(0); });
