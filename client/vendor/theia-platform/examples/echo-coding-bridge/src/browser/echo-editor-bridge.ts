import { inject, injectable } from '@theia/core/shared/inversify';
import { FrontendApplicationContribution } from '@theia/core/lib/browser';
import URI from '@theia/core/lib/common/uri';
import { FileUri } from '@theia/core/lib/common/file-uri';
import { Disposable, DisposableCollection } from '@theia/core/lib/common/disposable';
import { SelectionService } from '@theia/core/lib/common/selection-service';
import type { Diagnostic } from '@theia/core/shared/vscode-languageserver-protocol';
import { EditorManager } from '@theia/editor/lib/browser/editor-manager';
import { FileService, TextFileContent } from '@theia/filesystem/lib/browser/file-service';
import { FileOperationError, FileOperationResult } from '@theia/filesystem/lib/common/files';
import { MonacoEditor } from '@theia/monaco/lib/browser/monaco-editor';
import { MonacoEditorModel } from '@theia/monaco/lib/browser/monaco-editor-model';
import { MonacoTextModelService } from '@theia/monaco/lib/browser/monaco-text-model-service';
import { TerminalService } from '@theia/terminal/lib/browser/base/terminal-service';
import { WorkspaceService } from '@theia/workspace/lib/browser';
import { DiffService } from '@theia/workspace/lib/browser/diff-service';
import * as monaco from '@theia/monaco-editor-core';
import { echoHostBridge } from './echo-host-bridge';
import {
    ApplyResult, CONTEXT_CHARACTER_BUDGET, DiagnosticSnapshot, DocumentEditTarget, DocumentRange,
    DocumentSnapshot, ECHO_EDITOR_CAPABILITIES, EditOperation, EditorContextSnapshot, FileApplyResult,
    MAX_EDIT_CHARACTERS, applyTextEdits, boundedDocumentText, summarizeApplyResult, textHash, validateTextEdits
} from './echo-editor-protocol';
import { EchoInlineCompletion } from './echo-inline-completion';
import { EchoFileService } from './echo-file-service';
import { EchoDiffResources } from './echo-diff-resources';

interface PreparedEdit {
    target: DocumentEditTarget;
    uri: URI;
    text: string;
    model?: MonacoEditorModel;
    disk?: TextFileContent;
    create?: boolean;
}
interface SelectedProblem { marker: { kind: string; uri: string; owner: string; data: Diagnostic }; }

/** One coordinator for buffer reads, native undo and conditional disk writes. */
@injectable()
export class EchoEditorBridge implements FrontendApplicationContribution {
    @inject(EditorManager) protected readonly editors: EditorManager;
    @inject(MonacoTextModelService) protected readonly models: MonacoTextModelService;
    @inject(FileService) protected readonly files: FileService;
    @inject(WorkspaceService) protected readonly workspace: WorkspaceService;
    @inject(TerminalService) protected readonly terminals: TerminalService;
    @inject(SelectionService) protected readonly selections: SelectionService;
    @inject(DiffService) protected readonly diffs: DiffService;
    @inject(EchoDiffResources) protected readonly diffResources: EchoDiffResources;
    protected readonly disposables = new DisposableCollection();
    protected readonly operations = new Map<string, { fingerprint: string; result: Promise<ApplyResult> }>();
    protected readonly cancelledRuns = new Set<string>();
    protected readonly activeRuns = new Map<string, Set<Promise<ApplyResult>>>();
    protected inline?: EchoInlineCompletion;

    onStart(): void {
        if (!echoHostBridge.enabled) { return; }
        this.inline = new EchoInlineCompletion(this.editors, () => this.workspacePath());
        this.disposables.push(this.inline);
        const listener = (event: MessageEvent) => {
            if (!echoHostBridge.isTrusted(event)) { return; }
            const message = event.data as Record<string, unknown>;
            if (message.type === 'echo/set-inline-completion' && message.instanceId === echoHostBridge.instanceId) {
                this.inline?.setEnabled(message.enabled === true);
                return;
            }
            if (message.type === 'echo/cancel-run' && typeof message.runId === 'string'
                && message.instanceId === echoHostBridge.instanceId) {
                void this.cancelRun(message.runId).then(value => {
                    if (typeof message.id === 'string') { echoHostBridge.notify('echo/response', { id: message.id, ok: true, value }); }
                });
                return;
            }
            if (!['echo/get-document', 'echo/get-context', 'echo/apply-edits', 'echo/open-diff'].includes(String(message.type))
                || typeof message.id !== 'string') { return; }
            if (message.instanceId !== echoHostBridge.instanceId) {
                echoHostBridge.notify('echo/response', { id: message.id, ok: false, error: 'IDE 连接已过期，请重新获取上下文' });
                return;
            }
            void (async () => {
                if (message.type === 'echo/get-document') {
                    return this.documentSnapshot(typeof message.path === 'string' ? message.path : undefined,
                        message.full === true ? MAX_EDIT_CHARACTERS : undefined,
                        message.range as {offset:number;limit:number} | undefined);
                }
                if (message.type === 'echo/get-context') { return this.contextSnapshot(); }
                if (message.type === 'echo/open-diff') {
                    if (typeof message.path !== 'string' || typeof message.original !== 'string') { throw new Error('Diff 缺少文件路径和原始文本'); }
                    return this.openDiff(message.path, message.original, typeof message.title === 'string' ? message.title : undefined);
                }
                return this.applyOperation(message.operation as EditOperation);
            })().then(value => echoHostBridge.notify('echo/response', { id: message.id, ok: true, value }))
                .catch(error => echoHostBridge.notify('echo/response', { id: message.id, ok: false, error: String(error) }));
        };
        window.addEventListener('message', listener);
        this.disposables.push(Disposable.create(() => window.removeEventListener('message', listener)));
        // The main contribution also announces readiness after layout restoration.
        echoHostBridge.notify('echo/editor-capabilities', { protocolVersion: 1, capabilities: ECHO_EDITOR_CAPABILITIES });
    }

    onStop(): void { this.disposables.dispose(); }

    /** A leave/save acknowledgement must wait for already-admitted native edits and their filesystem RPCs. */
    async whenIdle(): Promise<void> {
        while (this.activeRuns.size) {
            await Promise.allSettled(Array.from(this.activeRuns.values()).flatMap(runs => Array.from(runs)));
        }
    }

    /** Stop ACK is emitted only after already-started writes settle; later requests for this run are fenced. */
    async cancelRun(runId: string): Promise<true> {
        this.cancelledRuns.add(runId);
        await Promise.allSettled(Array.from(this.activeRuns.get(runId) ?? []));
        return true;
    }

    protected assertRunActive(runId: string): void {
        if (this.cancelledRuns.has(runId)) { throw new Error('运行已停止，未继续应用修改'); }
    }

    protected async workspacePath(): Promise<string> {
        const roots = await this.workspace.roots;
        if (roots.length !== 1) { throw new Error('版本化编辑需要单一工作区根目录'); }
        return roots[0].resource.path.fsPath();
    }

    protected async checkedUri(path: string): Promise<URI> {
        const uri = /^[a-z][a-z\d+.-]*:\/\//i.test(path) ? new URI(path) : FileUri.create(path);
        const roots = await this.workspace.roots;
        if (uri.scheme !== 'file' || !roots.some(root => root.resource.isEqualOrParent(uri))) {
            throw new Error('文档不属于当前工作区');
        }
        return uri;
    }

    async openDiff(path: string, original: string, title?: string): Promise<void> {
        if (original.length > MAX_EDIT_CHARACTERS || original.includes('\0')) { throw new Error('Diff 文本超过大小预算或为二进制内容'); }
        const uri = await this.checkedUri(path);
        const current = await this.documentSnapshot(path, MAX_EDIT_CHARACTERS);
        if (current?.truncated || current?.content.includes('\0')) { throw new Error('当前文档超过 Diff 文本预算或为二进制内容'); }
        const existsOnDisk = await this.files.exists(uri);
        const baseline = uri.withScheme('echo-diff').withQuery(`original-${crypto.randomUUID()}`);
        const retained = [this.diffResources.add(baseline, original)];
        try {
            // Existing files use the real model URI, so dirty text, edits and undo
            // remain shared with the regular editor. A deletion has a read-only empty side.
            let modified = uri;
            if (!existsOnDisk && !this.models.get(uri.toString())?.dirty) {
                modified = uri.withScheme('echo-diff').withQuery(`deleted-${crypto.randomUUID()}`);
                retained.push(this.diffResources.add(modified, ''));
            }
            await this.diffs.openDiffEditor(baseline, modified, title?.slice(0, 240) || `${uri.path.base} · 本轮修改`);
        } finally {
            // Native Monaco model references retain their own resource lease.
            retained.forEach(resource => resource.dispose());
        }
    }

    async documentSnapshot(path?: string, budget?: number, range?: {offset:number;limit:number}): Promise<DocumentSnapshot | null> {
        const workspace = await this.workspacePath();
        const current = this.editors.currentEditor;
        const activeUri = current ? new URI(current.editor.document.uri) : undefined;
        const currentUri = activeUri?.scheme === 'file' ? activeUri : undefined;
        const uri = path ? await this.checkedUri(path) : currentUri;
        if (!uri) { return null; }
        await this.checkedUri(uri.toString());
        const model = this.models.get(uri.toString());
        let text: string;
        try {
            text = model ? model.getText() : (await this.files.read(uri, {
                acceptTextOnly: true, limits: { size: MAX_EDIT_CHARACTERS * 4 }
            })).value;
        } catch (error) {
            if (error instanceof FileOperationError && error.fileOperationResult === FileOperationResult.FILE_NOT_FOUND) {
                return { workspace: await this.workspacePath(), uri: uri.toString(), path: uri.path.fsPath(),
                    language: 'plaintext', version: null, hash: 'missing', dirty: false, source: 'disk', exists: false,
                    selection: null, selectedText: '', content: '', contentOffset: 0, totalCharacters: 0,
                    truncated: false, capturedAt: new Date().toISOString() };
            }
            throw error;
        }
        const version = model?.version ?? null;
        const selection: DocumentRange | null = currentUri?.isEqual(uri) ? current!.editor.selection : null;
        const hash = await textHash(text);
        // Hashing is asynchronous. A changed buffer must not produce a mixed snapshot.
        if (model && (model.version !== version || model.getText() !== text)) {
            throw new Error('获取上下文期间文档已变化，请重试');
        }
        if (workspace !== await this.workspacePath()) { throw new Error('工作区已切换，请重新获取上下文'); }
        let ranged: {content:string;contentOffset:number;selectedText:string;truncated:boolean;firstLine:number} | undefined;
        if (range) {
            if (!Number.isInteger(range.offset) || !Number.isInteger(range.limit) || range.limit < 1 || range.limit > 2000) { throw new Error('读取范围无效'); }
            const lines = text.split('\n');
            const start = range.offset < 0 ? Math.max(0, lines.length + range.offset) : Math.max(0, range.offset - 1);
            const content = lines.slice(start, start + range.limit).join('\n').slice(0, 128_000);
            ranged = {content, contentOffset:0, selectedText:'', truncated:true, firstLine:start+1};
        }
        return {
            workspace, uri: uri.toString(), path: uri.path.fsPath(),
            language: model?.languageId ?? 'plaintext', version, hash, dirty: model?.dirty ?? false,
            source: model ? 'buffer' : 'disk', exists: true, selection,
            ...(ranged ?? boundedDocumentText(text, selection, budget)), totalCharacters: text.length,
            capturedAt: new Date().toISOString()
        };
    }

    async contextSnapshot(): Promise<EditorContextSnapshot> {
        const workspace = await this.workspacePath();
        const document = await this.documentSnapshot();
        let budget = CONTEXT_CHARACTER_BUDGET - (document?.content.length ?? 0) - (document?.selectedText.length ?? 0);
        const openDocuments: DocumentSnapshot[] = [];
        let truncated = !!document?.truncated;
        const includeOpenDocument = (snapshot: DocumentSnapshot) => {
            const content = snapshot.content.slice(0, Math.min(8000, Math.max(0, budget)));
            const selectedText = snapshot.selectedText.slice(0, Math.min(8000, Math.max(0, budget - content.length)));
            const clipped = snapshot.truncated || content.length < snapshot.content.length || selectedText.length < snapshot.selectedText.length;
            openDocuments.push({ ...snapshot, content, selectedText, truncated: clipped });
            budget -= content.length + selectedText.length;
            truncated ||= clipped;
        };
        // The current editor is an open document even before EditorManager.all
        // catches up. Reuse its one captured version/hash instead of sampling it twice.
        if (document) { includeOpenDocument(document); }
        const paths = new Set(this.editors.all.map(widget => widget.editor.document.uri));
        for (const path of paths) {
            if (path === document?.uri) { continue; }
            if (budget <= 0 || openDocuments.length >= 12) { truncated = true; break; }
            const snapshot = await this.documentSnapshot(path, Math.min(budget, 8000)).catch(() => null);
            if (snapshot) { includeOpenDocument(snapshot); }
        }
        const selection = this.selections?.selection;
        const selectedProblems = (Array.isArray(selection) ? selection : [selection]).filter((value): value is SelectedProblem => {
            const marker = (value as SelectedProblem | undefined)?.marker;
            return marker?.kind === 'problem' && typeof marker.uri === 'string' && typeof marker.owner === 'string'
                && typeof marker.data?.message === 'string' && !!marker.data.range?.start && !!marker.data.range.end;
        });
        const selectedMarkers: monaco.editor.IMarker[] = selectedProblems.map(({ marker }) => ({
            resource: monaco.Uri.parse(marker.uri), owner: marker.owner, source: marker.data.source,
            message: marker.data.message, code: marker.data.code === undefined ? undefined : String(marker.data.code),
            severity: marker.data.severity === 1 ? monaco.MarkerSeverity.Error : marker.data.severity === 2 ? monaco.MarkerSeverity.Warning
                : marker.data.severity === 3 ? monaco.MarkerSeverity.Info : monaco.MarkerSeverity.Hint,
            startLineNumber: marker.data.range.start.line + 1, startColumn: marker.data.range.start.character + 1,
            endLineNumber: marker.data.range.end.line + 1, endColumn: marker.data.range.end.character + 1
        }));
        const markers = (selectedMarkers.length ? selectedMarkers : monaco.editor.getModelMarkers({})).filter(marker => {
            const uri = new URI(marker.resource.toString());
            return this.workspace.tryGetRoots().some(root => root.resource.isEqualOrParent(uri));
        });
        const diagnostics: DiagnosticSnapshot[] = [];
        for (const marker of markers.slice(0, 50)) {
            const message = marker.message.slice(0, Math.min(Math.max(budget, 0), 2000));
            if (!message) { truncated = true; break; }
            budget -= message.length;
            diagnostics.push({
                workspace, uri: marker.resource.toString(), source: marker.source ?? marker.owner,
                range: {
                    start: { line: marker.startLineNumber - 1, character: marker.startColumn - 1 },
                    end: { line: marker.endLineNumber - 1, character: marker.endColumn - 1 }
                },
                severity: marker.severity === monaco.MarkerSeverity.Error ? 'error'
                    : marker.severity === monaco.MarkerSeverity.Warning ? 'warning'
                        : marker.severity === monaco.MarkerSeverity.Info ? 'info' : 'hint',
                message, code: typeof marker.code === 'object' ? marker.code.value : marker.code,
                version: marker.modelVersionId ?? null, selected: selectedMarkers.length > 0
            });
        }
        const terminal = this.terminals.currentTerminal ?? this.terminals.lastUsedTerminal;
        const selected = terminal?.getSelection() ?? '';
        const output = selected || terminal?.buffer.getLines(Math.max(0, terminal.buffer.length - 80), 80, true).join('\n') || '';
        const terminalLimit = Math.min(8000, Math.max(0, budget));
        const terminalText = terminalLimit > 0 ? output.slice(-terminalLimit) : '';
        let terminalCwd: string | null = null;
        if (terminal) {
            // TerminalWidgetImpl queries the shell through this async getter;
            // lastCwd alone can be empty or stale after a shell `cd`.
            let timer: number | undefined;
            try {
                const current = await Promise.race([
                    Promise.resolve().then(() => (terminal as typeof terminal & { readonly cwd?: Promise<URI> }).cwd),
                    new Promise<undefined>(resolve => { timer = window.setTimeout(() => resolve(undefined), 1500); })
                ]);
                const path = current?.path.fsPath();
                if (path?.trim()) { terminalCwd = path; }
            } catch { /* An unstarted or disconnected shell may reject its CWD request. */ }
            finally { if (timer !== undefined) { window.clearTimeout(timer); } }
            if (terminalCwd === null) {
                const lastPath = terminal.lastCwd?.path.fsPath();
                if (lastPath?.trim()) { terminalCwd = lastPath; }
            }
        }
        if (workspace !== await this.workspacePath()) { throw new Error('工作区已切换，请重新获取上下文'); }
        return {
            workspace, document, openDocuments, diagnostics,
            diagnosticsTruncated: diagnostics.length < markers.length,
            terminal: terminal ? {
                workspace, terminalId: terminal.id, source: selected ? 'terminal-selection' : 'terminal-output',
                cwd: terminalCwd, text: terminalText,
                truncated: terminalText.length < output.length, exitCode: terminal.exitStatus?.code ?? null
            } : null,
            truncated: truncated || diagnostics.length < markers.length || terminalText.length < output.length
        };
    }

    async applyOperation(operation: EditOperation): Promise<ApplyResult> {
        if (!operation || ![operation.operationId, operation.sessionId, operation.runId].every(id => typeof id === 'string' && id.length > 0)
            || !Array.isArray(operation.files) || !operation.files.length || operation.files.length > 50) {
            throw new Error('版本化修改缺少操作、会话、运行身份，或文件数量超出预算');
        }
        this.assertRunActive(operation.runId);
        if (operation.workspace !== await this.workspacePath()) { throw new Error('修改属于其他工作区'); }
        const key = `${operation.workspace}:${operation.sessionId}:${operation.runId}:${operation.operationId}`;
        const fingerprint = await textHash(JSON.stringify(operation));
        this.assertRunActive(operation.runId);
        const known = this.operations.get(key);
        if (known) {
            if (known.fingerprint !== fingerprint) { throw new Error('operationId 已用于另一组修改'); }
            return known.result;
        }
        const storageKey = `echo.edit.${key}`;
        try {
            const persisted = JSON.parse(sessionStorage.getItem(storageKey) ?? 'null') as { fingerprint: string; result: ApplyResult } | null;
            if (persisted) {
                if (persisted.fingerprint !== fingerprint) { throw new Error('operationId 已用于另一组修改'); }
                return persisted.result;
            }
        } catch (error) {
            if (error instanceof Error && error.message.includes('operationId')) { throw error; }
        }
        const result = this.performOperation(operation).then(value => {
            try { sessionStorage.setItem(storageKey, JSON.stringify({ fingerprint, result: value })); } catch { /* Expected hashes remain the replay guard when storage is unavailable. */ }
            return value;
        });
        const active = this.activeRuns.get(operation.runId) ?? new Set<Promise<ApplyResult>>();
        this.activeRuns.set(operation.runId, active);
        active.add(result);
        void result.finally(() => { active.delete(result); if (!active.size) { this.activeRuns.delete(operation.runId); } }).catch(() => undefined);
        this.operations.set(key, { fingerprint, result });
        return result;
    }

    protected async performOperation(operation: EditOperation): Promise<ApplyResult> {
        const prepared: (PreparedEdit | FileApplyResult)[] = [];
        const seen = new Set<string>();
        // Preflight every target before the first write; valid files may still apply when another conflicts.
        for (const target of operation.files) {
            try {
                this.assertRunActive(operation.runId);
                const uri = await this.checkedUri(target.uri);
                this.assertRunActive(operation.runId);
                if (seen.has(uri.toString())) { throw new Error('同一操作重复包含文档'); }
                seen.add(uri.toString());
                const model = this.models.get(uri.toString());
                if (target.expectedHash === 'missing') {
                    if (target.delete || target.moveTo) { throw new Error('删除或移动必须提供现有文件的快照'); }
                    const exists = !!model || await this.files.exists(uri);
                    this.assertRunActive(operation.runId);
                    if (exists) {
                        prepared.push({ uri: target.uri, status: 'conflict', error: '新建目标已存在，未覆盖现有文档' });
                        continue;
                    }
                    validateTextEdits('', target.edits);
                    prepared.push({ target, uri, text: '', create: true });
                    continue;
                }
                if (!/^[a-f\d]{64}$/i.test(target.expectedHash)) { throw new Error('修改必须提供完整 SHA-256 快照'); }
                const disk = model ? undefined : await this.files.read(uri, { acceptTextOnly: true, limits: { size: MAX_EDIT_CHARACTERS * 4 } });
                this.assertRunActive(operation.runId);
                const text = model ? model.getText() : disk!.value;
                const hash = await textHash(text);
                this.assertRunActive(operation.runId);
                if ((target.expectedVersion != null && target.expectedVersion !== (model?.version ?? null)) || hash !== target.expectedHash) {
                    prepared.push({ uri: target.uri, status: 'conflict', error: '文档版本或内容已变化，未覆盖现有修改' });
                    continue;
                }
                if (model?.readOnly) { throw new Error('文档为只读'); }
                if ((target.delete || target.moveTo) && model?.dirty) { throw new Error('删除或移动前请先保存该文件，未保存内容已保留'); }
                if (target.moveTo) {
                    const destination = await this.checkedUri(target.moveTo);
                    if (this.models.get(destination.toString()) || await this.files.exists(destination)) {
                        throw new Error('移动目标已存在，未覆盖文档');
                    }
                }
                validateTextEdits(text, target.edits);
                prepared.push({ target, uri, text, model, disk });
            } catch (error) {
                prepared.push({ uri: target?.uri ?? '', status: 'rejected', error: String(error) });
            }
        }
        const results: FileApplyResult[] = [];
        for (const item of prepared) {
            if ('status' in item) { results.push(item); continue; }
            const { target, uri, text, model } = item;
            try {
                this.assertRunActive(operation.runId);
                if (operation.workspace !== await this.workspacePath()) { throw new Error('工作区已切换，未应用旧工作区修改'); }
                this.assertRunActive(operation.runId);
                const undoGroup = `${operation.runId}:${operation.operationId}:${uri.toString()}`;
                if (target.delete) {
                    if (!(this.files instanceof EchoFileService)) { throw new Error('版本化文件服务未装配'); }
                    await this.files.deleteTextFileConditional(uri, target.expectedHash, () => {
                        this.assertRunActive(operation.runId);
                        const current = this.models.get(uri.toString());
                        if (current?.dirty || current && (current.getText() !== text || target.expectedVersion != null && current.version !== target.expectedVersion)) {
                            throw new FileOperationError('文件已有新编辑，未删除', FileOperationResult.FILE_MODIFIED_SINCE);
                        }
                    });
                    results.push({ uri: target.uri, status: 'applied', version: null, hash: 'missing', dirty: false });
                    continue;
                }
                if (model) {
                    if (model.readOnly) {
                        results.push({ uri: target.uri, status: 'rejected', error: '预检后文档已变为只读，未应用修改' });
                        continue;
                    }
                    // This check and push are one synchronous critical section; typing stays enabled everywhere else.
                    if (model.getText() !== text || (target.expectedVersion != null && model.version !== target.expectedVersion)) {
                        results.push({ uri: target.uri, status: 'conflict', error: '预检后文档再次变化，未应用过时修改' });
                        continue;
                    }
                    const editor = MonacoEditor.findByDocument(this.editors, model)[0];
                    const selections = editor?.getControl().getSelections() ?? [];
                    model.textEditorModel.pushStackElement();
                    model.textEditorModel.pushEditOperations(selections, target.edits.map(edit => ({
                        range: new monaco.Range(edit.range.start.line + 1, edit.range.start.character + 1,
                            edit.range.end.line + 1, edit.range.end.character + 1), text: edit.text
                    })), () => selections);
                    model.textEditorModel.pushStackElement();
                    let appliedVersion = model.version;
                    let appliedText = model.getText();
                    const checkpointText = appliedText;
                    let saveError: string | undefined;
                    if (target.save) {
                        try {
                            this.assertRunActive(operation.runId);
                            await model.save();
                            // Monaco's save deliberately swallows OutOfSync. A resolved save is not proof of persistence.
                            const persisted = await this.files.read(uri, { acceptTextOnly: true });
                            if (model.dirty || persisted.value !== model.getText()) { throw new Error('编辑缓冲尚未保存，磁盘冲突或保存期间内容已变化'); }
                        } catch (error) { saveError = String(error); }
                        appliedVersion = model.version;
                        appliedText = model.getText();
                    }
                    const hash = await textHash(appliedText);
                    const checkpointHash = appliedText === checkpointText ? hash : await textHash(checkpointText);
                    results.push({ uri: target.uri, status: 'applied', version: appliedVersion,
                        hash, checkpointHash, dirty: model.dirty, undoGroup, saveError });
                } else {
                    // A buffer opened since preflight is now authoritative and must be re-read first.
                    if (this.models.get(uri.toString())) {
                        results.push({ uri: target.uri, status: 'conflict', error: '预检后文档已在编辑器中打开，请重新获取快照' });
                        continue;
                    }
                    const updated = applyTextEdits(text, target.edits);
                    const assertNoBuffer = () => {
                        this.assertRunActive(operation.runId);
                        if (this.models.get(uri.toString())) {
                            throw new FileOperationError('文档已在编辑器中打开，请重新获取快照', FileOperationResult.FILE_MODIFIED_SINCE);
                        }
                    };
                    if (!(this.files instanceof EchoFileService)) { throw new Error('版本化文件服务未装配'); }
                    if (item.create) {
                        await this.files.createTextFileExclusive(uri, updated, assertNoBuffer);
                    } else {
                        await this.files.writeTextFileConditional(uri, target.expectedHash, updated, assertNoBuffer);
                    }
                    results.push({ uri: target.uri, status: 'applied', version: null, hash: await textHash(updated), dirty: false });
                }
                const applied = results[results.length - 1];
                if (target.moveTo && applied?.status === 'applied' && !applied.saveError && applied.hash) {
                    try {
                        if (!(this.files instanceof EchoFileService)) { throw new Error('版本化文件服务未装配'); }
                        const destination = await this.checkedUri(target.moveTo);
                        await this.files.moveTextFileConditional(uri, destination, applied.hash, () => {
                            this.assertRunActive(operation.runId);
                            const current = this.models.get(uri.toString());
                            if (current?.dirty || current && current.version !== applied.version || this.models.get(destination.toString())) {
                                throw new FileOperationError('文件已有新编辑，未移动', FileOperationResult.FILE_MODIFIED_SINCE);
                            }
                        });
                        applied.movedTo = destination.toString();
                        applied.movedHash = applied.hash;
                        applied.hash = 'missing';
                        applied.checkpointHash = 'missing';
                    } catch (error) { applied.saveError = `内容已应用，但移动失败：${String(error)}`; }
                }
            } catch (error) {
                results.push({ uri: target.uri,
                    status: this.cancelledRuns.has(operation.runId) ? 'rejected'
                        : error instanceof FileOperationError && error.fileOperationResult === FileOperationResult.FILE_MODIFIED_SINCE
                        ? 'conflict' : 'failed', error: String(error) });
            }
        }
        return summarizeApplyResult(operation, results);
    }
}
