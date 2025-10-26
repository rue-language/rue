//! Dead code elimination pass for MIR
//!
//! This optimization pass removes assignments to temporaries that are never used.
//! It performs use-def analysis to track which temporaries are actually needed
//! and eliminates assignments to dead temporaries.

use crate::pass_manager::{Pass, PassResult};
use crate::visitor::{MirFolder, MirVisitor, VisitorControl};
use rue_ir::mir::{
    BasicBlock, BlockId, MirFunction, MirProgram, MirStatement, MirTerminator, MirValue, Temp,
};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Dead code elimination pass
pub struct DeadCodeElimination {
    /// Set of temporaries that are used (live)
    used_temps: HashSet<Temp>,
    /// Map from block ID to the set of temps used in that block
    block_uses: HashMap<BlockId, HashSet<Temp>>,
    /// Number of transformations made in the current run
    transformations: u32,
}

/// Helper visitor for collecting uses
struct UseCollector {
    used_temps: HashSet<Temp>,
    current_block: Option<BlockId>,
    block_uses: HashMap<BlockId, HashSet<Temp>>,
}

/// Helper folder for removing dead assignments
struct DeadAssignmentRemover {
    used_temps: HashSet<Temp>,
    transformations: u32,
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
            transformations: 0,
        }
    }

    /// Run dead code elimination on a MIR program (legacy interface)
    pub fn run(&mut self, program: &mut MirProgram) {
        self.transformations = 0;
        for func in &mut program.functions {
            self.optimize_function(func);
        }
    }

    /// Optimize a single function
    fn optimize_function(&mut self, func: &mut MirFunction) {
        // Clear state for new function
        self.used_temps.clear();
        self.block_uses.clear();

        // First pass: collect all uses using visitor
        let mut collector = UseCollector {
            used_temps: HashSet::new(),
            current_block: None,
            block_uses: HashMap::new(),
        };
        collector.visit_function(func);

        self.used_temps = collector.used_temps;
        self.block_uses = collector.block_uses;

        // Second pass: remove dead assignments using folder
        let mut remover = DeadAssignmentRemover {
            used_temps: self.used_temps.clone(),
            transformations: 0,
        };
        *func = remover.fold_function(func.clone()).expect("dead assignment removal should not fail");
        self.transformations += remover.transformations;

        // Third pass: remove unreachable blocks
        self.remove_unreachable_blocks(func);
    }

    /// Remove unreachable blocks from the function
    fn remove_unreachable_blocks(&mut self, func: &mut MirFunction) {
        // Find all reachable blocks starting from entry
        let reachable = self.find_reachable_blocks(func.entry_block, &func.blocks);

        let original_len = func.blocks.len();

        // Remove unreachable blocks
        func.blocks.retain(|block| reachable.contains(&block.id));

        // Count removed blocks as transformations
        self.transformations += (original_len - func.blocks.len()) as u32;
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
                        targets, default, ..
                    } => {
                        // Add all target blocks from switch cases
                        for (_, target_block, _) in targets {
                            worklist.push(*target_block);
                        }
                        // Add default block
                        worklist.push(*default);
                    }
                    MirTerminator::Return { .. } | MirTerminator::Unreachable { .. } => {}
                    MirTerminator::Uninit => {
                        // Uninit terminators should not exist in optimized MIR
                        panic!(
                            "Encountered Uninit terminator during dead code analysis - this indicates a lowering bug"
                        );
                    }
                }
            }
        }

        reachable
    }
}

// Visitor implementations

impl MirVisitor for UseCollector {
    fn visit_block(&mut self, block: &BasicBlock) -> VisitorControl {
        self.current_block = Some(block.id);
        let mut block_uses = HashSet::new();

        // Store the current set to collect block-specific uses
        let old_uses = self.used_temps.clone();

        // Visit all components of the block
        crate::visitor::walk_block(self, block);

        // Calculate block-specific uses
        for temp in &self.used_temps {
            if !old_uses.contains(temp) {
                block_uses.insert(*temp);
            }
        }

        self.block_uses.insert(block.id, block_uses);
        VisitorControl::Continue
    }

    fn visit_statement(&mut self, stmt: &MirStatement) -> VisitorControl {
        match stmt {
            MirStatement::Assign { value, .. } => {
                // Only visit the value, not the destination
                self.visit_value(value)
            }
            MirStatement::Return { value, .. } => {
                // Visit return value if present
                if let Some(temp) = value {
                    self.visit_temp(temp);
                }
                VisitorControl::Continue
            }
        }
    }

    fn visit_temp(&mut self, temp: &Temp) -> VisitorControl {
        self.used_temps.insert(*temp);
        VisitorControl::Continue
    }
}

impl MirFolder for DeadAssignmentRemover {
    type Error = ();

    fn fold_statement(&mut self, stmt: MirStatement) -> Result<Option<MirStatement>, Self::Error> {
        match &stmt {
            MirStatement::Assign { dest, value, .. } => {
                // Keep assignment if:
                // 1. The destination is used
                // 2. The value has side effects (function calls)
                if self.used_temps.contains(dest) {
                    Ok(Some(stmt))
                } else if !Self::is_pure_value(value) {
                    // Keep statements with side effects
                    Ok(Some(stmt))
                } else {
                    // Remove dead assignment
                    self.transformations += 1;
                    Ok(None)
                }
            }
            MirStatement::Return { .. } => {
                // Always keep return statements
                Ok(Some(stmt))
            }
        }
    }
}

impl DeadAssignmentRemover {
    /// Check if a value is pure (has no side effects)
    fn is_pure_value(value: &MirValue) -> bool {
        match value {
            MirValue::Use(_)
            | MirValue::Const(_)
            | MirValue::BinaryOp { .. }
            | MirValue::UnaryOp { .. } => true,
            MirValue::Call { kind, .. } => {
                // Use the CallKind to determine if the call has side effects
                match kind {
                    rue_ir::mir::CallKind::Pure => true, // Pure calls can be eliminated if unused
                    rue_ir::mir::CallKind::Impure => false, // Impure calls have side effects
                }
            }
            // Aggregate operations are pure (no side effects)
            MirValue::ConstructAggregate { .. }
            | MirValue::GetField { .. }
            | MirValue::SetField { .. }
            | MirValue::StructUpdate { .. } => true,
            // Dynamic array access has side effects (bounds checking can trap)
            MirValue::DynamicArrayAccess { .. } => false,
        }
    }
}

impl Pass for DeadCodeElimination {
    fn name(&self) -> &'static str {
        "dce"
    }

    fn run(&mut self, program: &mut MirProgram) -> PassResult {
        let start = Instant::now();
        self.transformations = 0;

        for func in &mut program.functions {
            self.optimize_function(func);
        }

        let duration = start.elapsed();
        let changed = self.transformations > 0;

        if changed {
            PassResult::changed(duration, self.transformations)
        } else {
            PassResult::no_change(duration)
        }
    }

    fn description(&self) -> &'static str {
        "Removes assignments to temporaries that are never used and eliminates unreachable blocks"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::mir::{
        BasicBlock, MirBinOp, MirConst, MirStatement, MirTerminator, MirValue, Temp,
    };
    use rue_ir::types::RueType;
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
                temp_types: HashMap::new(),
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
            function_signatures: HashMap::new(),
            type_context: rue_ir::types::TypeContext::new(),
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
                temp_types: HashMap::new(),
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![
                        MirStatement::Assign {
                            dest: Temp(0),
                            value: MirValue::Call {
                                func: "foo".to_string(),
                                args: vec![],
                                kind: rue_ir::mir::CallKind::Impure,
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
            function_signatures: HashMap::new(),
            type_context: rue_ir::types::TypeContext::new(),
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
