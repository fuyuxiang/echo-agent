/** Local rules/integration tests use the production TS with controlled file/model services.
 * They do not claim a desktop or real-model acceptance result. */
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { randomUUID, webcrypto } from 'node:crypto';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';

const nativeRequire = createRequire(import.meta.url);
const crypto = { subtle: webcrypto.subtle, randomUUID };
function load(name, imports = {}, globals = {}) {
    const source = ts.transpileModule(readFileSync(new URL(name, import.meta.url), 'utf8'), {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
            experimentalDecorators: true, emitDecoratorMetadata: false }
    }).outputText;
    const exports = {};
    vm.runInNewContext(source, {
        exports, crypto, TextEncoder, AbortController, console,
        window: { setTimeout, clearTimeout },
        require: id => id in imports ? imports[id] : nativeRequire(id), ...globals
    }, { filename: name });
    return exports;
}
const protocol = load('echo-editor-protocol.ts');
const position = (line, character) => ({ line, character });
const edit = (line, from, to, text) => ({ range: { start: position(line, from), end: position(line, to) }, text });

test('actual Echo frontend preference wiring disables default browser auto-save without a session or user override', async () => {
    const product=JSON.parse(readFileSync(new URL('../../../browser/package.json',import.meta.url),'utf8'));
    const decorator=()=>()=>undefined;
    const { FrontendConfigPreferenceContribution }=load('../../../../packages/core/src/browser/preferences/frontend-config-preference-contributions.ts',{
        'inversify':{inject:decorator,named:decorator,injectable:()=>value=>value},
        '../../common/preferences/preference-schema':{},
        '../frontend-application-config-provider':{FrontendApplicationConfigProvider:{get:()=>product.theia.frontend.config}},
        './preference-contribution':{FrontendApplicationPreferenceConfig:{is:config=>typeof config.preferences==='object'}},
        '../../common/preferences/preference-language-override-service':{PreferenceLanguageOverrideService:{testOverrideValue:()=>false}},
        '../../common/preferences':{PreferenceScope:{Folder:3}},
        '@theia/application-package/lib/application-props':{DefaultTheme:{defaultForOSTheme:()=> 'dark'}},
        '../../common/logger':{}
    });
    const defaults=[];const contribution=new FrontendConfigPreferenceContribution();
    contribution.logger={error:message=>assert.fail(message)};
    await contribution.initSchema({registerOverride:(...args)=>defaults.push(args)});
    assert.deepEqual(Array.from(defaults.find(([name])=>name==='files.autoSave')),['files.autoSave',undefined,'off']);
    assert.equal(product.theia.frontend.config.preferences['files.autoSaveDelay'],undefined);
});

class Disposables {
    constructor() { this.values = []; }
    push(value) { this.values.push(value); return value; }
    dispose() { this.values.splice(0).forEach(value => value.dispose()); }
}
class FileError extends Error { constructor(message, fileOperationResult) { super(message); this.fileOperationResult = fileOperationResult; } }
const fileResults = { FILE_NOT_FOUND: 1, FILE_MODIFIED_SINCE: 3 };
const uri = input => {
    const parsed = /^([a-z-]+):\/\/([^?]*)(?:\?(.*))?$/.exec(input);
    const scheme = parsed?.[1] ?? 'file'; const path = parsed?.[2] ?? input; const query = parsed?.[3];
    const value = `${scheme}://${path}${query ? `?${query}` : ''}`;
    return { scheme, toString: () => value, path: { fsPath: () => path, base: path.split('/').pop() },
        withScheme: next => uri(`${next}://${path}${query ? `?${query}` : ''}`),
        withQuery: next => uri(`${scheme}://${path}?${next}`), isEqual: other => other.toString() === value };
};
class EchoFileService {}
const decorator = () => () => undefined;
const sharedImports = {
    '@theia/core/shared/inversify': { inject: decorator, injectable: () => value => value },
    '@theia/core/lib/browser': {},
    '@theia/core/lib/common/uri': { default: class { constructor(input) { Object.assign(this, uri(input)); } } },
    '@theia/core/lib/common/file-uri': { FileUri: { create: uri } },
    '@theia/core/lib/common/disposable': { DisposableCollection: Disposables, Disposable: { create: dispose => ({ dispose }) } },
    '@theia/core/lib/common/selection-service': { SelectionService: class {} },
    '@theia/editor/lib/browser/editor-manager': { EditorManager: class {} },
    '@theia/filesystem/lib/browser/file-service': { FileService: class {} },
    '@theia/filesystem/lib/common/files': { FileOperationError: FileError, FileOperationResult: fileResults },
    '@theia/monaco/lib/browser/monaco-editor-model': {},
    '@theia/monaco/lib/browser/monaco-text-model-service': { MonacoTextModelService: class {} },
    '@theia/terminal/lib/browser/base/terminal-service': { TerminalService: Symbol() },
    '@theia/workspace/lib/browser': { WorkspaceService: class {} },
    '@theia/workspace/lib/browser/diff-service': { DiffService: class {} },
    './echo-editor-protocol': protocol,
    './echo-file-service': { EchoFileService },
    './echo-inline-completion': { EchoInlineCompletion: class {} },
    './echo-diff-resources': { EchoDiffResources: class {} }
};

function bridgeFixture(diskValues = { '/project/a.ts': 'old' }, storage = new Map()) {
    const disk = new Map(Object.entries(diskValues));
    const models = new Map();
    const log = [];
    const fileService = new class extends EchoFileService {
        async read(resource) {
            log.push(`read:${resource.toString()}`);
            if (!disk.has(resource.path.fsPath())) { throw new FileError('missing', fileResults.FILE_NOT_FOUND); }
            return { value: disk.get(resource.path.fsPath()), encoding: 'utf8', etag: '1', mtime: 1 };
        }
        async exists(resource) { return disk.has(resource.path.fsPath()); }
        async writeTextFileConditional(resource, expectedHash, text, assertCurrent) {
            assertCurrent?.();
            if (await protocol.textHash(disk.get(resource.path.fsPath())) !== expectedHash) {
                throw new FileError('disk conflict', fileResults.FILE_MODIFIED_SINCE);
            }
            log.push(`write:${resource.toString()}`); disk.set(resource.path.fsPath(), text);
        }
        async createTextFileExclusive(resource, text, assertCurrent) {
            assertCurrent?.();
            if (disk.has(resource.path.fsPath())) { throw new FileError('already exists', fileResults.FILE_MODIFIED_SINCE); }
            log.push(`create:${resource.toString()}`); disk.set(resource.path.fsPath(), text);
        }
        async deleteTextFileConditional(resource, expectedHash, assertCurrent) {
            assertCurrent?.();
            if (await protocol.textHash(disk.get(resource.path.fsPath())) !== expectedHash) {
                throw new FileError('disk conflict', fileResults.FILE_MODIFIED_SINCE);
            }
            disk.delete(resource.path.fsPath());
        }
        async moveTextFileConditional(resource, destination, expectedHash, assertCurrent) {
            assertCurrent?.();
            if (await protocol.textHash(disk.get(resource.path.fsPath())) !== expectedHash || disk.has(destination.path.fsPath())) {
                throw new FileError('disk conflict', fileResults.FILE_MODIFIED_SINCE);
            }
            disk.set(destination.path.fsPath(), disk.get(resource.path.fsPath()));
            disk.delete(resource.path.fsPath());
        }
    }();
    const bridge = load('echo-editor-bridge.ts', {
        ...sharedImports,
        '@theia/monaco/lib/browser/monaco-editor': { MonacoEditor: { findByDocument: () => [] } },
        '@theia/monaco-editor-core': { editor: { getModelMarkers: () => [] }, Uri: { parse: uri },
            MarkerSeverity: { Error: 8, Warning: 4, Info: 2, Hint: 1 }, Range: class { constructor(startLineNumber, startColumn, endLineNumber, endColumn) {
            Object.assign(this, { startLineNumber, startColumn, endLineNumber, endColumn });
        } } },
        './echo-host-bridge': { echoHostBridge: { enabled: false } }
    }, { sessionStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) } });
    const service = new bridge.EchoEditorBridge();
    service.files = fileService;
    service.models = { get: path => models.get(path) };
    service.editors = { all: [], currentEditor: undefined };
    service.workspace = { tryGetRoots: () => [{ resource: { isEqualOrParent: () => true } }] };
    service.terminals = {};
    const diffResources = new Map(); const diffCalls = [];
    service.diffResources = { add(resource, content) {
        const entry = { content, disposed: false }; diffResources.set(resource.toString(), entry);
        return { dispose: () => { entry.disposed = true; } };
    } };
    service.diffs = { async openDiffEditor(original, modified, title) {
        diffCalls.push({ original, modified, title });
        assert.equal(diffResources.get(original.toString()).disposed, false);
    } };
    service.workspacePath = async () => '/project';
    service.checkedUri = async path => uri(path);
    function buffer(path, text) {
        const model = { version: 4, dirty: text !== disk.get(path), languageId: 'typescript',
            readOnly: false, getText: () => text, stackBoundaries: 0, editsApplied: 0,
            async save() { disk.set(path, text); model.dirty = false; },
            textEditorModel: {
                pushStackElement() { model.stackBoundaries++; },
                pushEditOperations(_selections, edits) {
                    text = protocol.applyTextEdits(text, edits.map(edit => ({ text: edit.text,
                        range: { start: position(edit.range.startLineNumber - 1, edit.range.startColumn - 1),
                            end: position(edit.range.endLineNumber - 1, edit.range.endColumn - 1) } })));
                    model.version++; model.dirty = true; model.editsApplied++;
                }
            },
            type(next) { text = next; model.version++; model.dirty = true; }
        };
        models.set(uri(path).toString(), model);
        return model;
    }
    return { service, disk, models, log, buffer, storage, diffCalls, diffResources };
}
async function operation(path, text, changes, extra = {}) {
    return { workspace: '/project', sessionId: 'session', runId: 'run', operationId: randomUUID(),
        files: [{ uri: path, expectedHash: await protocol.textHash(text), edits: changes }], ...extra };
}

function frontendFixture(editorBridge, currentEditor) {
    const notifications = []; const messages = [];
    const currentChanged = event(); const activeChanged = event();
    const imports = { ...sharedImports };
    const source = readFileSync(new URL('echo-frontend-contribution.ts', import.meta.url), 'utf8');
    for (const match of source.matchAll(/from '(.*?)'/g)) {
        if (!(match[1] in imports)) imports[match[1]] = {};
    }
    imports['@theia/core/lib/browser/saveable'] = { Saveable: { getDirty: widget => widget.saveable?.dirty ? widget.saveable : undefined } };
    imports['./echo-editor-bridge'] = { EchoEditorBridge: class {} };
    imports['./echo-host-bridge'] = { echoHostBridge: { enabled: false, isTrusted: () => true,
        notify: (type, payload) => notifications.push({type,...payload}) } };
    const { EchoFrontendContribution } = load('echo-frontend-contribution.ts', imports, {
        window: { addEventListener: (name, listener) => { if (name === 'message') messages.push(listener); }, requestAnimationFrame: () => undefined }
    });
    const service = new EchoFrontendContribution();
    service.editorBridge = editorBridge;
    service.editors = { currentEditor, onCurrentEditorChanged: currentChanged, onActiveEditorChanged: activeChanged };
    service.terminals = { all: [], onDidCreateTerminal: () => undefined };
    service.applicationState = { reachedState: async () => undefined };
    service.workspace = { roots: Promise.resolve([{resource:uri('/project')}]), onWorkspaceChanged: () => undefined };
    service.shell = { widgets: currentEditor ? [currentEditor] : [] };
    service.onStart();
    return {service,notifications,messages,currentChanged,activeChanged};
}

test('LSP UTF-16 edits preserve CRLF and reject out-of-range/overlapping/binary edits', () => {
    assert.equal(protocol.applyTextEdits('😀abc\r\nsecond', [edit(0, 2, 5, 'new'), edit(1, 0, 6, 'line')]), '😀new\r\nline');
    assert.throws(() => protocol.applyTextEdits('abc', [edit(0, 0, 7, 'x')]), /列号/);
    assert.throws(() => protocol.applyTextEdits('abc', [edit(0, 0, 2, ''), edit(0, 1, 3, '')]), /重叠/);
    assert.throws(() => protocol.applyTextEdits('a\0b', [edit(0, 0, 1, '')]), /二进制/);
    assert.throws(() => protocol.applyTextEdits('😀abc', [edit(0, 1, 2, 'x')]), /代理对/);
    assert.throws(() => protocol.applyTextEdits('😀abc', [edit(0, 0, 1, 'x')]), /代理对/);
});

test('bounded selection snapshot reports absolute offset and truncation without losing selection', () => {
    const text = 'a'.repeat(100000);
    const snapshot = protocol.boundedDocumentText(text, { start: position(0, 80000), end: position(0, 80005) });
    assert.equal(snapshot.content.length, 32000);
    assert.equal(snapshot.selectedText, 'aaaaa');
    assert(snapshot.contentOffset > 0); assert.equal(snapshot.truncated, true);
    assert.equal(protocol.boundedDocumentText(text, null, 2000000).content.length, text.length);
});

test('document snapshot reads unsaved native buffer without saving, with full content hash', async () => {
    const fixture = bridgeFixture();
    const buffer = fixture.buffer('/project/a.ts', 'unsaved function');
    fixture.service.editors.currentEditor = { editor: { uri: uri('/project/a.ts'), document: { uri: 'file:///project/a.ts' }, selection: { start: position(0, 8), end: position(0, 16) } } };
    const snapshot = await fixture.service.documentSnapshot();
    assert.equal(snapshot.content, 'unsaved function'); assert.equal(snapshot.selectedText, 'function');
    assert.equal(snapshot.hash, await protocol.textHash('unsaved function'));
    assert.equal(snapshot.dirty, true); assert.equal(snapshot.version, buffer.version);
    assert.equal(fixture.disk.get('/project/a.ts'), 'old'); assert.equal(fixture.log.length, 0);
    const missing = await fixture.service.documentSnapshot('/project/new.ts');
    assert.equal(missing.hash, 'missing'); assert.equal(missing.exists, false);
});

test('native current-editor context survives composer focus and is initially announced after ready', async () => {
    const fixture = bridgeFixture(); const model=fixture.buffer('/project/a.ts','unsaved function');
    const widget={saveable:model,editor:{uri:uri('/project/a.ts'),selection:{start:position(0,8),end:position(0,16)},cursor:position(0,8),
        document:{uri:'file:///project/a.ts',getText:()=> 'function',getLineContent:()=>model.getText()},
        onCursorPositionChanged:event(),onSelectionChanged:event()}};
    fixture.service.editors.currentEditor=widget;
    const frontend=frontendFixture(fixture.service,widget);
    await new Promise(resolve=>setImmediate(resolve));
    assert(frontend.notifications.some(message=>message.type==='echo/active-file'&&message.path==='/project/a.ts'));
    frontend.activeChanged.fire(undefined); // External composer took focus; current editor remains the last used document.
    assert.equal(frontend.notifications.filter(message=>message.type==='echo/active-file').at(-1).path,'/project/a.ts');
    const snapshot=await fixture.service.documentSnapshot();
    assert.equal(snapshot.selectedText,'function');assert.equal(snapshot.content,'unsaved function');assert.equal(snapshot.dirty,true);
    const context=await fixture.service.contextSnapshot();
    assert.equal(context.openDocuments.length,1);assert.equal(context.openDocuments[0].uri,context.document.uri);
    assert.equal(context.openDocuments[0].version,context.document.version);assert.equal(context.openDocuments[0].hash,context.document.hash);
    assert.equal(context.openDocuments[0].dirty,true);assert.equal(context.openDocuments[0].selectedText,'function');
    assert.equal(fixture.disk.get('/project/a.ts'),'old');
});

test('openDocuments includes the current dirty document from one capture even if EditorManager.all lags or typing advances afterward', async () => {
    const fixture=bridgeFixture();const model=fixture.buffer('/project/a.ts','unsaved function');
    fixture.service.editors.currentEditor={editor:{document:{uri:'file:///project/a.ts'},selection:{start:position(0,8),end:position(0,16)}}};
    const capture=fixture.service.documentSnapshot.bind(fixture.service);let captures=0;
    fixture.service.documentSnapshot=async(...args)=>{
        captures++;const snapshot=await capture(...args);model.type('newer user typing');return snapshot;
    };
    const context=await fixture.service.contextSnapshot();
    assert.equal(captures,1);assert.equal(context.openDocuments.length,1);
    assert.equal(context.document.version,4);assert.equal(model.version,5);
    assert.equal(context.openDocuments[0].version,context.document.version);
    assert.equal(context.openDocuments[0].hash,context.document.hash);
    assert.equal(context.openDocuments[0].capturedAt,context.document.capturedAt);
    assert.equal(context.openDocuments[0].content,'unsaved function');assert.equal(context.openDocuments[0].selectedText,'function');
    assert.equal(context.openDocuments[0].dirty,true);assert.equal(fixture.disk.get('/project/a.ts'),'old');
});

test('get-dirty cannot acknowledge a leave guard while a native Agent save RPC is still in flight', async () => {
    const fixture=bridgeFixture(); const model=fixture.buffer('/project/a.ts','old');
    let finish; let saving=false;
    model.save=async()=>new Promise(resolve=>{saving=true;finish=()=>{fixture.disk.set('/project/a.ts',model.getText());model.dirty=false;resolve();};});
    const request=await operation('/project/a.ts','old',[edit(0,0,3,'agent')]);request.files[0].save=true;
    const applying=fixture.service.applyOperation(request);
    while(!saving) await new Promise(resolve=>setImmediate(resolve));
    const frontend=frontendFixture(fixture.service);
    frontend.messages[0]({data:{type:'echo/get-dirty',id:'leave-while-save'}});
    await new Promise(resolve=>setImmediate(resolve));
    assert.equal(frontend.notifications.some(message=>message.id==='leave-while-save'),false);
    finish(); await applying; await new Promise(resolve=>setImmediate(resolve));
    assert.equal(frontend.notifications.find(message=>message.id==='leave-while-save').value,0);
    assert.equal(fixture.disk.get('/project/a.ts'),'agent');
});

test('native diff uses the actual dirty model URI without saving or copying its undo state', async () => {
    const fixture = bridgeFixture(); const model = fixture.buffer('/project/a.ts', 'unsaved current');
    await fixture.service.openDiff('/project/a.ts', 'original baseline', 'Native review');
    const call = fixture.diffCalls[0];
    assert.equal(call.modified.toString(), 'file:///project/a.ts');
    assert.equal(call.original.scheme, 'echo-diff');
    assert.equal(fixture.diffResources.get(call.original.toString()).content, 'original baseline');
    assert.equal(fixture.diffResources.get(call.original.toString()).disposed, true);
    assert.equal(call.title, 'Native review');
    assert.equal(model.getText(), 'unsaved current'); assert.equal(model.editsApplied, 0);
    assert.equal(model.stackBoundaries, 0); assert.equal(model.dirty, true);
    assert.equal(fixture.disk.get('/project/a.ts'), 'old'); assert.equal(fixture.log.length, 0);
    fixture.service.editors.currentEditor = { editor: { uri: uri('diff:///synthetic'),
        document: { uri: 'file:///project/a.ts' }, selection: { start: position(0, 0), end: position(0, 7) } } };
    assert.equal((await fixture.service.documentSnapshot()).content, 'unsaved current');
});

test('deleted native diff has an empty read-only resource while an unsaved deleted buffer remains authoritative', async () => {
    const fixture = bridgeFixture({});
    await fixture.service.openDiff('/project/deleted.ts', 'removed content');
    assert.equal(fixture.diffCalls[0].modified.scheme, 'echo-diff');
    assert.equal(fixture.diffResources.get(fixture.diffCalls[0].modified.toString()).content, '');
    assert.equal(fixture.disk.has('/project/deleted.ts'), false);
    const model = fixture.buffer('/project/deleted.ts', 'surviving unsaved buffer');
    await fixture.service.openDiff('/project/deleted.ts', 'removed content');
    assert.equal(fixture.diffCalls[1].modified.toString(), 'file:///project/deleted.ts');
    model.dirty = false;
    await fixture.service.openDiff('/project/deleted.ts', 'removed content');
    assert.equal(fixture.diffCalls[2].modified.scheme, 'echo-diff');
    assert.equal(fixture.disk.has('/project/deleted.ts'), false);
});

test('native diff rejects binary and over-budget text and releases resource leases after an opener failure', async () => {
    const fixture = bridgeFixture();
    await assert.rejects(() => fixture.service.openDiff('/project/a.ts', 'a\0b'), /二进制/);
    await assert.rejects(() => fixture.service.openDiff('/project/a.ts', 'a'.repeat(2000001)), /预算/);
    fixture.buffer('/project/a.ts', 'a\0b');
    await assert.rejects(() => fixture.service.openDiff('/project/a.ts', 'original'), /二进制/);
    assert.equal(fixture.diffCalls.length, 0);
    fixture.buffer('/project/a.ts', 'text');
    fixture.service.diffs.openDiffEditor = async () => { throw new Error('opener failed'); };
    await assert.rejects(() => fixture.service.openDiff('/project/a.ts', 'original'), /opener failed/);
    assert.equal([...fixture.diffResources.values()].every(resource => resource.disposed), true);
});

test('native diff baseline resolver exposes an immutable, non-autosaving resource with a retained lease', async () => {
    let releases = 0;
    const { EchoDiffResources } = load('echo-diff-resources.ts', { ...sharedImports,
        '@theia/core/lib/common/resource': { InMemoryResources: class {
            resolve(resource) { return { uri: resource, readContents: async () => 'baseline',
                saveContents: async () => { throw new Error('must not expose saving'); }, dispose: () => releases++ }; }
        } }
    });
    const resources = new EchoDiffResources(); const resource = resources.resolve(uri('echo-diff:///project/a.ts?original-id'));
    assert.equal(resource.readOnly, true); assert.equal(resource.autosaveable, false);
    assert.equal(resource.saveContents, undefined); assert.equal(await resource.readContents(), 'baseline');
    assert.equal(releases, 0); resource.dispose(); assert.equal(releases, 1);
    assert.throws(() => resources.resolve(uri('/project/a.ts')), /Not an Echo diff/);
});

test('a full targeted snapshot above 32k remains complete and can back a real native edit', async () => {
    const fixture = bridgeFixture();
    const content = 'a'.repeat(100000);
    const buffer = fixture.buffer('/project/a.ts', content);
    const bounded = await fixture.service.documentSnapshot('/project/a.ts');
    const full = await fixture.service.documentSnapshot('/project/a.ts', 2000000);
    assert.equal(bounded.content.length, 32000); assert.equal(bounded.truncated, true);
    assert.equal(full.content, content); assert.equal(full.totalCharacters, 100000); assert.equal(full.truncated, false);
    assert.equal(full.hash, bounded.hash); assert.equal(full.hash, await protocol.textHash(content));
    const request = await operation('/project/a.ts', full.content, [edit(0, 99995, 100000, 'TAIL')]);
    request.files[0].expectedVersion = full.version;
    assert.equal((await fixture.service.applyOperation(request)).status, 'applied');
    assert.equal(buffer.getText(), `${'a'.repeat(99995)}TAIL`); assert.equal(buffer.editsApplied, 1);
});

test('stale version/hash never overwrites newer typing', async () => {
    const fixture = bridgeFixture(); const buffer = fixture.buffer('/project/a.ts', 'old');
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]);
    request.files[0].expectedVersion = buffer.version;
    buffer.type('user typed');
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.status, 'conflict'); assert.equal(buffer.getText(), 'user typed');
    assert.equal(buffer.editsApplied, 0); assert.equal(fixture.disk.get('/project/a.ts'), 'old');
});

test('same operation in parallel/retry/reconnect applies once and keeps native undo boundaries', async () => {
    const fixture = bridgeFixture(); const buffer = fixture.buffer('/project/a.ts', 'old');
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]);
    const [first, second] = await Promise.all([fixture.service.applyOperation(request), fixture.service.applyOperation(request)]);
    assert.equal(first.status, 'applied'); assert.equal(second.status, 'applied');
    assert.equal(buffer.editsApplied, 1); assert.equal(buffer.stackBoundaries, 2);
    assert.equal(fixture.disk.get('/project/a.ts'), 'old'); assert.equal(buffer.dirty, true);
    const reconnect = bridgeFixture({ '/project/a.ts': 'agent' }, fixture.storage);
    assert.equal((await reconnect.service.applyOperation(request)).status, 'applied');
    assert.equal(reconnect.log.length, 0);
    await assert.rejects(() => fixture.service.applyOperation({ ...request, files: [{ ...request.files[0], edits: [edit(0, 0, 3, 'other')] }] }), /operationId/);
});

test('multi-file preflight checks all files first and reports partial application', async () => {
    const fixture = bridgeFixture({ '/project/a.ts': 'old', '/project/b.ts': 'user' });
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'new')]);
    request.files.push({ uri: '/project/b.ts', expectedHash: await protocol.textHash('stale'), edits: [edit(0, 0, 5, 'agent')] });
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.status, 'partial'); assert.equal(result.files[1].status, 'conflict');
    assert.equal(fixture.disk.get('/project/a.ts'), 'new'); assert.equal(fixture.disk.get('/project/b.ts'), 'user');
    assert(fixture.log.indexOf('read:file:///project/b.ts') < fixture.log.indexOf('write:file:///project/a.ts'));
});

test('typing after multi-file preflight is checked again in the synchronous native edit section', async () => {
    const fixture = bridgeFixture({ '/project/a.ts': 'old', '/project/b.ts': 'old' });
    const buffer = fixture.buffer('/project/a.ts', 'old');
    const originalRead = fixture.service.files.read.bind(fixture.service.files);
    fixture.service.files.read = async resource => { buffer.type('user typed'); return originalRead(resource); };
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]);
    request.files.push({ uri: '/project/b.ts', expectedHash: await protocol.textHash('old'), edits: [edit(0, 0, 3, 'agent')] });
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.status, 'partial'); assert.equal(result.files[0].status, 'conflict');
    assert.equal(buffer.getText(), 'user typed'); assert.equal(buffer.editsApplied, 0);
});

test('a model that becomes read-only during multi-file preflight is never pushed into native undo', async () => {
    const fixture = bridgeFixture({ '/project/a.ts': 'old', '/project/b.ts': 'old' });
    const buffer = fixture.buffer('/project/a.ts', 'old');
    const originalRead = fixture.service.files.read.bind(fixture.service.files);
    fixture.service.files.read = async resource => { buffer.readOnly = true; return originalRead(resource); };
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]);
    request.files.push({ uri: '/project/b.ts', expectedHash: await protocol.textHash('old'), edits: [edit(0, 0, 3, 'agent')] });
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.status, 'partial'); assert.equal(result.files[0].status, 'rejected');
    assert.equal(buffer.getText(), 'old'); assert.equal(buffer.editsApplied, 0); assert.equal(buffer.stackBoundaries, 0);
});

test('save:true persists native edits and save failures retain recoverable buffer with honest result', async () => {
    const fixture = bridgeFixture(); const buffer = fixture.buffer('/project/a.ts', 'old');
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]); request.files[0].save = true;
    buffer.save = async () => { throw new Error('external disk conflict'); };
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.status, 'applied'); assert.match(result.files[0].saveError, /external disk conflict/);
    assert.equal(buffer.getText(), 'agent'); assert.equal(buffer.dirty, true); assert.equal(fixture.disk.get('/project/a.ts'), 'old');
    assert.equal(result.files[0].hash, await protocol.textHash('agent')); assert.equal(buffer.stackBoundaries, 2);
});

test('save:true verifies disk persistence, including a swallowed Monaco OutOfSync result', async () => {
    const fixture = bridgeFixture(); const buffer = fixture.buffer('/project/a.ts', 'old');
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]); request.files[0].save = true;
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.files[0].saveError, undefined); assert.equal(fixture.disk.get('/project/a.ts'), 'agent');
    buffer.save = async () => undefined;
    const next = await operation('/project/a.ts', 'agent', [edit(0, 0, 5, 'newer')]); next.files[0].save = true;
    const failed = await fixture.service.applyOperation(next);
    assert.equal(failed.status, 'applied'); assert.match(failed.files[0].saveError, /尚未保存/);
    assert.equal(fixture.disk.get('/project/a.ts'), 'agent'); assert.equal(buffer.getText(), 'newer');
});

test('typing during native save has its own final hash and is never attributed to the Agent checkpoint', async () => {
    const fixture = bridgeFixture(); const model = fixture.buffer('/project/a.ts', 'old');
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]); request.files[0].save = true;
    model.save = async () => { await Promise.resolve(); model.type('agent plus USER input'); };
    const result = (await fixture.service.applyOperation(request)).files[0];
    assert.equal(result.status, 'applied'); assert.match(result.saveError, /保存/);
    assert.equal(result.checkpointHash, await protocol.textHash('agent'));
    assert.equal(result.hash, await protocol.textHash('agent plus USER input'));
    assert.notEqual(result.hash, result.checkpointHash); assert.equal(result.version, model.version);
    assert.equal(model.getText(), 'agent plus USER input'); assert.equal(fixture.disk.get('/project/a.ts'), 'old');
});

test('missing-file edit uses exclusive create and refuses an existing target', async () => {
    const fixture = bridgeFixture();
    const request = { workspace: '/project', sessionId: 'session', runId: 'run', operationId: 'create',
        files: [{ uri: '/project/new.ts', expectedHash: 'missing', edits: [edit(0, 0, 0, 'new file')] }] };
    const result = await fixture.service.applyOperation(request);
    assert.equal(result.status, 'applied'); assert.equal(fixture.disk.get('/project/new.ts'), 'new file');
    const conflict = await fixture.service.applyOperation({ ...request, operationId: 'second-create' });
    assert.equal(conflict.status, 'conflict'); assert.equal(fixture.disk.get('/project/new.ts'), 'new file');
});

test('cancel fences native edits during preflight and rejects every subsequent operation from that run', async () => {
    const fixture = bridgeFixture({ '/project/a.ts': 'old', '/project/b.ts': 'old' });
    const buffer = fixture.buffer('/project/a.ts', 'old');
    let release; let started;
    const start = new Promise(resolve => { started = resolve; });
    const read = fixture.service.files.read.bind(fixture.service.files);
    fixture.service.files.read = async resource => {
        started(); await new Promise(resolve => { release = resolve; }); return read(resource);
    };
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]);
    request.files.push({ uri: '/project/b.ts', expectedHash: await protocol.textHash('old'), edits: [edit(0, 0, 3, 'agent')] });
    const pending = fixture.service.applyOperation(request); await start;
    let acknowledged = false; const stopped = fixture.service.cancelRun('run').then(() => { acknowledged = true; });
    await Promise.resolve(); assert.equal(acknowledged, false); release();
    const result = await pending; await stopped;
    assert.equal(result.status, 'rejected'); assert.equal(buffer.getText(), 'old'); assert.equal(buffer.editsApplied, 0);
    await assert.rejects(() => fixture.service.applyOperation({ ...request, operationId: 'late' }), /已停止/);
});

test('cancel ACK waits for a dispatched disk write, while later files remain untouched', async () => {
    const fixture = bridgeFixture({ '/project/a.ts': 'old', '/project/b.ts': 'old' });
    let release; let started;
    const start = new Promise(resolve => { started = resolve; });
    fixture.service.files.writeTextFileConditional = async (resource, _hash, text, assertCurrent) => {
        assertCurrent(); started(); await new Promise(resolve => { release = resolve; });
        fixture.disk.set(resource.path.fsPath(), text);
    };
    const request = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'agent')]);
    request.files.push({ uri: '/project/b.ts', expectedHash: await protocol.textHash('old'), edits: [edit(0, 0, 3, 'agent')] });
    const pending = fixture.service.applyOperation(request); await start;
    let acknowledged = false; const stopped = fixture.service.cancelRun('run').then(() => { acknowledged = true; });
    await Promise.resolve(); assert.equal(acknowledged, false); release();
    const result = await pending; await stopped;
    assert.equal(result.status, 'partial'); assert.equal(fixture.disk.get('/project/a.ts'), 'agent');
    assert.equal(fixture.disk.get('/project/b.ts'), 'old'); assert.equal(acknowledged, true);
});

test('context budget includes duplicated selection text and does not leak unbounded terminal output at zero budget', async () => {
    const fixture = bridgeFixture();
    for (let index = 0; index < 5; index++) {
        const path = `/project/${index}.ts`;
        fixture.buffer(path, 'a'.repeat(index === 0 ? 32000 : 8000));
        fixture.service.editors.all.push({ editor: { uri: uri(path), document: { uri: uri(path).toString() } } });
    }
    fixture.service.editors.currentEditor = { editor: { uri: uri('/project/0.ts'), document: { uri: 'file:///project/0.ts' },
        selection: { start: position(0, 0), end: position(0, 32000) } } };
    fixture.service.terminals.currentTerminal = { id: 'terminal', getSelection: () => '',
        buffer: { length: 80, getLines: () => ['output'.repeat(10000)] }, lastCwd: { path: { fsPath: () => '/project' } } };
    const context = await fixture.service.contextSnapshot();
    const all = [context.document, ...context.openDocuments];
    assert.equal(all.reduce((sum, snapshot) => sum + snapshot.content.length + snapshot.selectedText.length, 0), 96000);
    assert.equal(context.terminal.text, ''); assert.equal(context.terminal.truncated, true); assert.equal(context.truncated, true);
});

test('selected Problems and terminal selection preserve native source, range, cwd and unknown diagnostic version', async () => {
    const fixture = bridgeFixture();
    fixture.service.selections = { selection: [{ marker: { kind: 'problem', uri: 'file:///project/a.ts', owner: 'typescript',
        data: { message: 'selected error', severity: 1, source: 'ts', range: { start: position(2, 4), end: position(2, 8) } } } }] };
    fixture.service.terminals.currentTerminal = { id: 'terminal', getSelection: () => 'failed command output',
        buffer: { length: 0, getLines: () => [] }, lastCwd: { path: { fsPath: () => '/project/subdirectory' } } };
    const context = await fixture.service.contextSnapshot();
    assert.equal(context.diagnostics.length, 1); assert.equal(context.diagnostics[0].selected, true);
    assert.equal(context.diagnostics[0].range.start.line, 2); assert.equal(context.diagnostics[0].version, null);
    assert.equal(context.terminal.source, 'terminal-selection'); assert.equal(context.terminal.cwd, '/project/subdirectory');
    assert.equal(context.terminal.text, 'failed command output');
});

function terminalWithCwd(getCwd, lastPath = '') {
    return { id: 'terminal-cwd', getSelection: () => '', buffer: { length: 0, getLines: () => [] },
        lastCwd: { path: { fsPath: () => lastPath } }, get cwd() { return getCwd(); } };
}

test('terminal context awaits the actual asynchronous cwd getter and prefers fresh shell cwd over stale lastCwd', async () => {
    const fixture = bridgeFixture(); let release; let reads = 0;
    const fresh = new Promise(resolve => { release = resolve; });
    fixture.service.terminals.currentTerminal = terminalWithCwd(() => { reads++; return fresh; }, '/project/old');
    let settled = false; const snapshot = fixture.service.contextSnapshot().then(value => { settled = true; return value; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(reads, 1); assert.equal(settled, false);
    release(uri('file:///project/new-subdirectory'));
    assert.equal((await snapshot).terminal.cwd, '/project/new-subdirectory');
});

test('failed terminal cwd requests use only a nonempty lastCwd and never substitute the workspace', async () => {
    for (const [lastPath, expected] of [['/project/last-known', '/project/last-known'], ['', null]]) {
        const fixture = bridgeFixture();
        fixture.service.terminals.currentTerminal = terminalWithCwd(() => Promise.reject(new Error('terminal is not started')), lastPath);
        assert.equal((await fixture.service.contextSnapshot()).terminal.cwd, expected);
    }
});

test('an empty default URI from terminal cwd is reported as null when no previous directory is known', async () => {
    const fixture = bridgeFixture();
    fixture.service.terminals.currentTerminal = terminalWithCwd(async () => ({path:{fsPath:()=>''}}));
    assert.equal((await fixture.service.contextSnapshot()).terminal.cwd, null);
});

test('a hung terminal cwd getter is bounded and falls back to the last real directory', { timeout: 4000 }, async () => {
    const fixture = bridgeFixture();
    fixture.service.terminals.currentTerminal = terminalWithCwd(() => new Promise(() => {}), '/project/last-known');
    assert.equal((await fixture.service.contextSnapshot()).terminal.cwd, '/project/last-known');
});

test('context rejects a workspace switch while awaiting the real terminal cwd', async () => {
    const fixture = bridgeFixture(); let release; let root = '/project';
    fixture.service.workspacePath = async () => root;
    const fresh = new Promise(resolve => { release = resolve; });
    fixture.service.terminals.currentTerminal = terminalWithCwd(() => fresh);
    const snapshot = fixture.service.contextSnapshot();
    await new Promise(resolve => setImmediate(resolve));
    root = '/other-project'; release(uri('file:///project/shell-directory'));
    await assert.rejects(snapshot, /工作区已切换/);
});

function event() {
    const listeners = new Set();
    const subscribe = listener => { listeners.add(listener); return { dispose: () => listeners.delete(listener) }; };
    subscribe.fire = value => listeners.forEach(listener => listener(value)); return subscribe;
}
function inlineFixture(request) {
    const notifications = []; const writes = []; const contentChanged = event(); const cursorChanged = event(); const blurred = event(); const keydown = event();
    let version = 2; let cursor = { lineNumber: 1, column: 4, equals(other) { return this.lineNumber === other.lineNumber && this.column === other.column; } };
    let provider;
    const model = { uri: { toString: () => 'file:///project/a.ts', fsPath: '/project/a.ts' }, isDisposed: () => false,
        getValueLength: () => 3, getValue: () => 'old', getVersionId: () => version, getOffsetAt: () => 3, getLanguageId: () => 'typescript',
        setValue: (...args) => writes.push(['setValue', ...args]), applyEdits: (...args) => writes.push(['applyEdits', ...args]),
        pushEditOperations: (...args) => writes.push(['pushEditOperations', ...args]) };
    const control = { onDidChangeCursorPosition: cursorChanged, onDidChangeModelContent: contentChanged,
        onDidBlurEditorText: blurred, onKeyDown: keydown, updateOptions() {}, hasTextFocus: () => true,
        getPosition: () => cursor, getSelection: () => ({ isEmpty: () => true }), trigger() {},
        executeEdits: (...args) => writes.push(['executeEdits', ...args]) };
    const editor = { document: { textEditorModel: model, readOnly: false }, getControl: () => control };
    const editors = { currentEditor: { editor }, onCurrentEditorChanged: event(), onActiveEditorChanged: event() };
    const bridge = { enabled: true, notify: (type, payload) => notifications.push({ type, ...payload }), request };
    const monaco = { languages: { InlineCompletionTriggerKind: { Automatic: 0, Explicit: 1 }, InlineCompletionEndOfLifeReasonKind: { Accepted: 0, Rejected: 1 },
        registerInlineCompletionsProvider(_selector, registered) { provider = registered; return { dispose() {} }; } },
        KeyCode: { Escape: 9 }, Range: class { constructor(...values) { this.values = values; } } };
    const { EchoInlineCompletion } = load('echo-inline-completion.ts', { ...sharedImports,
        '@theia/monaco-editor-core': monaco,
        '@theia/monaco/lib/browser/monaco-editor': { MonacoEditor: { get: widget => widget?.editor } },
        './echo-host-bridge': { echoHostBridge: bridge }
    });
    const completion = new EchoInlineCompletion(editors, async () => '/project'); completion.setEnabled(true);
    const token = { isCancellationRequested: false, onCancellationRequested: event() };
    return { completion, provider, model, notifications, token, editors, control, writes,
        provide: () => provider.provideInlineCompletions(model, cursor, { triggerKind: 1 }, token),
        type() { version++; contentChanged.fire(); }, move() { cursor = { ...cursor, column: 3 }; cursorChanged.fire(); },
        escape() { keydown.fire({ keyCode: 9 }); } };
}

test('native inline provider requests bounded model completion and emits acceptance evidence without writes', async () => {
    let requests = 0;
    const fixture = inlineFixture(async (type, payload) => {
        requests++; assert.equal(type, 'echo/inline-completion'); assert.equal(payload.prefix, 'old');
        assert.equal(payload.version, 2); assert.equal(payload.maxTokens, 256); return { text: '\nnext line' };
    });
    const result = await fixture.provide();
    assert.equal(result.items[0].insertText, '\nnext line');
    fixture.provider.handleItemDidShow(result);
    fixture.provider.handleEndOfLifetime(result, result.items[0], { kind: 0 });
    assert(fixture.notifications.some(item => item.type === 'echo/inline-completion-shown' && item.latencyMs >= 0));
    assert(fixture.notifications.some(item => item.type === 'echo/inline-completion-accepted' && item.requestId === result.requestId));
    const cached = await fixture.provide(); assert.equal(requests, 1, 'unchanged snapshot uses bounded cache');
    assert.equal(cached.requestId,result.requestId,'cached acceptance retains its real model request identity');
    assert(fixture.notifications.some(item=>item.type==='echo/inline-completion-accepted'&&item.reason==='accepted'));
    fixture.completion.setEnabled(true); await fixture.provide(); assert.equal(requests,2,'model/config switch invalidates earlier cache');
    fixture.completion.dispose();
});

for (const action of ['type', 'move', 'escape']) test(`inline result is cancelled/discarded after ${action}`, async () => {
    let release; let started;
    const start = new Promise(resolve => { started = resolve; });
    const fixture = inlineFixture((_type, _payload, { signal }) => new Promise((resolve, reject) => {
        release = resolve; signal.addEventListener('abort', () => reject(new Error('cancelled')), { once: true }); started();
    }));
    const pending = fixture.provide(); await start; fixture[action](); release({ text: 'stale' });
    assert.equal((await pending).items.length, 0);
    assert(fixture.notifications.some(item => item.type === 'echo/cancel-inline-completion'));
    assert(fixture.notifications.some(item => item.type === 'echo/inline-completion-discarded'));
    fixture.completion.dispose();
});

test('switching the current EditorWidget to another model discards a late completion even when its transport ignores abort', async () => {
    let release; let started; let requestCount = 0; let firstRequest;
    const start = new Promise(resolve => { started = resolve; });
    const fixture = inlineFixture((_type, payload, { signal }) => {
        requestCount++;
        if (requestCount > 1) {
            assert.equal(payload.uri, 'file:///project/b.ts');
            return Promise.resolve({ text: 'fresh completion for B' });
        }
        firstRequest = { ...payload, signal };
        // Deliberately resolve after cancellation to exercise the provider's
        // editor/model identity fence rather than a cooperative transport.
        return new Promise(resolve => { release = resolve; started(); });
    });
    const pending = fixture.provide(); await start;
    const otherModel = { ...fixture.model, uri: { toString: () => 'file:///project/b.ts', fsPath: '/project/b.ts' },
        getValue: () => 'other file', getValueLength: () => 10, getVersionId: () => 8 };
    const otherControl = { ...fixture.control, onDidChangeCursorPosition: event(), onDidChangeModelContent: event(),
        onDidBlurEditorText: event(), onKeyDown: event() };
    const otherWidget = { editor: { document: { textEditorModel: otherModel, readOnly: false }, getControl: () => otherControl } };
    // EditorManager emits EditorWidget, and MonacoEditor.get unwraps .editor.
    fixture.editors.currentEditor = otherWidget;
    fixture.editors.onCurrentEditorChanged.fire(otherWidget);
    assert.equal(firstRequest.signal.aborted, true);
    release({ text: 'STALE_COMPLETION_FOR_A' });
    assert.equal((await pending).items.length, 0, 'Monaco receives no stale item to display or accept');
    assert(fixture.notifications.some(item => item.type === 'echo/cancel-inline-completion' && item.requestId === firstRequest.requestId));
    assert(fixture.notifications.some(item => item.type === 'echo/inline-completion-discarded' && item.requestId === firstRequest.requestId && item.reason === 'expired'));
    assert(!fixture.notifications.some(item => ['echo/inline-completion-shown', 'echo/inline-completion-accepted'].includes(item.type)));
    const next = await fixture.provider.provideInlineCompletions(otherModel, otherControl.getPosition(), { triggerKind: 1 }, fixture.token);
    assert.equal(next.items[0].insertText, 'fresh completion for B');
    assert.equal(requestCount, 2);
    assert.equal(fixture.model.getValue(), 'old'); assert.equal(otherModel.getValue(), 'other file');
    assert.equal(fixture.writes.length, 0, 'neither native model nor editor was written');
    fixture.completion.dispose();
});

test('disable cancels inline request immediately and failed model requests leave ordinary editing available', async () => {
    let requests = 0;
    const fixture = inlineFixture(async () => { requests++; throw new Error('no configured model'); });
    assert.equal((await fixture.provide()).items.length, 0);
    assert(fixture.notifications.some(item => item.type === 'echo/inline-completion-error'));
    fixture.type(); fixture.completion.setEnabled(false);
    assert.equal((await fixture.provide()).items.length, 0); assert.equal(requests, 1); fixture.completion.dispose();
});

test('automatic inline requests debounce and honor the per-minute request budget', async () => {
    let requests = 0;
    const fixture = inlineFixture(async () => { requests++; return { text: 'completion' }; });
    const first = fixture.provider.provideInlineCompletions(fixture.model, { lineNumber: 1, column: 4 }, { triggerKind: 0 }, fixture.token);
    const second = fixture.provider.provideInlineCompletions(fixture.model, { lineNumber: 1, column: 4 }, { triggerKind: 0 }, fixture.token);
    assert.equal((await first).items.length, 0); assert.equal((await second).items.length, 1); assert.equal(requests, 1);
    for (let index = 0; index < 13; index++) { fixture.type(); await fixture.provide(); }
    assert.equal(requests, 12); fixture.completion.dispose();
});

test('host bridge authenticates source/origin/token/instance and cancels pending requests', async () => {
    let listener; const sent = [];
    const parent = { postMessage: message => sent.push(message) };
    const hostWindow = { parent, addEventListener: (_event, value) => { listener = value; }, setTimeout, clearTimeout };
    const { EchoHostBridge } = load('echo-host-bridge.ts', {
        '@theia/core/lib/browser/echo-embed-session': { echoEmbedSession: { bridgeToken: 'secret', parentOrigin: 'https://app.example' } }
    }, { window: hostWindow });
    const bridge = new EchoHostBridge();
    const controller = new AbortController();
    const pending = bridge.request('echo/inline-completion', {}, { signal: controller.signal });
    const request = sent.at(-1);
    const respond = (options = {}) => listener({ source: parent, origin: 'https://app.example',
        data: { type: 'echo/response', id: request.id, ok: true, value: { text: 'accepted' }, token: 'secret', instanceId: bridge.instanceId }, ...options });
    respond({ origin: 'https://evil.example' });
    respond({ source: {} });
    respond({ data: { type: 'echo/response', id: request.id, ok: true, token: 'wrong', instanceId: bridge.instanceId } });
    respond({ data: { type: 'echo/response', id: request.id, ok: true, token: 'secret', instanceId: 'old' } });
    respond({ data: { type: 'echo/response', id: request.id, ok: true, token: 'secret' } });
    assert.equal(bridge.pending.size, 1); respond(); assert.equal((await pending).text, 'accepted');
    const next = bridge.request('echo/inline-completion', {}, { signal: controller.signal });
    controller.abort(); await assert.rejects(next, /取消/); assert.equal(bridge.pending.size, 0);
});

function conditionalFileService({enabled=false,beforeMutation=async()=>undefined,changeDuringEncode,delayWrite=false} = {}) {
    let disk = 'old'; let mtime = 100; let writes = 0; let fallbacks = 0;
    const etag = stat => `${stat.mtime}:${stat.size}`;
    class BaseFileService {
        modifiedSince() { fallbacks++; return false; }
        async read() { return {value:disk,mtime,etag:etag({mtime,size:disk.length}),encoding:'utf8'}; }
        async getWriteEncoding() { return {encoding:'utf8',hasBOM:false}; }
        async writeFile(_resource,encoded,options) {
            if (this.modifiedSince({mtime,size:disk.length},options)) { throw new FileError('modified since',fileResults.FILE_MODIFIED_SINCE); }
            if (delayWrite) { await new Promise(resolve=>setTimeout(resolve,5)); }
            writes++;disk=encoded;mtime++;
        }
    }
    const {EchoFileService:ConditionalService} = load('echo-file-service.ts',{
        ...sharedImports,
        '@theia/filesystem/lib/browser/file-service':{FileService:BaseFileService},
        '@theia/filesystem/lib/common/files':{...sharedImports['@theia/filesystem/lib/common/files'],ETAG_DISABLED:'',etag},
        './echo-host-bridge':{echoHostBridge:{enabled,request:type=>type==='echo/before-mutation'?beforeMutation():Promise.resolve(undefined)}}
    });
    const service = new ConditionalService();
    service.encodingService = {encodeStream:async text=>{
        if (changeDuringEncode) {disk=changeDuringEncode;mtime++;}
        return text;
    }};
    return {service,etag,writes:()=>writes,fallbacks:()=>fallbacks,disk:()=>disk,
        externalWrite(value,nextMtime=mtime+1) {disk=value;mtime=nextMtime;} };
}

test('strict dirty-write prevention detects same-size external edits and older restored mtimes', () => {
    const fixture = conditionalFileService();
    const expected = {mtime:100,etag:fixture.etag({mtime:100,size:3})};
    assert.equal(fixture.service.modifiedSince({mtime:100,size:3},expected),false);
    assert.equal(fixture.service.modifiedSince({mtime:101,size:3},expected),true);
    assert.equal(fixture.service.modifiedSince({mtime:99,size:3},expected),true);
    assert.equal(fixture.service.modifiedSince({mtime:100,size:4},expected),true);
    assert.equal(fixture.service.modifiedSince({mtime:101,size:3},{mtime:100,etag:''}),false);
    assert.equal(fixture.service.modifiedSince({mtime:101,size:3},undefined),false);
    assert.equal(fixture.fallbacks(),2);
});

test('conditional file write rechecks the real hash after asynchronous host authorization', async () => {
    let release;
    const fixture = conditionalFileService({enabled:true,beforeMutation:()=>new Promise(resolve=>{release=resolve;})});
    const pending = fixture.service.writeTextFileConditional(uri('/project/a.ts'),await protocol.textHash('old'),'agent');
    const rejected = assert.rejects(pending,error=>error.fileOperationResult===fileResults.FILE_MODIFIED_SINCE);
    fixture.externalWrite('usr');release({});await rejected;
    assert.equal(fixture.disk(),'usr');assert.equal(fixture.writes(),0);
});

test('same-path conditional writes serialize the full hash/stat/write transaction before touching disk', async () => {
    const fixture = conditionalFileService({delayWrite:true});
    const hash = await protocol.textHash('old');
    const results = await Promise.allSettled([
        fixture.service.writeTextFileConditional(uri('/project/a.ts'),hash,'one'),
        fixture.service.writeTextFileConditional(uri('/project/a.ts'),hash,'two')
    ]);
    assert.equal(results.filter(result=>result.status==='fulfilled').length,1);
    assert.equal(results.filter(result=>result.status==='rejected'&&result.reason.fileOperationResult===fileResults.FILE_MODIFIED_SINCE).length,1);
    assert.equal(fixture.writes(),1);assert.equal(fixture.disk(),'one');assert.equal(fixture.service.activeWrites.size,0);
});

test('same-size external edits during encoding are rejected by the final native save version check', async () => {
    const fixture = conditionalFileService({changeDuringEncode:'usr'});
    const hash = await protocol.textHash('old');
    await assert.rejects(()=>fixture.service.writeTextFileConditional(uri('/project/a.ts'),hash,'agent'),
        error=>error.fileOperationResult===fileResults.FILE_MODIFIED_SINCE);
    assert.equal(fixture.disk(),'usr');assert.equal(fixture.writes(),0);
});

test('versioned delete preserves dirty buffers and detects typing after preflight', async () => {
    const fixture = bridgeFixture();
    const model = fixture.buffer('/project/a.ts', 'unsaved');
    const dirtyDelete = await operation('/project/a.ts', 'unsaved', []);
    dirtyDelete.files[0].delete = true;
    dirtyDelete.files[0].expectedVersion = model.version;
    assert.equal((await fixture.service.applyOperation(dirtyDelete)).files[0].status, 'rejected');
    assert.equal(fixture.disk.get('/project/a.ts'), 'old');
    model.type('old'); model.dirty = false;
    const lateDelete = await operation('/project/a.ts', 'old', []);
    lateDelete.files[0].delete = true;
    lateDelete.files[0].expectedVersion = model.version;
    const remove = fixture.service.files.deleteTextFileConditional.bind(fixture.service.files);
    fixture.service.files.deleteTextFileConditional = (...args) => { model.type('late typing'); return remove(...args); };
    assert.equal((await fixture.service.applyOperation(lateDelete)).files[0].status, 'conflict');
    assert.equal(fixture.disk.get('/project/a.ts'), 'old');
    assert.equal(model.getText(), 'late typing');
});

test('patch operations move and delete clean files and report both sides of a move', async () => {
    const fixture = bridgeFixture();
    const moving = await operation('/project/a.ts', 'old', [edit(0, 0, 3, 'updated')]);
    moving.files[0].moveTo = '/project/moved.ts'; moving.files[0].save = true;
    const moved = (await fixture.service.applyOperation(moving)).files[0];
    assert.equal(moved.status, 'applied'); assert.equal(moved.hash, 'missing');
    assert.equal(moved.movedTo, 'file:///project/moved.ts');
    assert.equal(moved.movedHash, await protocol.textHash('updated'));
    assert.equal(fixture.disk.has('/project/a.ts'), false);
    assert.equal(fixture.disk.get('/project/moved.ts'), 'updated');
    const deleting = await operation('/project/moved.ts', 'updated', []);
    deleting.files[0].delete = true;
    assert.equal((await fixture.service.applyOperation(deleting)).files[0].hash, 'missing');
    assert.equal(fixture.disk.size, 0);
});

test('patch moves never overwrite an existing destination or act without a source snapshot', async () => {
    const fixture = bridgeFixture({'/project/a.ts':'old','/project/b.ts':'user'});
    const moving = await operation('/project/a.ts', 'old', []);
    moving.files[0].moveTo = '/project/b.ts';
    assert.equal((await fixture.service.applyOperation(moving)).files[0].status, 'rejected');
    assert.equal(fixture.disk.get('/project/a.ts'), 'old'); assert.equal(fixture.disk.get('/project/b.ts'), 'user');
    const missing = await operation('/project/new.ts', '', []);
    missing.files[0].expectedHash = 'missing'; missing.files[0].delete = true;
    assert.equal((await fixture.service.applyOperation(missing)).files[0].status, 'rejected');
});

test('large dirty document ranges use current buffer text with absolute line numbers', async () => {
    const fixture = bridgeFixture();
    const content = Array.from({length:30000}, (_,i) => `line ${i+1} ${'x'.repeat(100)}`).join('\n');
    fixture.buffer('/project/a.ts', content);
    const snapshot = await fixture.service.documentSnapshot('/project/a.ts', 32000, {offset:25000,limit:2});
    assert.equal(snapshot.firstLine, 25000); assert.equal(snapshot.dirty, true);
    assert.equal(snapshot.content, content.split('\n').slice(24999,25001).join('\n'));
    assert.equal(snapshot.hash, await protocol.textHash(content));
    assert.equal(fixture.disk.get('/project/a.ts'), 'old');
});
