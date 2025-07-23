use crate::label_utils::adjust_label_ids;
use crate::{CodegenError, Instruction, LabelId, Lowering, RegisterAllocator};
use rue_ir::target::MachineInstr;
use std::collections::HashMap;

/// Shared backend driver that handles common operations between HIR and MIR compilation pipelines.
/// This eliminates code duplication for function boundary discovery, label rebasing, and stack-patching.
pub struct BackendDriver {
    runtime_instructions: Vec<MachineInstr>,
    runtime_labels: HashMap<String, u32>,
}

impl BackendDriver {
    /// Create a new BackendDriver with runtime instructions loaded
    pub fn new() -> Result<Self, CodegenError> {
        let (runtime_instructions, runtime_labels) =
            rue_runtime::generate_runtime().map_err(|e| CodegenError { message: e })?;
        Ok(Self {
            runtime_instructions,
            runtime_labels,
        })
    }

    /// Discover function boundaries by scanning for labels that correspond to function entry points
    pub fn discover_function_boundaries(
        &self,
        instructions: &[Instruction],
        function_labels: &HashMap<String, LabelId>,
    ) -> Vec<(usize, usize)> {
        let mut function_boundaries = Vec::new();
        let mut current_start = 0;

        for (i, instr) in instructions.iter().enumerate() {
            if let Instruction::Label(label_id) = instr {
                if function_labels
                    .values()
                    .any(|&LabelId(id)| id == label_id.0)
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
    ) -> (HashMap<LabelId, u32>, u32) {
        let mut ir_to_machine_labels = HashMap::new();
        let mut label_id_counter = starting_id;

        for instr in instructions {
            if let Instruction::Label(label_id) = instr {
                ir_to_machine_labels.entry(*label_id).or_insert_with(|| {
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
        function_labels: HashMap<String, LabelId>,
        boundaries: Vec<(usize, usize)>,
        ir_to_machine_labels: HashMap<LabelId, u32>,
        starting_label_id: u32,
    ) -> Result<Vec<MachineInstr>, CodegenError> {
        let mut all_machine_instructions = Vec::new();
        let mut label_id_counter = starting_label_id;

        for (start, end) in boundaries {
            let function_instrs = &instructions[start..end];

            let mut function_allocator = RegisterAllocator::new();
            let mut function_machine_instrs = Vec::new();
            let next_label_id;

            {
                let mut lowering = Lowering::new(&mut function_allocator, label_id_counter);
                lowering.set_function_labels(function_labels.clone());
                lowering.set_label_map(ir_to_machine_labels.clone());

                // Process all non-label instructions for this function
                let mut function_instrs_without_labels = Vec::new();

                for instr in function_instrs {
                    match instr {
                        Instruction::Label(label_id) => {
                            // First, lower any accumulated instructions
                            if !function_instrs_without_labels.is_empty() {
                                let machine_instrs = lowering
                                    .lower(&function_instrs_without_labels)
                                    .map_err(|e| CodegenError { message: e })?;
                                function_machine_instrs.extend(machine_instrs);
                                function_instrs_without_labels.clear();
                            }

                            // Then emit the label
                            let machine_label_id = ir_to_machine_labels[label_id];
                            function_machine_instrs.push(MachineInstr::Label {
                                id: machine_label_id,
                            });
                        }
                        _ => {
                            function_instrs_without_labels.push(instr.clone());
                        }
                    }
                }

                // Lower any remaining instructions
                if !function_instrs_without_labels.is_empty() {
                    let machine_instrs = lowering
                        .lower(&function_instrs_without_labels)
                        .map_err(|e| CodegenError { message: e })?;
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

    /// Combine runtime and user code, adjusting user code labels to account for runtime labels
    pub fn combine_runtime_and_user_code(
        &self,
        user_instructions: Vec<MachineInstr>,
    ) -> Vec<MachineInstr> {
        let mut final_instructions = Vec::new();

        // Add runtime instructions first
        final_instructions.extend(self.runtime_instructions.clone());

        // Adjust user code labels to account for runtime labels
        let runtime_label_count = self.runtime_labels.values().max().copied().unwrap_or(0) + 1;

        // Adjust all label IDs in user code
        let adjusted_user_instructions = adjust_label_ids(user_instructions, runtime_label_count);
        final_instructions.extend(adjusted_user_instructions);

        final_instructions
    }

    /// Build the final function labels map, combining runtime and user labels
    pub fn build_final_labels(
        &self,
        function_labels: &HashMap<String, LabelId>,
        ir_to_machine_labels: &HashMap<LabelId, u32>,
    ) -> HashMap<String, LabelId> {
        let mut all_function_labels = HashMap::new();

        // Add runtime labels
        for (name, &id) in &self.runtime_labels {
            all_function_labels.insert(name.clone(), LabelId(id));
        }

        // Add user function labels (adjusted)
        let runtime_label_count = self.runtime_labels.values().max().copied().unwrap_or(0) + 1;
        for (name, ir_label_id) in function_labels {
            if let Some(&machine_label_id) = ir_to_machine_labels.get(ir_label_id) {
                all_function_labels.insert(
                    name.clone(),
                    LabelId(machine_label_id + runtime_label_count),
                );
            }
        }

        all_function_labels
    }
}
