//! JavaScript/JSX language configuration.

use crate::languages::types::TSLanguageConfig;

pub fn js_lang() -> TSLanguageConfig {
    TSLanguageConfig::new(
        vec![
            "JavaScript".to_owned(),
            "javascript".to_owned(),
            "js".to_owned(),
            "jsx".to_owned(),
        ],
        vec!["js".to_owned(), "jsx".to_owned()],
        vec![vec![
            "function".to_owned(),
            "class".to_owned(),
            "variable".to_owned(),
            "const".to_owned(),
            "let".to_owned(),
        ]],
        r#"
        ; Class definitions
        (class_declaration
            name: (identifier) @name.definition.class) @definition.class
        
        ; Function definitions
        (function_declaration
            name: (identifier) @name.definition.function) @definition.function
        
        ; Arrow function with variable
        (lexical_declaration
            (variable_declarator
                name: (identifier) @name.definition.function
                value: (arrow_function))) @definition.function
        
        ; Method definitions
        (method_definition
            name: (property_identifier) @name.definition.method) @definition.method
        
        ; Variable declarations
        (lexical_declaration
            (variable_declarator
                name: (identifier) @name.definition.variable)) @definition.variable
        
        ; Var declarations
        (variable_declaration
            (variable_declarator
                name: (identifier) @name.definition.variable)) @definition.variable
        
        ; ============ REFERENCES ============
        
        ; Function calls
        (call_expression
            function: (identifier) @name.reference.call) @reference.call
        
        ; Method calls
        (call_expression
            function: (member_expression
                property: (property_identifier) @name.reference.call)) @reference.call
        
        ; JSX element names
        (jsx_opening_element
            name: (identifier) @name.reference.jsx)
        
        (jsx_self_closing_element
            name: (identifier) @name.reference.jsx)
        
        ; ============ IMPORTS ============
        
        ; Named imports: import { Foo } from 'bar'
        (import_specifier
            name: (identifier) @name.reference.import)
        
        ; Default import: import Foo from 'bar'
        (import_clause
            (identifier) @name.reference.import)
        
        ; Import alias: import { Foo as Bar } from 'bar'
        (import_specifier
            name: (identifier) @alias.original
            alias: (identifier) @alias.name)
        
        ; Named exports: export { Foo }
        (export_specifier
            name: (identifier) @name.reference.export)
        
        ; Array element identifiers: [foo, bar] (e.g., React useCallback/useEffect dependency arrays)
        (array
            (identifier) @name.reference.variable)
        "#
        .to_owned(),
        || tree_sitter_javascript::LANGUAGE.into(),
    )
}
