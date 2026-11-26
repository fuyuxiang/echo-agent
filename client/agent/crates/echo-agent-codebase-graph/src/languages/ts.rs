use crate::languages::types::TSLanguageConfig;

pub fn ts_lang() -> TSLanguageConfig {
    TSLanguageConfig::new(
        vec![
            "Typescript".to_owned(),
            "TSX".to_owned(),
            "typescript".to_owned(),
            "tsx".to_owned(),
        ],
        vec!["ts".to_owned(), "tsx".to_owned()],
        vec![vec![
            "function".to_owned(),
            "class".to_owned(),
            "interface".to_owned(),
            "type".to_owned(),
            "enum".to_owned(),
            "variable".to_owned(),
            "const".to_owned(),
            "let".to_owned(),
        ]],
        // Comprehensive TypeScript query with full type coverage
        r#"
          ;; === DEFINITIONS ===
          
          (function_signature
            name: (identifier) @name.definition.function) @definition.function
          
          (method_signature
            name: (property_identifier) @name.definition.method) @definition.method
          
          (abstract_method_signature
            name: (property_identifier) @name.definition.method) @definition.method
          
          (abstract_class_declaration
            name: (type_identifier) @name.definition.class) @definition.class
          
          (module
            name: (identifier) @name.definition.module) @definition.module
          
          (interface_declaration
            name: (type_identifier) @name.definition.interface) @definition.interface
          
          (function_declaration
            name: (identifier) @name.definition.function) @definition.function
          
          (method_definition
            name: (property_identifier) @name.definition.method) @definition.method
          
          (class_declaration
            name: (type_identifier) @name.definition.class) @definition.class
          
          (type_alias_declaration
            name: (type_identifier) @name.definition.type) @definition.type
          
          (enum_declaration
            name: (identifier) @name.definition.enum) @definition.enum
          
          ;; Arrow function assigned to variable: const foo = () => {}
          (lexical_declaration
            (variable_declarator
              name: (identifier) @name.definition.function
              value: (arrow_function))) @definition.function
          
          ;; React component patterns: const Foo = React.forwardRef(...), React.memo(...)
          (lexical_declaration
            (variable_declarator
              name: (identifier) @name.definition.function
              value: (call_expression))) @definition.function
          
          ;; Variable declarations (const/let)
          (lexical_declaration
            (variable_declarator
              name: (identifier) @name.definition.variable)) @definition.variable
          
          ;; Var declarations
          (variable_declaration
            (variable_declarator
              name: (identifier) @name.definition.variable)) @definition.variable
          
          ;; Exported variable declarations: export const foo = ...
          (export_statement
            (lexical_declaration
              (variable_declarator
                name: (identifier) @name.definition.variable))) @definition.variable
          
          ;; === DESTRUCTURING DEFINITIONS ===
          
          ;; For-of/for-in loop with array destructuring: for (const [a, b] of items)
          (for_in_statement
            left: (array_pattern
              (identifier) @name.definition.variable))
          
          ;; For-of/for-in loop with object destructuring: for (const { a, b } of items)
          (for_in_statement
            left: (object_pattern
              (shorthand_property_identifier_pattern) @name.definition.variable))
          
          ;; For-of/for-in loop with object destructuring (aliased): for (const { a: b } of items)
          (for_in_statement
            left: (object_pattern
              (pair_pattern
                value: (identifier) @name.definition.variable)))
          
          ;; Regular array destructuring: const [a, b] = someArray
          (lexical_declaration
            (variable_declarator
              name: (array_pattern
                (identifier) @name.definition.variable)))
          
          ;; Regular object destructuring (shorthand): const { a, b } = someObject
          (lexical_declaration
            (variable_declarator
              name: (object_pattern
                (shorthand_property_identifier_pattern) @name.definition.variable)))
          
          ;; Regular object destructuring (aliased): const { a: b } = someObject
          (lexical_declaration
            (variable_declarator
              name: (object_pattern
                (pair_pattern
                  value: (identifier) @name.definition.variable))))
          
          ;; Var array destructuring: var [a, b] = someArray
          (variable_declaration
            (variable_declarator
              name: (array_pattern
                (identifier) @name.definition.variable)))
          
          ;; Var object destructuring (shorthand): var { a, b } = someObject
          (variable_declaration
            (variable_declarator
              name: (object_pattern
                (shorthand_property_identifier_pattern) @name.definition.variable)))
          
          ;; Var object destructuring (aliased): var { a: b } = someObject
          (variable_declaration
            (variable_declarator
              name: (object_pattern
                (pair_pattern
                  value: (identifier) @name.definition.variable))))
          
          ;; Function parameters with array destructuring: function foo([a, b]) {}
          (formal_parameters
            (required_parameter
              pattern: (array_pattern
                (identifier) @name.definition.variable)))
          
          ;; Function parameters with object destructuring (shorthand): function foo({ a, b }) {}
          (formal_parameters
            (required_parameter
              pattern: (object_pattern
                (shorthand_property_identifier_pattern) @name.definition.variable)))
          
          ;; Function parameters with object destructuring (aliased): function foo({ a: b }) {}
          (formal_parameters
            (required_parameter
              pattern: (object_pattern
                (pair_pattern
                  value: (identifier) @name.definition.variable))))
          
          ;; Function parameters (simple): function foo(a, b) {}
          (formal_parameters
            (required_parameter
              pattern: (identifier) @name.definition.variable))
          
          ;; === REFERENCES ===
          
          ;; Member expression object: foo.bar (capture foo as reference)
          (member_expression
            object: (identifier) @name.reference.variable)
          
          ;; Capture ALL type identifiers as references (comprehensive)
          (type_identifier) @name.reference.type
          
          ;; new expressions: new SomeClass()
          (new_expression
            constructor: (identifier) @name.reference.class) @reference.class
          
          ;; Named imports (simple): import { DiffViewer } from './code-viewer'
          (import_specifier
            name: (identifier) @name.reference.variable
            !alias) @reference.import
          
          ;; Named imports with alias: import { Foo as Bar } from './module'
          (import_specifier
            name: (identifier) @alias.original
            alias: (identifier) @alias.name) @reference.import.alias
          
          ;; Default imports: import Foo from 'bar'
          (import_clause
            (identifier) @name.reference.import)
          
          ;; JSX opening element: <DiffViewer ...>
          (jsx_opening_element
            name: (identifier) @name.reference.class)
          
          ;; JSX self-closing element: <DiffViewer ... />
          (jsx_self_closing_element
            name: (identifier) @name.reference.class)
          
          ;; JSX member expression element: <Foo.Bar />
          (jsx_opening_element
            name: (member_expression
              object: (identifier) @name.reference.variable))
          
          (jsx_self_closing_element
            name: (member_expression
              object: (identifier) @name.reference.variable))
          
          ;; Function calls: someFunction()
          (call_expression
            function: (identifier) @name.reference.call)
          
          ;; Method calls on objects: object.method()
          (call_expression
            function: (member_expression
              object: (identifier) @name.reference.variable))
          
          ;; Extends clause in class: class Foo extends Bar
          (class_heritage
            (extends_clause
              value: (identifier) @name.reference.class))
          
          ;; Implements clause: class Foo implements Bar
          (class_heritage
            (implements_clause
              (type_identifier) @name.reference.interface))
          
          ;; Named exports: export { Foo }
          (export_specifier
            name: (identifier) @name.reference.export)
          
          ;; Array element identifiers: [foo, bar] (e.g., React useCallback/useEffect dependency arrays)
          (array
            (identifier) @name.reference.variable)
        "#
        .to_owned(),
        || tree_sitter_typescript::LANGUAGE_TSX.into(),
    )
}
