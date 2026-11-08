// *****************************************************************************
// Copyright (C) 2025 EclipseSource and others.
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License v. 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0.
//
// This Source Code may also be made available under the following Secondary
// Licenses when the conditions for such availability set forth in the Eclipse
// Public License v. 2.0 are satisfied: GNU General Public License, version 2
// with the GNU Classpath Exception which is available at
// https://www.gnu.org/software/classpath/license.html.
//
// SPDX-License-Identifier: EPL-2.0 OR GPL-2.0-only WITH Classpath-exception-2.0
// *****************************************************************************

import * as assert from 'assert';
import * as React from 'react';
import { createRoot, Root } from 'react-dom/client';
import { enableJSDOM } from '../test/jsdom';
import { Markdown, LocalizedMarkdown } from './markdown';
import { MarkdownRenderer } from './markdown-renderer';
import { MarkdownString, MarkdownStringImpl } from '../../common/markdown-rendering/markdown-string';

let disableJSDOM: () => void;

function enableMarkdownDOM(): () => void {
    const environment = globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean };
    const previous = environment.IS_REACT_ACT_ENVIRONMENT;
    environment.IS_REACT_ACT_ENVIRONMENT = true;
    const disable = enableJSDOM();
    return () => {
        disable();
        environment.IS_REACT_ACT_ENVIRONMENT = previous;
    };
}

describe('Markdown', () => {
    let mockRenderer: MarkdownRenderer & { lastRenderedMarkdown?: MarkdownString };
    let container: HTMLElement;
    let root: Root;

    before(() => disableJSDOM = enableMarkdownDOM());
    after(() => disableJSDOM());

    beforeEach(() => {
        container = document.createElement('div');
        document.body.appendChild(container);
        root = createRoot(container);

        mockRenderer = {
            lastRenderedMarkdown: undefined,
            render: (markdown: MarkdownString | undefined) => {
                // Store the markdown for verification
                if (typeof markdown === 'object' && 'value' in markdown) {
                    mockRenderer.lastRenderedMarkdown = markdown;
                }
                const div = document.createElement('div');
                if (markdown) {
                    const p = document.createElement('p');
                    const value = typeof markdown === 'object' && 'value' in markdown
                        ? markdown.value
                        : String(markdown);
                    p.textContent = value;
                    div.appendChild(p);
                }
                return {
                    element: div,
                    dispose: () => { }
                };
            }
        };
    });

    afterEach(() => {
        React.act(() => root.unmount());
        document.body.removeChild(container);
    });

    it('should render markdown content', async () => {
        await React.act(async () => root.render(
            <Markdown
                markdown="**Hello World**"
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.ok(div?.textContent?.includes('Hello World'), 'Should contain markdown text');
    });

    it('should render empty div when markdown is undefined', async () => {
        await React.act(async () => root.render(
            <Markdown
                markdown={undefined}
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.strictEqual(div?.childNodes.length, 0, 'Should have no children');
    });

    it('should render empty div when markdown is empty string', async () => {
        await React.act(async () => root.render(
            <Markdown
                markdown=""
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.strictEqual(div?.childNodes.length, 0, 'Should have no children');
    });

    it('should render empty div when markdown is whitespace only', async () => {
        await React.act(async () => root.render(
            <Markdown
                markdown="   "
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.strictEqual(div?.childNodes.length, 0, 'Should have no children');
    });

    it('should accept MarkdownString object', async () => {
        const markdownString = new MarkdownStringImpl('**Bold Text**');
        await React.act(async () => root.render(
            <Markdown
                markdown={markdownString}
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.ok(div?.textContent?.includes('Bold Text'), 'Should contain markdown text');
    });

    it('should call onRender callback when content is rendered', async () => {
        let renderCallbackCalled = false;
        let receivedElement: HTMLElement | undefined;

        await React.act(async () => root.render(
            <Markdown
                markdown="Test content"
                markdownRenderer={mockRenderer}
                className="test-class"
                onRender={element => {
                    renderCallbackCalled = true;
                    receivedElement = element;
                }}
            />
        ));

        assert.ok(renderCallbackCalled, 'onRender should be called');
        assert.ok(receivedElement, 'Should receive element');
    });

    it('should call onRender callback with undefined when content is empty', async () => {
        let renderCallbackCalled = false;
        let receivedElement: HTMLElement | undefined = document.createElement('div'); // Initialize to non-undefined

        await React.act(async () => root.render(
            <Markdown
                markdown={undefined}
                markdownRenderer={mockRenderer}
                className="test-class"
                onRender={element => {
                    renderCallbackCalled = true;
                    receivedElement = element;
                }}
            />
        ));

        assert.ok(renderCallbackCalled, 'onRender should be called even for empty content');
        assert.strictEqual(receivedElement, undefined, 'Should receive undefined for empty content');
    });

    it('should apply className to container', async () => {
        await React.act(async () => root.render(
            <Markdown
                markdown="Test"
                markdownRenderer={mockRenderer}
                className="custom-class"
            />
        ));

        const div = container.querySelector('.custom-class');
        assert.ok(div, 'Container with custom class should exist');
    });
});

describe('LocalizedMarkdown', () => {
    let mockRenderer: MarkdownRenderer & { lastRenderedMarkdown?: MarkdownString };
    let container: HTMLElement;
    let root: Root;

    before(() => disableJSDOM = enableMarkdownDOM());
    after(() => disableJSDOM());

    beforeEach(() => {
        container = document.createElement('div');
        document.body.appendChild(container);
        root = createRoot(container);

        mockRenderer = {
            lastRenderedMarkdown: undefined,
            render: (markdown: MarkdownString | undefined) => {
                // Store the markdown for verification
                if (typeof markdown === 'object' && 'value' in markdown) {
                    mockRenderer.lastRenderedMarkdown = markdown;
                }
                const div = document.createElement('div');
                if (markdown) {
                    const p = document.createElement('p');
                    const value = typeof markdown === 'object' && 'value' in markdown
                        ? markdown.value
                        : String(markdown);
                    p.textContent = value;
                    div.appendChild(p);
                }
                return {
                    element: div,
                    dispose: () => { }
                };
            }
        };
    });

    afterEach(() => {
        React.act(() => root.unmount());
        document.body.removeChild(container);
    });

    it('should render localized markdown content', async () => {
        await React.act(async () => root.render(
            <LocalizedMarkdown
                localizationKey="test/basic"
                defaultMarkdown="Welcome to **Theia**!"
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        // The content should be localized (though in tests it will be the default)
        assert.ok(div?.textContent?.includes('Theia'), 'Should contain localized text');
    });

    it('should render localized markdown with parameters', async () => {
        await React.act(async () => root.render(
            <LocalizedMarkdown
                localizationKey="test/greeting"
                defaultMarkdown="Hello **{0}**! You have {1} messages."
                args={['Alice', 5]}
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.ok(div?.textContent?.includes('Alice'), 'Should contain first parameter');
        assert.ok(div?.textContent?.includes('5'), 'Should contain second parameter');
    });

    it('should render empty div when default markdown is empty', async () => {
        await React.act(async () => root.render(
            <LocalizedMarkdown
                localizationKey="test/empty"
                defaultMarkdown=""
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.strictEqual(div?.childNodes.length, 0, 'Should have no children');
    });

    it('should update when localization key changes', async () => {

        await React.act(async () => root.render(
            <LocalizedMarkdown
                localizationKey="test/first"
                defaultMarkdown="First content"
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div?.textContent?.includes('First'), 'Should contain first content');

        await React.act(async () => root.render(
            <LocalizedMarkdown
                localizationKey="test/second"
                defaultMarkdown="Second content"
                markdownRenderer={mockRenderer}
                className="test-class"
            />
        ));

        const updatedDiv = container.querySelector('.test-class');
        assert.ok(updatedDiv?.textContent?.includes('Second'), 'Should contain second content');
    });

    it('should pass markdown options correctly', async () => {
        await React.act(async () => root.render(
            <LocalizedMarkdown
                localizationKey="test/html"
                defaultMarkdown="Content with <span>HTML</span>"
                markdownRenderer={mockRenderer}
                className="test-class"
                markdownOptions={{ supportHtml: true, supportThemeIcons: true, isTrusted: true }}
            />
        ));

        const div = container.querySelector('.test-class');
        assert.ok(div, 'Container should exist');
        assert.ok(mockRenderer.lastRenderedMarkdown, 'Should have rendered markdown');
        assert.strictEqual(mockRenderer.lastRenderedMarkdown?.supportHtml, true, 'Should pass supportHtml option');
        assert.strictEqual(mockRenderer.lastRenderedMarkdown?.supportThemeIcons, true, 'Should pass supportThemeIcons option');
        assert.strictEqual(mockRenderer.lastRenderedMarkdown?.isTrusted, true, 'Should pass isTrusted option');
    });
});
