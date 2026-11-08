/********************************************************************************
 * Copyright (C) 2022 TypeFox and others.
 *
 * This program and the accompanying materials are made available under the
 * terms of the Eclipse Public License v. 2.0 which is available at
 * http://www.eclipse.org/legal/epl-2.0.
 *
 * This Source Code may also be made available under the following Secondary
 * Licenses when the conditions for such availability set forth in the Eclipse
 * Public License v. 2.0 are satisfied: GNU General Public License, version 2
 * with the GNU Classpath Exception which is available at
 * https://www.gnu.org/software/classpath/license.html.
 *
 * SPDX-License-Identifier: EPL-2.0 OR GPL-2.0-only WITH Classpath-exception-2.0
 ********************************************************************************/

import { expect } from 'chai';
import { RequestContext, RequestOptions, RequestService } from '@theia/request';
import { XHRBrowserRequestService } from './browser-request-service';

describe('Browser request proxy credential isolation', () => {
    let originalXHR: typeof XMLHttpRequest;
    let mode: 'success' | 'error' | 'forbidden';
    let sentHeaders: Record<string, string>;
    let fallbackOptions: RequestOptions | undefined;
    let configuredToken: string | undefined;
    let service: XHRBrowserRequestService;

    beforeEach(async () => {
        originalXHR = globalThis.XMLHttpRequest;
        mode = 'success';
        sentHeaders = {};
        fallbackOptions = undefined;
        configuredToken = undefined;
        class FakeXHR {
            status = 200;
            response = new ArrayBuffer(0);
            onerror?: () => void;
            onload?: () => void;
            open(): void { }
            setRequestHeader(name: string, value: string): void { sentHeaders[name.toLowerCase()] = value; }
            getAllResponseHeaders(): string { return ''; }
            send(): void {
                queueMicrotask(() => {
                    if (mode === 'error') {
                        this.onerror?.();
                    } else {
                        this.status = mode === 'forbidden' ? 403 : 200;
                        this.onload?.();
                    }
                });
            }
        }
        globalThis.XMLHttpRequest = FakeXHR as unknown as typeof XMLHttpRequest;
        const backend: RequestService = {
            async configure(config): Promise<void> { configuredToken = config.proxyAuthorization; },
            async resolveProxy(): Promise<undefined> { return undefined; },
            async request(options): Promise<RequestContext> {
                fallbackOptions = options;
                return { url: options.url, res: { statusCode: 200, headers: {} }, buffer: new Uint8Array() };
            }
        };
        service = new XHRBrowserRequestService();
        Object.defineProperty(service, 'backendRequestService', { value: backend });
        await service.configure({ proxyAuthorization: 'Basic configured-fixture' });
    });

    afterEach(() => { globalThis.XMLHttpRequest = originalXHR; });

    for (const authorization of ['configured', 'request override'] as const) {
        for (const result of ['success', 'error', 'forbidden'] as const) {
            it(`keeps proxy credentials out of origin headers on XHR ${result} with ${authorization}`, async () => {
                mode = result;
                const options: RequestOptions = {
                    url: 'https://origin.example.invalid/resource',
                    proxyAuthorization: authorization === 'request override' ? 'Bearer request-fixture' : undefined,
                    headers: { Authorization: 'Bearer origin-fixture', 'X-Fixture': 'preserved' }
                };
                const original = { ...options, headers: { ...options.headers } };
                const response = await service.request(options);
                expect(response.res.statusCode).to.equal(200);
                expect(options).to.deep.equal(original);
                expect(configuredToken).to.equal('Basic configured-fixture');
                expect(sentHeaders).to.deep.equal({ authorization: 'Bearer origin-fixture', 'x-fixture': 'preserved' });
                expect(fallbackOptions).to.deep.equal(result === 'success' ? undefined : original);
            });
        }
    }

    it('filters caller-supplied mixed-case proxy headers without removing origin authorization', async () => {
        await service.request({ url: 'https://origin.example.invalid/', headers: {
            'pRoXy-AuThOrIzAtIoN': 'Basic legacy-fixture', Authorization: 'Bearer origin-fixture'
        } });
        expect(sentHeaders).to.deep.equal({ authorization: 'Bearer origin-fixture' });
        expect(fallbackOptions).to.equal(undefined);
    });
});
