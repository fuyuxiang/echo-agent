import { inject, injectable } from '@theia/core/shared/inversify';
import { FrontendApplication, FrontendApplicationContribution } from '@theia/core/lib/browser';
import { ConnectionStatus, ConnectionStatusService } from '@theia/core/lib/browser/connection-status-service';
import { Widget } from '@theia/core/shared/@lumino/widgets';
import { FileUri } from '@theia/core/lib/common/file-uri';
import URI from '@theia/core/lib/common/uri';
import { CommandRegistry } from '@theia/core/lib/common/command';
import { MAIN_MENU_BAR } from '@theia/core/lib/common/menu';
import { CorePreferences } from '@theia/core/lib/common/core-preferences';
import { CommonCommands } from '@theia/core/lib/browser/common-commands';
import { EditorManager } from '@theia/editor/lib/browser/editor-manager';
import type { EditorWidget } from '@theia/editor/lib/browser/editor-widget';
import { FrontendApplicationStateService } from '@theia/core/lib/browser/frontend-application-state';
import { ThemeService } from '@theia/core/lib/browser/theming';
import { WorkspaceService } from '@theia/workspace/lib/browser';
import { Saveable } from '@theia/core/lib/browser/saveable';
import { Disposable } from '@theia/core/lib/common/disposable';
import { TerminalService } from '@theia/terminal/lib/browser/base/terminal-service';
import { TerminalWidget } from '@theia/terminal/lib/browser/base/terminal-widget';
import { echoHostBridge } from './echo-host-bridge';
import { ECHO_EDITOR_CAPABILITIES } from './echo-editor-protocol';
import { EchoEditorBridge } from './echo-editor-bridge';

@injectable()
export class EchoFrontendContribution implements FrontendApplicationContribution {
    protected emptyState?: HTMLElement;
    protected shell?: FrontendApplication['shell'];
    protected readonly dirtySubscriptions = new Map<Widget, Disposable>();
    protected cursorSubscription?: Disposable;
    protected readonly terminalOutputSubscriptions = new Map<TerminalWidget, Disposable>();
    @inject(EditorManager)
    protected readonly editors: EditorManager;
    @inject(CommandRegistry)
    protected readonly commands: CommandRegistry;
    @inject(FrontendApplicationStateService)
    protected readonly applicationState: FrontendApplicationStateService;
    @inject(WorkspaceService)
    protected readonly workspace: WorkspaceService;
    @inject(ThemeService)
    protected readonly themes: ThemeService;
    @inject(CorePreferences)
    protected readonly corePreferences: CorePreferences;
    @inject(TerminalService)
    protected readonly terminals: TerminalService;
    @inject(ConnectionStatusService)
    protected readonly connectionStatus: ConnectionStatusService;
    @inject(EchoEditorBridge) protected readonly editorBridge: EchoEditorBridge;
    onStart(): void {
        if (echoHostBridge.enabled) {
            document.body.classList.add('echo-embedded');
            this.connectionStatus.onStatusChange(status => {
                echoHostBridge.notify('echo/backend-connection', { online: status === ConnectionStatus.ONLINE });
            });
        }
        window.addEventListener('message', event => {
            if (!echoHostBridge.isTrusted(event)) {
                return;
            }
            const message = event.data as Record<string, unknown>;
            if (message.type === 'echo/open-file' && typeof message.path === 'string') {
                const line = typeof message.line === 'number' && Number.isSafeInteger(message.line) && message.line > 0
                    ? message.line - 1 : undefined;
                void this.editors.open(FileUri.create(message.path), line === undefined ? undefined : {
                    selection: { start: { line, character: 0 }, end: { line, character: 0 } },
                });
            } else if (message.type === 'echo/open-preview' && typeof message.url === 'string') {
                void this.commands.executeCommand('mini-browser.openUrl', message.url);
            } else if (message.type === 'echo/set-theme' && (message.theme === 'light' || message.theme === 'dark')) {
                this.themes.setCurrentTheme(message.theme, false);
            } else if (message.type === 'echo/save-all' && typeof message.id === 'string' && this.shell) {
                void this.editorBridge.whenIdle().then(() => this.shell!.saveAll()).then(() => {
                    this.reportDirtyState();
                    echoHostBridge.notify('echo/response', { id: message.id, ok: true, value: this.dirtyCount() });
                }).catch(error => {
                    echoHostBridge.notify('echo/response', { id: message.id, ok: false, error: String(error) });
                });
            } else if (message.type === 'echo/reload-clean' && typeof message.id === 'string') {
                void (async () => {
                    await this.editorBridge.whenIdle();
                    if (this.dirtyCount() !== 0) { throw new Error('IDE 有未保存内容，未覆盖编辑缓冲'); }
                    const documents = new Set(this.editors.all.map(editor => editor.editor.document));
                    await Promise.all(Array.from(documents, document => document.revert?.()));
                    this.reportDirtyState();
                    echoHostBridge.notify('echo/response', { id: message.id, ok: true, value: this.dirtyCount() });
                })().catch(error => echoHostBridge.notify('echo/response', { id: message.id, ok: false, error: String(error) }));
            } else if (message.type === 'echo/get-dirty'  && typeof message.id === 'string') {
                void this.editorBridge.whenIdle().then(() => {
                    echoHostBridge.notify('echo/response', { id: message.id, ok: true, value: this.dirtyCount() });
                }).catch(error => echoHostBridge.notify('echo/response', { id: message.id, ok: false, error: String(error) }));
            }
        });
        const reportCurrentEditor = (editor: EditorWidget | undefined) => {
            this.cursorSubscription?.dispose();
            echoHostBridge.notify('echo/active-file', {
                path: editor ? new URI(editor.editor.document.uri).path.fsPath() : null,
            });
            const reportSymbol = () => {
                if (!editor) {
                    echoHostBridge.notify('echo/active-symbol', { path: null, symbol: null });
                    return;
                }
                const textEditor = editor.editor;
                const cursor = textEditor.cursor;
                const selected = textEditor.document.getText(textEditor.selection).trim();
                const line = textEditor.document.getLineContent(cursor.line + 1);
                const before = line.slice(0, cursor.character).match(/[A-Za-z_$][A-Za-z0-9_$]*$/)?.[0] ?? '';
                const after = line.slice(cursor.character).match(/^[A-Za-z0-9_$]*/)?.[0] ?? '';
                const word = selected && /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(selected)
                    ? selected : `${before}${after}`;
                echoHostBridge.notify('echo/active-symbol', {
                    path: new URI(textEditor.document.uri).path.fsPath(),
                    symbol: /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(word) ? word : null,
                });
            };
            if (editor) {
                const cursorListener = editor.editor.onCursorPositionChanged(reportSymbol);
                const selectionListener = editor.editor.onSelectionChanged(reportSymbol);
                this.cursorSubscription = Disposable.create(() => {
                    cursorListener.dispose();
                    selectionListener.dispose();
                });
            }
            reportSymbol();
            window.requestAnimationFrame(() => this.updateEmptyState());
        };
        this.editors.onCurrentEditorChanged(reportCurrentEditor);
        for (const terminal of this.terminals.all) this.watchTerminalOutput(terminal);
        this.terminals.onDidCreateTerminal(terminal => this.watchTerminalOutput(terminal));
        void this.applicationState.reachedState('ready').then(async () => {
            echoHostBridge.notify('echo/ready', { protocolVersion: 1, capabilities: ECHO_EDITOR_CAPABILITIES });
            this.reportDirtyState();
            reportCurrentEditor(this.editors.currentEditor);
            const reportWorkspace = async () => {
                const roots = await this.workspace.roots;
                echoHostBridge.notify('echo/workspace', {
                    path: roots.length === 1 ? roots[0].resource.path.fsPath() : null,
                });
            };
            this.workspace.onWorkspaceChanged(() => void reportWorkspace());
            await reportWorkspace();
        });
    }

    protected watchTerminalOutput(terminal: TerminalWidget): void {
        if (this.terminalOutputSubscriptions.has(terminal)) return;
        let recent = '';
        const output = terminal.onOutput(chunk => {
            recent = (recent + chunk.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '')).slice(-1200);
            const matches = recent.matchAll(/\b(https?:\/\/)?(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{2,5})(?:\/[^\s]*)?/gi);
            for (const match of matches) {
                const port = Number(match[3]);
                if (!Number.isSafeInteger(port) || port < 1 || port > 65535) continue;
                const scheme = match[1] ?? 'http://';
                const host = match[2] === '0.0.0.0' ? '127.0.0.1' : match[2];
                echoHostBridge.notify('echo/preview-url', { url: `${scheme}${host}:${port}/` });
            }
        });
        this.terminalOutputSubscriptions.set(terminal, output);
        terminal.onDidDispose(() => {
            this.terminalOutputSubscriptions.get(terminal)?.dispose();
            this.terminalOutputSubscriptions.delete(terminal);
        });
    }

    async onDidInitializeLayout(app: FrontendApplication): Promise<void> {
        if (echoHostBridge.enabled) {
            this.shell = app.shell;
            for (const widget of app.shell.widgets) {
                this.watchDirtyWidget(widget);
            }
            app.shell.onDidAddWidget(widget => {
                this.watchDirtyWidget(widget);
                this.reportDirtyState();
            });
            app.shell.onDidRemoveWidget(widget => {
                this.dirtySubscriptions.get(widget)?.dispose();
                this.dirtySubscriptions.delete(widget);
                this.reportDirtyState();
            });
            await this.commands.executeCommand('workbench.files.action.focusFilesExplorer');
            await this.corePreferences.ready;
            if (this.corePreferences['window.menuBarVisibility'] === 'compact') {
                app.shell.leftPanelHandler.addTopMenu({
                    id: 'main-menu',
                    iconClass: 'theia-compact-menu codicon codicon-menu',
                    title: '应用菜单',
                    menuPath: MAIN_MENU_BAR,
                    order: 0,
                });
            }
            // The editor's empty hint lives behind the document area. It does
            // not create a second Welcome tab or compete with the Agent pane.
            for (const widget of app.shell.getWidgets('main')) {
                if (widget.id === 'getting.started.widget' || widget.id === 'echo-code-start') {
                    widget.close();
                }
            }
            const emptyState = document.createElement('div');
            emptyState.className = 'echo-editor-empty';
            emptyState.innerHTML = `<div class="echo-editor-empty__content">
                <span class="codicon codicon-file-code" aria-hidden="true"></span>
                <strong>打开文件开始编辑</strong>
                <span>从资源管理器选择文件，或使用 Ctrl/⌘ + P 快速打开</span>
                <button type="button">新建文件</button>
            </div>`;
            emptyState.querySelector('button')?.addEventListener('click', () => {
                void this.commands.executeCommand(CommonCommands.NEW_UNTITLED_TEXT_FILE.id);
            });
            app.shell.mainPanel.node.appendChild(emptyState);
            this.emptyState = emptyState;
            app.shell.onDidAddWidget(() => window.requestAnimationFrame(() => this.updateEmptyState()));
            app.shell.onDidRemoveWidget(() => window.requestAnimationFrame(() => this.updateEmptyState()));
            this.updateEmptyState();
        }
    }

    protected updateEmptyState(): void {
        if (this.emptyState && this.shell) {
            this.emptyState.hidden = this.shell.getWidgets('main').length > 0;
        }
    }

    protected watchDirtyWidget(widget: Widget): void {
        if (this.dirtySubscriptions.has(widget)) {
            return;
        }
        const saveable = Saveable.get(widget);
        if (saveable) {
            this.dirtySubscriptions.set(widget, saveable.onDirtyChanged(() => this.reportDirtyState()));
        }
    }

    protected dirtyCount(): number {
        // A file and its Diff widget can share the same document. Count each
        // saveable once while retaining independent unsaved buffers.
        const dirty = new Set<Saveable>();
        for (const widget of this.shell?.widgets ?? []) {
            const saveable = Saveable.getDirty(widget);
            if (saveable) {
                dirty.add(saveable);
            }
        }
        return dirty.size;
    }

    protected reportDirtyState(): void {
        echoHostBridge.notify('echo/dirty-state', { count: this.dirtyCount() });
    }

}
