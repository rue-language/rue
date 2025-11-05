use crate::CodegenError;
use rue_target::X8664Instr;
use std::collections::HashMap;

/// RuntimeProvider handles runtime code generation and provides runtime instructions and labels.
/// This struct is focused solely on runtime code generation, not compilation orchestration.
pub struct RuntimeProvider {
    runtime_instructions: Vec<X8664Instr>,
    runtime_labels: HashMap<String, u32>,
}

impl RuntimeProvider {
    /// Create a new RuntimeProvider with runtime instructions loaded
    ///
    /// If `use_rust_runtime` is true, returns empty vectors since runtime functions
    /// will be provided by linking with librue_runtime.a instead.
    pub fn new(use_rust_runtime: bool) -> Result<Self, CodegenError> {
        if use_rust_runtime {
            // When using Rust runtime, we don't generate runtime instructions
            // The runtime functions will be provided by linking with librue_runtime.a
            Ok(Self {
                runtime_instructions: Vec::new(),
                runtime_labels: HashMap::new(),
            })
        } else {
            // Generate runtime instructions as before
            let (runtime_instructions, runtime_labels) =
                crate::runtime::generate_runtime().map_err(|_| CodegenError::Io)?;
            Ok(Self {
                runtime_instructions,
                runtime_labels,
            })
        }
    }

    /// Get the runtime label count
    pub fn runtime_label_count(&self) -> u32 {
        self.runtime_labels.values().max().copied().unwrap_or(0) + 1
    }

    /// Get a reference to the runtime instructions
    pub fn runtime_instructions(&self) -> &[X8664Instr] {
        &self.runtime_instructions
    }

    /// Get a reference to the runtime labels
    pub fn runtime_labels(&self) -> &HashMap<String, u32> {
        &self.runtime_labels
    }

    /// Combine runtime and user code
    pub fn combine_runtime_and_user_code(
        &self,
        user_instructions: Vec<X8664Instr>,
    ) -> Vec<X8664Instr> {
        let mut final_instructions = Vec::new();

        // Add runtime instructions first
        final_instructions.extend(self.runtime_instructions.clone());

        // Add user instructions (labels are already correctly offset)
        final_instructions.extend(user_instructions);

        final_instructions
    }
}
