import assert from 'node:assert/strict';
import test from 'node:test';
import * as fs from 'node:fs';
import { mkdtemp, readFile, rm, stat, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';

const require = createRequire(import.meta.url);
class ProviderError extends Error { constructor(error, code) { super(String(error)); this.code = code; } }
class Collection { push() {} dispose() {} }
class Emitter { event = () => ({ dispose() {} }); fire() {} dispose() {} }
const decorator = () => () => undefined;

/** Production DiskFileSystemProvider methods run against real fs and real temporary files.
 * Only injected Theia services and the scheduling of an adversarial create are controlled. */
function providerFixture(beforeOpen) {
    const source = fs.readFileSync(new URL('../vendor/theia-platform/packages/filesystem/src/node/disk-file-system-provider.ts', import.meta.url), 'utf8');
    const js = ts.transpileModule(source, {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022,
        experimentalDecorators:true,emitDecoratorMetadata:false}}).outputText;
    let flushes = 0;
    const realFs = {...fs,
        open(path,flags,callback) { beforeOpen?.(path,flags); return fs.open(path,flags,callback); },
        fdatasync(fd,callback) { flushes++; return fs.fdatasync(fd,callback); },
    };
    const imports = {
        fs:realFs,
        '@theia/core/shared/inversify':{inject:decorator,injectable:()=>value=>value,postConstruct:decorator,named:decorator},
        '@theia/core':{ILogger:Symbol()},
        '@theia/core/lib/common/uuid':{generateUuid:()=> 'test'},
        '@theia/core/lib/common/uri':{default:class {}},
        '@theia/core/lib/common/path':{Path:class {}},
        '@theia/core/lib/common/file-uri':{FileUri:{fsPath:value=>value}},
        '@theia/core/lib/common/event':{Emitter,Event:{None:()=>({dispose(){}})}},
        '@theia/core/lib/common/disposable':{DisposableCollection:Collection},
        '@theia/core/lib/common/os':{isWindows:process.platform === 'win32',OS:{}},
        '@theia/core/lib/common/promise-util':{retry:async action=>action()},
        '../common/files':{FileSystemProviderError:ProviderError,
            FileSystemProviderErrorCode:{FileExists:'FileExists',FileNotFound:'FileNotFound',NoPermissions:'NoPermissions',Unknown:'Unknown'},
            createFileSystemProviderError:(error,code)=>new ProviderError(error,code)},
        '../common/filesystem-watcher-protocol':{FileSystemWatcherServer:Symbol()},
        trash:()=>undefined,
        'vscode-languageserver-textdocument':{},
        '@theia/core/lib/common/encoding-service':{EncodingService:class {}},
        '@theia/core/lib/common/buffer':{},
        '@theia/core/lib/common/stream':{},
        '../common/io':{},
        'stat-mode':{},
    };
    const exports = {};
    vm.runInNewContext(js,{exports,Buffer,process,console,require:id=>id in imports?imports[id]:require(id)});
    const provider = new exports.DiskFileSystemProvider();
    provider.logger = {error() {}};
    return {provider,flushes:()=>flushes};
}
async function temporary(run) {
    const directory = await mkdtemp(join(tmpdir(),'echo-exclusive-create-'));
    try { await run(directory); } finally { await rm(directory,{recursive:true,force:true}); }
}

test('parallel exclusive creates have exactly one real writer and leave no handles behind', () => temporary(async directory => {
    const {provider,flushes} = providerFixture();
    const path = join(directory,'new.ts');
    const texts = Array.from({length:12},(_,index)=>`writer-${index}`);
    const results = await Promise.allSettled(texts.map(text=>provider.writeFile(path,Buffer.from(text),{create:true,overwrite:false})));
    const winners = results.flatMap((result,index)=>result.status==='fulfilled'?[index]:[]);
    assert.equal(winners.length,1); assert.equal(await readFile(path,'utf8'),texts[winners[0]]);
    assert.equal(results.filter(result=>result.status==='rejected'&&result.reason.code==='FileExists').length,11);
    assert.equal(flushes(),1); assert.equal(provider.mapHandleToPos.size,0); assert.equal(provider.writeHandles.size,0);
    if (process.platform!=='win32') assert.equal((await stat(path)).mode&0o777,0o666&~process.umask());
}));

test('a file created immediately before the OS open is preserved instead of being truncated', () => temporary(async directory => {
    const path = join(directory,'race.ts'); let created = false;
    const {provider,flushes} = providerFixture((target)=>{
        if (!created && target===path) {created=true;fs.writeFileSync(path,'concurrent user contents');}
    });
    await assert.rejects(()=>provider.writeFile(path,Buffer.from('agent replacement'),{create:true,overwrite:false}),error=>error.code==='FileExists');
    assert.equal(await readFile(path,'utf8'),'concurrent user contents');
    assert.equal(flushes(),0); assert.equal(provider.mapHandleToPos.size,0); assert.equal(provider.writeHandles.size,0);
}));

test('ordinary overwrite preserves executable mode and retains the existing flush path', () => temporary(async directory => {
    const path = join(directory,'existing.sh'); fs.writeFileSync(path,'old');
    if (process.platform!=='win32') await chmod(path,0o751);
    const {provider,flushes} = providerFixture();
    await provider.writeFile(path,Buffer.from('updated'),{create:false,overwrite:true});
    assert.equal(await readFile(path,'utf8'),'updated'); assert.equal(flushes(),1);
    if (process.platform!=='win32') assert.equal((await stat(path)).mode&0o777,0o751);
    assert.equal(provider.mapHandleToPos.size,0); assert.equal(provider.writeHandles.size,0);
}));
