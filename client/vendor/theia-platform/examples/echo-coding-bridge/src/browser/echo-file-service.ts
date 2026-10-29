import { injectable } from '@theia/core/shared/inversify';
import { FileService } from '@theia/filesystem/lib/browser/file-service';
import URI from '@theia/core/lib/common/uri';
import { ETAG_DISABLED, FileOperationError, FileOperationResult, Stat, WriteFileOptions, etag, hasReadWriteCapability } from '@theia/filesystem/lib/common/files';
import { echoHostBridge } from './echo-host-bridge';
import { textHash } from './echo-editor-protocol';

/**
 * Keep Theia's file tree and editor, while letting EchoAgent's task orchestrator
 * approve a mutation before it reaches disk and reconcile its change set after.
 */
@injectable()
export class EchoFileService extends FileService {
    private readonly activeWrites = new Map<string, Promise<void>>();

    /** Include version validation in the per-file transaction; FileService's own queue starts after validation. */
    private async serializeWrite<T>(path: string, run: () => Promise<T>): Promise<T> {
        const slashed = path.replaceAll('\\', '/');
        const key = /^[a-z]:\//i.test(slashed) || slashed.startsWith('//') ? slashed.toLowerCase() : slashed;
        const previous = this.activeWrites.get(key) ?? Promise.resolve();
        let release!: () => void;
        const settled = new Promise<void>(resolve => { release = resolve; });
        this.activeWrites.set(key, settled);
        await previous;
        try { return await run(); }
        finally {
            if (this.activeWrites.get(key) === settled) { this.activeWrites.delete(key); }
            release();
        }
    }

    /** Same-size external edits and restored older mtimes are still conflicts. */
    protected override modifiedSince(stat: Stat, options?: WriteFileOptions): boolean {
        if (typeof options?.mtime === 'number' && typeof options.etag === 'string' && options.etag !== ETAG_DISABLED
            && typeof stat.mtime === 'number' && typeof stat.size === 'number') {
            return options.mtime !== stat.mtime || options.etag !== etag(stat);
        }
        return super.modifiedSince(stat, options);
    }

    /** FileService.createFile checks existence separately; use provider-exclusive create to avoid clobbering a concurrent create. */
    async createTextFileExclusive(resource: URI, text: string, assertCurrent?: () => void): Promise<void> {
        return this.mutate('write', [resource.path.fsPath()], async () => {
            const provider = this.throwIfFileSystemIsReadonly(await this.activateProvider(resource.scheme), resource);
            if (!hasReadWriteCapability(provider)) { throw new Error('当前文件系统不支持安全创建文本文件'); }
            assertCurrent?.();
            await super.createFolder(resource.parent, { fromUserGesture: false });
            assertCurrent?.();
            await provider.writeFile(resource, new TextEncoder().encode(text), { create: true, overwrite: false });
        });
    }

    /** Recheck the hash after the host's async mutation authorization, immediately before the conditional disk write. */
    async deleteTextFileConditional(resource: URI, expectedHash: string, assertCurrent: () => void): Promise<void> {
        return this.mutate('delete', [resource.path.fsPath()], async () => {
            const latest = await super.read(resource, { acceptTextOnly: true });
            if (await textHash(latest.value) !== expectedHash) {
                throw new FileOperationError('磁盘内容已变化，未删除文件', FileOperationResult.FILE_MODIFIED_SINCE);
            }
            assertCurrent();
            await super.delete(resource, { recursive: false });
        });
    }

    async moveTextFileConditional(resource: URI, destination: URI, expectedHash: string, assertCurrent: () => void): Promise<void> {
        return this.mutate('move', [resource.path.fsPath(), destination.path.fsPath()], async () => {
            const latest = await super.read(resource, { acceptTextOnly: true });
            if (await textHash(latest.value) !== expectedHash || await super.exists(destination)) {
                throw new FileOperationError('源文件已变化或目标已存在，未移动文件', FileOperationResult.FILE_MODIFIED_SINCE);
            }
            assertCurrent();
            await super.move(resource, destination, { overwrite: false });
        });
    }

    async writeTextFileConditional(resource: URI, expectedHash: string, text: string, assertCurrent?: () => void): Promise<void> {
        return this.mutate('write', [resource.path.fsPath()], async () => {
            const latest = await super.read(resource, { acceptTextOnly: true });
            if (await textHash(latest.value) !== expectedHash) {
                throw new FileOperationError('磁盘内容已变化，未覆盖外部修改', FileOperationResult.FILE_MODIFIED_SINCE);
            }
            const encoding = await this.getWriteEncoding(resource, { encoding: latest.encoding });
            const encoded = await this.encodingService.encodeStream(text, encoding);
            assertCurrent?.();
            await super.writeFile(resource, encoded, { mtime: latest.mtime, etag: latest.etag });
        });
    }

    private async mutate<T>(operation: string, paths: string[], run: () => Promise<T>): Promise<T> {
        const execute = () => operation === 'write' && paths[0] ? this.serializeWrite(paths[0], run) : run();
        if (!echoHostBridge.enabled || !paths.some(Boolean)) {
            return execute();
        }
        const ticket = await echoHostBridge.request('echo/before-mutation', { operation, paths });
        let result: T;
        try {
            result = await execute();
        } catch (error) {
            await echoHostBridge.request('echo/after-mutation', {
                ticket,
                success: false,
                error: String(error),
            }).catch(() => undefined);
            throw error;
        }
        // The disk write already succeeded. Reporting a sync failure as a save
        // failure makes Resource.save retry the same edit through writeFile.
        await echoHostBridge.request('echo/after-mutation', { ticket, success: true })
            .catch(error => console.warn('EchoAgent could not reconcile the saved file:', error));
        return result;
    }

    override async writeFile(...args: Parameters<FileService['writeFile']>): Promise<Awaited<ReturnType<FileService['writeFile']>>> {
        return this.mutate('write', [args[0].path.fsPath()], () => super.writeFile(...args));
    }

    // Monaco normally saves small edits through FileService.update, which goes
    // straight to provider.updateFile instead of calling writeFile.
    override async update(...args: Parameters<FileService['update']>): Promise<Awaited<ReturnType<FileService['update']>>> {
        return this.mutate('write', [args[0].path.fsPath()], () => super.update(...args));
    }

    override async createFolder(...args: Parameters<FileService['createFolder']>): Promise<Awaited<ReturnType<FileService['createFolder']>>> {
        return this.mutate('createFolder', [args[0].path.fsPath()], () => super.createFolder(...args));
    }

    override async move(...args: Parameters<FileService['move']>): Promise<Awaited<ReturnType<FileService['move']>>> {
        return this.mutate('move', [args[0].path.fsPath(), args[1].path.fsPath()], () => super.move(...args));
    }

    override async copy(...args: Parameters<FileService['copy']>): Promise<Awaited<ReturnType<FileService['copy']>>> {
        return this.mutate('copy', [args[0].path.fsPath(), args[1].path.fsPath()], () => super.copy(...args));
    }

    override async delete(...args: Parameters<FileService['delete']>): Promise<Awaited<ReturnType<FileService['delete']>>> {
        return this.mutate('delete', [args[0].path.fsPath()], () => super.delete(...args));
    }
}
