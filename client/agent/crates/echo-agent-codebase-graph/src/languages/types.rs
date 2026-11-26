use crate::scope_graph::nodes::SymbolId;

/// Function type for getting a tree-sitter language grammar.
pub type GrammarFn = fn() -> tree_sitter::Language;

/// Contains information about the language and extra information which we would need
/// per language
pub struct TSLanguageConfig {
    language_ids: Vec<String>,
    file_extensions: Vec<String>,
    namespaces: Vec<Vec<String>>,
    file_definition_queries: String,
    grammar: GrammarFn,
}

impl TSLanguageConfig {
    pub fn new(
        language_ids: Vec<String>,
        file_extensions: Vec<String>,
        namespaces: Vec<Vec<String>>,
        file_definition_queries: String,
        grammar: GrammarFn,
    ) -> Self {
        Self {
            language_ids,
            file_extensions,
            namespaces,
            file_definition_queries,
            grammar,
        }
    }

    /// Get the language IDs.
    pub fn language_ids(&self) -> &[String] {
        &self.language_ids
    }

    /// Get the first language ID, or "unknown" if none.
    pub fn primary_language_id(&self) -> &str {
        self.language_ids
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown")
    }

    /// Get the file extensions.
    pub fn file_extensions(&self) -> &[String] {
        &self.file_extensions
    }

    /// Get the namespaces.
    pub fn namespaces(&self) -> &[Vec<String>] {
        &self.namespaces
    }

    /// Get the file definition queries.
    pub fn file_definition_queries(&self) -> &str {
        &self.file_definition_queries
    }

    /// Get the tree-sitter language.
    pub fn language(&self) -> tree_sitter::Language {
        (self.grammar)()
    }

    /// Compile the definitions query for this language.
    pub fn compile_query(&self) -> Result<tree_sitter::Query, tree_sitter::QueryError> {
        tree_sitter::Query::new(&self.language(), &self.file_definition_queries)
    }

    /// Find a SymbolId for a given symbol type name.
    pub fn symbol_id_of(&self, symbol_type: &str) -> Option<SymbolId> {
        for (ns_idx, namespace) in self.namespaces.iter().enumerate() {
            for (sym_idx, sym) in namespace.iter().enumerate() {
                if sym == symbol_type {
                    return Some(SymbolId::new(ns_idx, sym_idx));
                }
            }
        }
        None
    }
}
