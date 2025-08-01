use rue_ir::pir::Label;
use rue_target::{ConditionCode, LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;

/// Convert machine instructions to AT&T assembly syntax
pub fn format_instructions_as_assembly(
    instructions: &[X8664Instr],
    function_labels: &HashMap<String, Label>,
    runtime_label_count: u32,
) -> String {
    let mut output = String::new();

    // Assembly header
    output.push_str(".section .text\n");
    output.push_str(".global _start\n\n");

    // Reverse map for labels
    let mut label_names: HashMap<u32, String> = HashMap::new();
    for (name, label) in function_labels {
        label_names.insert(label.to_machine_id(runtime_label_count), name.clone());
    }

    for instr in instructions {
        match instr {
            X8664Instr::Label { id } => {
                if let Some(name) = label_names.get(id) {
                    output.push_str(&format!("{name}:\n"));
                } else {
                    output.push_str(&format!(".L{id}:\n"));
                }
            }
            X8664Instr::MovRR { dest, src } => {
                output.push_str(&format!(
                    "    movq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::MovRI32 { dest, imm } => {
                // MovRI32 is encoded as sign-extending mov imm32 → r64 with REX.W
                // Use movq to reflect the actual 64-bit sign-extending behavior
                output.push_str(&format!("    movq ${}, %{}\n", imm, reg_name(dest)));
            }
            X8664Instr::MovRI64 { dest, imm } => {
                output.push_str(&format!("    movabsq ${}, %{}\n", imm, reg_name(dest)));
            }
            X8664Instr::MovRM { dest, base, offset } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movq (%{}), %{}\n",
                        reg_name(base),
                        reg_name(dest)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movq {:+}(%{}), %{}\n",
                        offset,
                        reg_name(base),
                        reg_name(dest)
                    ));
                }
            }
            X8664Instr::MovMR { base, offset, src } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movq %{}, (%{})\n",
                        reg_name(src),
                        reg_name(base)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movq %{}, {:+}(%{})\n",
                        reg_name(src),
                        offset,
                        reg_name(base)
                    ));
                }
            }
            X8664Instr::MovMR8 { base, offset, src } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movb %{}, (%{})\n",
                        reg_name_8(src),
                        reg_name(base)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movb %{}, {:+}(%{})\n",
                        reg_name_8(src),
                        offset,
                        reg_name(base)
                    ));
                }
            }
            X8664Instr::MovRM8 { dest, base, offset } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movb (%{}), %{}\n",
                        reg_name(base),
                        reg_name_8(dest)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movb {:+}(%{}), %{}\n",
                        offset,
                        reg_name(base),
                        reg_name_8(dest)
                    ));
                }
            }
            X8664Instr::AddRR { dest, src } => {
                output.push_str(&format!(
                    "    addq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::AddRI { dest, imm } => {
                output.push_str(&format!("    addq ${}, %{}\n", imm, reg_name(dest)));
            }
            X8664Instr::SubRR { dest, src } => {
                output.push_str(&format!(
                    "    subq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::SubRI { dest, imm } => {
                output.push_str(&format!("    subq ${}, %{}\n", imm, reg_name(dest)));
            }
            X8664Instr::ImulRR { dest, src } => {
                output.push_str(&format!(
                    "    imulq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::ImulRI { dest, imm } => {
                output.push_str(&format!("    imulq ${}, %{}\n", imm, reg_name(dest)));
            }
            X8664Instr::Idiv { divisor } => {
                output.push_str(&format!("    idivq %{}\n", reg_name(divisor)));
            }
            X8664Instr::Cqo => {
                output.push_str("    cqo\n");
            }
            X8664Instr::AndRR { dest, src } => {
                output.push_str(&format!(
                    "    andq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::Shl { dest, count: _ } => {
                output.push_str(&format!("    shlq %cl, %{}\n", reg_name(dest)));
            }
            X8664Instr::Sar { dest, count: _ } => {
                output.push_str(&format!("    sarq %cl, %{}\n", reg_name(dest)));
            }
            X8664Instr::CmpRR { left, right } => {
                output.push_str(&format!(
                    "    cmpq %{}, %{}\n",
                    reg_name(right),
                    reg_name(left)
                ));
            }
            X8664Instr::CmpRI { reg, imm } => {
                output.push_str(&format!("    cmpq ${}, %{}\n", imm, reg_name(reg)));
            }
            X8664Instr::SetCC { cc, dest } => {
                let cc_str = match cc {
                    ConditionCode::Equal => "sete",
                    ConditionCode::NotEqual => "setne",
                    ConditionCode::Less => "setl",
                    ConditionCode::LessEqual => "setle",
                    ConditionCode::Greater => "setg",
                    ConditionCode::GreaterEqual => "setge",
                };
                output.push_str(&format!("    {} %{}\n", cc_str, reg_name_8(dest)));
            }
            X8664Instr::Movzx { dest, src } => {
                output.push_str(&format!(
                    "    movzbq %{}, %{}\n",
                    reg_name_8(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::Movsxd { dest, src } => {
                output.push_str(&format!(
                    "    movslq %{}, %{}\n",
                    reg_name_32(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::Push { reg } => {
                output.push_str(&format!("    pushq %{}\n", reg_name(reg)));
            }
            X8664Instr::Pop { reg } => {
                output.push_str(&format!("    popq %{}\n", reg_name(reg)));
            }
            X8664Instr::Call { target } => {
                output.push_str(&format!("    call {target}\n"));
            }
            X8664Instr::Ret => {
                output.push_str("    ret\n");
            }
            X8664Instr::Jmp { target } => match target {
                LabelRef::Local(id) => {
                    if let Some(name) = label_names.get(id) {
                        output.push_str(&format!("    jmp {name}\n"));
                    } else {
                        output.push_str(&format!("    jmp .L{id}\n"));
                    }
                }
                LabelRef::Global(name) => {
                    output.push_str(&format!("    jmp {name}\n"));
                }
            },
            X8664Instr::JmpCC { cc, target } => {
                let cc_str = match cc {
                    ConditionCode::Equal => "je",
                    ConditionCode::NotEqual => "jne",
                    ConditionCode::Less => "jl",
                    ConditionCode::LessEqual => "jle",
                    ConditionCode::Greater => "jg",
                    ConditionCode::GreaterEqual => "jge",
                };
                match target {
                    LabelRef::Local(id) => {
                        if let Some(name) = label_names.get(id) {
                            output.push_str(&format!("    {cc_str} {name}\n"));
                        } else {
                            output.push_str(&format!("    {cc_str} .L{id}\n"));
                        }
                    }
                    LabelRef::Global(name) => {
                        output.push_str(&format!("    {cc_str} {name}\n"));
                    }
                }
            }
            X8664Instr::Syscall => {
                output.push_str("    syscall\n");
            }
            X8664Instr::AllocStack { size } => {
                // Align size to 16 bytes as done by the encoder
                let aligned_size = crate::util::align_to_16(*size as u64);
                output.push_str(&format!("    subq ${aligned_size}, %rsp\n"));
            }
            X8664Instr::LeaLabel { dest, label } => {
                output.push_str(&format!("    leaq {}(%rip), %{}\n", label, reg_name(dest)));
            }
            X8664Instr::Cld => {
                output.push_str("    cld\n");
            }
            X8664Instr::RepStosb => {
                output.push_str("    rep stosb\n");
            }
            X8664Instr::EnterFrame => {
                output.push_str("    pushq %rbp\n");
                output.push_str("    movq %rsp, %rbp\n");
            }
            X8664Instr::LeaveFrame => {
                output.push_str("    movq %rbp, %rsp\n");
                output.push_str("    popq %rbp\n");
            }
            X8664Instr::XorRR { dest, src } => {
                output.push_str(&format!(
                    "    xorq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            X8664Instr::Ud2 => {
                output.push_str("    ud2\n");
            }
            X8664Instr::DataBytes { bytes } => {
                // Emit raw data bytes using .byte directive
                for byte in bytes {
                    output.push_str(&format!("    .byte {byte}\n"));
                }
            }
            X8664Instr::ReserveBytes { count } => {
                // Reserve uninitialized space using .space directive
                output.push_str(&format!("    .space {count}\n"));
            }
        }
    }

    output
}

// Macro to generate register name tables consistently
macro_rules! register_names {
    (
        $(
            $reg:ident => { r64: $r64:literal, r32: $r32:literal, r8: $r8:literal }
        ),+ $(,)?
    ) => {
        fn reg_name(reg: &X86Register) -> &'static str {
            match reg {
                $(X86Register::$reg => $r64,)+
            }
        }

        fn reg_name_8(reg: &X86Register) -> &'static str {
            match reg {
                $(X86Register::$reg => $r8,)+
            }
        }

        fn reg_name_32(reg: &X86Register) -> &'static str {
            match reg {
                $(X86Register::$reg => $r32,)+
            }
        }
    };
}

// Define all register names in one place
register_names! {
    Rax => { r64: "rax", r32: "eax", r8: "al" },
    Rbx => { r64: "rbx", r32: "ebx", r8: "bl" },
    Rcx => { r64: "rcx", r32: "ecx", r8: "cl" },
    Rdx => { r64: "rdx", r32: "edx", r8: "dl" },
    Rsi => { r64: "rsi", r32: "esi", r8: "sil" },
    Rdi => { r64: "rdi", r32: "edi", r8: "dil" },
    Rbp => { r64: "rbp", r32: "ebp", r8: "bpl" },
    Rsp => { r64: "rsp", r32: "esp", r8: "spl" },
    R8  => { r64: "r8",  r32: "r8d", r8: "r8b" },
    R9  => { r64: "r9",  r32: "r9d", r8: "r9b" },
    R10 => { r64: "r10", r32: "r10d", r8: "r10b" },
    R11 => { r64: "r11", r32: "r11d", r8: "r11b" },
    R12 => { r64: "r12", r32: "r12d", r8: "r12b" },
    R13 => { r64: "r13", r32: "r13d", r8: "r13b" },
    R14 => { r64: "r14", r32: "r14d", r8: "r14b" },
    R15 => { r64: "r15", r32: "r15d", r8: "r15b" },
}
