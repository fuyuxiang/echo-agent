//! Location-based navigation APIs for go-to-definition and go-to-references.
//!
//! This module provides APIs that take a file path and position (row, column)
//! and return definition or reference locations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::languages::LanguageRegistry;
use crate::scope_graph::ScopeGraphIndex;

/// Result of a navigation operation (go-to-definition or go-to-references).
#[derive(Debug, Clone)]
pub struct NavigationResult {
    /// The symbol that was found at the query position.
    pub symbol: String,
    /// List of locations where the symbol is defined/referenced.
    pub locations: Vec<Location>,
}

/// A location in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Path to the file.
    pub path: String,
    /// 1-indexed line number.
    pub line: usize,
    /// Optional: the matched symbol name (useful for aliases).
    pub symbol: Option<String>,
}

impl Location {
    pub fn new(path: impl Into<String>, line: usize) -> Self {
        Self {
            path: path.into(),
            line,
            symbol: None,
        }
    }

    pub fn with_symbol(path: impl Into<String>, line: usize, symbol: String) -> Self {
        Self {
            path: path.into(),
            line,
            symbol: Some(symbol),
        }
    }

    /// Get the path as a Path reference.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.path)
    }
}

/// Error type for navigation operations.
#[derive(Debug)]
pub enum NavigationError {
    /// File not found or could not be read.
    FileNotFound(PathBuf),
    /// Position is out of bounds for the file.
    PositionOutOfBounds { row: usize, col: usize },
    /// No symbol found at the given position.
    NoSymbolAtPosition { row: usize, col: usize },
    /// Language not supported for this file type.
    UnsupportedLanguage(String),
    /// Parse error.
    ParseError(String),
    /// IO error.
    IoError(std::io::Error),
}

impl std::fmt::Display for NavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavigationError::FileNotFound(path) => {
                write!(f, "File not found: {}", path.display())
            }
            NavigationError::PositionOutOfBounds { row, col } => {
                write!(f, "Position out of bounds: {}:{}", row, col)
            }
            NavigationError::NoSymbolAtPosition { row, col } => {
                write!(f, "No symbol found at position {}:{}", row, col)
            }
            NavigationError::UnsupportedLanguage(ext) => {
                write!(f, "Unsupported language: {}", ext)
            }
            NavigationError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            NavigationError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for NavigationError {}

impl From<std::io::Error> for NavigationError {
    fn from(e: std::io::Error) -> Self {
        NavigationError::IoError(e)
    }
}

/// Navigator provides location-based code navigation.
///
/// It wraps a ScopeGraphIndex and provides methods to navigate code
/// based on file path and position (row, column).
pub struct Navigator {
    index: Arc<ScopeGraphIndex>,
    registry: LanguageRegistry,
}

impl Navigator {
    /// Create a new Navigator backed by a shared index.
    ///
    /// Accepts anything that converts into `Arc<ScopeGraphIndex>`, so both
    /// owned and already-shared indexes work without extra wrapping:
    ///
    /// ```rust,ignore
    /// // From an owned index (e.g. IndexBuilder)
    /// let navigator = Navigator::new(index);
    ///
    /// // From a shared snapshot (zero-cost)
    /// let snapshot = handle.get_snapshot()?;
    /// let navigator = Navigator::new(snapshot);
    /// ```
    pub fn new(index: impl Into<Arc<ScopeGraphIndex>>) -> Self {
        Self {
            index: index.into(),
            registry: LanguageRegistry::new(),
        }
    }

    /// Get a reference to the underlying index.
    pub fn index(&self) -> &ScopeGraphIndex {
        &self.index
    }

    /// Get a mutable reference to the underlying index.
    ///
    /// Uses copy-on-write: if other `Arc` clones of the index exist, the
    /// index is cloned before returning the mutable reference.
    pub fn index_mut(&mut self) -> &mut ScopeGraphIndex {
        Arc::make_mut(&mut self.index)
    }

    /// Get the symbol at the given file path and position.
    ///
    /// # Arguments
    /// * `file_path` - Path to the file
    /// * `row` - 1-indexed line number
    /// * `col` - 1-indexed column number
    ///
    /// # Returns
    /// The symbol name at the given position.
    pub fn get_symbol_at_position(
        &self,
        file_path: &Path,
        row: usize,
        col: usize,
    ) -> Result<String, NavigationError> {
        // Validate position (1-indexed)
        if row == 0 || col == 0 {
            return Err(NavigationError::PositionOutOfBounds { row, col });
        }

        let content = std::fs::read(file_path)
            .map_err(|_| NavigationError::FileNotFound(file_path.to_path_buf()))?;

        // Get the language config
        let lang_config = self.registry.for_file_path(file_path).ok_or_else(|| {
            NavigationError::UnsupportedLanguage(
                file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
            )
        })?;

        // Parse the file
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&lang_config.language())
            .map_err(|e| NavigationError::ParseError(format!("Failed to set language: {}", e)))?;

        let tree = parser
            .parse(&content, None)
            .ok_or_else(|| NavigationError::ParseError("Failed to parse file".to_string()))?;

        // Convert 1-indexed row/col to 0-indexed for tree-sitter
        let point = tree_sitter::Point::new(row - 1, col - 1);

        // Find the node at the position
        let root = tree.root_node();
        let node = find_smallest_named_node_at_point(root, point);

        match node {
            Some(n) => {
                let text = std::str::from_utf8(&content[n.byte_range()])
                    .map_err(|_| NavigationError::ParseError("Invalid UTF-8".to_string()))?;
                Ok(text.to_string())
            }
            None => Err(NavigationError::NoSymbolAtPosition { row, col }),
        }
    }

    /// Go to definition for the symbol at the given position.
    ///
    /// # Arguments
    /// * `file_path` - Path to the file
    /// * `row` - 1-indexed line number
    /// * `col` - 1-indexed column number
    ///
    /// # Returns
    /// NavigationResult containing the symbol and its definition locations.
    pub fn goto_definition(
        &self,
        file_path: &Path,
        row: usize,
        col: usize,
    ) -> Result<NavigationResult, NavigationError> {
        let symbol = self.get_symbol_at_position(file_path, row, col)?;

        // Look up definitions in the index
        let defs =
            self.index
                .find_definitions_smart(&symbol, Some(file_path), Some(&self.registry));

        let locations: Vec<Location> = defs
            .into_iter()
            .map(|(path, line)| Location::new(path, line))
            .collect();

        Ok(NavigationResult { symbol, locations })
    }

    /// Go to references for the symbol at the given position.
    ///
    /// This first resolves the symbol to its definition, then finds all references.
    ///
    /// # Arguments
    /// * `file_path` - Path to the file
    /// * `row` - 1-indexed line number  
    /// * `col` - 1-indexed column number
    /// * `include_definition` - Whether to include the definition location in results
    ///
    /// # Returns
    /// NavigationResult containing the symbol and its reference locations.
    pub fn goto_references(
        &self,
        file_path: &Path,
        row: usize,
        col: usize,
        include_definition: bool,
    ) -> Result<NavigationResult, NavigationError> {
        let symbol = self.get_symbol_at_position(file_path, row, col)?;

        // Get references (includes alias resolution)
        let refs = self
            .index
            .find_references_smart(&symbol, Some(file_path), Some(&self.registry));

        let mut locations: Vec<Location> = refs
            .into_iter()
            .map(|(sym, path, line)| Location::with_symbol(path, line, sym))
            .collect();

        // Optionally include definition locations
        if include_definition {
            let defs =
                self.index
                    .find_definitions_smart(&symbol, Some(file_path), Some(&self.registry));

            for (path, line) in defs {
                let loc = Location::new(path, line);
                if !locations
                    .iter()
                    .any(|l| l.path == loc.path && l.line == loc.line)
                {
                    locations.insert(0, loc);
                }
            }
        }

        Ok(NavigationResult { symbol, locations })
    }

    /// Go to definition by symbol name directly (without position lookup).
    pub fn goto_definition_by_name(
        &self,
        symbol: &str,
        context_file: Option<&Path>,
    ) -> NavigationResult {
        let defs = self
            .index
            .find_definitions_smart(symbol, context_file, Some(&self.registry));

        let locations: Vec<Location> = defs
            .into_iter()
            .map(|(path, line)| Location::new(path, line))
            .collect();

        NavigationResult {
            symbol: symbol.to_string(),
            locations,
        }
    }

    /// Go to references by symbol name directly (without position lookup).
    pub fn goto_references_by_name(
        &self,
        symbol: &str,
        context_file: Option<&Path>,
        include_definition: bool,
    ) -> NavigationResult {
        let refs = self
            .index
            .find_references_smart(symbol, context_file, Some(&self.registry));

        let mut locations: Vec<Location> = refs
            .into_iter()
            .map(|(sym, path, line)| Location::with_symbol(path, line, sym))
            .collect();

        if include_definition {
            let defs =
                self.index
                    .find_definitions_smart(symbol, context_file, Some(&self.registry));

            for (path, line) in defs {
                let loc = Location::new(path, line);
                if !locations
                    .iter()
                    .any(|l| l.path == loc.path && l.line == loc.line)
                {
                    locations.insert(0, loc);
                }
            }
        }

        NavigationResult {
            symbol: symbol.to_string(),
            locations,
        }
    }
}

/// Find the smallest named node that contains the given point.
fn find_smallest_named_node_at_point(
    node: tree_sitter::Node<'_>,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node<'_>> {
    // Check if point is within this node
    if point < node.start_position() || point > node.end_position() {
        return None;
    }

    // Try to find a smaller child node that contains the point
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_smallest_named_node_at_point(child, point) {
            // Prefer named nodes that look like identifiers
            if is_identifier_like(&found) {
                return Some(found);
            }
            // Keep searching for a better match
            if is_identifier_like(&child) {
                return Some(child);
            }
            return Some(found);
        }
    }

    // No smaller child contains the point, return this node if it's identifier-like
    if is_identifier_like(&node) {
        Some(node)
    } else {
        None
    }
}

/// Check if a node looks like an identifier.
fn is_identifier_like(node: &tree_sitter::Node<'_>) -> bool {
    let kind = node.kind();
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "property_identifier"
            | "field_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "attribute"           // Python
            | "package_identifier" // Go
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexBuilder;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_get_symbol_at_position() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.rs");

        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "fn hello_world() {{").unwrap();
        writeln!(file, "    println!(\"Hello\");").unwrap();
        writeln!(file, "}}").unwrap();

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Get symbol at "hello_world" (row 1, col 4)
        let symbol = navigator.get_symbol_at_position(&file_path, 1, 4).unwrap();
        assert_eq!(symbol, "hello_world");
    }

    #[test]
    fn test_typescript_for_of_array_destructuring() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"const myMap = new Map<string, {{ sessionId: string }}>();

function test(sessionId: string) {{
  for (const [toolCallId, request] of myMap) {{
    if (request.sessionId !== sessionId) continue;
    console.log(toolCallId);
  }}
}}"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'request' is found as a definition at line 4
        let def_result = navigator.goto_definition_by_name("request", Some(&file_path));
        assert!(
            !def_result.locations.is_empty(),
            "request should be found as a definition"
        );
        assert_eq!(
            def_result.locations[0].line, 4,
            "request should be defined on line 4"
        );

        // Test that 'toolCallId' is also found as a definition at line 4
        let def_result2 = navigator.goto_definition_by_name("toolCallId", Some(&file_path));
        assert!(
            !def_result2.locations.is_empty(),
            "toolCallId should be found as a definition"
        );
        assert_eq!(
            def_result2.locations[0].line, 4,
            "toolCallId should be defined on line 4"
        );

        // Test that references to 'request' are found
        let ref_result = navigator.goto_references_by_name("request", Some(&file_path), false);
        assert!(
            !ref_result.locations.is_empty(),
            "request should have references"
        );
        // Check that line 5 reference is found (request.sessionId)
        let line5_ref = ref_result.locations.iter().any(|loc| loc.line == 5);
        assert!(line5_ref, "request should be referenced on line 5");
    }

    #[test]
    fn test_typescript_for_of_object_destructuring() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"const items = [{{ name: "a", value: 1 }}];

for (const {{ name, value }} of items) {{
  console.log(name, value);
}}"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'name' is found as a definition at line 3
        let def_result = navigator.goto_definition_by_name("name", Some(&file_path));
        assert!(
            !def_result.locations.is_empty(),
            "name should be found as a definition"
        );
        assert_eq!(
            def_result.locations[0].line, 3,
            "name should be defined on line 3"
        );

        // Test that 'value' is found as a definition at line 3
        let def_result2 = navigator.goto_definition_by_name("value", Some(&file_path));
        assert!(
            !def_result2.locations.is_empty(),
            "value should be found as a definition"
        );
        assert_eq!(
            def_result2.locations[0].line, 3,
            "value should be defined on line 3"
        );
    }

    #[test]
    fn test_typescript_regular_array_destructuring() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"const arr = [1, 2, 3];
const [first, second, third] = arr;
console.log(first, second);"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'first' is found as a definition at line 2
        let def_result = navigator.goto_definition_by_name("first", Some(&file_path));
        assert!(
            !def_result.locations.is_empty(),
            "first should be found as a definition"
        );
        assert_eq!(
            def_result.locations[0].line, 2,
            "first should be defined on line 2"
        );

        // Test that 'second' is found as a definition at line 2
        let def_result2 = navigator.goto_definition_by_name("second", Some(&file_path));
        assert!(
            !def_result2.locations.is_empty(),
            "second should be found as a definition"
        );
        assert_eq!(
            def_result2.locations[0].line, 2,
            "second should be defined on line 2"
        );
    }

    #[test]
    fn test_typescript_regular_object_destructuring() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"const obj = {{ foo: 1, bar: 2 }};
const {{ foo, bar }} = obj;
console.log(foo, bar);"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'foo' is found as a definition at line 2
        let def_result = navigator.goto_definition_by_name("foo", Some(&file_path));
        assert!(
            !def_result.locations.is_empty(),
            "foo should be found as a definition"
        );
        assert_eq!(
            def_result.locations[0].line, 2,
            "foo should be defined on line 2"
        );

        // Test that 'bar' is found as a definition at line 2
        let def_result2 = navigator.goto_definition_by_name("bar", Some(&file_path));
        assert!(
            !def_result2.locations.is_empty(),
            "bar should be found as a definition"
        );
        assert_eq!(
            def_result2.locations[0].line, 2,
            "bar should be defined on line 2"
        );
    }

    #[test]
    fn test_typescript_member_expression_reference() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"const myObject = {{ value: 42 }};
const result = myObject.value;"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'myObject' is referenced on line 2 (myObject.value)
        let ref_result = navigator.goto_references_by_name("myObject", Some(&file_path), false);
        assert!(
            !ref_result.locations.is_empty(),
            "myObject should have references"
        );
        let line2_ref = ref_result.locations.iter().any(|loc| loc.line == 2);
        assert!(line2_ref, "myObject should be referenced on line 2");
    }

    #[test]
    fn test_typescript_function_parameter_destructuring() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"function process([first, second]: number[], {{ name }}: {{ name: string }}) {{
  console.log(first, second, name);
}}"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'first' is found as a definition at line 1
        let def_result = navigator.goto_definition_by_name("first", Some(&file_path));
        assert!(
            !def_result.locations.is_empty(),
            "first should be found as a definition"
        );
        assert_eq!(
            def_result.locations[0].line, 1,
            "first should be defined on line 1"
        );

        // Test that 'name' is found as a definition at line 1
        let def_result2 = navigator.goto_definition_by_name("name", Some(&file_path));
        assert!(
            !def_result2.locations.is_empty(),
            "name should be found as a definition"
        );
        assert_eq!(
            def_result2.locations[0].line, 1,
            "name should be defined on line 1"
        );
    }

    #[test]
    fn test_typescript_simple_function_parameters() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.ts");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"function greet(name: string, age: number) {{
  console.log(name, age);
}}"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'name' is found as a definition at line 1
        let def_result = navigator.goto_definition_by_name("name", Some(&file_path));
        assert!(
            !def_result.locations.is_empty(),
            "name should be found as a definition"
        );
        assert_eq!(
            def_result.locations[0].line, 1,
            "name should be defined on line 1"
        );

        // Test that 'age' is found as a definition at line 1
        let def_result2 = navigator.goto_definition_by_name("age", Some(&file_path));
        assert!(
            !def_result2.locations.is_empty(),
            "age should be found as a definition"
        );
        assert_eq!(
            def_result2.locations[0].line, 1,
            "age should be defined on line 1"
        );
    }

    #[test]
    fn test_typescript_react_dependency_array_references() {
        // Test that identifiers in React hook dependency arrays are captured as references
        // Regression test for dependency-array reference capture.
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("component.tsx");

        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            r#"import {{ useCallback, useEffect }} from 'react';

function FileTreeTab({{ basePath, onFileSelect }}) {{
  const fileTree = useFileTree();
  
  const loadDirectory = useCallback(
    (path: string) => {{
      return fileTree.listFiles(path);
    }},
    [fileTree],
  );

  const handleOpenPath = useCallback(
    (path: string) => {{
      loadDirectory(path);
    }},
    [loadDirectory, fileTree],
  );

  const handleFileSelect = useCallback(
    (file) => {{
      fileTree.setSelectedPath(file.absPath);
      onFileSelect(file.absPath);
    }},
    [fileTree, onFileSelect],
  );

  useEffect(() => {{
    loadDirectory(basePath);
  }}, [basePath, loadDirectory]);

  return null;
}}"#
        )
        .unwrap();
        drop(file);

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        let navigator = Navigator::new(index);

        // Test that 'fileTree' is found in dependency arrays
        let ref_result = navigator.goto_references_by_name("fileTree", Some(&file_path), false);
        assert!(
            !ref_result.locations.is_empty(),
            "fileTree should have references"
        );

        // Check references in dependency arrays:
        // Line 10: [fileTree]
        // Line 17: [loadDirectory, fileTree]
        // Line 25: [fileTree, onFileSelect]
        let dep_array_lines: Vec<usize> = ref_result
            .locations
            .iter()
            .filter(|loc| loc.line == 10 || loc.line == 17 || loc.line == 25)
            .map(|loc| loc.line)
            .collect();

        assert!(
            dep_array_lines.contains(&10),
            "fileTree should be referenced on line 10 (first dependency array)"
        );
        assert!(
            dep_array_lines.contains(&17),
            "fileTree should be referenced on line 17 (second dependency array)"
        );
        assert!(
            dep_array_lines.contains(&25),
            "fileTree should be referenced on line 25 (third dependency array)"
        );

        // Test that 'loadDirectory' is found in dependency arrays
        let ref_result2 =
            navigator.goto_references_by_name("loadDirectory", Some(&file_path), false);
        assert!(
            !ref_result2.locations.is_empty(),
            "loadDirectory should have references"
        );

        // Line 17: [loadDirectory, fileTree]
        // Line 30: [basePath, loadDirectory]
        let load_dir_refs: Vec<usize> = ref_result2
            .locations
            .iter()
            .filter(|loc| loc.line == 17 || loc.line == 30)
            .map(|loc| loc.line)
            .collect();

        assert!(
            load_dir_refs.contains(&17),
            "loadDirectory should be referenced on line 17"
        );
        assert!(
            load_dir_refs.contains(&30),
            "loadDirectory should be referenced on line 30"
        );

        // Test that 'onFileSelect' is found in dependency array
        let ref_result3 =
            navigator.goto_references_by_name("onFileSelect", Some(&file_path), false);
        assert!(
            !ref_result3.locations.is_empty(),
            "onFileSelect should have references"
        );
        let on_file_select_line25 = ref_result3.locations.iter().any(|loc| loc.line == 25);
        assert!(
            on_file_select_line25,
            "onFileSelect should be referenced on line 25 (dependency array)"
        );

        // Test that 'basePath' is found in dependency array
        let ref_result4 = navigator.goto_references_by_name("basePath", Some(&file_path), false);
        assert!(
            !ref_result4.locations.is_empty(),
            "basePath should have references"
        );
        let base_path_line30 = ref_result4.locations.iter().any(|loc| loc.line == 30);
        assert!(
            base_path_line30,
            "basePath should be referenced on line 30 (useEffect dependency array)"
        );
    }
}
