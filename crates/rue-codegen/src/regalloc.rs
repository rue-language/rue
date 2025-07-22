use crate::VReg;
use rue_ir::target::Register;
use std::collections::HashMap;

/// Location where a virtual register is stored
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Stored in a physical register
    Register(Register),
    /// Stored in a stack slot at the given offset from RBP
    StackSlot(i32),
}

/// Operations that need to be emitted by the lowering layer
#[derive(Debug, Clone)]
pub enum SpillReloadOp {
    /// Spill a register to stack
    Spill { reg: Register, stack_offset: i32 },
    /// Reload from stack to register
    Reload { reg: Register, stack_offset: i32 },
    /// Move between registers
    Move { from: Register, to: Register },
}

/// Register allocator with stack spilling support
pub struct RegisterAllocator {
    /// Mapping from virtual registers to their locations
    allocation: HashMap<VReg, Location>,
    /// Available physical registers (in order of preference)
    available_registers: Vec<Register>,
    /// Free registers available for allocation
    free_registers: Vec<Register>,
    /// Current stack offset for new stack slots (negative from RBP)
    stack_offset: i32,
    /// LRU order for spill victim selection (least recent first, most recent last)
    lru_order: Vec<VReg>,
    /// Home stack slots for each vreg (once allocated, never changes)
    home_slots: HashMap<VReg, i32>,
    /// Pending spill/reload operations to be emitted
    pending_ops: Vec<SpillReloadOp>,
}

impl RegisterAllocator {
    pub fn new() -> Self {
        // Use only caller-saved registers to avoid ABI violations
        // R15 is reserved as a scratch register for the assembler
        let registers = vec![
            Register::Rcx,
            Register::Rdx,
            Register::Rsi,
            Register::Rdi,
            Register::R8,
            Register::R9,
            Register::R10,
            Register::R11,
        ];

        Self {
            allocation: HashMap::new(),
            available_registers: registers.clone(),
            free_registers: registers, // Initially all registers are free
            stack_offset: 0,
            lru_order: Vec::new(),
            home_slots: HashMap::new(),
            pending_ops: Vec::new(),
        }
    }

    /// Allocate a location (register or stack slot) for a virtual register
    pub fn allocate(&mut self, vreg: VReg) -> Result<(), String> {
        match self.allocation.get(&vreg).copied() {
            Some(Location::Register(_)) => {
                // Already in a register - just update LRU
                self.update_lru(vreg);
                Ok(())
            }
            Some(Location::StackSlot(_stack_offset)) => {
                // Currently spilled - need to reload into a register
                let physical_reg = if let Some(reg) = self.free_registers.pop() {
                    reg
                } else {
                    // No free registers - spill another to make room
                    self.spill_register()?
                };

                // Update allocation to the new register
                self.allocation
                    .insert(vreg, Location::Register(physical_reg));

                // Add to LRU order (it was removed when spilled)
                self.lru_order.push(vreg);
                Ok(())
            }
            None => {
                // Not yet allocated - allocate a new register
                if let Some(physical_reg) = self.free_registers.pop() {
                    // Free register available - use it
                    self.allocation
                        .insert(vreg, Location::Register(physical_reg));
                    self.lru_order.push(vreg);
                    Ok(())
                } else {
                    // No free registers - spill LRU register and use the freed one
                    let freed_reg = self.spill_register()?;
                    self.allocation.insert(vreg, Location::Register(freed_reg));
                    self.lru_order.push(vreg);
                    Ok(())
                }
            }
        }
    }

    /// Get the location for a virtual register
    pub fn get_location(&self, vreg: VReg) -> Option<Location> {
        self.allocation.get(&vreg).copied()
    }

    /// Get the physical register for a virtual register (must be already allocated)
    pub fn get_register(&self, vreg: VReg) -> Option<Register> {
        match self.allocation.get(&vreg) {
            Some(Location::Register(reg)) => Some(*reg),
            Some(Location::StackSlot(_)) => None,
            None => None,
        }
    }

    /// Check if a physical register is currently allocated to any virtual register
    pub fn is_register_allocated(&self, reg: Register) -> bool {
        self.allocation
            .values()
            .any(|loc| matches!(loc, Location::Register(r) if *r == reg))
    }

    /// Invalidate a register - mark any vreg in this register as spilled
    /// and generate spill operations to preserve their values
    pub fn invalidate_register(&mut self, reg: Register) {
        // Find any vreg that thinks it's in this register
        let vregs_to_spill: Vec<VReg> = self
            .allocation
            .iter()
            .filter_map(|(vreg, loc)| match loc {
                Location::Register(r) if *r == reg => Some(*vreg),
                _ => None,
            })
            .collect();

        // Spill those vregs and generate spill operations
        for vreg in vregs_to_spill {
            // Get or allocate a home slot
            let home_slot = if let Some(&slot) = self.home_slots.get(&vreg) {
                slot
            } else {
                let new_slot = self.allocate_stack_slot();
                self.home_slots.insert(vreg, new_slot);
                new_slot
            };

            // Generate spill operation to save the current value in the register
            self.pending_ops.push(SpillReloadOp::Spill {
                reg,
                stack_offset: home_slot,
            });

            // Update the allocation to indicate it's on the stack
            self.allocation.insert(vreg, Location::StackSlot(home_slot));

            // Remove from LRU order since it's no longer in a register
            self.lru_order.retain(|&v| v != vreg);

            // The register is now free
            if !self.free_registers.contains(&reg) {
                self.free_registers.push(reg);
            }
        }
    }

    /// Spill the least recently used register to stack and return the freed register
    fn spill_register(&mut self) -> Result<Register, String> {
        // Find the LRU register that's currently in a physical register
        // Iterate from front since least recently used is at the beginning
        let lru_vreg = self
            .lru_order
            .iter()
            .find(|&&vreg| matches!(self.allocation.get(&vreg), Some(Location::Register(_))))
            .copied()
            .ok_or("No registers available to spill")?;

        // Get the physical register before we remove the mapping
        let freed_register = match self.allocation.get(&lru_vreg) {
            Some(Location::Register(reg)) => *reg,
            _ => return Err("LRU vreg is not in a register".to_string()),
        };

        // Get or allocate a home stack slot for the spilled register
        let stack_slot = if let Some(&slot) = self.home_slots.get(&lru_vreg) {
            slot
        } else {
            let new_slot = self.allocate_stack_slot();
            self.home_slots.insert(lru_vreg, new_slot);
            new_slot
        };

        // Update the allocation to point to stack
        self.allocation
            .insert(lru_vreg, Location::StackSlot(stack_slot));

        // Remove from LRU order since it's now spilled
        self.lru_order.retain(|&vreg| vreg != lru_vreg);

        // Return the freed register (caller will use it)
        Ok(freed_register)
    }

    /// Allocate a new stack slot with proper alignment
    fn allocate_stack_slot(&mut self) -> i32 {
        self.stack_offset -= 8; // 8-byte alignment for 64-bit values
        self.stack_offset
    }

    /// Update LRU order (move vreg to end as most recently used)
    fn update_lru(&mut self, vreg: VReg) {
        self.lru_order.retain(|&v| v != vreg);
        self.lru_order.push(vreg); // Push to end - most recent is last
    }

    /// Get total stack space needed for spilled registers
    pub fn get_stack_space_needed(&self) -> u32 {
        (-self.stack_offset) as u32
    }

    /// Get all current vreg locations for saving caller-saved registers
    pub fn get_all_locations(&self) -> impl Iterator<Item = (VReg, Location)> + '_ {
        self.allocation.iter().map(|(&vreg, &loc)| (vreg, loc))
    }

    /// Reset allocator state for a new function
    /// This is critical for recursive functions to avoid stack slot conflicts
    pub fn reset_for_new_function(&mut self) {
        self.allocation.clear();
        // Reset free registers to all available registers
        self.free_registers = self.available_registers.clone();
        self.stack_offset = 0;
        self.lru_order.clear();
        self.home_slots.clear();
        self.pending_ops.clear();
    }

    /// Ensure a virtual register is in a physical register.
    /// This is the new primary API that encapsulates all spilling logic.
    ///
    /// If the vreg is already in a register, returns that register.
    /// If the vreg is spilled, emits reload instructions and returns the register.
    /// If the vreg is unallocated, allocates a register (potentially spilling others).
    ///
    /// The `forbidden` parameter lists registers that must not be used for this vreg.
    pub fn ensure_reg(&mut self, vreg: VReg, forbidden: &[Register]) -> Result<Register, String> {
        // Handle spill/reload tracking internally
        let mut spill_instructions = Vec::new();

        match self.allocation.get(&vreg).copied() {
            Some(Location::Register(reg)) => {
                // Already in a register
                if forbidden.contains(&reg) {
                    // Need to move to a different register
                    let new_reg =
                        self.allocate_register_avoiding(forbidden, &mut spill_instructions)?;
                    self.allocation.insert(vreg, Location::Register(new_reg));
                    self.update_lru(vreg);

                    // Record that we need to move from old reg to new reg
                    spill_instructions.push(SpillReloadOp::Move {
                        from: reg,
                        to: new_reg,
                    });

                    // Free the old register
                    self.free_registers.push(reg);

                    Ok(new_reg)
                } else {
                    self.update_lru(vreg);
                    Ok(reg)
                }
            }
            Some(Location::StackSlot(stack_offset)) => {
                // Currently spilled - need to reload
                let reg = self.allocate_register_avoiding(forbidden, &mut spill_instructions)?;

                // Update allocation
                self.allocation.insert(vreg, Location::Register(reg));

                // Add to LRU order (it was removed when spilled)
                self.lru_order.push(vreg);

                // Record reload operation
                spill_instructions.push(SpillReloadOp::Reload { reg, stack_offset });

                // Store spill/reload ops for the lowering layer to emit
                self.pending_ops.extend(spill_instructions);

                Ok(reg)
            }
            None => {
                // Not yet allocated
                let reg = self.allocate_register_avoiding(forbidden, &mut spill_instructions)?;
                self.allocation.insert(vreg, Location::Register(reg));
                self.lru_order.push(vreg);

                // Store spill/reload ops for the lowering layer to emit
                self.pending_ops.extend(spill_instructions);

                Ok(reg)
            }
        }
    }

    /// Allocate a register, avoiding the forbidden list.
    /// May spill other vregs to make room.
    fn allocate_register_avoiding(
        &mut self,
        forbidden: &[Register],
        spill_ops: &mut Vec<SpillReloadOp>,
    ) -> Result<Register, String> {
        // First try to find a free register not in forbidden list
        if let Some(pos) = self
            .free_registers
            .iter()
            .position(|r| !forbidden.contains(r))
        {
            return Ok(self.free_registers.remove(pos));
        }

        // No free registers - need to spill
        // Find LRU vreg that's in a register not in forbidden list
        let spill_candidate = self
            .lru_order
            .iter()
            .find(|&&vreg| {
                if let Some(Location::Register(reg)) = self.allocation.get(&vreg) {
                    !forbidden.contains(reg)
                } else {
                    false
                }
            })
            .copied()
            .ok_or("No registers available to spill")?;

        // Get the register before spilling
        let freed_reg = match self.allocation.get(&spill_candidate) {
            Some(Location::Register(reg)) => *reg,
            _ => return Err("Spill candidate not in register".to_string()),
        };

        // Get or allocate home slot
        let stack_slot = if let Some(&slot) = self.home_slots.get(&spill_candidate) {
            slot
        } else {
            let new_slot = self.allocate_stack_slot();
            self.home_slots.insert(spill_candidate, new_slot);
            new_slot
        };

        // Record spill operation
        spill_ops.push(SpillReloadOp::Spill {
            reg: freed_reg,
            stack_offset: stack_slot,
        });

        // Update allocation
        self.allocation
            .insert(spill_candidate, Location::StackSlot(stack_slot));

        // Remove from LRU
        self.lru_order.retain(|&v| v != spill_candidate);

        Ok(freed_reg)
    }

    /// Get pending spill/reload operations and clear the buffer
    pub fn take_pending_ops(&mut self) -> Vec<SpillReloadOp> {
        std::mem::take(&mut self.pending_ops)
    }

    /// Get the total stack space needed for spill slots
    pub fn get_stack_size(&self) -> i32 {
        // Stack grows downward, so negate the offset
        -self.stack_offset
    }
}

impl Default for RegisterAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_allocation() {
        let mut allocator = RegisterAllocator::new();

        let vreg1 = VReg(1);
        let vreg2 = VReg(2);

        // Allocate registers
        allocator.allocate(vreg1).unwrap();
        allocator.allocate(vreg2).unwrap();

        // Should get consistent allocation
        let reg1 = allocator.get_register(vreg1).unwrap();
        let reg2 = allocator.get_register(vreg2).unwrap();

        // Re-allocating should work
        allocator.allocate(vreg1).unwrap();
        allocator.allocate(vreg2).unwrap();

        // Should still get same registers
        assert_eq!(allocator.get_register(vreg1).unwrap(), reg1);
        assert_eq!(allocator.get_register(vreg2).unwrap(), reg2);

        // Should allocate different registers
        assert_ne!(reg1, reg2);
    }

    #[test]
    fn test_spilling_when_out_of_registers() {
        let mut allocator = RegisterAllocator::new();

        // Successfully allocate all available registers
        for i in 0..8 {
            let vreg = VReg(i);
            let result = allocator.allocate(vreg);
            assert!(result.is_ok(), "Should allocate register {i}");
            assert!(matches!(
                allocator.get_location(vreg),
                Some(Location::Register(_))
            ));
        }

        // Attempting to allocate one more should trigger spilling
        let vreg9 = VReg(8);
        let result = allocator.allocate(vreg9);
        assert!(result.is_ok(), "Should allocate register via spilling");

        // The LRU register should now be spilled to stack
        let vreg0 = VReg(0);
        assert!(matches!(
            allocator.get_location(vreg0),
            Some(Location::StackSlot(_))
        ));

        // The new register should be in a physical register
        assert!(matches!(
            allocator.get_location(vreg9),
            Some(Location::Register(_))
        ));

        // Should have allocated some stack space
        assert!(allocator.get_stack_space_needed() > 0);

        // The critical test: verify proper register reuse after spilling
        // When we allocate many vregs, we should reuse registers efficiently

        // First, allocate enough to trigger spilling (vregs 8-11)
        for i in 8..12 {
            allocator.allocate(VReg(i)).unwrap();
        }

        // At this point, we should have spilled 4 vregs (0-3) to make room
        assert_eq!(
            allocator.get_stack_space_needed(),
            32,
            "Should have 4 spills after allocating 12 vregs"
        );

        // Now test the key behavior: re-allocating a spilled vreg should work
        allocator.allocate(VReg(0)).unwrap(); // Re-access spilled vreg
        allocator.allocate(VReg(1)).unwrap(); // Re-access another spilled vreg

        // Stack usage should increase by 16 bytes (2 more spills) because we need to
        // spill 2 current registers to make room for reloading VReg(0) and VReg(1)
        assert_eq!(
            allocator.get_stack_space_needed(),
            48,
            "Should have 6 total spills after reloading 2 vregs"
        );

        // Continue with the original test - allocate many more
        for i in 12..40 {
            allocator.allocate(VReg(i)).unwrap();
        }

        // With 40 unique vregs and 8 registers, we need 32 stack slots minimum
        // With home slot optimization, vregs that are reloaded and spilled again
        // reuse their existing slots, so we have exactly 32 unique stack slots
        assert_eq!(
            allocator.get_stack_space_needed(),
            256,
            "Should have exactly 32 unique stack slots (256 bytes) with home slot optimization"
        );

        // Verify no aliasing - count vregs in each location
        let mut in_registers = 0;
        let mut in_stack = 0;
        for i in 0..40 {
            match allocator.get_location(VReg(i)) {
                Some(Location::Register(_)) => in_registers += 1,
                Some(Location::StackSlot(_)) => in_stack += 1,
                None => panic!("VReg {i} should be allocated"),
            }
        }
        assert_eq!(in_registers, 8, "Exactly 8 vregs should be in registers");
        assert_eq!(in_stack, 32, "Exactly 32 vregs should be on stack");
    }

    #[test]
    fn test_repeated_allocation_same_vreg() {
        let mut allocator = RegisterAllocator::new();

        let vreg = VReg(1);
        allocator.allocate(vreg).unwrap();
        let reg1 = allocator.get_register(vreg).unwrap();

        allocator.allocate(vreg).unwrap(); // Should work for repeated allocation
        let reg2 = allocator.get_register(vreg).unwrap();

        assert_eq!(
            reg1, reg2,
            "Repeated allocation should return same register"
        );
        assert_eq!(
            allocator.free_registers.len(),
            7,
            "One register should be consumed from free list"
        );
    }

    #[test]
    fn test_no_register_aliasing_after_spill() {
        let mut allocator = RegisterAllocator::new();

        // Allocate all available registers
        for i in 0..8 {
            let vreg = VReg(i);
            allocator.allocate(vreg).unwrap();
        }

        // Allocate one more to trigger spilling
        let vreg9 = VReg(8);
        allocator.allocate(vreg9).unwrap();

        // Check that no two vregs share a physical register
        for (i, loc_i) in allocator.allocation.values().enumerate() {
            if let Location::Register(r_i) = loc_i {
                for loc_j in allocator.allocation.values().skip(i + 1) {
                    if let Location::Register(r_j) = loc_j {
                        assert_ne!(r_i, r_j, "alias detected: two vregs share register {r_i:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn test_reload_from_stack() {
        let mut allocator = RegisterAllocator::new();

        // Allocate VReg(0)
        allocator.allocate(VReg(0)).unwrap();
        assert!(matches!(
            allocator.get_location(VReg(0)),
            Some(Location::Register(_))
        ));

        // Allocate 8 more to force VReg(0) to spill
        for i in 1..9 {
            allocator.allocate(VReg(i)).unwrap();
        }

        // VReg(0) should now be on stack
        assert!(matches!(
            allocator.get_location(VReg(0)),
            Some(Location::StackSlot(_))
        ));

        // Re-allocate VReg(0) - should load it back into a register
        allocator.allocate(VReg(0)).unwrap();

        // VReg(0) should now be in a register again
        assert!(matches!(
            allocator.get_location(VReg(0)),
            Some(Location::Register(_))
        ));
    }

    #[test]
    fn test_no_callee_saved_regs_used() {
        let mut alloc = RegisterAllocator::new();

        // Allocate more VRegs than there are physical registers to force spills.
        for i in 0..20 {
            alloc.allocate(VReg(i)).unwrap();
        }

        // Callee‑saved registers must never appear in any live mapping.
        let callee_saved = [
            Register::Rbx, /*, Register::R12, Register::R13, Register::R14, Register::R15 */
        ];
        for reg in callee_saved {
            assert!(
                !alloc.is_register_allocated(reg),
                "callee‑saved {reg:?} should not be allocated"
            );
        }
    }

    #[test]
    fn test_same_stack_slot_reused() {
        let mut a = RegisterAllocator::new();

        // Force VReg(0) to spill once
        a.allocate(VReg(0)).unwrap();
        for i in 1..=8 {
            a.allocate(VReg(i)).unwrap();
        } // all regs full, v0 spilled
        let first_slot = match a.get_location(VReg(0)).unwrap() {
            Location::StackSlot(o) => o,
            _ => panic!("v0 should be on stack"),
        };

        // Reload v0, then spill it again
        a.allocate(VReg(0)).unwrap(); // reload
        for i in 9..=16 {
            a.allocate(VReg(i)).unwrap();
        } // spill v0 again
        let second_slot = match a.get_location(VReg(0)).unwrap() {
            Location::StackSlot(o) => o,
            _ => panic!("v0 should be on stack again"),
        };

        assert_eq!(
            first_slot, second_slot,
            "vreg spilled to different stack slots; must reuse the same slot"
        );
    }
}
