//! Python language configuration.

use crate::languages::types::TSLanguageConfig;

pub fn python_lang() -> TSLanguageConfig {
    TSLanguageConfig::new(
        vec!["Python".to_owned(), "python".to_owned(), "py".to_owned()],
        vec!["py".to_owned()],
        vec![vec![
            "function".to_owned(),
            "class".to_owned(),
            "variable".to_owned(),
            "module".to_owned(),
        ]],
        // Python definitions query
        r#"
        ; Class definitions
        (class_definition
            name: (identifier) @name.definition.class) @definition.class
        
        ; Function definitions
        (function_definition
            name: (identifier) @name.definition.function) @definition.function
        
        ; ============ REFERENCES ============
        
        ; Function calls (direct and method calls)
        (call
            function: [
                (identifier) @name.reference.call
                (attribute
                    attribute: (identifier) @name.reference.call)
            ]) @reference.call
        "#
        .to_owned(),
        || tree_sitter_python::LANGUAGE.into(),
    )
}
