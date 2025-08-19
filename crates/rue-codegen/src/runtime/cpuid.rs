//! CPU feature detection for runtime dispatch
//!
//! This module implements CPUID-based feature detection to enable
//! optimized code paths on modern CPUs. Currently detects:
//! - ERMS (Enhanced REP MOVSB/STOSB) for fast memory operations

use crate::runtime::context::RuntimeContext;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};

impl RuntimeContext {
    /// Generate CPU feature detection and vtable initialization
    pub fn generate_cpu_feature_detection(&mut self) {
        let detect_label = self.define_label("__rue_detect_cpu_features");
        let has_erms = self.new_label();
        let no_erms = self.new_label();
        let done = self.new_label();

        self.instructions
            .push(X8664Instr::Label { id: detect_label });

        // Save registers that CPUID clobbers
        self.instructions.push(X8664Instr::Push {
            reg: X86Register::Rbx,
        });

        // Check for ERMS support
        // CPUID with EAX=7, ECX=0 returns feature flags in EBX
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 7,
        });
        self.instructions.push(X8664Instr::XorRR {
            dest: X86Register::Rcx,
            src: X86Register::Rcx,
        });
        self.instructions.push(X8664Instr::Cpuid);

        // Check bit 9 of EBX for ERMS support
        self.instructions.push(X8664Instr::BtRI {
            reg: X86Register::Rbx,
            bit: 9,
        });
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Below, // CF=1 means bit was set
            target: LabelRef::Local(has_erms),
        });

        // No ERMS - use baseline implementations
        self.instructions.push(X8664Instr::Label { id: no_erms });

        // Set memcpy_ptr to baseline
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memcpy".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memcpy_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Set memmove_ptr to baseline
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memmove".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memmove_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Set memset_ptr to baseline (memzero)
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memzero".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memset_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Set memzero_ptr to baseline
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memzero".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memzero_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Store CPU features (0 = no ERMS)
        self.instructions.push(X8664Instr::XorRR {
            dest: X86Register::Rax,
            src: X86Register::Rax,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_cpu_features".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Local(done),
        });

        // Has ERMS - use optimized implementations
        self.instructions.push(X8664Instr::Label { id: has_erms });

        // Set memcpy_ptr to ERMS version
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memcpy_erms".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memcpy_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Set memmove_ptr to ERMS version
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memmove_erms".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memmove_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Set memset_ptr to ERMS version
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memset_erms".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memset_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Set memzero_ptr to ERMS version
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rax,
            label: "__rue_memzero_erms".to_string(),
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_memzero_ptr".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Store CPU features (1 = ERMS)
        self.instructions.push(X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 1,
        });
        self.instructions.push(X8664Instr::LeaLabel {
            dest: X86Register::Rdx,
            label: "__rue_cpu_features".to_string(),
        });
        self.instructions.push(X8664Instr::MovMR {
            base: X86Register::Rdx,
            offset: 0,
            src: X86Register::Rax,
        });

        // Done
        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Pop {
            reg: X86Register::Rbx,
        });
        self.instructions.push(X8664Instr::Ret);
    }

    /// Generate stub ERMS functions (temporary until we have assembly)
    pub fn generate_erms_stubs(&mut self) {
        // Generate memcpy_erms stub - just uses rep movsb
        let memcpy_erms_label = self.define_label("__rue_memcpy_erms");
        self.instructions.push(X8664Instr::Label {
            id: memcpy_erms_label,
        });

        // Check if len == 0
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 0,
        });
        let done = self.new_label();
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done),
        });

        // Use rep movsb for all sizes (ERMS makes this fast)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::RepMovsb);

        self.instructions.push(X8664Instr::Label { id: done });
        self.instructions.push(X8664Instr::Ret);

        // Generate memmove_erms stub - for now, just call baseline
        let memmove_erms_label = self.define_label("__rue_memmove_erms");
        self.instructions.push(X8664Instr::Label {
            id: memmove_erms_label,
        });
        // For now, just jump to baseline implementation
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Global("__rue_memmove".to_string()),
        });

        // Generate memset_erms stub - uses rep stosb
        let memset_erms_label = self.define_label("__rue_memset_erms");
        self.instructions.push(X8664Instr::Label {
            id: memset_erms_label,
        });

        // Parameters: RDI = dest, RSI = byte value, RDX = len
        // For memzero, RSI will be 0

        // Check if len == 0
        self.instructions.push(X8664Instr::CmpRI {
            reg: X86Register::Rdx,
            imm: 0,
        });
        let done2 = self.new_label();
        self.instructions.push(X8664Instr::JmpCC {
            cc: ConditionCode::Equal,
            target: LabelRef::Local(done2),
        });

        // Move byte value to AL
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rax,
            src: X86Register::Rsi,
        });

        // Use rep stosb for all sizes (ERMS makes this fast)
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rcx,
            src: X86Register::Rdx,
        });
        self.instructions.push(X8664Instr::Cld);
        self.instructions.push(X8664Instr::RepStosb);

        self.instructions.push(X8664Instr::Label { id: done2 });
        self.instructions.push(X8664Instr::Ret);

        // Generate memzero_erms stub - wrapper around memset_erms with value=0
        // This matches the calling convention of __rue_memzero: (dest, size) in RDI, RSI
        let memzero_erms_label = self.define_label("__rue_memzero_erms");
        self.instructions.push(X8664Instr::Label {
            id: memzero_erms_label,
        });

        // Parameters: RDI = dest, RSI = size
        // Need to set up for memset_erms: RDI = dest, RSI = 0, RDX = size

        // Move size from RSI to RDX
        self.instructions.push(X8664Instr::MovRR {
            dest: X86Register::Rdx,
            src: X86Register::Rsi,
        });

        // Set RSI to 0 (the byte value to set)
        self.instructions.push(X8664Instr::XorRR {
            dest: X86Register::Rsi,
            src: X86Register::Rsi,
        });

        // Jump to memset_erms (tail call)
        self.instructions.push(X8664Instr::Jmp {
            target: LabelRef::Global("__rue_memset_erms".to_string()),
        });
    }
}
