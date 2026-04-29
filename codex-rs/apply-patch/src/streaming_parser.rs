use std::path::PathBuf;

use crate::parser::ADD_FILE_MARKER;
use crate::parser::BEGIN_PATCH_MARKER;
use crate::parser::CHANGE_CONTEXT_MARKER;
use crate::parser::DELETE_FILE_MARKER;
use crate::parser::EMPTY_CHANGE_CONTEXT_MARKER;
use crate::parser::END_PATCH_MARKER;
use crate::parser::EOF_MARKER;
use crate::parser::Hunk;
use crate::parser::MOVE_TO_MARKER;
use crate::parser::ParseError;
use crate::parser::UPDATE_FILE_MARKER;
use crate::parser::UpdateFileChunk;

use Hunk::*;
use ParseError::*;

#[derive(Debug, Default, Clone)]
pub struct StreamingPatchParser {
    line_buffer: String,
    state: StreamingParserState,
    line_number: usize,
}

#[derive(Debug, Default, Clone)]
struct StreamingParserState {
    mode: StreamingParserMode,
    hunks: Vec<Hunk>,
}

#[derive(Debug, Default, Clone)]
enum StreamingParserMode {
    #[default]
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile,
    EndedPatch,
}

fn handle_hunk_headers_and_end_patch(
    trimmed: &str,
    hunks: &mut Vec<Hunk>,
) -> Result<Option<StreamingParserMode>, String> {
    if trimmed == END_PATCH_MARKER {
        ensure_update_hunk_is_not_empty(hunks)?;
        return Ok(Some(StreamingParserMode::EndedPatch));
    }
    if let Some(path) = trimmed.strip_prefix(ADD_FILE_MARKER) {
        ensure_update_hunk_is_not_empty(hunks)?;
        hunks.push(AddFile {
            path: PathBuf::from(path),
            contents: String::new(),
        });
        return Ok(Some(StreamingParserMode::AddFile));
    }
    if let Some(path) = trimmed.strip_prefix(DELETE_FILE_MARKER) {
        ensure_update_hunk_is_not_empty(hunks)?;
        hunks.push(DeleteFile {
            path: PathBuf::from(path),
        });
        return Ok(Some(StreamingParserMode::DeleteFile));
    }
    if let Some(path) = trimmed.strip_prefix(UPDATE_FILE_MARKER) {
        ensure_update_hunk_is_not_empty(hunks)?;
        hunks.push(UpdateFile {
            path: PathBuf::from(path),
            move_path: None,
            chunks: Vec::new(),
        });
        return Ok(Some(StreamingParserMode::UpdateFile));
    }
    Ok(None)
}

fn ensure_update_hunk_is_not_empty(hunks: &[Hunk]) -> Result<(), String> {
    if let Some(UpdateFile { chunks, .. }) = hunks.last()
        && (chunks.is_empty()
            || chunks
                .last()
                .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty()))
    {
        return Err("Update hunk does not contain any lines".to_string());
    }
    Ok(())
}

impl StreamingPatchParser {
    pub fn push_delta(&mut self, delta: &str) -> Result<Vec<Hunk>, ParseError> {
        for ch in delta.chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut self.line_buffer);
                let state = std::mem::take(&mut self.state);
                self.line_number += 1;
                self.state =
                    Self::process_line(state, line.trim_end_matches('\r'), self.line_number)?;
            } else {
                self.line_buffer.push(ch);
            }
        }

        Ok(self.state.hunks.clone())
    }

    pub fn finish(&mut self) -> Result<Vec<Hunk>, ParseError> {
        if !self.line_buffer.is_empty() {
            self.push_delta("\n")?;
        }

        if !matches!(self.state.mode, StreamingParserMode::EndedPatch) {
            return Err(InvalidPatchError(
                "The last line of the patch must be '*** End Patch'".to_string(),
            ));
        }

        Ok(self.state.hunks.clone())
    }

    fn process_line(
        state: StreamingParserState,
        line: &str,
        line_number: usize,
    ) -> Result<StreamingParserState, ParseError> {
        let trimmed = line.trim();
        let StreamingParserState {
            mut mode,
            mut hunks,
        } = state;
        mode = match mode {
            StreamingParserMode::NotStarted => {
                if trimmed == BEGIN_PATCH_MARKER {
                    return Ok(StreamingParserState {
                        mode: StreamingParserMode::StartedPatch,
                        hunks,
                    });
                }
                return Err(InvalidPatchError(
                    "The first line of the patch must be '*** Begin Patch'".to_string(),
                ));
            }
            StreamingParserMode::StartedPatch => {
                if let Some(mode) =
                    handle_hunk_headers_and_end_patch(trimmed, &mut hunks).map_err(|message| {
                        InvalidHunkError {
                            message,
                            line_number,
                        }
                    })?
                {
                    return Ok(StreamingParserState { mode, hunks });
                }
                return Err(InvalidHunkError {
                    message: format!(
                        "'{trimmed}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
                    ),
                    line_number,
                });
            }
            StreamingParserMode::AddFile => {
                if let Some(mode) =
                    handle_hunk_headers_and_end_patch(trimmed, &mut hunks).map_err(|message| {
                        InvalidHunkError {
                            message,
                            line_number,
                        }
                    })?
                {
                    return Ok(StreamingParserState { mode, hunks });
                }
                if let Some(line_to_add) = line.strip_prefix('+')
                    && let Some(AddFile { contents, .. }) = hunks.last_mut()
                {
                    contents.push_str(line_to_add);
                    contents.push('\n');
                    return Ok(StreamingParserState {
                        mode: StreamingParserMode::AddFile,
                        hunks,
                    });
                }
                return Err(InvalidHunkError {
                    message: format!(
                        "Unexpected line found in add file hunk: '{line}'. Every line should start with '+'"
                    ),
                    line_number,
                });
            }
            StreamingParserMode::DeleteFile => {
                if let Some(mode) =
                    handle_hunk_headers_and_end_patch(trimmed, &mut hunks).map_err(|message| {
                        InvalidHunkError {
                            message,
                            line_number,
                        }
                    })?
                {
                    return Ok(StreamingParserState { mode, hunks });
                }
                return Err(InvalidHunkError {
                    message: format!(
                        "'{trimmed}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
                    ),
                    line_number,
                });
            }
            StreamingParserMode::UpdateFile => {
                let update_line = line.trim_end();
                if let Some(mode) = handle_hunk_headers_and_end_patch(update_line, &mut hunks)
                    .map_err(|message| InvalidHunkError {
                        message,
                        line_number,
                    })?
                {
                    return Ok(StreamingParserState { mode, hunks });
                }

                if let Some(UpdateFile {
                    move_path, chunks, ..
                }) = hunks.last_mut()
                {
                    if chunks.is_empty()
                        && move_path.is_none()
                        && let Some(move_to_path) = update_line.strip_prefix(MOVE_TO_MARKER)
                    {
                        *move_path = Some(PathBuf::from(move_to_path));
                        return Ok(StreamingParserState {
                            mode: StreamingParserMode::UpdateFile,
                            hunks,
                        });
                    }

                    match update_line {
                        EMPTY_CHANGE_CONTEXT_MARKER => {
                            chunks.push(UpdateFileChunk {
                                change_context: None,
                                old_lines: Vec::new(),
                                new_lines: Vec::new(),
                                is_end_of_file: false,
                            });
                            return Ok(StreamingParserState {
                                mode: StreamingParserMode::UpdateFile,
                                hunks,
                            });
                        }
                        line => {
                            if let Some(change_context) = line.strip_prefix(CHANGE_CONTEXT_MARKER) {
                                chunks.push(UpdateFileChunk {
                                    change_context: Some(change_context.to_string()),
                                    old_lines: Vec::new(),
                                    new_lines: Vec::new(),
                                    is_end_of_file: false,
                                });
                                return Ok(StreamingParserState {
                                    mode: StreamingParserMode::UpdateFile,
                                    hunks,
                                });
                            }
                        }
                    }

                    if update_line == EOF_MARKER {
                        if let Some(chunk) = chunks.last_mut() {
                            chunk.is_end_of_file = true;
                        }
                        return Ok(StreamingParserState {
                            mode: StreamingParserMode::UpdateFile,
                            hunks,
                        });
                    }

                    if line.is_empty() {
                        if chunks.is_empty() {
                            chunks.push(UpdateFileChunk {
                                change_context: None,
                                old_lines: Vec::new(),
                                new_lines: Vec::new(),
                                is_end_of_file: false,
                            });
                        }
                        if let Some(chunk) = chunks.last_mut() {
                            chunk.old_lines.push(String::new());
                            chunk.new_lines.push(String::new());
                        }
                        return Ok(StreamingParserState {
                            mode: StreamingParserMode::UpdateFile,
                            hunks,
                        });
                    }

                    if let Some(line_to_add) = line.strip_prefix(' ') {
                        if chunks.is_empty() {
                            chunks.push(UpdateFileChunk {
                                change_context: None,
                                old_lines: Vec::new(),
                                new_lines: Vec::new(),
                                is_end_of_file: false,
                            });
                        }
                        if let Some(chunk) = chunks.last_mut() {
                            chunk.old_lines.push(line_to_add.to_string());
                            chunk.new_lines.push(line_to_add.to_string());
                        }
                        return Ok(StreamingParserState {
                            mode: StreamingParserMode::UpdateFile,
                            hunks,
                        });
                    }

                    if let Some(line_to_add) = line.strip_prefix('+') {
                        if chunks.is_empty() {
                            chunks.push(UpdateFileChunk {
                                change_context: None,
                                old_lines: Vec::new(),
                                new_lines: Vec::new(),
                                is_end_of_file: false,
                            });
                        }
                        if let Some(chunk) = chunks.last_mut() {
                            chunk.new_lines.push(line_to_add.to_string());
                        }
                        return Ok(StreamingParserState {
                            mode: StreamingParserMode::UpdateFile,
                            hunks,
                        });
                    }

                    if let Some(line_to_remove) = line.strip_prefix('-') {
                        if chunks.is_empty() {
                            chunks.push(UpdateFileChunk {
                                change_context: None,
                                old_lines: Vec::new(),
                                new_lines: Vec::new(),
                                is_end_of_file: false,
                            });
                        }
                        if let Some(chunk) = chunks.last_mut() {
                            chunk.old_lines.push(line_to_remove.to_string());
                        }
                        return Ok(StreamingParserState {
                            mode: StreamingParserMode::UpdateFile,
                            hunks,
                        });
                    }
                }
                return Err(InvalidHunkError {
                    message: format!(
                        "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                    ),
                    line_number,
                });
            }
            StreamingParserMode::EndedPatch => mode,
        };
        Ok(StreamingParserState { mode, hunks })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_streaming_patch_parser_streams_complete_lines_before_end_patch() {
        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Add File: src/hello.txt\n+hello\n+wor"),
            Ok(vec![AddFile {
                path: PathBuf::from("src/hello.txt"),
                contents: "hello\n".to_string(),
            }])
        );
        assert_eq!(
            parser.push_delta("ld\n"),
            Ok(vec![AddFile {
                path: PathBuf::from("src/hello.txt"),
                contents: "hello\nworld\n".to_string(),
            }])
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta(
                "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new\n",
            ),
            Ok(vec![UpdateFile {
                path: PathBuf::from("src/old.rs"),
                move_path: Some(PathBuf::from("src/new.rs")),
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec!["old".to_string()],
                    new_lines: vec!["new".to_string()],
                    is_end_of_file: false,
                }],
            }])
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Delete File: gone.txt"),
            Ok(Vec::new())
        );
        assert_eq!(
            parser.push_delta("\n"),
            Ok(vec![DeleteFile {
                path: PathBuf::from("gone.txt"),
            }])
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta(
                "*** Begin Patch\n*** Add File: src/one.txt\n+one\n*** Delete File: src/two.txt\n",
            ),
            Ok(vec![
                AddFile {
                    path: PathBuf::from("src/one.txt"),
                    contents: "one\n".to_string(),
                },
                DeleteFile {
                    path: PathBuf::from("src/two.txt"),
                },
            ])
        );
    }

    #[test]
    fn test_streaming_patch_parser_large_patch_split_by_character() {
        let patch = "\
*** Begin Patch
*** Add File: docs/release-notes.md
+# Release notes
+
+## CLI
+- Surface apply_patch progress while arguments stream.
+- Keep final patch application gated on the completed tool call.
+- Include file summaries in the progress event payload.
*** Update File: src/config.rs
@@ impl Config
-    pub apply_patch_progress: bool,
+    pub stream_apply_patch_progress: bool,
     pub include_diagnostics: bool,
@@ fn default_progress_interval()
-    Duration::from_millis(500)
+    Duration::from_millis(250)
*** Delete File: src/legacy_patch_progress.rs
*** Update File: crates/cli/src/main.rs
*** Move to: crates/cli/src/bin/codex.rs
@@ fn run()
-    let args = Args::parse();
-    dispatch(args)
+    let cli = Cli::parse();
+    dispatch(cli)
*** Add File: tests/fixtures/apply_patch_progress.json
+{
+  \"type\": \"apply_patch_progress\",
+  \"hunks\": [
+    { \"operation\": \"add\", \"path\": \"docs/release-notes.md\" },
+    { \"operation\": \"update\", \"path\": \"src/config.rs\" }
+  ]
+}
*** Update File: README.md
@@ Development workflow
 Build the Rust workspace before opening a pull request.
+When touching streamed tool calls, include parser coverage for partial input.
+Prefer tests that exercise the exact event payload shape.
*** Delete File: docs/old-apply-patch-progress.md
*** End Patch";

        let mut parser = StreamingPatchParser::default();
        let mut max_hunk_count = 0;
        let mut saw_hunk_counts = Vec::new();
        let mut hunks = Vec::new();
        for ch in patch.chars() {
            let updated_hunks = parser.push_delta(&ch.to_string()).unwrap();
            if !updated_hunks.is_empty() {
                let hunk_count = updated_hunks.len();
                assert!(
                    hunk_count >= max_hunk_count,
                    "hunk count should never decrease while streaming: {hunk_count} < {max_hunk_count}",
                );
                if hunk_count > max_hunk_count {
                    saw_hunk_counts.push(hunk_count);
                    max_hunk_count = hunk_count;
                }
                hunks = updated_hunks;
            }
        }

        assert_eq!(saw_hunk_counts, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(hunks.len(), 7);
        assert_eq!(
            hunks
                .iter()
                .map(|hunk| match hunk {
                    AddFile { .. } => "add",
                    DeleteFile { .. } => "delete",
                    UpdateFile {
                        move_path: Some(_), ..
                    } => "move-update",
                    UpdateFile {
                        move_path: None, ..
                    } => "update",
                })
                .collect::<Vec<_>>(),
            vec![
                "add",
                "update",
                "delete",
                "move-update",
                "add",
                "update",
                "delete"
            ]
        );
    }

    #[test]
    fn test_streaming_patch_parser_keeps_indented_update_markers_as_context_lines() {
        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta(
                "\
*** Begin Patch
*** Update File: a.txt
@@
-old a
+new a
 *** Update File: b.txt
@@
-old b
+new b
*** End Patch
",
            ),
            Ok(vec![UpdateFile {
                path: PathBuf::from("a.txt"),
                move_path: None,
                chunks: vec![
                    UpdateFileChunk {
                        change_context: None,
                        old_lines: vec!["old a".to_string(), "*** Update File: b.txt".to_string()],
                        new_lines: vec!["new a".to_string(), "*** Update File: b.txt".to_string()],
                        is_end_of_file: false,
                    },
                    UpdateFileChunk {
                        change_context: None,
                        old_lines: vec!["old b".to_string()],
                        new_lines: vec!["new b".to_string()],
                        is_end_of_file: false,
                    },
                ],
            }])
        );
    }

    #[test]
    fn test_streaming_patch_parser_preserves_bare_empty_update_lines() {
        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta(
                "\
*** Begin Patch
*** Update File: file.txt
@@
 context before

 context after
*** End Patch
",
            ),
            Ok(vec![UpdateFile {
                path: PathBuf::from("file.txt"),
                move_path: None,
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    // The normal parser treats a bare empty line in an update hunk as an
                    // empty context line. Preserve that leniency in the streaming parser.
                    old_lines: vec![
                        "context before".to_string(),
                        String::new(),
                        "context after".to_string(),
                    ],
                    new_lines: vec![
                        "context before".to_string(),
                        String::new(),
                        "context after".to_string(),
                    ],
                    is_end_of_file: false,
                }],
            }])
        );
    }

    #[test]
    fn test_streaming_patch_parser_finish_processes_final_line_without_newline() {
        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch"),
            Ok(vec![AddFile {
                path: PathBuf::from("file.txt"),
                contents: "hello\n".to_string(),
            }])
        );
        assert_eq!(
            parser.finish(),
            Ok(vec![AddFile {
                path: PathBuf::from("file.txt"),
                contents: "hello\n".to_string(),
            }])
        );
    }

    #[test]
    fn test_streaming_patch_parser_finish_requires_end_patch() {
        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Add File: file.txt\n+hello\n"),
            Ok(vec![AddFile {
                path: PathBuf::from("file.txt"),
                contents: "hello\n".to_string(),
            }])
        );
        assert_eq!(
            parser.finish(),
            Err(InvalidPatchError(
                "The last line of the patch must be '*** End Patch'".to_string(),
            ))
        );
    }

    #[test]
    fn test_streaming_patch_parser_returns_errors() {
        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("bad\n"),
            Err(InvalidPatchError(
                "The first line of the patch must be '*** Begin Patch'".to_string(),
            ))
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(parser.push_delta("*** Begin Patch\n"), Ok(Vec::new()));
        assert_eq!(
            parser.push_delta("bad\n"),
            Err(InvalidHunkError {
                message: "'bad' is not a valid hunk header. Valid hunk headers: '*** Add File: {path}', '*** Delete File: {path}', '*** Update File: {path}'"
                    .to_string(),
                line_number: 2,
            })
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Add File: file.txt\nbad\n"),
            Err(InvalidHunkError {
                message:
                    "Unexpected line found in add file hunk: 'bad'. Every line should start with '+'"
                        .to_string(),
                line_number: 3,
            })
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Delete File: file.txt\nbad\n"),
            Err(InvalidHunkError {
                message: "'bad' is not a valid hunk header. Valid hunk headers: '*** Add File: {path}', '*** Delete File: {path}', '*** Update File: {path}'"
                    .to_string(),
                line_number: 3,
            })
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Update File: file.txt\n*** End Patch\n"),
            Err(InvalidHunkError {
                message: "Update hunk does not contain any lines".to_string(),
                line_number: 3,
            })
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta(
                "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n*** Delete File: other.txt\n",
            ),
            Err(InvalidHunkError {
                message: "Update hunk does not contain any lines".to_string(),
                line_number: 4,
            })
        );

        let mut parser = StreamingPatchParser::default();
        assert_eq!(
            parser.push_delta("*** Begin Patch\n*** Update File: file.txt\n@@\n*** End Patch\n"),
            Err(InvalidHunkError {
                message: "Update hunk does not contain any lines".to_string(),
                line_number: 4,
            })
        );
    }
}
