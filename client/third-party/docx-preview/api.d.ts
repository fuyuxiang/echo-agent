export declare function decryptDocx(data: ArrayBuffer | Uint8Array, password: string): Promise<ArrayBuffer>;

export declare const defaultOptions: Options;

export declare function detectOfficeFileKind(data: ArrayBuffer | Uint8Array): OfficeFileKind;

export declare class DocxEncryptionUnsupportedError extends Error {
    constructor(message?: string);
}

export declare class DocxPasswordError extends Error {
    constructor(message?: string);
}

export declare interface FontDefinition {
    name: string;
    src: string;
    weight?: string | number;
    style?: string;
}

declare function h(elem: HElement | Node | string): Node;

declare type HElement = {
    ns?: ns;
    tagName: "#fragment" | "#comment" | string;
    className?: string;
    style?: string | Record<string, string>;
    children?: (HElement | Node | string)[];
} & Record<string, any>;

export declare function isEncryptedDocx(data: ArrayBuffer | Uint8Array): boolean;

declare enum ns {
    html = "http://www.w3.org/1999/xhtml",
    svg = "http://www.w3.org/2000/svg",
    mathML = "http://www.w3.org/1998/Math/MathML"
}

export declare type OfficeFileKind = 'ooxml' | 'encrypted' | 'legacy-binary' | 'unknown';

export declare interface Options {
    inWrapper: boolean;
    hideWrapperOnPrint: boolean;
    ignoreWidth: boolean;
    ignoreHeight: boolean;
    ignoreFonts: boolean;
    breakPages: boolean;
    paginate: boolean;
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

export declare function parseAsync(data: Blob | any, userOptions?: Partial<Options>): Promise<any>;

export declare function renderAsync(data: Blob | any, bodyContainer: HTMLElement, styleContainer?: HTMLElement, userOptions?: Partial<Options>): Promise<any>;

export declare function renderDocument(document: any, userOptions?: Partial<Options>): Promise<any>;

export { }
