mod position;

use position::PositionCalculator;
use rue_lexer::Lexer;
use rue_parser::{ParseError, parse};
use rue_semantic::{SemanticError, analyze_cst};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug)]
pub struct RueLanguageServer {
    client: Client,
    documents: RwLock<HashMap<Url, String>>,
}

impl RueLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
        }
    }

    async fn parse_document(&self, _uri: &Url, text: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let mut lexer = Lexer::new(text);
        let tokens = match lexer.tokenize() {
            Ok(tokens) => tokens,
            Err(e) => {
                let calc = PositionCalculator::new(text);
                let (line, col) = calc.offset_to_position(e.position);
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position {
                            line,
                            character: col,
                        },
                        end: Position {
                            line,
                            character: col + 1,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    code_description: None,
                    source: Some("rue".to_string()),
                    message: e.message,
                    related_information: None,
                    tags: None,
                    data: None,
                });
                return diagnostics;
            }
        };

        match parse(tokens) {
            Ok(ast) => {
                // Parse succeeded, now run semantic analysis
                if let Err(error) = analyze_cst(&ast) {
                    diagnostics.push(self.semantic_error_to_diagnostic(error, text));
                }
            }
            Err(error) => {
                // Parse failed, report syntax error
                diagnostics.push(self.parse_error_to_diagnostic(error, text));
            }
        }

        diagnostics
    }

    fn parse_error_to_diagnostic(&self, error: ParseError, text: &str) -> Diagnostic {
        let calc = PositionCalculator::new(text);
        let range = calc.span_to_range(error.span);

        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("rue-lsp".to_string()),
            message: error.message,
            related_information: None,
            tags: None,
            data: None,
        }
    }

    fn semantic_error_to_diagnostic(&self, error: SemanticError, text: &str) -> Diagnostic {
        let calc = PositionCalculator::new(text);
        let range = calc.span_to_range(error.span);

        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("rue-lsp".to_string()),
            message: error.message,
            related_information: None,
            tags: None,
            data: None,
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for RueLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: None,
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "rue-language-server".to_string(),
                version: Some("0.1.0".to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Rue Language Server initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        // Store document
        self.documents
            .write()
            .await
            .insert(uri.clone(), text.clone());

        // Parse and send diagnostics
        let diagnostics = self.parse_document(&uri, &text).await;
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().next() {
            let text = change.text;

            // Update stored document
            self.documents
                .write()
                .await
                .insert(uri.clone(), text.clone());

            // Parse and send diagnostics
            let diagnostics = self.parse_document(&uri, &text).await;
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        // Remove document from storage
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);

        // Clear diagnostics
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn completion(&self, _params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let mut items = vec![CompletionItem {
            label: "exit".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("exit(code: i64) -> ()".to_string()),
            documentation: Some(Documentation::String(
                "Terminates the program with the specified exit code.".to_string(),
            )),
            insert_text: Some("exit($1)".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        }];

        items.push(CompletionItem {
            label: "println_i64".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("println_i64(value: i64) -> ()".to_string()),
            documentation: Some(Documentation::String(
                "Prints a 64-bit integer to stdout followed by a newline.".to_string(),
            )),
            insert_text: Some("println_i64($1)".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });

        items.push(CompletionItem {
            label: "println_i32".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("println_i32(value: i32) -> ()".to_string()),
            documentation: Some(Documentation::String(
                "Prints a 32-bit integer to stdout followed by a newline.".to_string(),
            )),
            insert_text: Some("println_i32($1)".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });

        items.push(CompletionItem {
            label: "println_bool".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("println_bool(value: bool) -> ()".to_string()),
            documentation: Some(Documentation::String(
                "Prints a boolean value as \"true\" or \"false\" followed by a newline."
                    .to_string(),
            )),
            insert_text: Some("println_bool($1)".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });

        items.push(CompletionItem {
            label: "println_unit".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("println_unit(value: ()) -> ()".to_string()),
            documentation: Some(Documentation::String(
                "Prints \"()\" to stdout followed by a newline.".to_string(),
            )),
            insert_text: Some("println_unit($1)".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        });

        items.push(CompletionItem {
            label: "input".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("input() -> i64".to_string()),
            documentation: Some(Documentation::String(
                "Reads a line from stdin and parses it as an i64. Returns 0 on parse error."
                    .to_string(),
            )),
            insert_text: Some("input()".to_string()),
            insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
            ..Default::default()
        });

        // Add keywords
        for keyword in &[
            "fn", "let", "if", "else", "while", "true", "false", "i32", "i64", "bool",
        ] {
            items.push(CompletionItem {
                label: keyword.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        // Get the document text
        let documents = self.documents.read().await;
        let text = match documents.get(&uri) {
            Some(text) => text,
            None => return Ok(None),
        };

        // Simple check for built-in functions
        let line = match text.lines().nth(position.line as usize) {
            Some(line) => line,
            None => return Ok(None),
        };

        // Check if cursor is on a built-in function
        let hover_info = if line.contains("exit") && position.character < line.len() as u32 {
            Some((
                "exit(code: i64) -> ()",
                "Terminates the program with the specified exit code.",
            ))
        } else if line.contains("println_i64") {
            Some((
                "println_i64(value: i64) -> ()",
                "Prints a 64-bit integer to stdout followed by a newline.",
            ))
        } else if line.contains("println_i32") {
            Some((
                "println_i32(value: i32) -> ()",
                "Prints a 32-bit integer to stdout followed by a newline.",
            ))
        } else if line.contains("println_bool") {
            Some((
                "println_bool(value: bool) -> ()",
                "Prints a boolean value as \"true\" or \"false\" followed by a newline.",
            ))
        } else if line.contains("println_unit") {
            Some((
                "println_unit(value: ()) -> ()",
                "Prints \"()\" to stdout followed by a newline.",
            ))
        } else if line.contains("input") {
            Some((
                "input() -> i64",
                "Reads a line from stdin and parses it as an i64. Returns 0 on parse error.",
            ))
        } else {
            None
        };

        if let Some((signature, docs)) = hover_info {
            Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```rue\n{signature}\n```\n\n{docs}"),
                }),
                range: None,
            }))
        } else {
            Ok(None)
        }
    }
}

pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(RueLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use rue_lexer::Lexer;
    use rue_parser::parse;
    use rue_semantic::analyze_cst;

    #[test]
    fn test_while_loop_parsing() {
        let text = r#"
fn test_while(n) {
    while n > 0 {
        n + 1
    };
    42
}

fn main() {
    test_while(5)
}
"#;

        let mut lexer = Lexer::new(text);
        let tokens = lexer.tokenize().unwrap();
        let result = parse(tokens);

        assert!(result.is_ok(), "While loop should parse without errors");
    }

    #[test]
    fn test_invalid_while_syntax() {
        let text = r#"
fn test_invalid() {
    while {
        42
    }
}
"#;

        let mut lexer = Lexer::new(text);
        let tokens = lexer.tokenize().unwrap();
        let result = parse(tokens);

        assert!(
            result.is_err(),
            "Invalid while syntax should produce errors"
        );
    }

    #[test]
    fn test_assignment_parsing() {
        let text = r#"
fn test_assignment() {
    let x = 42;
    x = 100;
    x
}

fn main() {
    test_assignment()
}
"#;

        let mut lexer = Lexer::new(text);
        let tokens = lexer.tokenize().unwrap();
        let result = parse(tokens);

        assert!(result.is_ok(), "Assignment should parse without errors");
    }

    #[test]
    fn test_semantic_undefined_variable_detection() {
        let text = r#"
fn test_undefined() {
    x + 5
}
"#;

        let mut lexer = Lexer::new(text);
        let tokens = lexer.tokenize().unwrap();
        let ast = parse(tokens).expect("Should parse");
        let result = analyze_cst(&ast);

        assert!(result.is_err(), "Should detect undefined variable");
        if let Err(err) = result {
            assert!(err.message.contains("Undefined variable"));
        }
    }

    #[test]
    fn test_semantic_type_mismatch_detection() {
        let text = r#"
fn test_type_error() {
    let x = 42;
    let y = true;
    x + y
}
"#;

        let mut lexer = Lexer::new(text);
        let tokens = lexer.tokenize().unwrap();
        let ast = parse(tokens).expect("Should parse");
        let result = analyze_cst(&ast);

        assert!(result.is_err(), "Should detect type mismatch");
        if let Err(err) = result {
            assert!(err.message.contains("Type mismatch") || err.message.contains("Cannot add"));
        }
    }

    #[test]
    fn test_comments_are_ignored() {
        let text = r#"
// Single line comment
fn test_comments() {
    /* Multi
       line
       comment */
    let x = 42; // inline comment
    /* Nested /* comments */ work */
    x
}
"#;

        let mut lexer = Lexer::new(text);
        let tokens = lexer.tokenize().unwrap();
        let result = parse(tokens);

        assert!(result.is_ok(), "Comments should not affect parsing");
    }

    #[test]
    fn test_position_conversion() {
        use crate::position::PositionCalculator;

        let text = "fn test(): i32 {\n    undefined_var\n}";
        let calc = PositionCalculator::new(text);

        // Test various positions
        let (line, col) = calc.offset_to_position(0);
        assert_eq!((line, col), (0, 0));

        let (line, col) = calc.offset_to_position(17); // Start of second line
        assert_eq!((line, col), (1, 0));
    }
}
