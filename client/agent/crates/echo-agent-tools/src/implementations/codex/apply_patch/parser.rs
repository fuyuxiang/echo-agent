//! Patch parser for the codex apply-patch format.
//!
//! Ported from the codex `apply-patch` crate (`codex-rs/apply-patch/src/parser.rs`).
//! Parses a custom patch format (not unified diff) with `*** Add File`,
//! `*** Delete File`, and `*** Update File` operations into a list of [`Hunk`]s.
//!
//! The official Lark grammar for the apply-patch format is:
//!
//! ```text
//! start: begin_patch hunk+ end_patch
//! begin_patch: "*** Begin Patch" LF
//! end_patch: "*** End Patch" LF?
//!
//! hunk: add_hunk | delete_hunk | update_hunk
//! add_hunk: "*** Add File: " filename LF add_line+
//! delete_hunk: "*** Delete File: " filename LF
//! update_hunk: "*** Update File: " filename LF change_move? change?
//! filename: /(.+)/
//! add_line: "+" /(.+)/ LF -> line
//!
//! change_move: "*** Move to: " filename LF
//! change: (change_context | change_line)+ eof_line?
//! change_context: ("@@" | "@@ " /(.+)/) LF
//! change_line: ("+" | "-" | " ") /(.+)/ LF
//! eof_line: "*** End of File" LF
//! ```
//!
//! The parser is slightly more lenient than the explicit spec and allows for
//! leading/trailing whitespace around patch markers.

use std::path::PathBuf;

use super::errors::ParseError;
use ParseError::*;

// ─── Marker constants ────────────────────────────────────────────────

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

/// We always use lenient mode (matching the codex default).
const PARSE_IN_STRICT_MODE: bool = false;

// ─── Public types ────────────────────────────────────────────────────

/// A parsed patch: the list of hunks plus the normalised patch text.
#[derive(Debug, PartialEq)]
pub struct ParsedPatch {
    pub hunks: Vec<Hunk>,
    pub patch: String,
}

/// A single hunk within a parsed patch.
#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        /// Chunks should be in order — the `change_context` of one chunk
        /// should occur later in the file than the previous chunk.
        chunks: Vec<UpdateFileChunk>,
    },
}

/// A single contiguous edit within an `UpdateFile` hunk.
#[derive(Debug, PartialEq, Clone)]
pub struct UpdateFileChunk {
    /// A single line of context used to narrow down the position of the chunk
    /// (usually a class, method, or function definition).
    pub change_context: Option<String>,

    /// A contiguous block of lines that should be replaced with `new_lines`.
    /// `old_lines` must occur strictly after `change_context`.
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,

    /// If set to true, `old_lines` must occur at the end of the source file.
    pub is_end_of_file: bool,
}

// ─── Public entry point ──────────────────────────────────────────────

/// Parse a patch string into a [`ParsedPatch`].
pub fn parse_patch(patch: &str) -> Result<ParsedPatch, ParseError> {
    let mode = if PARSE_IN_STRICT_MODE {
        ParseMode::Strict
    } else {
        ParseMode::Lenient
    };
    parse_patch_text(patch, mode)
}

// ─── Internal helpers ────────────────────────────────────────────────

enum ParseMode {
    /// Parse the patch text argument as-is.
    Strict,
    /// In lenient mode we strip heredoc wrappers (`<<EOF` / `<<'EOF'` /
    /// `<<"EOF"`) before trying strict parsing.
    Lenient,
}

fn parse_patch_text(patch: &str, mode: ParseMode) -> Result<ParsedPatch, ParseError> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    let lines: &[&str] = match check_patch_boundaries_strict(&lines) {
        Ok(()) => &lines,
        Err(e) => match mode {
            ParseMode::Strict => {
                return Err(e);
            }
            ParseMode::Lenient => check_patch_boundaries_lenient(&lines, e)?,
        },
    };

    let mut hunks: Vec<Hunk> = Vec::new();
    // The boundary checks guarantee lines.len() >= 2.
    let last_line_index = lines.len().saturating_sub(1);
    let mut remaining_lines = &lines[1..last_line_index];
    let mut line_number = 2;
    while !remaining_lines.is_empty() {
        let (hunk, hunk_lines) = parse_one_hunk(remaining_lines, line_number)?;
        hunks.push(hunk);
        line_number += hunk_lines;
        remaining_lines = &remaining_lines[hunk_lines..];
    }
    let patch = lines.join("\n");
    Ok(ParsedPatch { hunks, patch })
}

fn check_patch_boundaries_strict(lines: &[&str]) -> Result<(), ParseError> {
    let (first_line, last_line) = match lines {
        [] => (None, None),
        [first] => (Some(first), Some(first)),
        [first, .., last] => (Some(first), Some(last)),
    };
    check_start_and_end_lines_strict(first_line, last_line)
}

fn check_patch_boundaries_lenient<'a>(
    original_lines: &'a [&'a str],
    original_parse_error: ParseError,
) -> Result<&'a [&'a str], ParseError> {
    match original_lines {
        [first, .., last] => {
            if (first == &"<<EOF" || first == &"<<'EOF'" || first == &"<<\"EOF\"")
                && last.ends_with("EOF")
                && original_lines.len() >= 4
            {
                let inner_lines = &original_lines[1..original_lines.len() - 1];
                match check_patch_boundaries_strict(inner_lines) {
                    Ok(()) => Ok(inner_lines),
                    Err(e) => Err(e),
                }
            } else {
                Err(original_parse_error)
            }
        }
        _ => Err(original_parse_error),
    }
}

fn check_start_and_end_lines_strict(
    first_line: Option<&&str>,
    last_line: Option<&&str>,
) -> Result<(), ParseError> {
    let first_line = first_line.map(|line| line.trim());
    let last_line = last_line.map(|line| line.trim());

    match (first_line, last_line) {
        (Some(first), Some(last)) if first == BEGIN_PATCH_MARKER && last == END_PATCH_MARKER => {
            Ok(())
        }
        (Some(first), _) if first != BEGIN_PATCH_MARKER => Err(InvalidPatchError(String::from(
            "The first line of the patch must be '*** Begin Patch'",
        ))),
        _ => Err(InvalidPatchError(String::from(
            "The last line of the patch must be '*** End Patch'",
        ))),
    }
}

/// Parse a single hunk from the start of `lines`.
/// Returns the parsed hunk and the number of lines consumed.
fn parse_one_hunk(lines: &[&str], line_number: usize) -> Result<(Hunk, usize), ParseError> {
    let first_line = lines[0].trim();
    if let Some(path) = first_line.strip_prefix(ADD_FILE_MARKER) {
        // ── Add File ─────────────────────────────────────────────
        let mut contents = String::new();
        let mut parsed_lines = 1;
        for add_line in &lines[1..] {
            if let Some(line_to_add) = add_line.strip_prefix('+') {
                contents.push_str(line_to_add);
                contents.push('\n');
                parsed_lines += 1;
            } else {
                break;
            }
        }
        return Ok((
            Hunk::AddFile {
                path: PathBuf::from(path),
                contents,
            },
            parsed_lines,
        ));
    } else if let Some(path) = first_line.strip_prefix(DELETE_FILE_MARKER) {
        // ── Delete File ──────────────────────────────────────────
        return Ok((
            Hunk::DeleteFile {
                path: PathBuf::from(path),
            },
            1,
        ));
    } else if let Some(path) = first_line.strip_prefix(UPDATE_FILE_MARKER) {
        // ── Update File ──────────────────────────────────────────
        let mut remaining_lines = &lines[1..];
        let mut parsed_lines = 1;

        // Optional: move-to line.
        let move_path = remaining_lines
            .first()
            .and_then(|x| x.strip_prefix(MOVE_TO_MARKER));

        if move_path.is_some() {
            remaining_lines = &remaining_lines[1..];
            parsed_lines += 1;
        }

        let mut chunks = Vec::new();
        while !remaining_lines.is_empty() {
            // Skip blank lines between chunks.
            if remaining_lines[0].trim().is_empty() {
                parsed_lines += 1;
                remaining_lines = &remaining_lines[1..];
                continue;
            }
            // Stop at the next hunk header.
            if remaining_lines[0].starts_with("***") {
                break;
            }

            let (chunk, chunk_lines) = parse_update_file_chunk(
                remaining_lines,
                line_number + parsed_lines,
                chunks.is_empty(),
            )?;
            chunks.push(chunk);
            parsed_lines += chunk_lines;
            remaining_lines = &remaining_lines[chunk_lines..];
        }

        if chunks.is_empty() {
            return Err(InvalidHunkError {
                message: format!("Update file hunk for path '{path}' is empty"),
                line_number,
            });
        }

        return Ok((
            Hunk::UpdateFile {
                path: PathBuf::from(path),
                move_path: move_path.map(PathBuf::from),
                chunks,
            },
            parsed_lines,
        ));
    }

    Err(InvalidHunkError {
        message: format!(
            "'{first_line}' is not a valid hunk header. Valid hunk headers: \
             '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
        ),
        line_number,
    })
}

fn parse_update_file_chunk(
    lines: &[&str],
    line_number: usize,
    allow_missing_context: bool,
) -> Result<(UpdateFileChunk, usize), ParseError> {
    if lines.is_empty() {
        return Err(InvalidHunkError {
            message: "Update hunk does not contain any lines".to_string(),
            line_number,
        });
    }

    // Check for explicit @@ context marker.
    let (change_context, start_index) = if lines[0] == EMPTY_CHANGE_CONTEXT_MARKER {
        (None, 1)
    } else if let Some(context) = lines[0].strip_prefix(CHANGE_CONTEXT_MARKER) {
        (Some(context.to_string()), 1)
    } else {
        if !allow_missing_context {
            return Err(InvalidHunkError {
                message: format!(
                    "Expected update hunk to start with a @@ context marker, got: '{}'",
                    lines[0]
                ),
                line_number,
            });
        }
        (None, 0)
    };

    if start_index >= lines.len() {
        return Err(InvalidHunkError {
            message: "Update hunk does not contain any lines".to_string(),
            line_number: line_number + 1,
        });
    }

    let mut chunk = UpdateFileChunk {
        change_context,
        old_lines: Vec::new(),
        new_lines: Vec::new(),
        is_end_of_file: false,
    };
    let mut parsed_lines = 0;
    for line in &lines[start_index..] {
        match *line {
            EOF_MARKER => {
                if parsed_lines == 0 {
                    return Err(InvalidHunkError {
                        message: "Update hunk does not contain any lines".to_string(),
                        line_number: line_number + 1,
                    });
                }
                chunk.is_end_of_file = true;
                parsed_lines += 1;
                break;
            }
            line_contents => match line_contents.chars().next() {
                None => {
                    // Interpret empty line as a context line.
                    chunk.old_lines.push(String::new());
                    chunk.new_lines.push(String::new());
                }
                Some(' ') => {
                    chunk.old_lines.push(line_contents[1..].to_string());
                    chunk.new_lines.push(line_contents[1..].to_string());
                }
                Some('+') => {
                    chunk.new_lines.push(line_contents[1..].to_string());
                }
                Some('-') => {
                    chunk.old_lines.push(line_contents[1..].to_string());
                }
                _ => {
                    if parsed_lines == 0 {
                        return Err(InvalidHunkError {
                            message: format!(
                                "Unexpected line found in update hunk: '{line_contents}'. \
                                 Every line should start with ' ' (context line), \
                                 '+' (added line), or '-' (removed line)"
                            ),
                            line_number: line_number + 1,
                        });
                    }
                    // Assume this is the start of the next hunk.
                    break;
                }
            },
        }
        parsed_lines += 1;
    }

    Ok((chunk, parsed_lines + start_index))
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_patch_bad_first_line() {
        assert_eq!(
            parse_patch_text("bad", ParseMode::Strict),
            Err(InvalidPatchError(
                "The first line of the patch must be '*** Begin Patch'".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_patch_bad_last_line() {
        assert_eq!(
            parse_patch_text("*** Begin Patch\nbad", ParseMode::Strict),
            Err(InvalidPatchError(
                "The last line of the patch must be '*** End Patch'".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_patch_add_file_with_whitespace_around_markers() {
        assert_eq!(
            parse_patch_text(
                concat!(
                    "*** Begin Patch",
                    " ",
                    "\n*** Add File: foo\n+hi\n",
                    " ",
                    "*** End Patch"
                ),
                ParseMode::Strict
            )
            .unwrap()
            .hunks,
            vec![Hunk::AddFile {
                path: PathBuf::from("foo"),
                contents: "hi\n".to_string()
            }]
        );
    }

    #[test]
    fn test_parse_patch_empty_update_hunk() {
        assert_eq!(
            parse_patch_text(
                "*** Begin Patch\n\
                 *** Update File: test.py\n\
                 *** End Patch",
                ParseMode::Strict
            ),
            Err(InvalidHunkError {
                message: "Update file hunk for path 'test.py' is empty".to_string(),
                line_number: 2,
            })
        );
    }

    #[test]
    fn test_parse_patch_empty_hunks() {
        assert_eq!(
            parse_patch_text(
                "*** Begin Patch\n\
                 *** End Patch",
                ParseMode::Strict
            )
            .unwrap()
            .hunks,
            Vec::new()
        );
    }

    #[test]
    fn test_parse_patch_all_hunk_types() {
        assert_eq!(
            parse_patch_text(
                "*** Begin Patch\n\
                 *** Add File: path/add.py\n\
                 +abc\n\
                 +def\n\
                 *** Delete File: path/delete.py\n\
                 *** Update File: path/update.py\n\
                 *** Move to: path/update2.py\n\
                 @@ def f():\n\
                 -    pass\n\
                 +    return 123\n\
                 *** End Patch",
                ParseMode::Strict
            )
            .unwrap()
            .hunks,
            vec![
                Hunk::AddFile {
                    path: PathBuf::from("path/add.py"),
                    contents: "abc\ndef\n".to_string()
                },
                Hunk::DeleteFile {
                    path: PathBuf::from("path/delete.py")
                },
                Hunk::UpdateFile {
                    path: PathBuf::from("path/update.py"),
                    move_path: Some(PathBuf::from("path/update2.py")),
                    chunks: vec![UpdateFileChunk {
                        change_context: Some("def f():".to_string()),
                        old_lines: vec!["    pass".to_string()],
                        new_lines: vec!["    return 123".to_string()],
                        is_end_of_file: false
                    }]
                }
            ]
        );
    }

    #[test]
    fn test_parse_patch_update_followed_by_add() {
        assert_eq!(
            parse_patch_text(
                "*** Begin Patch\n\
                 *** Update File: file.py\n\
                 @@\n\
                 +line\n\
                 *** Add File: other.py\n\
                 +content\n\
                 *** End Patch",
                ParseMode::Strict
            )
            .unwrap()
            .hunks,
            vec![
                Hunk::UpdateFile {
                    path: PathBuf::from("file.py"),
                    move_path: None,
                    chunks: vec![UpdateFileChunk {
                        change_context: None,
                        old_lines: vec![],
                        new_lines: vec!["line".to_string()],
                        is_end_of_file: false
                    }],
                },
                Hunk::AddFile {
                    path: PathBuf::from("other.py"),
                    contents: "content\n".to_string()
                }
            ]
        );
    }

    #[test]
    fn test_parse_patch_update_without_explicit_context_marker() {
        assert_eq!(
            parse_patch_text(
                r#"*** Begin Patch
*** Update File: file2.py
 import foo
+bar
*** End Patch"#,
                ParseMode::Strict
            )
            .unwrap()
            .hunks,
            vec![Hunk::UpdateFile {
                path: PathBuf::from("file2.py"),
                move_path: None,
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec!["import foo".to_string()],
                    new_lines: vec!["import foo".to_string(), "bar".to_string()],
                    is_end_of_file: false,
                }],
            }]
        );
    }

    #[test]
    fn test_parse_patch_lenient_heredoc_variants() {
        let patch_text = r#"*** Begin Patch
*** Update File: file2.py
 import foo
+bar
*** End Patch"#;
        let expected_hunks = vec![Hunk::UpdateFile {
            path: PathBuf::from("file2.py"),
            move_path: None,
            chunks: vec![UpdateFileChunk {
                change_context: None,
                old_lines: vec!["import foo".to_string()],
                new_lines: vec!["import foo".to_string(), "bar".to_string()],
                is_end_of_file: false,
            }],
        }];
        let expected_error =
            InvalidPatchError("The first line of the patch must be '*** Begin Patch'".to_string());

        // <<EOF variant
        let patch_in_heredoc = format!("<<EOF\n{patch_text}\nEOF\n");
        assert_eq!(
            parse_patch_text(&patch_in_heredoc, ParseMode::Strict),
            Err(expected_error.clone())
        );
        assert_eq!(
            parse_patch_text(&patch_in_heredoc, ParseMode::Lenient)
                .unwrap()
                .hunks,
            expected_hunks
        );

        // <<'EOF' variant
        let patch_in_sq_heredoc = format!("<<'EOF'\n{patch_text}\nEOF\n");
        assert_eq!(
            parse_patch_text(&patch_in_sq_heredoc, ParseMode::Strict),
            Err(expected_error.clone())
        );
        assert_eq!(
            parse_patch_text(&patch_in_sq_heredoc, ParseMode::Lenient)
                .unwrap()
                .hunks,
            expected_hunks
        );

        // <<"EOF" variant
        let patch_in_dq_heredoc = format!("<<\"EOF\"\n{patch_text}\nEOF\n");
        assert_eq!(
            parse_patch_text(&patch_in_dq_heredoc, ParseMode::Strict),
            Err(expected_error.clone())
        );
        assert_eq!(
            parse_patch_text(&patch_in_dq_heredoc, ParseMode::Lenient)
                .unwrap()
                .hunks,
            expected_hunks
        );

        // Mismatched quotes → fail even in lenient mode
        let patch_in_mismatch = format!("<<\"EOF'\n{patch_text}\nEOF\n");
        assert_eq!(
            parse_patch_text(&patch_in_mismatch, ParseMode::Strict),
            Err(expected_error.clone())
        );
        assert_eq!(
            parse_patch_text(&patch_in_mismatch, ParseMode::Lenient),
            Err(expected_error)
        );

        // Missing closing heredoc marker
        let patch_missing_close =
            "<<EOF\n*** Begin Patch\n*** Update File: file2.py\nEOF\n".to_string();
        assert_eq!(
            parse_patch_text(&patch_missing_close, ParseMode::Lenient),
            Err(InvalidPatchError(
                "The last line of the patch must be '*** End Patch'".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_one_hunk_bad_header() {
        assert_eq!(
            parse_one_hunk(&["bad"], 234),
            Err(InvalidHunkError {
                message: "'bad' is not a valid hunk header. Valid hunk headers: \
                          '*** Add File: {path}', '*** Delete File: {path}', \
                          '*** Update File: {path}'"
                    .to_string(),
                line_number: 234
            })
        );
    }

    #[test]
    fn test_update_file_chunk_bad_start() {
        assert_eq!(
            parse_update_file_chunk(&["bad"], 123, false),
            Err(InvalidHunkError {
                message: "Expected update hunk to start with a @@ context marker, got: 'bad'"
                    .to_string(),
                line_number: 123
            })
        );
    }

    #[test]
    fn test_update_file_chunk_empty_after_context() {
        assert_eq!(
            parse_update_file_chunk(&["@@"], 123, false),
            Err(InvalidHunkError {
                message: "Update hunk does not contain any lines".to_string(),
                line_number: 124
            })
        );
    }

    #[test]
    fn test_update_file_chunk_bad_diff_line() {
        assert_eq!(
            parse_update_file_chunk(&["@@", "bad"], 123, false),
            Err(InvalidHunkError {
                message: "Unexpected line found in update hunk: 'bad'. \
                          Every line should start with ' ' (context line), \
                          '+' (added line), or '-' (removed line)"
                    .to_string(),
                line_number: 124
            })
        );
    }

    #[test]
    fn test_update_file_chunk_eof_with_no_lines() {
        assert_eq!(
            parse_update_file_chunk(&["@@", "*** End of File"], 123, false),
            Err(InvalidHunkError {
                message: "Update hunk does not contain any lines".to_string(),
                line_number: 124
            })
        );
    }

    #[test]
    fn test_update_file_chunk_with_context_and_diff_lines() {
        assert_eq!(
            parse_update_file_chunk(
                &[
                    "@@ change_context",
                    "",
                    " context",
                    "-remove",
                    "+add",
                    " context2",
                    "*** End Patch",
                ],
                123,
                false
            ),
            Ok((
                UpdateFileChunk {
                    change_context: Some("change_context".to_string()),
                    old_lines: vec![
                        "".to_string(),
                        "context".to_string(),
                        "remove".to_string(),
                        "context2".to_string()
                    ],
                    new_lines: vec![
                        "".to_string(),
                        "context".to_string(),
                        "add".to_string(),
                        "context2".to_string()
                    ],
                    is_end_of_file: false
                },
                6
            ))
        );
    }

    #[test]
    fn test_update_file_chunk_with_eof_marker() {
        assert_eq!(
            parse_update_file_chunk(&["@@", "+line", "*** End of File"], 123, false),
            Ok((
                UpdateFileChunk {
                    change_context: None,
                    old_lines: vec![],
                    new_lines: vec!["line".to_string()],
                    is_end_of_file: true
                },
                3
            ))
        );
    }

    #[test]
    fn test_parse_patch_public_api() {
        let patch = "*** Begin Patch\n\
                     *** Add File: hello.txt\n\
                     +hello world\n\
                     *** End Patch";
        let result = parse_patch(patch).unwrap();
        assert_eq!(
            result.hunks,
            vec![Hunk::AddFile {
                path: PathBuf::from("hello.txt"),
                contents: "hello world\n".to_string(),
            }]
        );
    }
}
