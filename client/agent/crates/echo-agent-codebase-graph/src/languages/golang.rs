use crate::languages::types::TSLanguageConfig;

pub fn golang() -> TSLanguageConfig {
    TSLanguageConfig::new(
        vec!["Go".to_owned(), "go".to_owned()],
        vec!["go".to_owned()],
        vec![vec![
            "function".to_owned(),
            "type".to_owned(),
            "struct".to_owned(),
            "interface".to_owned(),
            "const".to_owned(),
            "var".to_owned(),
            "package".to_owned(),
        ]],
        r#"
        ; Function definitions
        (function_declaration
            name: (identifier) @name.definition.function) @definition.function
        
        ; Method definitions
        (method_declaration
            name: (field_identifier) @name.definition.method) @definition.method
        
        ; Type definitions (struct, interface, etc.)
        (type_declaration
            (type_spec
                name: (type_identifier) @name.definition.type)) @definition.type
        
        ; Const declarations
        (const_declaration
            (const_spec
                name: (identifier) @name.definition.const)) @definition.const
        
        ; Var declarations
        (var_declaration
            (var_spec
                name: (identifier) @name.definition.var)) @definition.var
        
        ; ============ REFERENCES ============
        
        ; Function calls
        (call_expression
            function: (identifier) @name.reference.call) @reference.call
        
        ; Method calls
        (call_expression
            function: (selector_expression
                field: (field_identifier) @name.reference.call)) @reference.call
        
        ; Type references
        (type_identifier) @name.reference.type
        
        ; Package references in qualified names
        (qualified_type
            package: (package_identifier) @name.reference.package
            name: (type_identifier) @name.reference.type)
        
        ; ============ IMPORTS ============
        
        ; import "package"
        (import_spec
            path: (interpreted_string_literal) @name.reference.import)
        
        ; import alias "package"
        (import_spec
            name: (package_identifier) @alias.name
            path: (interpreted_string_literal) @alias.original)
        "#
        .to_owned(),
        || tree_sitter_go::LANGUAGE.into(),
    )
}
