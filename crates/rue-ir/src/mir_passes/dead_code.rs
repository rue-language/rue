//! Dead code elimination pass for MIR
//!
//! This optimization pass removes assignments to temporaries that are never used.
//! It performs use-def analysis to track which temporaries are actually needed
//! and eliminates assignments to dead temporaries.

use crate::mir::{
    BasicBlock, BlockId, MirFunction, MirProgram, MirStatement, MirTerminator, MirValue, Temp,
};
use std::collections::{HashMap, HashSet};

/// Dead code elimination pass
pub struct DeadCodeElimination {
    /// Set of temporaries that are used (live)
    used_temps: HashSet<Temp>,
    /// Map from block ID to the set of temps used in that block
    block_uses: HashMap<BlockId, HashSet<Temp>>,
}

impl Default for DeadCodeElimination {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadCodeElimination {
    pub fn new() -> Self {
        Self {
            used_temps: HashSet::new(),
            block_uses: HashMap::new(),
        }
    }

    /// Run dead code elimination on a MIR program
    pub fn run(&mut self, program: &mut MirProgram) {
        for func in &mut program.functions {
            self.optimize_function(func);
        }
    }

    /// Optimize a single function
    fn optimize_function(&mut self, func: &mut MirFunction) {
        // Clear state for new function
        self.used_temps.clear();
        self.block_uses.clear();

        // First pass: collect all uses
        self.collect_uses(func);

        // Second pass: remove dead assignments
        self.remove_dead_assignments(func);

        // Third pass: remove unreachable blocks
        self.remove_unreachable_blocks(func);
    }

    /// Collect all temporary uses in the function
    fn collect_uses(&mut self, func: &MirFunction) {
        for block in &func.blocks {
            let mut block_uses = HashSet::new();

            // Collect uses from statements
            for stmt in &block.statements {
                match stmt {
                    MirStatement::Assign { value, .. } => {
                        self.collect_value_uses(value, &mut block_uses);
                    }
                }
            }

            // Collect uses from terminator
            self.collect_terminator_uses(&block.terminator, &mut block_uses);

            // Add to global used set
            self.used_temps.extend(&block_uses);
            self.block_uses.insert(block.id, block_uses);
        }

        // Also mark block parameters as used if they're passed as arguments
        for block in &func.blocks {
            match &block.terminator {
                MirTerminator::Goto { args, .. } => {
                    // Mark arguments as used
                    for arg in args {
                        self.used_temps.insert(*arg);
                    }
                }
                MirTerminator::Branch {
                    then_args,
                    else_args,
                    ..
                } => {
                    // Mark arguments as used
                    for arg in then_args {
                        self.used_temps.insert(*arg);
                    }
                    for arg in else_args {
                        self.used_temps.insert(*arg);
                    }
                }
                MirTerminator::Switch {
                    targets,
                    default_args,
                    ..
                } => {
                    // Mark all target arguments as used
                    for (_, _, target_args) in targets {
                        for arg in target_args {
                            self.used_temps.insert(*arg);
                        }
                    }
                    // Mark default arguments as used
                    for arg in default_args {
                        self.used_temps.insert(*arg);
                    }
                }
                _ => {}
            }
        }
    }

    /// Collect uses from a value
    fn collect_value_uses(&self, value: &MirValue, uses: &mut HashSet<Temp>) {
        match value {
            MirValue::Use(temp) => {
                uses.insert(*temp);
            }
            MirValue::BinaryOp { lhs, rhs, .. } => {
                uses.insert(*lhs);
                uses.insert(*rhs);
            }
            MirValue::UnaryOp { operand, .. } => {
                uses.insert(*operand);
            }
            MirValue::Call { args, .. } => {
                for arg in args {
                    uses.insert(*arg);
                }
            }
            MirValue::Const(_) => {}
        }
    }

    /// Collect uses from a terminator
    fn collect_terminator_uses(&self, term: &MirTerminator, uses: &mut HashSet<Temp>) {
        match term {
            MirTerminator::Return { value, .. } => {
                if let Some(temp) = value {
                    uses.insert(*temp);
                }
            }
            MirTerminator::Branch { condition, .. } => {
                uses.insert(*condition);
            }
            MirTerminator::Switch { discriminant, .. } => {
                uses.insert(*discriminant);
            }
            MirTerminator::Goto { .. } | MirTerminator::Unreachable { .. } => {}
        }
    }

    /// Remove assignments to dead temporaries
    fn remove_dead_assignments(&mut self, func: &mut MirFunction) {
        for block in &mut func.blocks {
            // Filter out dead assignments
            block.statements.retain(|stmt| match stmt {
                MirStatement::Assign { dest, value, .. } => {
                    // Keep assignment if:
                    // 1. The destination is used
                    // 2. The value has side effects (function calls)
                    if self.used_temps.contains(dest) {
                        true
                    } else {
                        // Check if the value has side effects
                        !self.is_pure_value(value)
                    }
                }
            });
        }
    }

    /// Check if a value is pure (has no side effects)
    fn is_pure_value(&self, value: &MirValue) -> bool {
        match value {
            MirValue::Use(_)
            | MirValue::Const(_)
            | MirValue::BinaryOp { .. }
            | MirValue::UnaryOp { .. } => true,
            MirValue::Call { kind, .. } => {
                // Use the CallKind to determine if the call has side effects
                match kind {
                    crate::mir::CallKind::Pure => true,   // Pure calls can be eliminated if unused
                    crate::mir::CallKind::Impure => false, // Impure calls have side effects
                }
            }
        }
    }

    /// Remove unreachable blocks from the function
    fn remove_unreachable_blocks(&mut self, func: &mut MirFunction) {
        // Find all reachable blocks starting from entry
        let reachable = self.find_reachable_blocks(func.entry_block, &func.blocks);

        // Remove unreachable blocks
        func.blocks.retain(|block| reachable.contains(&block.id));
    }

    /// Find all reachable blocks from a starting block
    fn find_reachable_blocks(&self, start: BlockId, blocks: &[BasicBlock]) -> HashSet<BlockId> {
        let mut reachable = HashSet::new();
        let mut worklist = vec![start];

        // Build a map for quick lookup
        let block_map: HashMap<BlockId, &BasicBlock> = blocks.iter().map(|b| (b.id, b)).collect();

        while let Some(block_id) = worklist.pop() {
            if !reachable.insert(block_id) {
                continue; // Already visited
            }

            if let Some(block) = block_map.get(&block_id) {
                match &block.terminator {
                    MirTerminator::Goto { target, .. } => {
                        worklist.push(*target);
                    }
                    MirTerminator::Branch {
                        then_block,
                        else_block,
                        ..
                    } => {
                        worklist.push(*then_block);
                        worklist.push(*else_block);
                    }
                    MirTerminator::Switch {
                        targets,
                        default,
                        ..
                    } => {
                        // Add all target blocks from switch cases
                        for (_, target_block, _) in targets {
                            worklist.push(*target_block);
                        }
                        // Add default block
                        worklist.push(*default);
                    }
                    MirTerminator::Return { .. } | MirTerminator::Unreachable { .. } => {}
                }
            }
        }

        reachable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BasicBlock, MirBinOp, MirConst, MirStatement, MirTerminator, MirValue, Temp};
    use crate::types::RueType;
    use rue_lexer::Span;

    #[test]
    fn test_dead_assignment_removal() {
        // Create a MIR program with dead assignments:
        // t0 = 10      // Used
        // t1 = 20      // Dead - never used anywhere
        // t2 = t0 + t0 // Used in return
        // t3 = 30      // Dead - never used
        // return t2

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![
                        MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Const(MirConst::Int32(10)),
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(1),
                            value: MirValue::Const(MirConst::Int32(20)),
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(2),
                            value: MirValue::BinaryOp {
                                op: MirBinOp::Add,
                                lhs: Temp(0),
                                rhs: Temp(0),
                            },
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(3),
                            value: MirValue::Const(MirConst::Int32(30)),
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(2)),
                        span: None,
                    },
                }],
            }],
        };

        // Run dead code elimination
        let mut dce = DeadCodeElimination::new();
        dce.run(&mut program);

        // Check that dead assignments were removed
        let block = &program.functions[0].blocks[0];
        assert_eq!(block.statements.len(), 2); // Only t0 and t2 should remain

        // Verify the remaining assignments are the correct ones
        match &block.statements[0] {
            MirStatement::Assign { dest: Temp(0), .. } => {} // t0 assignment kept
            _ => panic!("Expected t0 assignment"),
        }
        match &block.statements[1] {
            MirStatement::Assign { dest: Temp(2), .. } => {} // t2 assignment kept
            _ => panic!("Expected t2 assignment"),
        }
    }

    #[test]
    fn test_keep_side_effects() {
        // Create a MIR program with function calls:
        // t0 = call foo()  // Should be kept even if t0 is unused
        // t1 = 10          // Dead - should be removed
        // return 42

        let mut program = MirProgram {
            functions: vec![MirFunction {
                name: "test".to_string(),
                params: vec![],
                return_type: RueType::I32,
                entry_block: BlockId(0),
                span: Span::dummy(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![
                        MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Call {
                                func: "foo".to_string(),
                                args: vec![],
                                kind: crate::mir::CallKind::Impure,
                            },
                            span: None,
                        },
                        MirStatement::Assign {
                            dest: Temp(1),
                            value: MirValue::Const(MirConst::Int32(10)),
                            span: None,
                        },
                    ],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(42)), // Using a different temp
                        span: None,
                    },
                }],
            }],
        };

        // Run dead code elimination
        let mut dce = DeadCodeElimination::new();
        dce.run(&mut program);

        // Check that function call was kept but dead assignment was removed
        let block = &program.functions[0].blocks[0];
        assert_eq!(block.statements.len(), 1);

        // Verify the function call was kept
        match &block.statements[0] {
            MirStatement::Assign {
                value: MirValue::Call { func, .. },
                ..
            } if func == "foo" => {} // Function call kept
            _ => panic!("Expected function call to be kept"),
        }
    }
}
