import { Disposable, DisposableCollection } from '@theia/core/lib/common/disposable';
import { EditorManager } from '@theia/editor/lib/browser/editor-manager';
import { MonacoEditor } from '@theia/monaco/lib/browser/monaco-editor';
import * as monaco from '@theia/monaco-editor-core';
import { echoHostBridge } from './echo-host-bridge';
import { textHash } from './echo-editor-protocol';

interface EchoCompletionList extends monaco.languages.InlineCompletions {
    requestId?: string;
    uri?: string;
    version?: number;
    hash?: string;
    startedAt?: number;
}

/** Monaco owns ghost text, Tab/Escape behavior and the resulting undo operation. */
export class EchoInlineCompletion implements Disposable {
    protected readonly disposables = new DisposableCollection();
    protected editorSubscriptions = new DisposableCollection();
    protected current?: AbortController;
    protected readonly cache = new Map<string, { text: string; time: number; requestId: string }>();
    protected requestTimes: number[] = [];
    protected cooldownUntil = 0;
    protected enabled = false;

    constructor(protected readonly editors: EditorManager, protected readonly workspacePath: () => Promise<string>) {
        this.disposables.push(monaco.languages.registerInlineCompletionsProvider({ scheme: 'file' }, {
            provideInlineCompletions: (model, position, context, token) => this.provide(model, position, context, token),
            handleItemDidShow: completions => {
                const metadata = completions as EchoCompletionList;
                echoHostBridge.notify('echo/inline-completion-shown', {
                    requestId: metadata.requestId, uri: metadata.uri, version: metadata.version,
                    latencyMs: Date.now() - (metadata.startedAt ?? Date.now())
                });
            },
            handleEndOfLifetime: (completions, item, reason) => {
                const metadata = completions as EchoCompletionList;
                echoHostBridge.notify(reason.kind === monaco.languages.InlineCompletionEndOfLifeReasonKind.Accepted
                    ? 'echo/inline-completion-accepted' : 'echo/inline-completion-discarded', {
                    requestId: metadata.requestId, uri: metadata.uri, version: metadata.version, hash: metadata.hash,
                    characters: typeof item.insertText === 'string' ? item.insertText.length : 0,
                    reason: reason.kind === monaco.languages.InlineCompletionEndOfLifeReasonKind.Accepted ? 'accepted'
                        : reason.kind === monaco.languages.InlineCompletionEndOfLifeReasonKind.Rejected ? 'rejected' : 'expired'
                });
            },
            disposeInlineCompletions: () => undefined
        }));
        this.disposables.push(editors.onCurrentEditorChanged(() => this.watchEditor()));
        this.disposables.push(editors.onActiveEditorChanged(() => this.cancel()));
        this.watchEditor();
    }

    setEnabled(enabled: boolean): void {
        // The host also resends this setting when its model changes. Fence any
        // request, cached text and visible ghost from the previous model.
        this.cancel();
        this.cache.clear();
        MonacoEditor.get(this.editors.currentEditor)?.getControl().trigger('echo', 'editor.action.inlineSuggest.hide', undefined);
        this.enabled = enabled;
    }

    dispose(): void {
        this.cancel();
        this.editorSubscriptions.dispose();
        this.disposables.dispose();
        this.cache.clear();
    }

    protected cancel(): void {
        this.current?.abort();
        this.current = undefined;
    }

    protected watchEditor(): void {
        this.cancel();
        this.editorSubscriptions.dispose();
        this.editorSubscriptions = new DisposableCollection();
        const editor = MonacoEditor.get(this.editors.currentEditor);
        if (!editor) { return; }
        const control = editor.getControl();
        this.editorSubscriptions.push(control.onDidChangeCursorPosition(() => this.cancel()));
        this.editorSubscriptions.push(control.onDidChangeModelContent(() => this.cancel()));
        this.editorSubscriptions.push(control.onDidBlurEditorText(() => this.cancel()));
        this.editorSubscriptions.push(control.onKeyDown(event => {
            if (event.keyCode === monaco.KeyCode.Escape) {
                this.cancel();
                control.trigger('echo', 'editor.action.inlineSuggest.hide', undefined);
            }
        }));
        // Uses the editor's normal Tab binding; no outer React keyboard interception.
        control.updateOptions({ inlineSuggest: { enabled: true } });
    }

    protected async provide(model: monaco.editor.ITextModel, position: monaco.Position,
        context: monaco.languages.InlineCompletionContext, token: monaco.CancellationToken): Promise<EchoCompletionList> {
        this.cancel();
        if (!this.enabled || !echoHostBridge.enabled || token.isCancellationRequested || model.isDisposed()
            || model.getValueLength() > 2_000_000 || Date.now() < this.cooldownUntil) { return { items: [] }; }
        const editor = MonacoEditor.get(this.editors.currentEditor);
        if (!editor || editor.document.textEditorModel !== model || editor.document.readOnly
            || !editor.getControl().hasTextFocus()) { return { items: [] }; }
        const selection = editor.getControl().getSelection();
        if (selection && !selection.isEmpty()) { return { items: [] }; }
        const controller = new AbortController();
        this.current = controller;
        const cancellation = token.onCancellationRequested(() => controller.abort());
        let requestId: string = crypto.randomUUID();
        let sent = false;
        let complete = false;
        let cancelSent = false;
        const startedAt = Date.now();
        const cancelHost = () => {
            if (sent && !complete && !cancelSent) {
                cancelSent = true;
                echoHostBridge.notify('echo/cancel-inline-completion', { requestId });
            }
        };
        controller.signal.addEventListener('abort', cancelHost, { once: true });
        const version = model.getVersionId();
        const source = model.getValue();
        const offset = model.getOffsetAt(position);
        const stillCurrent = () => this.enabled && !controller.signal.aborted && !token.isCancellationRequested && !model.isDisposed()
            && model.getVersionId() === version && editor.document.textEditorModel === model
            && MonacoEditor.get(this.editors.currentEditor) === editor && editor.getControl().hasTextFocus()
            && editor.getControl().getPosition()?.equals(position);
        try {
            if (context.triggerKind === monaco.languages.InlineCompletionTriggerKind.Automatic) {
                await new Promise<void>(resolve => {
                    const finish = () => { window.clearTimeout(timer); controller.signal.removeEventListener('abort', finish); resolve(); };
                    const timer = window.setTimeout(finish, 300);
                    controller.signal.addEventListener('abort', finish, { once: true });
                });
            }
            if (!stillCurrent()) { return { items: [] }; }
            const hash = await textHash(source);
            const key = `${model.uri.toString()}:${version}:${offset}:${hash}`;
            const cached = this.cache.get(key);
            let text: string | undefined = cached && Date.now() - cached.time < 30_000 ? cached.text : undefined;
            if (text && cached) { requestId = cached.requestId; }
            if (!text) {
                this.requestTimes = this.requestTimes.filter(time => Date.now() - time < 60_000);
                if (this.requestTimes.length >= 12 || !stillCurrent()) { return { items: [] }; }
                const workspace = await this.workspacePath();
                if (!stillCurrent()) { return { items: [] }; }
                this.requestTimes.push(Date.now());
                sent = true;
                const result = await echoHostBridge.request('echo/inline-completion', {
                    requestId, workspace, uri: model.uri.toString(), path: model.uri.fsPath,
                    language: model.getLanguageId(), version, hash,
                    position: { line: position.lineNumber - 1, character: position.column - 1 },
                    prefix: source.slice(Math.max(0, offset - 6000), offset), suffix: source.slice(offset, offset + 2000),
                    prefixTruncated: offset > 6000, suffixTruncated: source.length - offset > 2000,
                    maxTokens: 256
                }, { signal: controller.signal, timeoutMs: 8000 }) as { text?: unknown } | undefined;
                complete = true;
                text = typeof result?.text === 'string' ? result.text.slice(0, 2048) : undefined;
                if (text?.includes('\0')) { text = undefined; }
                if (text && stillCurrent()) {
                    this.cache.set(key, { text, time: Date.now(), requestId });
                    while (this.cache.size > 20) { this.cache.delete(this.cache.keys().next().value!); }
                }
            }
            if (!text || !stillCurrent()) {
                if (sent) { echoHostBridge.notify('echo/inline-completion-discarded', { requestId, reason: 'expired' }); }
                return { items: [] };
            }
            return { items: [{ insertText: text, range: new monaco.Range(position.lineNumber, position.column, position.lineNumber, position.column) }],
                enableForwardStability: false, requestId, uri: model.uri.toString(), version, hash, startedAt };
        } catch (error) {
            // Completion is optional. Network/model failures never interrupt typing or surface as an edit.
            if (!controller.signal.aborted) {
                this.cooldownUntil = Date.now() + 5000;
                echoHostBridge.notify('echo/inline-completion-error', { requestId, error: String(error) });
            } else if (sent) {
                echoHostBridge.notify('echo/inline-completion-discarded', { requestId, reason: 'cancelled' });
            }
            return { items: [] };
        } finally {
            if (sent && !complete) { cancelHost(); }
            complete = true;
            cancellation.dispose();
            controller.signal.removeEventListener('abort', cancelHost);
            if (this.current === controller) { this.current = undefined; }
        }
    }
}
