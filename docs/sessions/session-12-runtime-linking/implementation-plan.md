# Runtime Linking Implementation Plan

## Problem Statement
The Rust runtime static library (.a) contains internal relocations with local/section symbols that we're currently skipping, causing segfaults due to unresolved call instructions.

## Implementation Checklist

### Phase 1: Archive & Symbol Handling
- [ ] **1.1** Fix archive extraction to properly handle all symbols
  - [ ] **1.1a** Read ALL symbols including locals and section symbols
  - [ ] **1.1b** Handle symbols with empty names (section symbols)
  - [ ] **1.1c** Preserve st_type (SECTION/FUNC/OBJECT/NOTYPE)
  - [ ] **1.1d** Keep track of symbol visibility (st_other)

- [ ] **1.2** Update symbol resolution 
  - [ ] **1.2a** Create symbol entries for STT_SECTION symbols
  - [ ] **1.2b** Map section symbols to their section base addresses
  - [ ] **1.2c** Handle SHN_COMMON symbols (allocate in .bss)

### Phase 2: Section Layout & Alignment
- [ ] **2.1** Fix section merging with proper alignment
  - [ ] **2.1a** Track max alignment for each output section
  - [ ] **2.1b** Align each input section within output section
  - [ ] **2.1c** Record input_section.out_base offset

- [ ] **2.2** Compute final virtual addresses
  - [ ] **2.2a** Assign VAs to output sections before relocations
  - [ ] **2.2b** Calculate symbol.out_va for ALL symbols
  - [ ] **2.2c** Handle section symbols (VA = section_base + out_base)

### Phase 3: Relocation Processing
- [ ] **3.1** Process ALL relocations (not just globals)
  - [ ] **3.1a** Remove skip logic for empty/local symbols
  - [ ] **3.1b** Remove skip logic for .eh_frame sections
  - [ ] **3.1c** Process relocations against section symbols

- [ ] **3.2** Implement correct relocation formulas
  - [ ] **3.2a** R_X86_64_PC32: value = (S + A) - P
  - [ ] **3.2b** R_X86_64_PLT32: treat same as PC32
  - [ ] **3.2c** R_X86_64_64: value = S + A
  - [ ] **3.2d** Use r_addend from RELA, not hardcoded -4

### Phase 4: Cleanup & Validation
- [ ] **4.1** Remove assembler workarounds
  - [ ] **4.1a** Stop auto-creating empty .rodata/.bss sections
  - [ ] **4.1b** Let linker own all section creation/merging

- [ ] **4.2** Add validation checks
  - [ ] **4.2a** Verify all relocations resolved
  - [ ] **4.2b** Check section alignments preserved
  - [ ] **4.2c** Validate call targets point to valid code

## Key Code Changes Needed

### 1. Archive Reader (`archive.rs`)
- Parse symbol table including locals
- Don't filter by symbol binding

### 2. Object Builder (`asm_object.rs`)
- Add section symbol support
- Track symbol types properly

### 3. Linker (`new_linker.rs`)
- Remove all relocation skip logic
- Implement proper section layout with alignment
- Add section symbol resolution
- Fix relocation formulas to use actual addend

### 4. Runtime Build (`rue-runtime/Cargo.toml`)
- Already has `panic = "abort"`
- Add `-C relocation-model=static` to rustflags

## Testing Strategy
1. Start with simplest program (just println)
2. Verify with `readelf -rW` (no remaining relocs)
3. Check with `objdump -d` (valid call targets)
4. Test with increasingly complex programs

## Success Criteria
- [ ] Simple println program runs without segfault
- [ ] All runtime functions callable from generated code
- [ ] No unresolved relocations in final executable
- [ ] Clean separation between runtime and codegen maintained