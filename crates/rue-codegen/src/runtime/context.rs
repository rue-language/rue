//! Runtime context and helper methods

use rue_target::X8664Instr;
use std::collections::HashMap;

/// Context for generating runtime functions
pub struct RuntimeContext {
    pub instructions: Vec<X8664Instr>,
    pub labels: HashMap<String, u32>,
    pub label_counter: u32,
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeContext {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            labels: HashMap::new(),
            label_counter: 0,
        }
    }

    pub fn new_label(&mut self) -> u32 {
        let id = self.label_counter;
        self.label_counter += 1;
        id
    }

    pub fn define_label(&mut self, name: &str) -> u32 {
        let label = self.new_label();
        self.labels.insert(name.to_string(), label);
        label
    }
}
