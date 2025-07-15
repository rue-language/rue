use crate::{Register, VReg};
use std::collections::HashMap;

/// Simple linear scan register allocator
pub struct RegisterAllocator {
    /// Mapping from virtual registers to physical registers
    allocation: HashMap<VReg, Register>,
    /// Available physical registers (in order of preference)
    available_registers: Vec<Register>,
    /// Next register to allocate
    next_register_index: usize,
}

impl RegisterAllocator {
    pub fn new() -> Self {
        Self {
            allocation: HashMap::new(),
            // Use safe register set that supports basic type system tests
            // Added R8, R9, R10 which are caller-saved but don't need complex handling
            available_registers: vec![
                Register::Rbx,
                Register::Rcx,
                Register::Rdx,
                Register::Rsi,
                Register::Rdi,
                Register::R8,
                Register::R9,
                Register::R10,
            ],
            next_register_index: 0,
        }
    }

    /// Allocate a physical register for a virtual register
    pub fn allocate(&mut self, vreg: VReg) -> Result<Register, String> {
        if let Some(&physical_reg) = self.allocation.get(&vreg) {
            // Already allocated
            Ok(physical_reg)
        } else {
            // Check if we have available registers
            if self.next_register_index >= self.available_registers.len() {
                return Err(format!(
                    "Register allocation failed: out of registers. \
                     Requested vreg {:?} but only {} registers available. \
                     Consider refactoring to use fewer variables.",
                    vreg,
                    self.available_registers.len()
                ));
            }

            let physical_reg = self.available_registers[self.next_register_index];
            self.next_register_index += 1;

            self.allocation.insert(vreg, physical_reg);
            Ok(physical_reg)
        }
    }

    /// Get the allocation mapping
    pub fn get_allocation(&self) -> &HashMap<VReg, Register> {
        &self.allocation
    }

    /// Get the physical register for a virtual register (must be already allocated)
    pub fn get_register(&self, vreg: VReg) -> Option<Register> {
        self.allocation.get(&vreg).copied()
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

        let reg1 = allocator.allocate(vreg1).unwrap();
        let reg2 = allocator.allocate(vreg2).unwrap();

        // Should get consistent allocation
        assert_eq!(allocator.allocate(vreg1).unwrap(), reg1);
        assert_eq!(allocator.allocate(vreg2).unwrap(), reg2);

        // Should allocate different registers
        assert_ne!(reg1, reg2);
    }

    #[test]
    fn test_out_of_registers_error() {
        let mut allocator = RegisterAllocator::new();

        // Successfully allocate all available registers
        for i in 0..8 {
            let vreg = VReg(i);
            let result = allocator.allocate(vreg);
            assert!(result.is_ok(), "Should allocate register {i}");
        }

        // Attempting to allocate one more should fail
        let vreg9 = VReg(8);
        let result = allocator.allocate(vreg9);
        assert!(result.is_err(), "Should fail when out of registers");

        let error_msg = result.unwrap_err();
        assert!(error_msg.contains("Register allocation failed"));
        assert!(error_msg.contains("out of registers"));
    }

    #[test]
    fn test_repeated_allocation_same_vreg() {
        let mut allocator = RegisterAllocator::new();

        let vreg = VReg(1);
        let reg1 = allocator.allocate(vreg).unwrap();
        let reg2 = allocator.allocate(vreg).unwrap(); // Should return same register

        assert_eq!(
            reg1, reg2,
            "Repeated allocation should return same register"
        );
        assert_eq!(
            allocator.next_register_index, 1,
            "Index should not advance for repeated allocation"
        );
    }
}
