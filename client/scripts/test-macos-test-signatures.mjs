import assert from 'node:assert/strict';
import test from 'node:test';
import { ownedTestExecutables } from './prepare-macos-test-signatures.mjs';
const base = { reason: 'compiler-artifact', executable: '/repo/src-tauri/target/debug/deps/echoagent_lib-abc',
  profile: { test: true }, target: { src_path: '/repo/src-tauri/src/lib.rs', kind: ['lib'] } };
const select = values => ownedTestExecutables(values, '/repo/src-tauri', '/repo/src-tauri/target');
test('accepts only own Cargo test artifacts and deduplicates without directory scans', () => {
  assert.deepEqual(select([base, base, { reason: 'build-finished', success: true }]), [base.executable]);
});
for (const [name, change] of [
  ['ordinary application', { profile: { test: false } }],
  ['bin target', { target: { src_path: base.target.src_path, kind: ['bin'] } }],
  ['foreign workspace', { target: { src_path: '/other/src/lib.rs', kind: ['lib'] } }],
  ['outside target', { executable: '/repo/bin/user-app' }],
  ['neighbor directory prefix', { executable: '/repo/src-tauri/target-other/bin/test' }],
]) test(`rejects ${name}`, () => assert.throws(() => select([{ ...base, ...change }])));
test('does not treat absent build results as ready for signing', () => assert.throws(() => select([])));
test('accepts the continuation integration harness within the bounded artifact count', () => {
  const artifacts=Array.from({length:17},(_,i)=>({...base,executable:`${base.executable}-${i}`}));
  assert.equal(select(artifacts).length,17);
  assert.throws(()=>select([...artifacts,{...base,executable:`${base.executable}-extra`}]));
});

test('accepts Cargo main.rs test harness in deps, while refusing ordinary application path', () => {
  const binary = { ...base, executable: '/repo/src-tauri/target/debug/deps/echoagent-0123456789abcdef',
    target: { name: 'echoagent', src_path: '/repo/src-tauri/src/main.rs', kind: ['bin'] } };
  assert.deepEqual(select([binary]), [binary.executable]);
  assert.throws(() => select([{ ...binary, executable: '/repo/src-tauri/target/debug/echoagent' }]));
});

test('recognizes actual Tauri library crate kinds and leaves ordinary app artifacts untouched', () => {
  const library = { ...base, target: { ...base.target, kind: ['staticlib', 'cdylib', 'rlib'] } };
  const app = { ...base, profile: { test: false }, executable: '/repo/src-tauri/target/debug/echoagent',
    target: { name: 'echoagent', src_path: '/repo/src-tauri/src/main.rs', kind: ['bin'] } };
  assert.deepEqual(select([library, app]), [library.executable]);
});
