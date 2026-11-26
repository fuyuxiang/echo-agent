//! Formatting helpers for LSP results.

use async_lsp::lsp_types::{self, Location, SymbolInformation, Url};

pub fn markup_string_to_text(ms: lsp_types::MarkedString) -> String {
    match ms {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}

pub fn flatten_document_symbols(
    symbols: &[lsp_types::DocumentSymbol],
    uri: &Url,
    out: &mut Vec<SymbolInformation>,
) {
    for sym in symbols {
        #[allow(deprecated)] // container_name is deprecated but still the LSP way
        out.push(SymbolInformation {
            name: sym.name.clone(),
            kind: sym.kind,
            tags: sym.tags.clone(),
            deprecated: sym.deprecated,
            location: Location {
                uri: uri.clone(),
                range: sym.range,
            },
            container_name: None,
        });
        if let Some(ref children) = sym.children {
            flatten_document_symbols(children, uri, out);
        }
    }
}

pub fn format_locations_labeled(label: &str, locations: &[Location]) -> String {
    if locations.is_empty() {
        return "No results found.".to_string();
    }
    let paths: Vec<_> = locations
        .iter()
        .map(|loc| {
            let path = loc.uri.path();
            let line = loc.range.start.line + 1;
            let col = loc.range.start.character + 1;
            format!("  {path}:{line}:{col}")
        })
        .collect();
    format!(
        "{label} ({} location{}):\n{}",
        paths.len(),
        if paths.len() == 1 { "" } else { "s" },
        paths.join("\n")
    )
}

pub fn format_symbols(symbols: &[SymbolInformation]) -> String {
    if symbols.is_empty() {
        return "No symbols found.".to_string();
    }
    symbols
        .iter()
        .map(|sym| {
            let path = sym.location.uri.path();
            let line = sym.location.range.start.line + 1;
            format!("{:?} {} ({}:{})", sym.kind, sym.name, path, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
