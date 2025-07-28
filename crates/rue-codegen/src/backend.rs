use crate::lowering::Lowering;
use crate::regalloc::RegisterAllocator;
use crate::{CodegenError, Instruction, Label};
use rue_ir::target::MachineInstr;
use std::collections::HashMap;

/// Backend that handles compilation from high-level IR to machine instructions.
/// Manages function boundaries, label assignment, and register allocation.
pub struct Backend {
    runtime_instructions: Vec<MachineInstr>,
    runtime_labels: HashMap<String, u32>,
}

impl Backend {
    /// Create a new Backend with runtime instructions loaded
    pub fn new() -> Result<Self, CodegenError> {
        let (runtime_instructions, runtime_labels) =
            rue_runtime::generate_runtime().map_err(|_| CodegenError::Io)?;
        Ok(Self {
            runtime_instructions,
            runtime_labels,
        })
    }

    /// Get the runtime label count
    pub fn runtime_label_count(&self) -> u32 {
        self.runtime_labels.values().max().copied().unwrap_or(0) + 1
    }

    /// Discover function boundaries by scanning for labels that correspond to function entry points
    pub fn discover_function_boundaries(
        &self,
        instructions: &[Instruction],
        function_labels: &HashMap<String, Label>,
    ) -> Vec<(usize, usize)> {
        let mut function_boundaries = Vec::new();
        let mut current_start = 0;

        for (i, instr) in instructions.iter().enumerate() {
            if let Instruction::Label(label) = instr {
                if function_labels
                    .values()
                    .any(|&func_label| func_label == *label)
                {
                    if current_start < i {
                        function_boundaries.push((current_start, i));
                    }
                    current_start = i;
                }
            }
        }
        if current_start < instructions.len() {
            function_boundaries.push((current_start, instructions.len()));
        }

        function_boundaries
    }

    /// Assign machine instruction label IDs to IR labels
    pub fn assign_label_ids(
        &self,
        instructions: &[Instruction],
        starting_id: u32,
    ) -> (HashMap<Label, u32>, u32) {
        let mut ir_to_machine_labels = HashMap::new();
        let mut label_id_counter = starting_id;

        for instr in instructions {
            if let Instruction::Label(label) = instr {
                ir_to_machine_labels.entry(*label).or_insert_with(|| {
                    let id = label_id_counter;
                    label_id_counter += 1;
                    id
                });
            }
        }

        (ir_to_machine_labels, label_id_counter)
    }

    /// Lower functions to machine instructions with proper label handling and stack patching
    pub fn lower_functions(
        &self,
        instructions: &[Instruction],
        function_labels: &HashMap<String, Label>,
        boundaries: Vec<(usize, usize)>,
        ir_to_machine_labels: &HashMap<Label, u32>,
        starting_label_id: u32,
        mir_lowerer: &crate::mir_to_instructions::MirToInstructions,
    ) -> Result<Vec<MachineInstr>, CodegenError> {
        let mut all_machine_instructions = Vec::new();
        let mut label_id_counter = starting_label_id;

        for (start, end) in boundaries {
            let function_instrs = &instructions[start..end];

            // Find the function name by looking for the first label
            let mut function_name = None;
            for instr in function_instrs {
                if let Instruction::Label(label) = instr {
                    // Find which function this label belongs to
                    for (name, &func_label) in function_labels {
                        if func_label == *label {
                            function_name = Some(name.clone());
                            break;
                        }
                    }
                    if function_name.is_some() {
                        break;
                    }
                }
            }

            let mut function_allocator = RegisterAllocator::new();

            // Set the initial stack offset based on block parameters used in this function
            if let Some(ref name) = function_name {
                let stack_offset = mir_lowerer.get_function_stack_offset(name);
                if std::env::var("RUE_DEBUG").is_ok() {
                    eprintln!("Setting initial stack offset for function '{name}': {stack_offset}");
                }
                function_allocator.set_initial_stack_offset(stack_offset);
            }

            let mut function_machine_instrs = Vec::new();
            let next_label_id;

            {
                let mut lowering = Lowering::new(&mut function_allocator, label_id_counter);
                lowering.set_label_map(ir_to_machine_labels);

                // Mark all block parameter offsets for proper handling in Store instructions
                let block_param_offsets = mir_lowerer.get_block_param_offsets();
                for offset in block_param_offsets {
                    lowering.mark_block_param_offset(offset);
                }

                // Block parameters now handled via Load/Store with "always spill" approach
                // No need to mark VRegs specially

                // Process instructions in batches between labels
                let mut batch_start = 0;

                for (i, instr) in function_instrs.iter().enumerate() {
                    if let Instruction::Label(label) = instr {
                        // Lower any instructions before this label
                        if batch_start < i {
                            let batch = &function_instrs[batch_start..i];
                            let machine_instrs =
                                lowering.lower(batch).map_err(CodegenError::Lowering)?;
                            function_machine_instrs.extend(machine_instrs);
                        }

                        // Emit the label
                        let machine_label_id = ir_to_machine_labels[label];
                        function_machine_instrs.push(MachineInstr::Label {
                            id: machine_label_id,
                        });

                        // Next batch starts after this label
                        batch_start = i + 1;
                    }
                }

                // Lower any remaining instructions after the last label
                if batch_start < function_instrs.len() {
                    let batch = &function_instrs[batch_start..];
                    let machine_instrs = lowering.lower(batch).map_err(CodegenError::Lowering)?;
                    function_machine_instrs.extend(machine_instrs);
                }

                next_label_id = lowering.next_label_id();
            }

            // Patch stack allocation with actual required space
            Lowering::patch_stack_allocation(&mut function_machine_instrs, &function_allocator);

            // Add this function's instructions to the overall list
            all_machine_instructions.extend(function_machine_instrs);

            // Update the global label counter for the next function
            label_id_counter = next_label_id;
        }

        Ok(all_machine_instructions)
    }

    /// Combine runtime and user code
    pub fn combine_runtime_and_user_code(
        &self,
        user_instructions: Vec<MachineInstr>,
    ) -> Vec<MachineInstr> {
        let mut final_instructions = Vec::new();

        // Add runtime instructions first
        final_instructions.extend(self.runtime_instructions.clone());

        // Add user instructions (labels are already correctly offset)
        final_instructions.extend(user_instructions);

        final_instructions
    }

    /// Build the final function labels map, combining runtime and user labels
    pub fn build_final_labels(
        &self,
        function_labels: &HashMap<String, Label>,
        ir_to_machine_labels: &HashMap<Label, u32>,
    ) -> HashMap<String, Label> {
        let mut all_function_labels = HashMap::new();

        // Add runtime labels
        for (name, &id) in &self.runtime_labels {
            all_function_labels.insert(name.clone(), Label::runtime(id));
        }

        // Add user function labels
        let runtime_label_count = self.runtime_labels.values().max().copied().unwrap_or(0) + 1;
        for (name, ir_label_id) in function_labels {
            if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
                // The machine_label_id already includes the runtime offset from assign_label_ids
                // Use from_machine_id to create the correct label with proper space
                all_function_labels.insert(
                    name.clone(),
                    Label::from_machine_id(machine_label_id, runtime_label_count),
                );
            }
        }

        all_function_labels
    }
}
