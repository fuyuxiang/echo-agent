import { ThemeService } from '@theia/core/lib/browser/theming';
import { MonacoThemeRegistry } from '@theia/monaco/lib/browser/textmate/monaco-theme-registry';

/** Consume resolved host colours while inheriting Theia's syntax grammar.
 * This bridge has no preference or theme state separate from its host. */
export function applyEchoTheme(themes: ThemeService, registry: MonacoThemeRegistry,
    mode: 'light' | 'dark', supplied: unknown, commentColor?: unknown, syntaxColors?: unknown): void {
    const base = registry.getThemeData(mode === 'dark' ? 'dark-theia' : 'light-theia');
    if (!base || !supplied || typeof supplied !== 'object' || Array.isArray(supplied)) {
        themes.setCurrentTheme(mode, false);
        return;
    }
    const colors = Object.fromEntries(Object.entries(supplied).filter(([key, value]) =>
        /^[a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9]+)*$/.test(key)
        && typeof value === 'string' && /^#[\da-f]{6}(?:[\da-f]{2})?$/i.test(value)));
    const id = `echo-${mode}`;
    const comment = typeof commentColor === 'string' && /^#[\da-f]{6}$/i.test(commentColor) ? commentColor : undefined;
    const syntax = syntaxColors && typeof syntaxColors === 'object' && !Array.isArray(syntaxColors)
        ? Object.fromEntries(Object.entries(syntaxColors).filter(([key, value]) => ['type', 'number', 'constant'].includes(key)
            && typeof value === 'string' && /^#[\da-f]{6}$/i.test(value))) : {};
    const inherited = base.settings.filter(setting => setting.scope).map(setting => {
        const foreground = setting.settings.foreground?.toLowerCase();
        const replacement = foreground === (mode === 'dark' ? '#4ec9b0' : '#267f99') ? syntax.type
            : foreground === (mode === 'dark' ? '#b5cea8' : '#098658') ? syntax.number
            : foreground === (mode === 'dark' ? '#4fc1ff' : '#0070c1') ? syntax.constant : undefined;
        return replacement ? { ...setting, settings: { ...setting.settings, foreground: replacement } } : setting;
    });
    // Rebuild the TextMate colour map through the public registration path.
    // Copying encodedTokensColors after adding a scope shifts token indexes,
    // producing unrelated syntax colours despite correct workbench surfaces.
    registry.register({ name: id, colors: { ...base.colors, ...colors },
        tokenColors: [...inherited,
            ...(comment ? [{ scope: 'comment', settings: { foreground: comment } }] : [])]
    }, undefined, id, base.base);
    if (!themes.getThemes().some(theme => theme.id === id)) {
        themes.register({ id, type: mode, label: mode === 'dark' ? 'EchoAgent Dark' : 'EchoAgent Light', editorTheme: id });
    }
    themes.setCurrentTheme(id, false);
}
