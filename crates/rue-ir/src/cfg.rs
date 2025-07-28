//! Control Flow Graph utilities for MIR
//!
//! This module provides utilities for analyzing and manipulating the control flow
//! structure of MIR functions, including predecessor computation and caching.

use crate::mir::{BasicBlock, BlockId, MirFunction, MirTerminator};
use std::collections::HashMap;

/// Control flow graph analysis for a MIR function
///
/// This struct provides efficient access to control flow relationships,
/// particularly predecessor information which is needed by many optimization passes.
#[derive(Debug)]
pub struct ControlFlowGraph {
    /// Map from block ID to list of predecessor block IDs
    predecessors: HashMap<BlockId, Vec<BlockId>>,
    /// Map from block ID to list of successor block IDs
    successors: HashMap<BlockId, Vec<BlockId>>,
}

impl ControlFlowGraph {
    /// Build a control flow graph from a MIR function
    pub fn new(func: &MirFunction) -> Self {
        let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut successors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();

        // Initialize empty lists for all blocks
        for block in &func.blocks {
            predecessors.insert(block.id, Vec::new());
            successors.insert(block.id, Vec::new());
        }

        // Compute predecessor and successor relationships
        for block in &func.blocks {
            let block_successors = Self::get_terminator_successors(&block.terminator);

            // Record successors for this block
            successors.insert(block.id, block_successors.clone());

            // Record this block as a predecessor of its successors
            for &succ in &block_successors {
                if let Some(preds) = predecessors.get_mut(&succ) {
                    preds.push(block.id);
                }
            }
        }

        Self {
            predecessors,
            successors,
        }
    }

    /// Get the predecessors of a block
    pub fn predecessors(&self, block: BlockId) -> &[BlockId] {
        self.predecessors
            .get(&block)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get the successors of a block
    pub fn successors(&self, block: BlockId) -> &[BlockId] {
        self.successors
            .get(&block)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Check if a block has any predecessors
    pub fn has_predecessors(&self, block: BlockId) -> bool {
        self.predecessors
            .get(&block)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Check if a block has any successors
    pub fn has_successors(&self, block: BlockId) -> bool {
        self.successors
            .get(&block)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Get the number of predecessors for a block
    pub fn predecessor_count(&self, block: BlockId) -> usize {
        self.predecessors.get(&block).map(|v| v.len()).unwrap_or(0)
    }

    /// Get the number of successors for a block
    pub fn successor_count(&self, block: BlockId) -> usize {
        self.successors.get(&block).map(|v| v.len()).unwrap_or(0)
    }

    /// Extract successor blocks from a terminator
    fn get_terminator_successors(term: &MirTerminator) -> Vec<BlockId> {
        match term {
            MirTerminator::Goto { target, .. } => vec![*target],
            MirTerminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            MirTerminator::Switch {
                targets, default, ..
            } => {
                let mut successors = Vec::new();
                // Add all target blocks from switch cases
                for (_, target_block, _) in targets {
                    successors.push(*target_block);
                }
                // Add default block
                successors.push(*default);
                successors
            }
            MirTerminator::Return { .. } | MirTerminator::Unreachable { .. } => vec![],
        }
    }
}

/// Extension trait for MIR functions to provide CFG analysis
pub trait MirFunctionExt {
    /// Build a control flow graph for this function
    fn cfg(&self) -> ControlFlowGraph;

    /// Find a block by ID
    fn find_block(&self, id: BlockId) -> Option<&BasicBlock>;

    /// Find a mutable block by ID
    fn find_block_mut(&mut self, id: BlockId) -> Option<&mut BasicBlock>;
}

impl MirFunctionExt for MirFunction {
    fn cfg(&self) -> ControlFlowGraph {
        ControlFlowGraph::new(self)
    }

    fn find_block(&self, id: BlockId) -> Option<&BasicBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }

    fn find_block_mut(&mut self, id: BlockId) -> Option<&mut BasicBlock> {
        self.blocks.iter_mut().find(|b| b.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{MirConst, MirStatement, MirValue, Temp};
    use crate::types::RueType;
    use rue_lexer::Span;

    #[test]
    fn test_cfg_simple_linear() {
        // Create a simple linear function: B0 -> B1 -> return
        let func = MirFunction {
            name: "test".to_string(),
            params: vec![],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            span: Span::dummy(),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Goto {
                        target: BlockId(1),
                        args: vec![],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: None,
                        span: None,
                    },
                },
            ],
        };

        let cfg = func.cfg();

        // B0 has no predecessors, one successor (B1)
        assert_eq!(cfg.predecessors(BlockId(0)), &[]);
        assert_eq!(cfg.successors(BlockId(0)), &[BlockId(1)]);

        // B1 has one predecessor (B0), no successors
        assert_eq!(cfg.predecessors(BlockId(1)), &[BlockId(0)]);
        assert_eq!(cfg.successors(BlockId(1)), &[]);
    }

    #[test]
    fn test_cfg_branching() {
        // Create a branching function: B0 -> B1 or B2 -> B3
        let func = MirFunction {
            name: "test".to_string(),
            params: vec![],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            span: Span::dummy(),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(0),
                        value: MirValue::Const(MirConst::Bool(true)),
                        span: None,
                    }],
                    terminator: MirTerminator::Branch {
                        condition: Temp(0),
                        then_block: BlockId(1),
                        then_args: vec![],
                        else_block: BlockId(2),
                        else_args: vec![],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Goto {
                        target: BlockId(3),
                        args: vec![],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Goto {
                        target: BlockId(3),
                        args: vec![],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    statements: vec![],
                    terminator: MirTerminator::Return {
                        value: None,
                        span: None,
                    },
                },
            ],
        };

        let cfg = func.cfg();

        // B0 has no predecessors, two successors
        assert_eq!(cfg.predecessors(BlockId(0)), &[]);
        assert_eq!(cfg.successors(BlockId(0)), &[BlockId(1), BlockId(2)]);

        // B1 and B2 each have B0 as predecessor
        assert_eq!(cfg.predecessors(BlockId(1)), &[BlockId(0)]);
        assert_eq!(cfg.predecessors(BlockId(2)), &[BlockId(0)]);

        // B3 has two predecessors (B1 and B2)
        let b3_preds = cfg.predecessors(BlockId(3));
        assert_eq!(b3_preds.len(), 2);
        assert!(b3_preds.contains(&BlockId(1)));
        assert!(b3_preds.contains(&BlockId(2)));
    }
}
