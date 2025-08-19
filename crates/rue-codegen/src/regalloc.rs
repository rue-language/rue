//! A correct "spill everything" register allocator with proper state tracking
//!
//! This allocator implements the simplest possible correct allocation strategy:
//! - Every VReg gets a permanent home slot on the stack
//! - Values are loaded into registers only when needed
//! - Values are stored back immediately after use
//! - No values persist in registers between instructions
//! - Proper tracking prevents pending-store clobbering
//!
//! This generates inefficient code but is guaranteed to be correct.
//!
//! ## Contract with Lowering Layer
//!
//! The allocator guarantees:
//! - Every Dirty value is spilled before its register is reused or clobbered
//! - No load from an uninitialized stack slot (tracks initialized_vregs)
//! - Stack size is 16-byte aligned (SysV ABI requirement)
//! - Scratch registers don't clash with special-purpose registers
//! - 8-bit capable registers are available for SetCC operations
//!
//! The lowering layer must:
//! - Call `emit_spill_reload_ops()` whenever requesting/invalidating a scratch reg
//! - Call `mark_vreg_initialized()` when emitting MovMR stores
//! - Handle caller-saved register preservation around function calls

use rue_ir::pir::VReg;
use rue_target::{X86Register, X8664Instr};
use std::collections::HashMap;
use tracing::{debug, trace};

/// Size of a stack slot in bytes
const SLOT_SIZE: i64 = 8;

/// Stack alignment requirement (SysV ABI)
const STACK_ALIGNMENT: i64 = 16;

/// State of a register - Empty, Clean (loaded but not modified), or Dirty (modified)
#[derive(Clone, PartialEq, Eq)]
enum RegState {
    /// X86Register contains no useful value
    Empty,
    /// X86Register contains a VReg value that matches memory (read-only)
    Clean(VReg),
    /// X86Register contains a VReg value that has been modified (needs spill)
    Dirty(VReg),
}

impl std::fmt::Debug for RegState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegState::Empty => write!(f, "Empty"),
            RegState::Clean(vreg) => write!(f, "Clean({vreg:?})"),
            RegState::Dirty(vreg) => write!(f, "Dirty({vreg:?})"),
        }
    }
}

/// State of a scratch register
#[derive(Debug, Clone)]
struct ScratchState {
    /// The register
    reg: X86Register,
    /// Current state of the register
    state: RegState,
}

/// A "spill everything" register allocator with proper state tracking
///
/// This allocator ensures correctness by:
/// - Tracking which registers contain dirty values
/// - Preventing register reuse before spilling
/// - Maintaining proper stack alignment
/// - Supporting variable-sized values
pub struct RegisterAllocator {
    /// Home stack slot for each VReg (allocated on first use)
    home_slots: HashMap<VReg, i64>,
    /// Current stack allocation pointer (grows downward)
    /// Using i64 to prevent overflow with large functions
    stack_offset: i64,
    /// Scratch register states
    scratch_states: Vec<ScratchState>,
    /// Pending operations to emit
    pending_ops: Vec<X8664Instr>,
    /// Track which VRegs have been initialized (have valid values in their home slots)
    /// This prevents spurious loads from uninitialized stack slots
    initialized_vregs: std::collections::HashSet<VReg>,
}

impl RegisterAllocator {
    /// Create a new allocator for a function
    pub fn new() -> Self {
        // Initialize scratch register states
        // CRITICAL: Only use caller-saved registers that don't conflict with special operations:
        // - RAX is used for division results and syscall returns
        // - RCX is used for shift counts and division
        // - RDX is used for division high bits and syscall args
        // - R15 is callee-saved (excluded to avoid save/restore overhead)
        // Using: R10, R11 (both caller-saved and available)
        // Could also use: R8, R9 (but they're often used for function args)
        let scratch_states = vec![
            ScratchState {
                reg: X86Register::R10,
                state: RegState::Empty,
            },
            ScratchState {
                reg: X86Register::R11,
                state: RegState::Empty,
            },
            // Adding more caller-saved registers for better allocation
            ScratchState {
                reg: X86Register::R8,
                state: RegState::Empty,
            },
            ScratchState {
                reg: X86Register::R9,
                state: RegState::Empty,
            },
        ];

        Self {
            home_slots: HashMap::new(),
            stack_offset: 0,
            scratch_states,
            pending_ops: Vec::new(),
            initialized_vregs: std::collections::HashSet::new(),
        }
    }

    /// Set the initial stack offset to avoid overlapping with pre-allocated slots
    /// (e.g., block parameter slots)
    pub fn set_initial_stack_offset(&mut self, offset: i64) {
        debug_assert!(
            offset <= 0,
            "Stack offset must be non-positive (stack grows downward), got {offset}"
        );
        self.stack_offset = offset;
    }

    /// Get or allocate a home slot for a VReg
    /// For now, assumes all values are 8 bytes (will be enhanced later)
    fn get_home_slot(&mut self, vreg: VReg) -> i64 {
        *self.home_slots.entry(vreg).or_insert_with(|| {
            // Allocate 8-byte slot (will be enhanced for variable sizes)
            self.stack_offset -= SLOT_SIZE;
            debug!(
                target: "rue::codegen::regalloc",
                ?vreg,
                offset = self.stack_offset,
                "Allocating home slot for VReg"
            );
            self.stack_offset
        })
    }

    /// Generate a load instruction from memory to register
    fn gen_load(&mut self, vreg: VReg, dest_reg: X86Register) -> X8664Instr {
        let slot = self.get_home_slot(vreg);
        // Ensure the offset fits in an i32
        assert!(
            slot >= i32::MIN as i64 && slot <= i32::MAX as i64,
            "Stack offset {slot} exceeds i32 range"
        );
        X8664Instr::MovRM {
            dest: dest_reg,
            base: X86Register::Rbp,
            offset: slot as i32,
        }
    }

    /// Generate a store instruction from register to memory
    fn gen_store(&mut self, vreg: VReg, src_reg: X86Register) -> X8664Instr {
        let slot = self.get_home_slot(vreg);
        // Ensure the offset fits in an i32
        assert!(
            slot >= i32::MIN as i64 && slot <= i32::MAX as i64,
            "Stack offset {slot} exceeds i32 range"
        );
        // NOTE: VReg is marked as initialized in the lowering layer when the store is actually emitted
        // This prevents race conditions where VRegs are marked initialized before their stores are emitted
        X8664Instr::MovMR {
            src: src_reg,
            base: X86Register::Rbp,
            offset: slot as i32,
        }
    }

    /// Mark a VReg as initialized (called when its store instruction is actually emitted)
    pub fn mark_vreg_initialized(&mut self, vreg: VReg) {
        self.initialized_vregs.insert(vreg);
    }

    /// Get the home slot offset for a VReg (for extracting from MovMR instructions)
    pub fn get_vreg_home_slot(&self, vreg: VReg) -> Option<i64> {
        self.home_slots.get(&vreg).copied()
    }

    /// Find the VReg that corresponds to a given stack offset
    pub fn find_vreg_for_slot(&self, offset: i64) -> Option<VReg> {
        for (&vreg, &slot) in &self.home_slots {
            if slot == offset {
                return Some(vreg);
            }
        }
        None
    }

    /// Get total stack space needed, properly aligned to 16 bytes
    pub fn get_stack_size(&self) -> u32 {
        self.aligned_stack_size()
    }

    /// Get aligned stack frame size (internal helper)
    fn aligned_stack_size(&self) -> u32 {
        // Align to 16 bytes for System V ABI
        let size = -self.stack_offset;
        let aligned = (size + STACK_ALIGNMENT - 1) & !(STACK_ALIGNMENT - 1); // Round up to next 16-byte boundary
        aligned as u32
    }

    /// Spill a dirty value from a register and clear all mappings for that VReg
    fn spill_dirty_value(&mut self, vreg: VReg, reg: X86Register) {
        let spill = self.gen_store(vreg, reg);
        self.pending_ops.push(spill);

        // CRITICAL: Clear any stale mappings for the spilled VReg
        // The VReg we just spilled might be marked as present in other registers too
        // We must clear all those stale mappings to prevent ensure_reg from using them
        for state in &mut self.scratch_states {
            match &state.state {
                RegState::Clean(v) | RegState::Dirty(v) if *v == vreg => {
                    state.state = RegState::Empty;
                }
                _ => {}
            }
        }
    }

    /// Assign a register for defining a VReg with 8-bit operation constraints
    /// This is specifically for operations like SetCC that require 8-bit capable registers
    pub fn assign_reg_for_8bit_def(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String> {
        // Allocate home slot if needed
        self.get_home_slot(vreg);

        // Find an available 8-bit capable scratch register
        let scratch_idx = self.find_8bit_capable_scratch(forbidden)?;

        // Handle existing state in the target register
        match &self.scratch_states[scratch_idx].state {
            RegState::Dirty(existing_vreg) => {
                if existing_vreg != &vreg {
                    // Spill the existing dirty value
                    let existing = *existing_vreg;
                    self.spill_dirty_value(existing, self.scratch_states[scratch_idx].reg);
                }
            }
            _ => {
                // Clean or Empty - no spill needed
            }
        }

        // Mark the register as containing this VReg (dirty because we're about to write to it)
        let reg = self.scratch_states[scratch_idx].reg;
        self.scratch_states[scratch_idx].state = RegState::Dirty(vreg);

        Ok(reg)
    }

    /// Assign a register for defining a VReg (never loads old value)
    /// This is used for phi nodes and other cases where the VReg is being defined, not read
    pub fn assign_reg_for_def(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String> {
        // Allocate home slot if needed
        self.get_home_slot(vreg);

        // DO NOT mark as initialized here! VRegs should only be marked as initialized
        // when they're actually stored to memory via schedule_store -> gen_store.
        // This fixes the bug where assign_reg_for_def would mark VRegs as initialized
        // before any value was actually written, causing ensure_reg to load garbage.

        // Find an available scratch register
        let scratch_idx = self.find_available_scratch_with_filter(forbidden, None)?;

        // Handle existing state in the target register
        match &self.scratch_states[scratch_idx].state {
            RegState::Dirty(existing_vreg) => {
                if existing_vreg != &vreg {
                    // Spill the existing dirty value
                    let existing = *existing_vreg;
                    self.spill_dirty_value(existing, self.scratch_states[scratch_idx].reg);
                }
            }
            _ => {
                // Clean or Empty - no spill needed
            }
        }

        // Mark the register as containing this VReg (dirty because we're about to write to it)
        let reg = self.scratch_states[scratch_idx].reg;
        self.scratch_states[scratch_idx].state = RegState::Dirty(vreg);

        Ok(reg)
    }

    /// Main API: ensure a VReg is in a register
    pub fn ensure_reg(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String> {
        // First check if the vreg is already in a register (Clean or Dirty)
        for state in &self.scratch_states {
            match &state.state {
                RegState::Clean(v) | RegState::Dirty(v)
                    if *v == vreg && !forbidden.contains(&state.reg) =>
                {
                    // Already in this register, no need to reload
                    trace!(
                        target: "rue::codegen::regalloc",
                        ?vreg,
                        register = ?state.reg,
                        state = ?state.state,
                        "ensure_reg: VReg found in register"
                    );
                    return Ok(state.reg);
                }
                _ => {}
            }
        }

        // Find an available scratch register
        let scratch_idx = self.find_available_scratch_with_filter(forbidden, None)?;

        // Handle existing state in the target register
        match &self.scratch_states[scratch_idx].state {
            RegState::Dirty(existing_vreg) => {
                if existing_vreg != &vreg {
                    // Spill the existing dirty value
                    let existing = *existing_vreg;
                    self.spill_dirty_value(existing, self.scratch_states[scratch_idx].reg);
                }
            }
            RegState::Clean(existing_vreg) => {
                if existing_vreg == &vreg {
                    // The register already contains the vreg we want
                    return Ok(self.scratch_states[scratch_idx].reg);
                }
                // Clean value is being overwritten, no spill needed
            }
            RegState::Empty => {
                // X86Register is empty, no cleanup needed
            }
        }

        // Load the requested value
        let reg = self.scratch_states[scratch_idx].reg;

        // For the simple allocator, we need to be conservative about initialization.
        // Only load from memory if we're absolutely certain the VReg has been stored.
        // This fixes the bug where temporary VRegs for function call arguments
        // (like the result of "n - 1") were loading garbage from uninitialized memory.
        let should_load = self.initialized_vregs.contains(&vreg);

        debug!(
            target: "rue::codegen::regalloc",
            ?vreg,
            initialized = should_load,
            "ensure_reg: VReg initialization status"
        );

        if should_load {
            // Load from home slot
            let load = self.gen_load(vreg, reg);
            self.pending_ops.push(load);
            self.scratch_states[scratch_idx].state = RegState::Clean(vreg);
        } else {
            // VReg has never been initialized, mark register as containing it
            // but don't generate a load from uninitialized memory
            self.scratch_states[scratch_idx].state = RegState::Dirty(vreg);
        }

        Ok(reg)
    }

    /// Schedule a store operation for a VReg value that's currently in a register
    pub fn schedule_store(&mut self, vreg: VReg, reg: X86Register) {
        // Check if this is one of our scratch registers
        let mut found_idx = None;
        for (idx, state) in self.scratch_states.iter().enumerate() {
            if state.reg == reg {
                found_idx = Some(idx);
                break;
            }
        }

        if let Some(idx) = found_idx {
            // Handle existing state in the register
            match &self.scratch_states[idx].state {
                RegState::Dirty(old_vreg) if *old_vreg != vreg => {
                    // Spill the old dirty value before overwriting
                    let store = self.gen_store(*old_vreg, reg);
                    self.pending_ops.push(store);
                }
                _ => {
                    // Either Empty, Clean, or already Dirty with same vreg - no spill needed
                }
            }
            // Mark the register as dirty with the new vreg
            self.scratch_states[idx].state = RegState::Dirty(vreg);
        } else {
            // Not a scratch register - spill immediately
            let store = self.gen_store(vreg, reg);
            self.pending_ops.push(store);
        }
    }

    /// Flush all pending stores
    pub fn flush_stores(&mut self) {
        // Collect dirty registers first to avoid borrow checker issues
        let dirty_regs: Vec<(VReg, X86Register)> = self
            .scratch_states
            .iter_mut()
            .filter_map(|state| {
                match std::mem::replace(&mut state.state, RegState::Empty) {
                    RegState::Dirty(vreg) => Some((vreg, state.reg)),
                    other_state => {
                        // Restore non-dirty state
                        state.state = other_state;
                        None
                    }
                }
            })
            .collect();

        // Generate stores for all dirty registers
        for (vreg, reg) in dirty_regs {
            let store = self.gen_store(vreg, reg);
            self.pending_ops.push(store);
        }
    }

    /// Flush all pending stores except for the specified registers
    pub fn flush_stores_except(&mut self, except: &[X86Register]) {
        // Collect dirty registers first to avoid borrow checker issues
        let dirty_regs: Vec<(VReg, X86Register)> = self
            .scratch_states
            .iter_mut()
            .filter_map(|state| {
                if except.contains(&state.reg) {
                    // Skip registers in the exception list
                    None
                } else {
                    match std::mem::replace(&mut state.state, RegState::Empty) {
                        RegState::Dirty(vreg) => Some((vreg, state.reg)),
                        other_state => {
                            // Restore non-dirty state
                            state.state = other_state;
                            None
                        }
                    }
                }
            })
            .collect();

        // Generate stores for all dirty registers (except the exceptions)
        for (vreg, reg) in dirty_regs {
            let store = self.gen_store(vreg, reg);
            self.pending_ops.push(store);
        }

        // CRITICAL: Mark excepted dirty registers as Clean after the flush
        // This prevents them from being stored again in a later flush_stores()
        for state in &mut self.scratch_states {
            if except.contains(&state.reg)
                && let RegState::Dirty(vreg) = state.state
            {
                state.state = RegState::Clean(vreg);
            }
        }
    }

    /// Find an available scratch register with optional constraint filter
    fn find_available_scratch_with_filter(
        &self,
        forbidden: &[X86Register],
        filter: Option<fn(&X86Register) -> bool>,
    ) -> Result<usize, String> {
        // First, try to find an empty register
        for (idx, state) in self.scratch_states.iter().enumerate() {
            if matches!(state.state, RegState::Empty)
                && !forbidden.contains(&state.reg)
                && filter.is_none_or(|f| f(&state.reg))
            {
                return Ok(idx);
            }
        }

        // Then try to find a clean register (can be overwritten without spill)
        for (idx, state) in self.scratch_states.iter().enumerate() {
            if matches!(state.state, RegState::Clean(_))
                && !forbidden.contains(&state.reg)
                && filter.is_none_or(|f| f(&state.reg))
            {
                return Ok(idx);
            }
        }

        // Finally, find a dirty register that's not forbidden
        for (idx, state) in self.scratch_states.iter().enumerate() {
            if !forbidden.contains(&state.reg) && filter.is_none_or(|f| f(&state.reg)) {
                return Ok(idx);
            }
        }

        Err("No available registers matching constraints".to_string())
    }

    /// Find an available scratch register suitable for 8-bit operations
    fn find_8bit_capable_scratch(&self, forbidden: &[X86Register]) -> Result<usize, String> {
        self.find_available_scratch_with_filter(
            forbidden,
            Some(|reg| {
                // Only these registers have 8-bit encodings in x86-64:
                // - Classic x86 registers: Rax, Rbx, Rcx, Rdx, Rsi, Rdi
                // - Extended registers: R8-R13 (but not R14, R15)
                // We only have R8-R11 in our scratch pool, all of which support 8-bit ops
                matches!(
                    reg,
                    X86Register::Rax
                        | X86Register::Rbx
                        | X86Register::Rcx
                        | X86Register::Rdx
                        | X86Register::Rsi
                        | X86Register::Rdi
                        | X86Register::R8
                        | X86Register::R9
                        | X86Register::R10
                        | X86Register::R11
                        | X86Register::R12
                        | X86Register::R13
                )
            }),
        )
    }

    /// Take any pending spill/reload operations
    pub fn take_pending_ops(&mut self) -> Vec<X8664Instr> {
        std::mem::take(&mut self.pending_ops)
    }

    /// Reset for a new function
    pub fn reset_for_new_function(&mut self) {
        self.home_slots.clear();
        self.stack_offset = 0;
        self.pending_ops.clear();
        self.initialized_vregs.clear();
        for state in &mut self.scratch_states {
            state.state = RegState::Empty;
        }
    }

    /// Clear all register state but keep home slots (used at control flow joins)
    pub fn clear_all_registers(&mut self) {
        // First, flush any dirty values to memory
        self.flush_stores();

        // Then clear all register states
        for state in &mut self.scratch_states {
            state.state = RegState::Empty;
        }
    }

    /// Check if a register contains a value (dirty or clean)
    /// Returns true if the register contains a Clean or Dirty VReg
    /// This is crucial for lower_call's caller-saved register detection
    pub fn is_register_allocated(&self, reg: X86Register) -> bool {
        self.scratch_states
            .iter()
            .any(|s| s.reg == reg && matches!(s.state, RegState::Clean(_) | RegState::Dirty(_)))
    }

    /// Check if a register contains any vreg (for get_scratch_register safety)
    pub fn is_scratch_register(&self, reg: X86Register) -> bool {
        self.scratch_states.iter().any(|s| s.reg == reg)
    }

    /// Invalidate a register - forces spill if dirty
    pub fn invalidate_register(&mut self, reg: X86Register) {
        if let Some(state) = self.scratch_states.iter_mut().find(|s| s.reg == reg) {
            match std::mem::replace(&mut state.state, RegState::Empty) {
                RegState::Dirty(vreg) => {
                    let store = self.gen_store(vreg, reg);
                    self.pending_ops.push(store);
                    // CRITICAL FIX: Mark VReg as initialized immediately when we spill it
                    // This ensures that subsequent ensure_reg calls will reload from memory
                    self.initialized_vregs.insert(vreg);
                }
                RegState::Clean(vreg) => {
                    // CRITICAL FIX: Even clean values need to be marked as initialized
                    // when their register is invalidated, so they can be reloaded later
                    self.initialized_vregs.insert(vreg);
                }
                RegState::Empty => {
                    // Empty register - no spill needed
                }
            }
        }
    }

    /// Get register containing a VReg
    pub fn get_register(&self, vreg: VReg) -> Option<X86Register> {
        // Check if the vreg is currently in any register
        for state in &self.scratch_states {
            match &state.state {
                RegState::Clean(v) | RegState::Dirty(v) if *v == vreg => {
                    return Some(state.reg);
                }
                _ => {}
            }
        }
        None
    }

    /// Get the VReg currently stored in a register (inverse of get_register)
    pub fn get_vreg_in_register(&self, reg: X86Register) -> Option<VReg> {
        for state in &self.scratch_states {
            if state.reg == reg {
                match &state.state {
                    RegState::Clean(vreg) | RegState::Dirty(vreg) => {
                        return Some(*vreg);
                    }
                    RegState::Empty => {
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Restore a VReg-to-register mapping after a pop operation
    /// This tells the allocator that a VReg is now back in a register after being restored from stack
    pub fn restore_vreg_mapping(&mut self, vreg: VReg, reg: X86Register) {
        // Find the scratch register state for this register
        for state in &mut self.scratch_states {
            if state.reg == reg {
                // Mark the register as containing the VReg in clean state
                // (clean because it matches what's in memory)
                state.state = RegState::Clean(vreg);
                break;
            }
        }
    }

    /// Handle clobbered registers by spilling any dirty values they contain
    ///
    /// This is called before instructions that clobber specific registers
    /// (e.g., division clobbers RDX, function calls clobber caller-saved regs)
    pub fn handle_clobbers(&mut self, clobbered_regs: &[X86Register]) {
        for &clobbered_reg in clobbered_regs {
            // Find if this register is one of our scratch registers and has a dirty value
            for state in &mut self.scratch_states {
                if state.reg == clobbered_reg {
                    match std::mem::replace(&mut state.state, RegState::Empty) {
                        RegState::Dirty(vreg) => {
                            // Spill the dirty value before it gets clobbered
                            let store = self.gen_store(vreg, clobbered_reg);
                            self.pending_ops.push(store);
                        }
                        _ => {
                            // Clean or Empty register - no spill needed, just mark as empty
                        }
                    }
                    break;
                }
            }
        }
    }

    /// Get a scratch register that's not in the reserved set
    /// This is useful for instructions that require specific registers
    ///
    /// **WARNING**: The returned register may contain a Dirty value!
    /// The caller MUST call `invalidate_register()` on the returned register
    /// immediately after getting it to ensure any dirty value is spilled.
    pub fn get_unreserved_scratch(&self, reserved: &[X86Register]) -> Option<X86Register> {
        for state in &self.scratch_states {
            if !reserved.contains(&state.reg) {
                return Some(state.reg);
            }
        }
        None
    }

    /// Get the total stack space used (before alignment)
    pub fn get_stack_usage(&self) -> i32 {
        (-self.stack_offset) as i32
    }

    /// Get the aligned stack frame size in bytes
    pub fn stack_size_bytes(&self) -> u32 {
        self.get_stack_size()
    }
}

impl Default for RegisterAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// Implement the RegisterAllocator trait from x86_64_backend
impl crate::x86_64_backend::RegisterAllocator for RegisterAllocator {
    fn ensure_reg(&mut self, vreg: VReg, forbidden: &[X86Register]) -> Result<X86Register, String> {
        self.ensure_reg(vreg, forbidden)
    }

    fn assign_reg_for_def(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String> {
        self.assign_reg_for_def(vreg, forbidden)
    }

    fn assign_reg_for_8bit_def(
        &mut self,
        vreg: VReg,
        forbidden: &[X86Register],
    ) -> Result<X86Register, String> {
        self.assign_reg_for_8bit_def(vreg, forbidden)
    }

    fn get_register(&self, vreg: VReg) -> Option<X86Register> {
        self.get_register(vreg)
    }

    fn is_register_allocated(&self, reg: X86Register) -> bool {
        self.is_register_allocated(reg)
    }

    fn is_scratch_register(&self, reg: X86Register) -> bool {
        self.is_scratch_register(reg)
    }

    fn get_unreserved_scratch(&mut self, forbidden: &[X86Register]) -> Option<X86Register> {
        RegisterAllocator::get_unreserved_scratch(self, forbidden)
    }

    fn invalidate_register(&mut self, reg: X86Register) {
        self.invalidate_register(reg)
    }

    fn schedule_store(&mut self, vreg: VReg, reg: X86Register) {
        self.schedule_store(vreg, reg)
    }

    fn take_pending_ops(&mut self) -> Vec<X8664Instr> {
        self.take_pending_ops()
    }

    fn flush_stores(&mut self) {
        self.flush_stores()
    }

    fn clear_all_registers(&mut self) {
        self.clear_all_registers()
    }

    fn find_vreg_for_slot(&self, offset: i64) -> Option<VReg> {
        self.find_vreg_for_slot(offset)
    }

    fn mark_vreg_initialized(&mut self, vreg: VReg) {
        self.mark_vreg_initialized(vreg)
    }

    fn handle_clobbers(&mut self, clobbered_regs: &[X86Register]) {
        self.handle_clobbers(clobbered_regs)
    }

    fn get_stack_size(&self) -> u64 {
        self.get_stack_usage() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_home_slot_allocation() {
        let mut alloc = RegisterAllocator::new();

        // First vreg gets slot -8
        let slot1 = alloc.get_home_slot(VReg(1));
        assert_eq!(slot1, -8);

        // Second vreg gets slot -16
        let slot2 = alloc.get_home_slot(VReg(2));
        assert_eq!(slot2, -16);

        // Requesting same vreg returns same slot
        let slot1_again = alloc.get_home_slot(VReg(1));
        assert_eq!(slot1_again, -8);

        // Stack size should be 16 bytes, already aligned
        assert_eq!(alloc.get_stack_size(), 16);
    }

    #[test]
    fn test_no_pending_store_clobbering() {
        let mut alloc = RegisterAllocator::new();

        // Ensure VReg(1) gets loaded to first scratch register (R10)
        let r1 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        assert_eq!(r1, X86Register::R10);

        // Schedule store of VReg(1) from RAX
        alloc.schedule_store(VReg(1), r1);

        // Now try to load VReg(2) - the allocator should avoid R10 since it's dirty
        let r2 = alloc.ensure_reg(VReg(2), &[]).unwrap();

        // The allocator should have chosen a different register
        assert_ne!(r2, r1, "Allocator should not reuse dirty register");

        // Check pending ops - should have no ops (both VRegs are uninitialized)
        let ops = alloc.take_pending_ops();
        assert_eq!(
            ops.len(),
            0,
            "Should not generate loads for uninitialized VRegs"
        );

        // Now if we force the allocator to use all registers, it should spill
        let r3 = alloc.ensure_reg(VReg(3), &[]).unwrap();
        alloc.schedule_store(VReg(3), r3);

        // Force reuse by forbidding other registers
        let forbidden = if r1 == X86Register::R10 {
            vec![X86Register::R11, X86Register::R8, X86Register::R9]
        } else if r1 == X86Register::R11 {
            vec![X86Register::R10, X86Register::R8, X86Register::R9]
        } else if r1 == X86Register::R8 {
            vec![X86Register::R10, X86Register::R11, X86Register::R9]
        } else {
            vec![X86Register::R10, X86Register::R11, X86Register::R8]
        };

        // This should force a spill
        let _r4 = alloc.ensure_reg(VReg(4), &forbidden).unwrap();

        let ops = alloc.take_pending_ops();
        // Should have: spill v1 or v3 (no loads for uninitialized VRegs)
        assert!(!ops.is_empty());

        // Verify we got a spill
        let has_spill = ops.iter().any(|op| matches!(op, X8664Instr::MovMR { .. }));
        assert!(has_spill, "Should have generated a spill");
    }

    #[test]
    fn test_stack_alignment() {
        let mut alloc = RegisterAllocator::new();

        // Allocate odd number of slots
        alloc.get_home_slot(VReg(1));
        alloc.get_home_slot(VReg(2));
        alloc.get_home_slot(VReg(3));

        // Stack size should be 24 bytes (3 * 8)
        // But get_stack_size should return 32 (aligned to 16)
        assert_eq!(alloc.get_stack_size(), 32);
    }

    #[test]
    fn test_forbidden_exhaustion() {
        let mut alloc = RegisterAllocator::new();

        // Forbid all scratch registers
        let forbidden = vec![
            X86Register::R10,
            X86Register::R11,
            X86Register::R8,
            X86Register::R9,
        ];
        let result = alloc.ensure_reg(VReg(1), &forbidden);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No available registers"));
    }

    #[test]
    fn test_flush_clears_dirty_state() {
        let mut alloc = RegisterAllocator::new();

        // Load and mark registers as dirty
        let r1 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        alloc.schedule_store(VReg(1), r1);

        let r2 = alloc.ensure_reg(VReg(2), &[X86Register::Rax]).unwrap();
        alloc.schedule_store(VReg(2), r2);

        // Clear pending ops
        alloc.take_pending_ops();

        // Flush stores
        alloc.flush_stores();

        // Should have exactly 2 stores
        let ops = alloc.take_pending_ops();
        assert_eq!(ops.len(), 2);

        // All registers should be empty now
        for state in &alloc.scratch_states {
            assert!(matches!(state.state, RegState::Empty));
        }
    }

    #[test]
    fn test_load_store_generation() {
        let mut alloc = RegisterAllocator::new();

        // Generate load for VReg(1) to RAX
        let load = alloc.gen_load(VReg(1), X86Register::Rax);
        match load {
            X8664Instr::MovRM { dest, base, offset } => {
                assert_eq!(dest, X86Register::Rax);
                assert_eq!(base, X86Register::Rbp);
                assert_eq!(offset, -8);
            }
            _ => panic!("Expected MovRM instruction"),
        }

        // Generate store from RCX to VReg(2)
        let store = alloc.gen_store(VReg(2), X86Register::Rcx);
        match store {
            X8664Instr::MovMR { src, base, offset } => {
                assert_eq!(src, X86Register::Rcx);
                assert_eq!(base, X86Register::Rbp);
                assert_eq!(offset, -16);
            }
            _ => panic!("Expected MovMR instruction"),
        }
    }

    #[test]
    fn test_ensure_reg() {
        let mut alloc = RegisterAllocator::new();

        // Test ensure_reg for uninitialized VReg
        let reg = alloc.ensure_reg(VReg(1), &[]).unwrap();
        assert!(matches!(
            reg,
            X86Register::R10 | X86Register::R11 | X86Register::R8 | X86Register::R9
        ));

        // Should NOT have generated a load for uninitialized VReg
        let ops = alloc.take_pending_ops();
        assert_eq!(
            ops.len(),
            0,
            "Should not generate load for uninitialized VReg"
        );

        // After scheduling a store, the VReg becomes initialized
        alloc.schedule_store(VReg(1), reg);
        alloc.flush_stores();
        let ops = alloc.take_pending_ops();
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], X8664Instr::MovMR { .. }));

        // Simulate what the lowering layer does when emitting the store
        alloc.mark_vreg_initialized(VReg(1));

        // Now if we ensure_reg again after clearing the register state,
        // it should generate a load because the VReg is initialized
        for state in &mut alloc.scratch_states {
            state.state = RegState::Empty;
        }
        let _reg2 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        let ops = alloc.take_pending_ops();
        assert_eq!(ops.len(), 1, "Should generate load for initialized VReg");
        assert!(matches!(ops[0], X8664Instr::MovRM { .. }));
    }

    #[test]
    fn test_reset() {
        let mut alloc = RegisterAllocator::new();

        // Allocate some slots
        alloc.get_home_slot(VReg(1));
        alloc.get_home_slot(VReg(2));
        assert_eq!(alloc.get_stack_usage(), 16);

        // Reset should clear everything
        alloc.reset_for_new_function();
        assert_eq!(alloc.get_stack_usage(), 0);

        // New allocations should start fresh
        let slot = alloc.get_home_slot(VReg(1));
        assert_eq!(slot, -8);
    }

    #[test]
    fn test_store_flush_order() {
        let mut alloc = RegisterAllocator::new();

        // Schedule multiple stores
        let r1 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        alloc.schedule_store(VReg(1), r1);

        let r2 = alloc.ensure_reg(VReg(2), &[r1]).unwrap();
        alloc.schedule_store(VReg(2), r2);

        let r3 = alloc.ensure_reg(VReg(3), &[r1, r2]).unwrap();
        alloc.schedule_store(VReg(3), r3);

        // Clear initial loads
        alloc.take_pending_ops();

        // Flush stores
        alloc.flush_stores();

        // Get pending operations
        let ops = alloc.take_pending_ops();

        // Should have 3 store operations
        assert_eq!(ops.len(), 3);

        // Verify all are stores
        for op in &ops {
            assert!(matches!(op, X8664Instr::MovMR { .. }));
        }
    }

    #[test]
    fn test_multiple_stores_same_register() {
        let mut alloc = RegisterAllocator::new();

        // This test validates that overwriting a dirty register
        // spills the old value before marking the new one
        let r1 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        alloc.schedule_store(VReg(1), r1);
        alloc.schedule_store(VReg(2), r1); // Should spill VReg(1) first

        // Get ops without flushing yet
        let ops = alloc.take_pending_ops();

        // Should have: spill v1 (no load for uninitialized v1, v2 is still dirty in register)
        assert!(!ops.is_empty());

        // Find the spill of v1
        let mut found_v1_spill = false;
        for op in &ops {
            if let X8664Instr::MovMR { offset, .. } = op
                && *offset == -8
            {
                // VReg(1)'s slot
                found_v1_spill = true;
            }
        }
        assert!(found_v1_spill, "VReg(1) should have been spilled");

        // Now flush remaining stores
        alloc.flush_stores();
        let ops = alloc.take_pending_ops();

        // Should have the store for VReg(2)
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            X8664Instr::MovMR { offset, .. } => {
                assert_eq!(*offset, -16); // VReg(2)'s slot
            }
            _ => panic!("Expected MovMR"),
        }
    }

    #[test]
    fn test_overwrite_without_schedule_store() {
        let mut alloc = RegisterAllocator::new();

        // Load v1 into R10
        let r1 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        assert_eq!(r1, X86Register::R10);
        alloc.schedule_store(VReg(1), r1);

        // Clear pending ops
        alloc.take_pending_ops();

        // Simulate a three-address operation that overwrites R10
        // without calling schedule_store first (e.g., add R10, v2)
        // This would be done by loading v2 and then doing the add
        let r2 = alloc.ensure_reg(VReg(2), &[X86Register::R10]).unwrap();
        assert_ne!(r2, X86Register::R10); // Should get a different register

        // Now force allocation for v3 - this should spill v1 first
        let forbidden_for_v3 = vec![r2, X86Register::R8, X86Register::R9];
        let _r3 = alloc.ensure_reg(VReg(3), &forbidden_for_v3).unwrap();

        let ops = alloc.take_pending_ops();

        // Should have spilled v1 before loading v3
        let mut found_v1_spill = false;
        let mut found_v3_load = false;
        for op in &ops {
            match op {
                X8664Instr::MovMR { offset, .. } if *offset == -8 => {
                    found_v1_spill = true;
                }
                X8664Instr::MovRM { offset, .. } if *offset == -24 => {
                    found_v3_load = true;
                }
                _ => {}
            }
        }

        assert!(found_v1_spill, "VReg(1) should have been spilled");
        // v3 is uninitialized so no load should be generated
        assert!(
            !found_v3_load,
            "VReg(3) should NOT have been loaded (uninitialized)"
        );
    }

    #[test]
    fn test_handle_clobbers() {
        let mut alloc = RegisterAllocator::new();

        // Load values into registers and mark them dirty
        let r1 = alloc.ensure_reg(VReg(1), &[]).unwrap();
        alloc.schedule_store(VReg(1), r1);

        let r2 = alloc.ensure_reg(VReg(2), &[X86Register::R10]).unwrap();
        alloc.schedule_store(VReg(2), r2);

        // Clear pending ops
        alloc.take_pending_ops();

        // Simulate an instruction that clobbers R10 and R11
        let clobbers = vec![X86Register::R10, X86Register::R11];
        alloc.handle_clobbers(&clobbers);

        // Check that dirty values in clobbered registers were spilled
        let ops = alloc.take_pending_ops();

        // Should have at least one spill (for VReg(1) in R10)
        let mut found_spill = false;
        for op in &ops {
            if let X8664Instr::MovMR { .. } = op {
                found_spill = true;
                break;
            }
        }
        assert!(
            found_spill,
            "Should have spilled value in clobbered register"
        );

        // After handling clobbers, the registers should be clean
        assert!(!alloc.is_register_allocated(X86Register::R10));
        if alloc
            .scratch_states
            .iter()
            .any(|s| s.reg == X86Register::R11)
        {
            assert!(!alloc.is_register_allocated(X86Register::R11));
        }
    }

    #[test]
    fn test_get_unreserved_scratch() {
        let alloc = RegisterAllocator::new();

        // Test with no reserved registers
        let scratch = alloc.get_unreserved_scratch(&[]);
        assert!(scratch.is_some());

        // Test with some reserved registers
        let reserved = vec![X86Register::R10, X86Register::R11];
        let scratch = alloc.get_unreserved_scratch(&reserved);
        assert!(scratch.is_some());
        assert!(!reserved.contains(&scratch.unwrap()));

        // Test with all scratch registers reserved
        let all_reserved = vec![
            X86Register::R10,
            X86Register::R11,
            X86Register::R8,
            X86Register::R9,
        ];
        let scratch = alloc.get_unreserved_scratch(&all_reserved);
        assert!(scratch.is_none());
    }
}
