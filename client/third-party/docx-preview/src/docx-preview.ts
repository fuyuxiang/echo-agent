import { WordDocument } from './word-document';
import { DocumentParser } from './document-parser';
import { HtmlRenderer } from './html-renderer';
import { h } from './html';
import { paginateWrapper } from './pagination';

// Encrypted-document support: classify the container, then decrypt with a
// password. Kept framework-agnostic so every viewer shares one implementation.
export {
	detectOfficeFileKind,
	isEncryptedDocx,
	decryptDocx,
	DocxPasswordError,
	DocxEncryptionUnsupportedError,
} from './encryption';
export type { OfficeFileKind } from './encryption';

export interface FontDefinition {
    /** font-family name as used in the document, e.g. "SimSun" / "宋体" */
    name: string;
    /** stylesheet src value, e.g. 'url(fonts/simsun.woff2) format("woff2")' */
    src: string;
    weight?: string | number;
    style?: string;
}

export interface Options {
    inWrapper: boolean;
    hideWrapperOnPrint: boolean;
    ignoreWidth: boolean;
    ignoreHeight: boolean;
    ignoreFonts: boolean;
    breakPages: boolean;
    /** re-flow content into page-sized sections after render (needs DOM layout) */
    paginate: boolean;
    /** external webfonts injected as @font-face before rendering */
    fonts?: FontDefinition[];
    debug: boolean;
    experimental: boolean;
    className: string;
    trimXmlDeclaration: boolean;
    renderHeaders: boolean;
    renderFooters: boolean;
    renderFootnotes: boolean;
	renderEndnotes: boolean;
    ignoreLastRenderedPageBreak: boolean;
	useBase64URL: boolean;
	renderChanges: boolean;
    renderComments: boolean;
    renderAltChunks: boolean;
    h: typeof h;
}

export const defaultOptions: Options = {
    ignoreHeight: false,
    ignoreWidth: false,
    ignoreFonts: false,
    breakPages: true,
    paginate: false,
    debug: false,
    experimental: false,
    className: "docx",
    inWrapper: true,
    hideWrapperOnPrint: false,
    trimXmlDeclaration: true,
    ignoreLastRenderedPageBreak: true,
    renderHeaders: true,
    renderFooters: true,
    renderFootnotes: true,
	renderEndnotes: true,
	useBase64URL: false,
	renderChanges: false,
    renderComments: false,
    renderAltChunks: true,
    h: h
};

export function parseAsync(data: Blob | any, userOptions?: Partial<Options>): Promise<any>  {
    const ops = { ...defaultOptions, ...userOptions };
    return WordDocument.load(data, new DocumentParser(ops), ops);
}

export async function renderDocument(document: any, userOptions?: Partial<Options>): Promise<any> {
    const ops = { ...defaultOptions, ...userOptions };
    const renderer = new HtmlRenderer();
    return await renderer.render(document, ops);
}

function renderFontFaces(fonts: FontDefinition[]): HTMLElement {
    const css = fonts.map(f => {
        const family = f.name.replace(/["\\\r\n]/g, '');
        const props = [`font-family: "${family}"`, `src: ${f.src}`];
        if (f.weight != null) props.push(`font-weight: ${f.weight}`);
        if (f.style != null) props.push(`font-style: ${f.style}`);
        return `@font-face { ${props.join('; ')}; }`;
    }).join('\n');

    const el = document.createElement("style");
    el.textContent = css;
    return el;
}

export async function renderAsync(data: Blob | any, bodyContainer: HTMLElement, styleContainer?: HTMLElement, userOptions?: Partial<Options>): Promise<any> {
	const doc = await parseAsync(data, userOptions);
	const nodes = await renderDocument(doc, userOptions);
    const ops = { ...defaultOptions, ...userOptions };

    styleContainer ??= bodyContainer;
    styleContainer.innerHTML = "";
    bodyContainer.innerHTML = "";

    if (ops.fonts?.length) {
        nodes.unshift(renderFontFaces(ops.fonts));
    }

    for (let n of nodes) {
        const c = n.nodeName === "STYLE" ? styleContainer : bodyContainer;
        c.appendChild(n);
    }

    // Make sure webfonts are actually loaded before the content is measured
    // (pagination) or painted, otherwise fallback-font metrics leak in.
    // Modified by EchoAgent: measure after fonts in the rendering document load.
    const fontSet = bodyContainer.ownerDocument.fonts;
    if (ops.fonts?.length && fontSet?.load) {
        try {
            await Promise.all(ops.fonts.map(f =>
                fontSet.load(`${f.style ?? 'normal'} ${f.weight ?? 'normal'} 12px "${f.name.replace(/["\\\r\n]/g, '')}"`)));
        } catch { /* a missing webfont must not break rendering */ }
    }

    if (ops.paginate) {
        if (fontSet?.ready) {
            try { await fontSet.ready; } catch { /* ignore */ }
        }
        paginateWrapper(bodyContainer, ops.className);
    }

    return doc;
}