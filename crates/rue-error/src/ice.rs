//! Internal Compiler Error (ICE) handling.
//!
//! This module provides rich context capture for internal compiler errors to
//! improve bug reports and developer debugging experience.

use std::backtrace::Backtrace;
use std::fmt;

/// Context information for an Internal Compiler Error (ICE).
///
/// This struct captures detailed information about the compiler state when an
/// ICE occurs, making it easier to diagnose and fix compiler bugs.
///
/// # Example
/// ```ignore
/// let ice = IceContext::new("unexpected type in codegen")
///     .with_version("0.1.0")
///     .with_target("x86_64-unknown-linux-gnu")
///     .with_phase("codegen/emit")
///     .with_backtrace();
/// ```
#[derive(Debug)]
pub struct IceContext {
    /// The error message describing what went wrong.
    pub message: String,
    /// Compiler version (from CARGO_PKG_VERSION).
    pub version: Option<String>,
    /// Target architecture (e.g., "x86_64-unknown-linux-gnu").
    pub target: Option<String>,
    /// Compilation phase (e.g., "codegen/emit", "sema", "cfg_builder").
    pub phase: Option<String>,
    /// Additional context-specific details.
    ///
    /// This can include things like:
    /// - Current function being compiled
    /// - Instruction that triggered the ICE
    /// - Type information
    /// - Any other relevant state
    pub details: Vec<(String, String)>,
    /// Backtrace captured at the ICE site.
    pub backtrace: Option<Backtrace>,
}

impl IceContext {
    /// Create a new ICE context with the given error message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            version: None,
            target: None,
            phase: None,
            details: Vec::new(),
            backtrace: None,
        }
    }

    /// Set the compiler version.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Set the target architecture.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Set the compilation phase.
    pub fn with_phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = Some(phase.into());
        self
    }

    /// Add a detail key-value pair.
    ///
    /// Details provide context-specific information about the compiler state.
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.push((key.into(), value.into()));
        self
    }

    /// Capture a backtrace at the current call site.
    ///
    /// This should be called at the point where the ICE is detected to capture
    /// the most relevant stack trace.
    pub fn with_backtrace(mut self) -> Self {
        self.backtrace = Some(Backtrace::capture());
        self
    }

    /// Format the ICE context for display.
    ///
    /// This produces a user-friendly representation suitable for error messages.
    pub fn format_details(&self) -> String {
        let mut output = String::new();

        if let Some(version) = &self.version {
            output.push_str(&format!("  rue version: {}\n", version));
        }

        if let Some(target) = &self.target {
            output.push_str(&format!("  target: {}\n", target));
        }

        if let Some(phase) = &self.phase {
            output.push_str(&format!("  phase: {}\n", phase));
        }

        if !self.details.is_empty() {
            output.push_str("\n  relevant state:\n");
            for (key, value) in &self.details {
                output.push_str(&format!("    {}: {}\n", key, value));
            }
        }

        output
    }

    /// Format the backtrace for display.
    ///
    /// Returns a formatted backtrace if one was captured, or None otherwise.
    /// The backtrace is formatted with frame numbers and source locations.
    pub fn format_backtrace(&self) -> Option<String> {
        self.backtrace.as_ref().map(|bt| {
            let bt_str = format!("{}", bt);
            if bt_str.trim().is_empty() || bt_str.contains("disabled") {
                // Backtrace capture is disabled
                "  (backtrace capture disabled; set RUST_BACKTRACE=1 to enable)".to_string()
            } else {
                // Format each frame with indentation
                bt_str
                    .lines()
                    .map(|line| format!("  {}", line))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        })
    }
}

impl fmt::Display for IceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "internal compiler error: {}", self.message)?;

        let has_details = self.version.is_some()
            || self.target.is_some()
            || self.phase.is_some()
            || !self.details.is_empty();

        if has_details {
            write!(f, "\n\ndebug info:\n{}", self.format_details())?;
        }

        if let Some(backtrace) = self.format_backtrace() {
            if has_details {
                write!(f, "\n")?;
            } else {
                write!(f, "\n\n")?;
            }
            write!(f, "backtrace:\n{}", backtrace)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ice_context_new() {
        let ice = IceContext::new("test error");
        assert_eq!(ice.message, "test error");
        assert!(ice.version.is_none());
        assert!(ice.target.is_none());
        assert!(ice.phase.is_none());
        assert!(ice.details.is_empty());
    }

    #[test]
    fn test_ice_context_with_version() {
        let ice = IceContext::new("test error").with_version("0.1.0");
        assert_eq!(ice.version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn test_ice_context_with_target() {
        let ice = IceContext::new("test error").with_target("x86_64-unknown-linux-gnu");
        assert_eq!(ice.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn test_ice_context_with_phase() {
        let ice = IceContext::new("test error").with_phase("codegen/emit");
        assert_eq!(ice.phase.as_deref(), Some("codegen/emit"));
    }

    #[test]
    fn test_ice_context_with_detail() {
        let ice = IceContext::new("test error")
            .with_detail("current_function", "main")
            .with_detail("instruction", "Call");
        assert_eq!(ice.details.len(), 2);
        assert_eq!(
            ice.details[0],
            ("current_function".to_string(), "main".to_string())
        );
        assert_eq!(
            ice.details[1],
            ("instruction".to_string(), "Call".to_string())
        );
    }

    #[test]
    fn test_ice_context_builder_chain() {
        let ice = IceContext::new("unexpected type")
            .with_version("0.1.0")
            .with_target("x86_64-unknown-linux-gnu")
            .with_phase("codegen/emit")
            .with_detail("function", "main")
            .with_detail("instruction", "Call");

        assert_eq!(ice.message, "unexpected type");
        assert_eq!(ice.version.as_deref(), Some("0.1.0"));
        assert_eq!(ice.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(ice.phase.as_deref(), Some("codegen/emit"));
        assert_eq!(ice.details.len(), 2);
    }

    #[test]
    fn test_ice_context_format_details_minimal() {
        let ice = IceContext::new("test error");
        let formatted = ice.format_details();
        assert_eq!(formatted, "");
    }

    #[test]
    fn test_ice_context_format_details_with_version() {
        let ice = IceContext::new("test error").with_version("0.1.0");
        let formatted = ice.format_details();
        assert!(formatted.contains("rue version: 0.1.0"));
    }

    #[test]
    fn test_ice_context_format_details_full() {
        let ice = IceContext::new("test error")
            .with_version("0.1.0")
            .with_target("x86_64-unknown-linux-gnu")
            .with_phase("codegen")
            .with_detail("function", "main");

        let formatted = ice.format_details();
        assert!(formatted.contains("rue version: 0.1.0"));
        assert!(formatted.contains("target: x86_64-unknown-linux-gnu"));
        assert!(formatted.contains("phase: codegen"));
        assert!(formatted.contains("relevant state:"));
        assert!(formatted.contains("function: main"));
    }

    #[test]
    fn test_ice_context_display_minimal() {
        let ice = IceContext::new("test error");
        assert_eq!(ice.to_string(), "internal compiler error: test error");
    }

    #[test]
    fn test_ice_context_display_with_details() {
        let ice = IceContext::new("test error")
            .with_version("0.1.0")
            .with_phase("codegen");

        let output = ice.to_string();
        assert!(output.contains("internal compiler error: test error"));
        assert!(output.contains("debug info:"));
        assert!(output.contains("rue version: 0.1.0"));
        assert!(output.contains("phase: codegen"));
    }

    #[test]
    fn test_ice_context_with_backtrace() {
        let ice = IceContext::new("test error").with_backtrace();
        assert!(ice.backtrace.is_some());
    }

    #[test]
    fn test_ice_context_format_backtrace_when_none() {
        let ice = IceContext::new("test error");
        assert!(ice.format_backtrace().is_none());
    }

    #[test]
    fn test_ice_context_format_backtrace_when_captured() {
        let ice = IceContext::new("test error").with_backtrace();
        let formatted = ice.format_backtrace();
        assert!(formatted.is_some());
        // The backtrace should either contain actual frames or the disabled message
        let bt_str = formatted.unwrap();
        assert!(bt_str.contains("backtrace capture disabled") || bt_str.len() > 0);
    }

    #[test]
    fn test_ice_context_display_with_backtrace() {
        let ice = IceContext::new("test error")
            .with_version("0.1.0")
            .with_backtrace();

        let output = ice.to_string();
        assert!(output.contains("internal compiler error: test error"));
        assert!(output.contains("backtrace:"));
    }

    #[test]
    fn test_ice_context_full_builder() {
        // Test the full builder chain with backtrace
        let ice = IceContext::new("unexpected type")
            .with_version("0.1.0")
            .with_target("x86_64-unknown-linux-gnu")
            .with_phase("codegen/emit")
            .with_detail("function", "main")
            .with_backtrace();

        assert_eq!(ice.message, "unexpected type");
        assert!(ice.version.is_some());
        assert!(ice.target.is_some());
        assert!(ice.phase.is_some());
        assert_eq!(ice.details.len(), 1);
        assert!(ice.backtrace.is_some());
    }
}
