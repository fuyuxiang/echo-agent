/** Bridge coordinates use LSP's zero-based UTF-16 positions, including ranges. */
export interface DocumentPosition { line: number; character: number; }
export interface DocumentRange { start: DocumentPosition; end: DocumentPosition; }
export interface DocumentSnapshot {
    workspace: string;
    uri: string;
    path: string;
    language: string;
    version: number | null;
    hash: string;
    dirty: boolean;
    source: 'buffer' | 'disk';
    exists: boolean;
    selection: DocumentRange | null;
    selectedText: string;
    content: string;
    contentOffset: number;
    totalCharacters: number;
    truncated: boolean;
    capturedAt: string;
    firstLine?: number;
}
export interface DocumentTextEdit { range: DocumentRange; text: string; }
export interface DocumentEditTarget {
    uri: string;
    expectedVersion?: number | null;
    /** SHA-256 of the entire UTF-8 encoded text, or 'missing' for exclusive creation. */
    expectedHash: string;
    edits: DocumentTextEdit[];
    /** Open documents stay in the native undo stack. Saving is explicitly requested. */
    save?: boolean;
    delete?: boolean;
    moveTo?: string;
}
export interface EditOperation {
    operationId: string;
    sessionId: string;
    runId: string;
    workspace: string;
    files: DocumentEditTarget[];
}
export interface FileApplyResult {
    uri: string;
    status: 'applied' | 'conflict' | 'rejected' | 'failed';
    version?: number | null;
    hash?: string;
    /** Text immediately after this operation, before asynchronous saving or later user input. */
    checkpointHash?: string;
    dirty?: boolean;
    undoGroup?: string;
    error?: string;
    /** The native edit succeeded but persistence failed. The buffer remains recoverable. */
    saveError?: string;
    movedTo?: string;
    movedHash?: string;
}
export interface ApplyResult {
    operationId: string;
    sessionId: string;
    runId: string;
    status: 'applied' | 'partial' | 'conflict' | 'rejected' | 'failed';
    files: FileApplyResult[];
}
export interface DiagnosticSnapshot {
    workspace: string;
    uri: string;
    source: string;
    range: DocumentRange;
    severity: 'error' | 'warning' | 'info' | 'hint';
    message: string;
    code?: string;
    version: number | null;
    selected?: boolean;
}
export interface TerminalContext {
    workspace: string;
    terminalId: string;
    source: 'terminal-selection' | 'terminal-output';
    cwd: string | null;
    text: string;
    truncated: boolean;
    exitCode: number | null;
}
export interface EditorContextSnapshot {
    workspace: string;
    document: DocumentSnapshot | null;
    openDocuments: DocumentSnapshot[];
    diagnostics: DiagnosticSnapshot[];
    diagnosticsTruncated: boolean;
    terminal: TerminalContext | null;
    truncated: boolean;
}
export const ECHO_EDITOR_CAPABILITIES = [
    'document-snapshots', 'versioned-edits', 'inline-completion', 'diagnostics', 'terminal-context', 'native-diff'
] as const;
export const SNAPSHOT_CHARACTER_BUDGET = 32_000;
export const CONTEXT_CHARACTER_BUDGET = 96_000;
export const MAX_EDIT_CHARACTERS = 2_000_000;

export async function textHash(text: string): Promise<string> {
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
    return Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, '0')).join('');
}

function comparePosition(left: DocumentPosition, right: DocumentPosition): number {
    return left.line - right.line || left.character - right.character;
}

/** Strict bounds checks avoid Monaco's automatic range clamping changing another location. */
export function positionOffset(text: string, position: DocumentPosition): number {
    if (!Number.isSafeInteger(position?.line) || !Number.isSafeInteger(position?.character)
        || position.line < 0 || position.character < 0) {
        throw new Error('编辑位置必须是非负的零基行列');
    }
    let offset = 0;
    for (let line = 0; line < position.line; line++) {
        const newline = text.indexOf('\n', offset);
        if (newline < 0) { throw new Error('编辑行号超出当前文档'); }
        offset = newline + 1;
    }
    const newline = text.indexOf('\n', offset);
    let lineEnd = newline < 0 ? text.length : newline;
    if (lineEnd > offset && text[lineEnd - 1] === '\r') { lineEnd--; }
    if (position.character > lineEnd - offset) { throw new Error('编辑列号超出当前文档'); }
    const result = offset + position.character;
    const before = text.charCodeAt(result - 1);
    const after = text.charCodeAt(result);
    if (before >= 0xD800 && before <= 0xDBFF && after >= 0xDC00 && after <= 0xDFFF) {
        throw new Error('编辑位置不能切开 Unicode 代理对');
    }
    return result;
}

export function validateTextEdits(text: string, edits: DocumentTextEdit[]): void {
    if (text.includes('\0')) { throw new Error('二进制文档不支持文本编辑'); }
    if (text.length > MAX_EDIT_CHARACTERS || !Array.isArray(edits) || edits.length > 2000) {
        throw new Error('文本编辑超过大小预算');
    }
    let replacementCharacters = 0;
    const ranges = edits.map(edit => {
        if (typeof edit?.text !== 'string' || !edit.range?.start || !edit.range?.end) {
            throw new Error('文本编辑格式无效');
        }
        replacementCharacters += edit.text.length;
        if (replacementCharacters > MAX_EDIT_CHARACTERS || edit.text.includes('\0')) {
            throw new Error('编辑内容超过预算或包含二进制数据');
        }
        if (comparePosition(edit.range.start, edit.range.end) > 0) { throw new Error('编辑范围起点晚于终点'); }
        return { start: positionOffset(text, edit.range.start), end: positionOffset(text, edit.range.end) };
    }).sort((a, b) => a.start - b.start || a.end - b.end);
    for (let index = 1; index < ranges.length; index++) {
        if (ranges[index].start < ranges[index - 1].end || ranges[index].start === ranges[index - 1].start) {
            throw new Error('同一操作中的编辑范围重叠');
        }
    }
}

export function applyTextEdits(text: string, edits: DocumentTextEdit[]): string {
    validateTextEdits(text, edits);
    const ordered = edits.map(edit => ({
        start: positionOffset(text, edit.range.start), end: positionOffset(text, edit.range.end), text: edit.text
    })).sort((a, b) => b.start - a.start);
    for (const edit of ordered) { text = text.slice(0, edit.start) + edit.text + text.slice(edit.end); }
    return text;
}

/** Center large snapshots on the selection while retaining absolute source coordinates. */
export function boundedDocumentText(text: string, selection: DocumentRange | null, budget = SNAPSHOT_CHARACTER_BUDGET): {
    content: string; contentOffset: number; selectedText: string; truncated: boolean;
} {
    const limit = Math.max(0, Math.min(MAX_EDIT_CHARACTERS, budget));
    const start = selection ? positionOffset(text, selection.start) : 0;
    const end = selection ? positionOffset(text, selection.end) : start;
    const contentOffset = text.length > limit ? Math.max(0, Math.min(start - Math.floor(limit / 3), text.length - limit)) : 0;
    return {
        content: text.slice(contentOffset, contentOffset + limit),
        contentOffset,
        selectedText: text.slice(start, Math.min(end, start + limit)),
        truncated: text.length > limit || end - start > limit
    };
}

export function summarizeApplyResult(operation: EditOperation, files: FileApplyResult[]): ApplyResult {
    const applied = files.filter(file => file.status === 'applied').length;
    return {
        operationId: operation.operationId, sessionId: operation.sessionId, runId: operation.runId,
        status: applied === files.length ? 'applied' : applied > 0 ? 'partial'
            : files.some(file => file.status === 'conflict') ? 'conflict'
                : files.some(file => file.status === 'failed') ? 'failed' : 'rejected',
        files
    };
}
