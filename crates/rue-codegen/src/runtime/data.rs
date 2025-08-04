//! Static data section groundwork

use crate::runtime::context::RuntimeContext;
use rue_target::{LabelRef, X8664Instr};

impl RuntimeContext {
    /// Generate data section with static strings
    pub fn generate_data_section(&mut self) {
        // Jump over data section
        let data_end_label = self.new_label();
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(data_end_label),
        });

        // Newline character
        let newline_label = self.define_label("__rue_newline");
        self.instructions
            .push(X8664Instr::Label { id: newline_label });
        // Store raw byte for newline (0x0A)
        self.instructions
            .push(X8664Instr::DataBytes { bytes: vec![0x0A] });

        // "true" string
        let true_str_label = self.define_label("__rue_true_str");
        self.instructions
            .push(X8664Instr::Label { id: true_str_label });
        // Store "true" as 4 bytes: 't' 'r' 'u' 'e'
        self.instructions.push(X8664Instr::DataBytes {
            bytes: b"true".to_vec(),
        });

        // "false" string
        let false_str_label = self.define_label("__rue_false_str");
        self.instructions.push(X8664Instr::Label {
            id: false_str_label,
        });
        // Store "false" as 5 bytes: 'f' 'a' 'l' 's' 'e'
        self.instructions.push(X8664Instr::DataBytes {
            bytes: b"false".to_vec(),
        });

        // "()" string
        let unit_str_label = self.define_label("__rue_unit_str");
        self.instructions
            .push(X8664Instr::Label { id: unit_str_label });
        self.instructions.push(X8664Instr::DataBytes {
            bytes: b"()".to_vec(),
        });

        // Buffer space (1024 bytes for input)
        let input_buffer_label = self.define_label("__rue_input_buffer");
        self.instructions.push(X8664Instr::Label {
            id: input_buffer_label,
        });
        // Reserve 1024 bytes for input buffer
        self.instructions
            .push(X8664Instr::ReserveBytes { count: 1024 });

        // End of data section
        self.instructions
            .push(X8664Instr::Label { id: data_end_label });
    }
}
