import { echoEmbedSession } from '@theia/core/lib/browser/echo-embed-session';

/** Messages are accepted only from the embedding EchoAgent window. */
export class EchoHostBridge {
    readonly instanceId = crypto.randomUUID();
    private readonly token = echoEmbedSession.bridgeToken;
    private readonly parentOrigin = echoEmbedSession.parentOrigin;
    private readonly pending = new Map<string, {
        resolve: (value: unknown) => void;
        reject: (reason: Error) => void;
        timeout: number;
        cleanup: () => void;
        instanceRequired: boolean;
    }>();

    constructor() {
        window.addEventListener('message', event => {
            if (!this.isTrusted(event)) {
                return;
            }
            const message = event.data as Record<string, unknown>;
            if (message.type !== 'echo/response' || typeof message.id !== 'string') {
                return;
            }
            const pending = this.pending.get(message.id);
            if (!pending) {
                return;
            }
            if (pending.instanceRequired && message.instanceId !== this.instanceId) { return; }
            this.pending.delete(message.id);
            window.clearTimeout(pending.timeout);
            pending.cleanup();
            if (message.ok === true) {
                pending.resolve(message.value);
            } else {
                pending.reject(new Error(typeof message.error === 'string' ? message.error : 'EchoAgent 拒绝了文件操作'));
            }
        });
    }

    get enabled(): boolean {
        return !!this.token && !!this.parentOrigin && window.parent !== window;
    }

    isTrusted(event: MessageEvent): boolean {
        return this.enabled && event.source === window.parent && event.origin === this.parentOrigin
            && typeof event.data === 'object' && event.data !== null
            && (event.data as Record<string, unknown>).token === this.token
            && ((event.data as Record<string, unknown>).instanceId === undefined
                || (event.data as Record<string, unknown>).instanceId === this.instanceId);
    }

    notify(type: string, payload: Record<string, unknown> = {}): void {
        if (this.enabled) {
            window.parent.postMessage({ ...payload, type, token: this.token, instanceId: this.instanceId }, this.parentOrigin!);
        }
    }

    request(type: string, payload: Record<string, unknown>, options: { signal?: AbortSignal; timeoutMs?: number } = {}): Promise<unknown> {
        if (!this.enabled) {
            return Promise.resolve(undefined);
        }
        const id = crypto.randomUUID();
        return new Promise((resolve, reject) => {
            if (options.signal?.aborted) {
                reject(new Error('EchoAgent 请求已取消'));
                return;
            }
            const abort = () => {
                this.pending.delete(id);
                window.clearTimeout(timeout);
                cleanup();
                reject(new Error('EchoAgent 请求已取消'));
            };
            const cleanup = () => options.signal?.removeEventListener('abort', abort);
            const timeout = window.setTimeout(() => {
                this.pending.delete(id);
                cleanup();
                reject(new Error('EchoAgent 文件操作响应超时'));
            }, options.timeoutMs ?? 15_000);
            this.pending.set(id, { resolve, reject, timeout, cleanup, instanceRequired: type === 'echo/inline-completion' });
            options.signal?.addEventListener('abort', abort, { once: true });
            this.notify(type, { ...payload, id });
        });
    }
}

export const echoHostBridge = new EchoHostBridge();
