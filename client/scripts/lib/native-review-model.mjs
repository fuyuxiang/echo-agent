import assert from 'node:assert/strict';
import { lstatSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { StringDecoder } from 'node:string_decoder';

export const PRIVATE_REVIEW_MODEL_ID = 'echoagent-review/claude-opus-5-5';
export const DEFAULT_REVIEW_MODEL_ID = 'echoagent-ojlab/Echo Reed';
export function isReviewModelId(value) {
  return [DEFAULT_REVIEW_MODEL_ID, 'echoagent-ojlab/Echo Grove', PRIVATE_REVIEW_MODEL_ID].includes(value);
}
let cached;
function privateConnection() {
  if (!process.env.NATIVE_REVIEW_MODEL_CONFIG) return null;
  if (cached) return cached;
  const path = process.env.NATIVE_REVIEW_MODEL_CONFIG;
  const file = lstatSync(path), parent = lstatSync(dirname(path));
  assert(file.isFile() && !file.isSymbolicLink() && file.size < 16384 && realpathSync(path) === path
    && !(file.mode & 0o077) && parent.isDirectory() && !parent.isSymbolicLink() && !(parent.mode & 0o077), 'Model resource must be an ordinary owner-only file in a private directory');
  let value, url;
  try { value = JSON.parse(readFileSync(path, 'utf8')); }
  catch { throw new Error('Private model resource must contain valid JSON'); }
  try { url = new URL(value?.baseUrl); }
  catch { throw new Error('Private model requires a valid HTTPS base URL'); }
  assert(url.protocol === 'https:' && !url.username && !url.password && !url.search && !url.hash, 'Private model requires a credential-free HTTPS base URL');
  assert(value.model === 'claude-opus-5-5' && value.protocol === 'chat_completions', 'Unsupported explicit review model resource');
  assert(typeof value.apiKey === 'string' && value.apiKey.length >= 16 && !/[\r\n]/.test(value.apiKey), 'Private model credential is invalid');
  cached = { ...value, baseUrl: url.href.replace(/\/$/, '') };
  return cached;
}
export function reviewModelId(fallback = DEFAULT_REVIEW_MODEL_ID) {
  assert(isReviewModelId(fallback));
  return privateConnection() ? PRIVATE_REVIEW_MODEL_ID : fallback;
}
export function seedReviewModel(home) {
  const connection = privateConnection();
  if (!connection) return DEFAULT_REVIEW_MODEL_ID;
  const path = join(home, 'config.toml');
  const config = readFileSync(path, 'utf8');
  assert(!config.includes('[models]') && !config.includes('[model_providers.echoagent-review]'), 'Refusing to overwrite existing test model configuration');
  const quote = JSON.stringify;
  writeFileSync(path, `${config}\n[models]\ndefault = ${quote(PRIVATE_REVIEW_MODEL_ID)}\n[model_providers.echoagent-review]\nprovider_kind = "custom"\nbase_url = ${quote(connection.baseUrl)}\napi_key = ${quote(connection.apiKey)}\napi_backend = "chat_completions"\nauth_scheme = "bearer"\ncontext_window = 200000\n[model.${quote(PRIVATE_REVIEW_MODEL_ID)}]\nmodel = ${quote(connection.model)}\nmodel_provider = "echoagent-review"\nname = ${quote(connection.model)}\nmax_completion_tokens = 8192\n`, { mode: 0o600 });
  return PRIVATE_REVIEW_MODEL_ID;
}
const replacement = '[REDACTED_MODEL_CREDENTIAL]';
function secretPatterns(apiKey) {
  // Reports may JSON-escape a key containing quotes or backslashes.
  return apiKey ? [...new Set([apiKey, JSON.stringify(apiKey).slice(1, -1)])].sort((a, b) => b.length - a.length) : [];
}

export function redactReviewModelSecrets(bytes) {
  let text = String(bytes);
  for (const pattern of secretPatterns(privateConnection()?.apiKey)) text = text.split(pattern).join(replacement);
  return text;
}

/** One instance per pipe: retain only a possible secret prefix, never split UTF-8. */
export function createReviewModelSecretRedactor(apiKey = privateConnection()?.apiKey) {
  const patterns = secretPatterns(apiKey), decoder = new StringDecoder('utf8');
  let pending = '', ended = false;
  const drain = final => {
    let output = '';
    for (;;) {
      let index = -1, matched;
      for (const pattern of patterns) {
        const next = pending.indexOf(pattern);
        if (next >= 0 && (index < 0 || next < index)) { index = next; matched = pattern; }
      }
      if (index >= 0) {
        output += pending.slice(0, index) + replacement;
        pending = pending.slice(index + matched.length);
        continue;
      }
      let retained = 0;
      if (!final) for (const pattern of patterns) {
        for (let length = Math.min(pattern.length - 1, pending.length); length > retained; length--) {
          if (pending.endsWith(pattern.slice(0, length))) { retained = length; break; }
        }
      }
      output += pending.slice(0, pending.length - retained);
      pending = retained ? pending.slice(-retained) : '';
      return output;
    }
  };
  return {
    write(bytes) {
      assert(!ended, 'Cannot append to a completed review log');
      pending += decoder.write(Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes));
      return drain(false);
    },
    end() {
      if (ended) return '';
      ended = true; pending += decoder.end(); return drain(true);
    },
  };
}

/** Stream directly to a private log, with independent stdout/stderr redaction. */
export function captureReviewModelLogs(child, path, { maxBytes = 8 * 1024 * 1024, closeTimeoutMs = 3000, apiKey = privateConnection()?.apiKey } = {}) {
  let bytes = 0, overflow = false, incomplete = false, closeTimer;
  const append = text => { if (text) writeFileSync(path, text, { flag: 'a', mode: 0o600 }); };
  const redactors = [child.stdout, child.stderr].filter(Boolean).map(stream => {
    const redactor = createReviewModelSecretRedactor(apiKey);
    stream.on('data', chunk => {
      bytes += chunk.length;
      if (bytes > maxBytes) {
        if (!overflow) { overflow = true; child.kill('SIGTERM'); }
        return;
      }
      append(redactor.write(chunk));
    });
    stream.once('end', () => append(redactor.end()));
    return redactor;
  });
  const finished = new Promise(resolve => {
    const finish = () => {
      clearTimeout(closeTimer);
      for (const redactor of redactors) append(redactor.end());
      resolve();
    };
    child.once('close', finish);
    // An inherited pipe in a surviving descendant must not bypass the runner's
    // process deadline. Mark the capture incomplete instead of silently passing.
    child.once('exit', () => {
      closeTimer = setTimeout(() => {
        incomplete = true;
        child.stdout?.destroy(); child.stderr?.destroy(); finish();
      }, closeTimeoutMs);
    });
  });
  return { get bytes() { return bytes; }, get overflow() { return overflow; }, get incomplete() { return incomplete; }, finished };
}
