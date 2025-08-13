use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Individual directive parsed from //!@ comments
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestDirective {
    Id(String),
    Kind(TestKind),
    Spec(String),
    ExpectExit(i32),
    ExpectStdout(String),
    ExpectStderr(String),
    Flags(Vec<String>),
    Skip(String),
}

/// Complete test specification built from directives
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSpec {
    pub id: String,
    pub kind: TestKind,
    pub spec_refs: Vec<String>,
    pub expected_exit: Option<i32>,
    pub expected_stdout: Vec<String>,
    pub expected_stderr: Vec<String>,
    pub compiler_flags: Vec<String>,
    pub skip_reason: Option<String>,
}

/// Types of tests supported by the runner
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestKind {
    /// Test should compile successfully
    CompilePass,
    /// Test should fail to compile
    CompileFail,
    /// Test should compile and run successfully
    RunPass,
    /// Test should compile but fail at runtime
    RunFail,
    /// Test for MIR snapshot comparison
    SnapshotMir,
    /// Test for assembly snapshot comparison
    SnapshotAsm,
}

impl FromStr for TestKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "compile-pass" => Ok(TestKind::CompilePass),
            "compile-fail" => Ok(TestKind::CompileFail),
            "run-pass" => Ok(TestKind::RunPass),
            "run-fail" => Ok(TestKind::RunFail),
            "snapshot-mir" => Ok(TestKind::SnapshotMir),
            "snapshot-asm" => Ok(TestKind::SnapshotAsm),
            _ => Err(anyhow!("Unknown test kind: {}", s)),
        }
    }
}

impl TestKind {
    /// Convert test kind to string
    pub fn as_str(&self) -> &'static str {
        match self {
            TestKind::CompilePass => "compile-pass",
            TestKind::CompileFail => "compile-fail",
            TestKind::RunPass => "run-pass",
            TestKind::RunFail => "run-fail",
            TestKind::SnapshotMir => "snapshot-mir",
            TestKind::SnapshotAsm => "snapshot-asm",
        }
    }
}

/// Parse test directives from file content
pub fn parse_directives(content: &str) -> Result<Vec<TestDirective>> {
    let directive_regex =
        Regex::new(r"//!@\s*(.+)").context("Failed to compile directive regex")?;

    let mut directives = Vec::new();

    for line in content.lines() {
        if let Some(captures) = directive_regex.captures(line) {
            let directive_text = captures.get(1).unwrap().as_str().trim();
            let directive = parse_single_directive_line(directive_text)
                .with_context(|| format!("Failed to parse directive: {}", directive_text))?;
            directives.push(directive);
        }
    }

    Ok(directives)
}

/// Build a TestSpec from a list of directives
pub fn build_test_spec(directives: &[TestDirective]) -> Result<TestSpec> {
    let mut test_id: Option<String> = None;
    let mut test_kind: Option<TestKind> = None;
    let mut spec_refs = Vec::new();
    let mut expected_exit: Option<i32> = None;
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let mut compiler_flags = Vec::new();
    let mut skip_reason: Option<String> = None;

    for directive in directives {
        match directive {
            TestDirective::Id(id) => {
                if test_id.is_some() {
                    return Err(anyhow!("Duplicate 'id' directive"));
                }
                test_id = Some(id.clone());
            }
            TestDirective::Kind(kind) => {
                if test_kind.is_some() {
                    return Err(anyhow!("Duplicate 'kind' directive"));
                }
                test_kind = Some(kind.clone());
            }
            TestDirective::Spec(spec) => {
                spec_refs.push(spec.clone());
            }
            TestDirective::ExpectExit(code) => {
                if expected_exit.is_some() {
                    return Err(anyhow!("Duplicate 'expect exit' directive"));
                }
                expected_exit = Some(*code);
            }
            TestDirective::ExpectStdout(stdout) => {
                expected_stdout.push(stdout.clone());
            }
            TestDirective::ExpectStderr(stderr) => {
                expected_stderr.push(stderr.clone());
            }
            TestDirective::Flags(flags) => {
                compiler_flags.extend(flags.clone());
            }
            TestDirective::Skip(reason) => {
                if skip_reason.is_some() {
                    return Err(anyhow!("Duplicate 'skip' directive"));
                }
                skip_reason = Some(reason.clone());
            }
        }
    }

    // Validate required fields
    let id = test_id.ok_or_else(|| anyhow!("Missing required 'id' directive"))?;
    let kind = test_kind.ok_or_else(|| anyhow!("Missing required 'kind' directive"))?;

    Ok(TestSpec {
        id,
        kind,
        spec_refs,
        expected_exit,
        expected_stdout,
        expected_stderr,
        compiler_flags,
        skip_reason,
    })
}

fn parse_single_directive_line(text: &str) -> Result<TestDirective> {
    if text.is_empty() {
        return Err(anyhow!("Empty directive"));
    }

    // Split on first whitespace to get key and value
    let (key, value) = if let Some(pos) = text.find(char::is_whitespace) {
        let key = &text[..pos];
        let value = text[pos..].trim_start();
        (key, Some(value))
    } else {
        (text, None)
    };

    match key {
        "id" => {
            let id_value = value.ok_or_else(|| anyhow!("'id' directive requires a value"))?;
            Ok(TestDirective::Id(id_value.to_string()))
        }
        "kind" => {
            let kind_value = value.ok_or_else(|| anyhow!("'kind' directive requires a value"))?;
            Ok(TestDirective::Kind(kind_value.parse()?))
        }
        "spec" => {
            let spec_value = value.ok_or_else(|| anyhow!("'spec' directive requires a value"))?;
            Ok(TestDirective::Spec(spec_value.to_string()))
        }
        "expect" => {
            let expect_value =
                value.ok_or_else(|| anyhow!("'expect' directive requires a value"))?;
            parse_expect_directive(expect_value)
        }
        "flags" => {
            let flags_value = value.ok_or_else(|| anyhow!("'flags' directive requires a value"))?;
            // Parse flags as space-separated arguments
            let flags: Vec<String> = flags_value
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            Ok(TestDirective::Flags(flags))
        }
        "skip" => {
            let skip_reason = value.unwrap_or("Test skipped").to_string();
            Ok(TestDirective::Skip(skip_reason))
        }
        _ => Err(anyhow!("Unknown directive key: {}", key)),
    }
}

fn parse_expect_directive(value: &str) -> Result<TestDirective> {
    // Split on first whitespace to get expect type and expected value
    let (expect_type, expect_value) = if let Some(pos) = value.find(char::is_whitespace) {
        let expect_type = &value[..pos];
        let expect_value = value[pos..].trim_start();
        (expect_type, Some(expect_value))
    } else {
        (value, None)
    };

    match expect_type {
        "exit" => {
            let exit_code_str =
                expect_value.ok_or_else(|| anyhow!("'expect exit' requires a code value"))?;
            // Strip inline comments (anything after //)
            let exit_code_str = if let Some(comment_pos) = exit_code_str.find("//") {
                exit_code_str[..comment_pos].trim()
            } else {
                exit_code_str
            };
            let exit_code = exit_code_str
                .parse::<i32>()
                .with_context(|| format!("Invalid exit code: {}", exit_code_str))?;
            Ok(TestDirective::ExpectExit(exit_code))
        }
        "stdout" => {
            let stdout_value =
                expect_value.ok_or_else(|| anyhow!("'expect stdout' requires a value"))?;
            // Remove quotes if present
            let stdout_clean = if stdout_value.starts_with('"') && stdout_value.ends_with('"') {
                &stdout_value[1..stdout_value.len() - 1]
            } else {
                stdout_value
            };
            Ok(TestDirective::ExpectStdout(stdout_clean.to_string()))
        }
        "stderr" => {
            let stderr_value =
                expect_value.ok_or_else(|| anyhow!("'expect stderr' requires a value"))?;
            // Remove quotes if present
            let stderr_clean = if stderr_value.starts_with('"') && stderr_value.ends_with('"') {
                &stderr_value[1..stderr_value.len() - 1]
            } else {
                stderr_value
            };
            Ok(TestDirective::ExpectStderr(stderr_clean.to_string()))
        }
        _ => Err(anyhow!("Unknown expect type: {}", expect_type)),
    }
}

impl TestSpec {
    /// Get expected exit code for run-fail tests
    pub fn expected_exit_code(&self) -> Option<i32> {
        self.expected_exit
    }

    /// Get expected stdout patterns (multiple allowed)
    pub fn expected_stdout_patterns(&self) -> &[String] {
        &self.expected_stdout
    }

    /// Get expected stderr patterns (multiple allowed)
    pub fn expected_stderr_patterns(&self) -> &[String] {
        &self.expected_stderr
    }

    /// Get first expected stdout pattern (for compatibility)
    pub fn expected_stdout(&self) -> Option<&str> {
        self.expected_stdout.first().map(|s| s.as_str())
    }

    /// Get first expected stderr pattern (for compatibility)
    pub fn expected_stderr(&self) -> Option<&str> {
        self.expected_stderr.first().map(|s| s.as_str())
    }

    /// Get specification references
    pub fn spec_references(&self) -> Vec<&str> {
        self.spec_refs.iter().map(|s| s.as_str()).collect()
    }

    /// Check if this is a normative test (references specs)
    pub fn is_normative(&self) -> bool {
        !self.spec_refs.is_empty()
    }

    /// Get compiler flags
    pub fn get_compiler_flags(&self) -> &[String] {
        &self.compiler_flags
    }
}

// For backward compatibility, implement similar methods on TestDirective
// These will be used when working with the old interface
impl TestDirective {
    /// Check if this directive contributes to a normative test
    pub fn is_normative(&self) -> bool {
        matches!(self, TestDirective::Spec(_))
    }

    /// Get spec references from this directive (returns empty vec for non-spec directives)
    pub fn spec_references(&self) -> Vec<&str> {
        match self {
            TestDirective::Spec(spec) => vec![spec.as_str()],
            _ => vec![],
        }
    }

    /// Get expected exit code (returns None for non-exit directives)
    pub fn expected_exit_code(&self) -> Option<i32> {
        match self {
            TestDirective::ExpectExit(code) => Some(*code),
            _ => None,
        }
    }

    /// Get expected stdout (returns None for non-stdout directives)
    pub fn expected_stdout(&self) -> Option<&str> {
        match self {
            TestDirective::ExpectStdout(stdout) => Some(stdout.as_str()),
            _ => None,
        }
    }

    /// Get expected stderr (returns None for non-stderr directives)
    pub fn expected_stderr(&self) -> Option<&str> {
        match self {
            TestDirective::ExpectStderr(stderr) => Some(stderr.as_str()),
            _ => None,
        }
    }

    /// Get the test kind (returns None for non-kind directives)
    pub fn kind(&self) -> Option<&TestKind> {
        match self {
            TestDirective::Kind(kind) => Some(kind),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_individual_directives() {
        let content = r#"//!@ id test_basic
//!@ kind compile-pass
//!@ spec type-inference
fn main() {}"#;
        let directives = parse_directives(content).unwrap();

        assert_eq!(directives.len(), 3);
        assert!(matches!(directives[0], TestDirective::Id(ref id) if id == "test_basic"));
        assert!(matches!(
            directives[1],
            TestDirective::Kind(TestKind::CompilePass)
        ));
        assert!(matches!(directives[2], TestDirective::Spec(ref spec) if spec == "type-inference"));
    }

    #[test]
    fn test_build_test_spec() {
        let content = r#"//!@ id test_basic
//!@ kind compile-pass
//!@ spec type-inference
fn main() {}"#;
        let directives = parse_directives(content).unwrap();
        let test_spec = build_test_spec(&directives).unwrap();

        assert_eq!(test_spec.id, "test_basic");
        assert_eq!(test_spec.kind, TestKind::CompilePass);
        assert!(test_spec.is_normative());
        assert_eq!(test_spec.spec_references(), vec!["type-inference"]);
    }

    #[test]
    fn test_parse_directive_with_multiple_specs() {
        let content = r#"//!@ id test_multi_spec
//!@ kind run-pass
//!@ spec type-inference
//!@ spec control-flow
fn main() {}"#;
        let directives = parse_directives(content).unwrap();
        let test_spec = build_test_spec(&directives).unwrap();

        assert_eq!(
            test_spec.spec_references(),
            vec!["type-inference", "control-flow"]
        );
    }

    #[test]
    fn test_parse_directive_with_expect() {
        let content = r#"//!@ id test_expect
//!@ kind run-fail
//!@ expect exit 1
//!@ expect stderr "overflow error"
fn main() {}"#;
        let directives = parse_directives(content).unwrap();
        let test_spec = build_test_spec(&directives).unwrap();

        assert_eq!(test_spec.expected_exit_code(), Some(1));
        assert_eq!(test_spec.expected_stderr(), Some("overflow error"));
    }

    #[test]
    fn test_parse_directive_with_flags() {
        let content = r#"//!@ id test_flags
//!@ kind compile-pass
//!@ flags -O2 --verbose
fn main() {}"#;
        let directives = parse_directives(content).unwrap();
        let test_spec = build_test_spec(&directives).unwrap();

        assert_eq!(
            test_spec.get_compiler_flags(),
            &["-O2".to_string(), "--verbose".to_string()]
        );
    }

    #[test]
    fn test_parse_directive_with_multiple_stdout() {
        let content = r#"//!@ id test_multi_stdout
//!@ kind run-pass
//!@ expect stdout "hello"
//!@ expect stdout "world"
fn main() {}"#;
        let directives = parse_directives(content).unwrap();
        let test_spec = build_test_spec(&directives).unwrap();

        assert_eq!(
            test_spec.expected_stdout_patterns(),
            &["hello".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn test_parse_missing_required_fields() {
        let content = "//!@ spec type-inference\nfn main() {}";
        let directives = parse_directives(content).unwrap();
        let result = build_test_spec(&directives);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing required 'id' directive")
        );
    }

    #[test]
    fn test_parse_invalid_directive_key() {
        let content = "//!@ id test1\n//!@ kind compile-pass\n//!@ invalid_key value\nfn main() {}";
        let result = parse_directives(content);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        // The error is wrapped with context, so check for the root cause
        assert!(error_msg.contains("Unknown directive key") || error_msg.contains("invalid_key"));
    }

    #[test]
    fn test_individual_directive_methods() {
        let directive = TestDirective::Kind(TestKind::CompilePass);
        assert_eq!(directive.kind(), Some(&TestKind::CompilePass));
        assert!(!directive.is_normative());

        let spec_directive = TestDirective::Spec("test-spec".to_string());
        assert!(spec_directive.is_normative());
        assert_eq!(spec_directive.spec_references(), vec!["test-spec"]);
    }

    #[test]
    fn test_test_kind_conversions() {
        assert_eq!(
            "compile-pass".parse::<TestKind>().unwrap(),
            TestKind::CompilePass
        );
        assert_eq!(TestKind::CompilePass.as_str(), "compile-pass");

        assert!("invalid".parse::<TestKind>().is_err());
    }
}
