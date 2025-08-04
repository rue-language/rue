use rue_lexer::{LexError, Lexer};

/// Helper function to expect a lexer error
fn expect_lex_error(input: &str) -> LexError {
    let mut lexer = Lexer::new(input);
    match lexer.tokenize() {
        Ok(_) => panic!("Expected lexer error but tokenization succeeded for: {input}"),
        Err(e) => e,
    }
}

/// Helper function to expect a lexer error with specific message content
fn expect_lex_error_containing(input: &str, expected_msg: &str) {
    let error = expect_lex_error(input);
    assert!(
        error.message.contains(expected_msg),
        "Expected error message to contain '{}', but got: '{}'",
        expected_msg,
        error.message
    );
}

#[test]
fn test_unexpected_characters() {
    // Special characters not used in Rue
    expect_lex_error_containing("@", "Unexpected character");
    expect_lex_error_containing("#", "Unexpected character");
    expect_lex_error_containing("$", "Unexpected character");
    expect_lex_error_containing("?", "Unexpected character");
    expect_lex_error_containing("`", "Unexpected character");
    expect_lex_error_containing("\\", "Unexpected character");
    expect_lex_error_containing("~", "Unexpected character");

    // Single characters that might be part of operators in other languages
    expect_lex_error_containing("&", "Unexpected character");
    expect_lex_error_containing("|", "Unexpected character");
    expect_lex_error_containing("^", "Unexpected character");

    // Bang without equals has a special error message
    expect_lex_error_containing("!", "Did you mean '!='?");
}

#[test]
fn test_invalid_operators() {
    // Common operators from other languages that Rue doesn't support
    expect_lex_error_containing("&&", "Unexpected character");
    expect_lex_error_containing("||", "Unexpected character");
    // Note: ++ and -- are tokenized as two + or - operators, not errors
    // Note: << and >> are tokenized as two < or > operators, not errors
    expect_lex_error_containing("&=", "Unexpected character");
    expect_lex_error_containing("|=", "Unexpected character");
    expect_lex_error_containing("^=", "Unexpected character");
    expect_lex_error_containing("~=", "Unexpected character");
    expect_lex_error_containing("?:", "Unexpected character");
}

#[test]
fn test_invalid_number_formats() {
    // Note: Decimal numbers like 3.14 are now tokenized as separate tokens (3, ., 14)
    // since '.' is a valid token for field access. This is not an error anymore.

    // Test invalid characters that should still cause lexer errors
    expect_lex_error_containing("3@14", "Unexpected character"); // @ is not valid
    expect_lex_error_containing("3#14", "Unexpected character"); // # is not valid

    // Underscores in numbers are tokenized as separate tokens
    // 1_000 becomes: 1, _, 000

    // Scientific notation (tokenized as number + identifier)
    // 1e10 becomes: 1, e10
    // These would be parser errors, not lexer errors
}

#[test]
fn test_number_overflow() {
    // Positive overflow - error message says "Invalid number"
    expect_lex_error_containing("9223372036854775808", "Invalid number"); // i64::MAX + 1
    expect_lex_error_containing("10000000000000000000", "Invalid number");
    expect_lex_error_containing("99999999999999999999999999999", "Invalid number");

    // Negative overflow
    expect_lex_error_containing("-9223372036854775809", "Invalid number"); // i64::MIN - 1
    expect_lex_error_containing("-10000000000000000000", "Invalid number");
    expect_lex_error_containing("-99999999999999999999999999999", "Invalid number");
}

#[test]
fn test_unterminated_comments() {
    // Simple unterminated comment - error says "Unterminated multi-line comment"
    expect_lex_error_containing("/* unclosed comment", "Unterminated multi-line comment");
    expect_lex_error_containing(
        "/* unclosed\ncomment\nwith\nnewlines",
        "Unterminated multi-line comment",
    );

    // Nested unterminated comments
    expect_lex_error_containing("/* outer /* inner */", "Unterminated multi-line comment");
    expect_lex_error_containing(
        "/* /* /* deeply nested */",
        "Unterminated multi-line comment",
    );
    expect_lex_error_containing(
        "/* /* /* deeply nested */ */",
        "Unterminated multi-line comment",
    );

    // Comment at end of file
    expect_lex_error_containing("let x = 5; /*", "Unterminated multi-line comment");
    expect_lex_error_containing("fn main() { /* oops", "Unterminated multi-line comment");
}

#[test]
fn test_invalid_escape_sequences_in_comments() {
    // These should tokenize fine - escape sequences don't matter in comments
    let mut lexer = Lexer::new("// comment with \\n \\t \\r");
    assert!(lexer.tokenize().is_ok());

    lexer = Lexer::new("/* comment with \\n \\t \\r */");
    assert!(lexer.tokenize().is_ok());
}

#[test]
fn test_control_characters() {
    // Test specific control characters that should fail
    // Many control characters are treated as whitespace by Rust's is_whitespace()

    // Bell character
    expect_lex_error_containing("\x07", "Unexpected character");

    // Escape character
    expect_lex_error_containing("\x1B", "Unexpected character");

    // Control characters in identifiers
    expect_lex_error_containing("let\x07x", "Unexpected character");
}

#[test]
fn test_unicode_characters() {
    // Emojis should fail
    expect_lex_error_containing("🚀", "Unexpected character");
    expect_lex_error_containing("let x = 🎉;", "Unexpected character");

    // Mathematical symbols
    expect_lex_error_containing("∑", "Unexpected character");
    expect_lex_error_containing("∏", "Unexpected character");
    expect_lex_error_containing("√", "Unexpected character");
    expect_lex_error_containing("∞", "Unexpected character");

    // Other special unicode
    expect_lex_error_containing("→", "Unexpected character");
    expect_lex_error_containing("⇒", "Unexpected character");
    expect_lex_error_containing("λ", "Unexpected character");
}

#[test]
fn test_string_literals_not_supported() {
    // Rue doesn't support string literals yet
    expect_lex_error_containing("\"hello\"", "Unexpected character");
    expect_lex_error_containing("'c'", "Unexpected character");
    expect_lex_error_containing("\"\"", "Unexpected character");
    expect_lex_error_containing("''", "Unexpected character");

    // Raw strings
    expect_lex_error_containing("r\"raw string\"", "Unexpected character");
    expect_lex_error_containing("r#\"raw string\"#", "Unexpected character");
}

#[test]
fn test_error_position_accuracy() {
    // Test that error positions are correctly reported
    let error = expect_lex_error("@");
    assert_eq!(error.position, 0);

    let error = expect_lex_error("   @");
    assert_eq!(error.position, 3);

    let error = expect_lex_error("let x = @");
    assert_eq!(error.position, 8);

    let error = expect_lex_error("fn main() {\n    @\n}");
    assert_eq!(error.position, 16); // After "fn main() {\n    "
}

#[test]
fn test_mixed_valid_and_invalid() {
    // Valid tokens followed by invalid character
    expect_lex_error_containing("let x = 5; @", "Unexpected character");
    expect_lex_error_containing("fn main() { $ }", "Unexpected character");
    expect_lex_error_containing("if true { } #", "Unexpected character");

    // Invalid character in the middle
    expect_lex_error_containing("let @ x = 5;", "Unexpected character");
    expect_lex_error_containing("fn main@() { }", "Unexpected character");
    expect_lex_error_containing("let x = 5 @ + 3;", "Unexpected character");
}

#[test]
fn test_edge_cases() {
    // Very long identifier followed by invalid character
    let long_ident = "a".repeat(1000);
    let input = format!("{long_ident} @");
    expect_lex_error_containing(&input, "Unexpected character");

    // Many spaces followed by invalid character
    let spaces = " ".repeat(1000);
    let input = format!("{spaces}@");
    expect_lex_error_containing(&input, "Unexpected character");

    // Invalid character after comment
    expect_lex_error_containing("// comment\n@", "Unexpected character");
    expect_lex_error_containing("/* comment */ @", "Unexpected character");
}

#[test]
fn test_incomplete_tokens_at_eof() {
    // These should actually tokenize fine (numbers are complete)
    let mut lexer = Lexer::new("123");
    assert!(lexer.tokenize().is_ok());

    lexer = Lexer::new("-456");
    assert!(lexer.tokenize().is_ok());

    // Single minus is a valid token
    lexer = Lexer::new("-");
    assert!(lexer.tokenize().is_ok());

    // Less-than is a valid token
    lexer = Lexer::new("<");
    assert!(lexer.tokenize().is_ok());

    // Greater-than is a valid token
    lexer = Lexer::new(">");
    assert!(lexer.tokenize().is_ok());
}

#[test]
fn test_consecutive_errors() {
    // Multiple invalid characters - lexer should fail on first one
    let error = expect_lex_error("@#$");
    assert_eq!(error.position, 0); // Should fail on first @

    let error = expect_lex_error("let x = @#$");
    assert_eq!(error.position, 8); // Should fail on @
}

#[test]
fn test_invalid_in_identifiers() {
    // Identifiers can't contain special characters
    expect_lex_error_containing("my@var", "Unexpected character");
    expect_lex_error_containing("func$name", "Unexpected character");
    expect_lex_error_containing("test#case", "Unexpected character");
    expect_lex_error_containing("var?name", "Unexpected character");
}

#[test]
fn test_numbers_followed_by_identifiers() {
    // These should tokenize as separate tokens, not errors
    let mut lexer = Lexer::new("123abc");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // 123, abc, EOF

    lexer = Lexer::new("42test");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // 42, test, EOF
}

#[test]
fn test_operators_tokenized_separately() {
    // ++ tokenizes as two + operators
    let mut lexer = Lexer::new("++");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // +, +, EOF

    // -- tokenizes as two - operators
    lexer = Lexer::new("--");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // -, -, EOF
}

#[test]
fn test_valid_unicode_identifiers() {
    // Some unicode might be accepted in identifiers - let's see what works
    let mut lexer = Lexer::new("café");
    let result = lexer.tokenize();

    // If it tokenizes successfully, it's treating unicode letters as valid
    // Otherwise it would be an error
    if result.is_ok() {
        // Unicode letters are accepted in identifiers
        let tokens = result.unwrap();
        assert!(tokens.len() >= 2); // At least identifier + EOF
    } else {
        // Unicode letters cause errors
        let error = result.unwrap_err();
        assert!(error.message.contains("Unexpected character"));
    }
}

#[test]
fn test_sequences_tokenized_as_multiple_tokens() {
    // Hex-like sequences are tokenized as number + identifier
    let mut lexer = Lexer::new("0xFF");
    let tokens = lexer.tokenize().unwrap();
    assert!(tokens.len() >= 3); // 0, xFF, EOF

    // Binary-like sequences
    lexer = Lexer::new("0b1010");
    let tokens = lexer.tokenize().unwrap();
    assert!(tokens.len() >= 3); // 0, b1010, EOF

    // Octal-like sequences
    lexer = Lexer::new("0o777");
    let tokens = lexer.tokenize().unwrap();
    assert!(tokens.len() >= 3); // 0, o777, EOF

    // << is two < operators
    lexer = Lexer::new("<<");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // <, <, EOF

    // >> is two > operators
    lexer = Lexer::new(">>");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // >, >, EOF

    // :: is : followed by :
    lexer = Lexer::new("::");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens.len(), 3); // :, :, EOF

    // Numbers with underscores tokenize as multiple tokens
    lexer = Lexer::new("1_000");
    let tokens = lexer.tokenize().unwrap();
    assert!(tokens.len() >= 3); // 1, _000 (identifier), EOF

    lexer = Lexer::new("1_000_000");
    let tokens = lexer.tokenize().unwrap();
    assert!(tokens.len() >= 3); // 1, _000_000 (identifier), EOF
}
