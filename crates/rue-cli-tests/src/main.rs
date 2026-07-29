//! End-to-end CLI integration tests for the Rue compiler (RUE-12).
//!
//! Unlike the spec suite (which exercises the compiler through the test
//! harness with pre-injected files), these tests exercise the compiler
//! **the way a user does**: real files on disk in a temp directory, the
//! actual `rue` binary invoked with relative paths and a controlled
//! environment, stdin piped to the compiled program, and stdout/exit codes
//! asserted.
//!
//! # Scope
//!
//! Keep a case here only when invoking the shipped driver is part of what the
//! case proves: argument and environment handling, filesystem/project loading,
//! linking, executable structure, process I/O, or an end-to-end runtime/codegen
//! boundary that a lower-level suite cannot exercise. Diagnostic wording and
//! rendering belong in `rue-ui-tests`; language semantics belong in `rue-spec`
//! or a focused `CompilerSession` test. Do not add a second CLI copy of a
//! regression merely to preserve its historical issue name—the dedicated case
//! is the regression test.
//!
//! # Why this exists
//!
//! The spec suite gave a false sense of health: `@import` resolution from
//! disk and large by-value aggregates were both broken in the shipped driver
//! while 100% of spec tests passed. See Linear issues RUE-12/13/14.
//!
//! # Case format
//!
//! Cases live in `crates/rue-cli-tests/cases/*.toml`:
//!
//! ```toml
//! [section]
//! id = "cli.basics"
//! name = "Basic CLI behavior"
//!
//! [[case]]
//! name = "hello_string"
//! files = [{ path = "main.rue", source = """
//! fn main() -> i32 {
//!     @dbg("Hello!");
//!     0
//! }
//! """ }]
//! stdout = "Hello!\n"
//! exit_code = 0
//! ```
//!
//! Optional fields:
//! - `args`: explicit compiler args (default: `[<first file>, "-o", "prog"]`);
//!   `${SOURCE}` expands to the resolved `source_path`
//! - `source_path`: repo-root-relative source path to compile instead of
//!   inline `files`, for cases that should pin a checked-in example/program
//! - `output`: name of produced executable (default `"prog"`)
//! - `env`: extra env vars for the compiler; the value `"${REAL_STD}"`
//!   expands to the absolute path of the repo's `std/` directory
//! - `program_args`: command-line arguments for the compiled program's run
//!   (its `argv[1..]`; `argv[0]` is the program path). Distinct from `args`,
//!   which are the compiler's flags (RUE-935)
//! - `program_env`: extra env vars for the compiled program's run, layered on
//!   the inherited environment. Distinct from `env` (the compiler's env)
//!   (RUE-935)
//! - `stdin`: piped to the compiled program when it runs
//! - `compile_fail` + `error_contains`: expect compilation failure
//! - `compile_only`: don't run the produced binary
//! - `executable_target`: validate the produced executable's bounded ELF or
//!   Mach-O structure, architecture, load commands, entry point, and resolved
//!   entry relocation against this Rue target
//! - `execute_if_native`: run the executable only when `executable_target`
//!   matches the current host; foreign matrix members remain inspection-only
//! - `compile_stdout_contains`: substrings that must appear in compiler stdout (e.g. `--emit`)
//! - `compile_stdout_not_contains`: substrings that must not appear in compiler stdout
//! - `stdout` / `stdout_contains`: assert on the program's stdout
//! - `runtime_error_contains`: assert on the program's stderr
//! - `exit_code`: expected program exit code (default 0)
//! - `contract`: named execution contract supplying scheduling class and
//!   separate compile/runtime budgets (ordinary cases default to 10s for each)
//! - `tier = "slow"` on a section or `[[automatic_example]]`: keep exhaustive
//!   or full-program large-example coverage behind the dedicated slow Buck
//!   target
//! - `known_bug = "RUE-NN"`: expected failure (xfail). An ordinary assertion
//!   failure is ignored with the bug reference. A fatal subprocess failure or
//!   unexpected pass fails loudly.
//! - `only_on = ["x86-64-linux", ...]`: run the case only on these hosts
//!   (ignored elsewhere). For behavior that depends on the host platform,
//!   e.g. whether `--target X` is a cross-compile or a native compile.
//! - `RUE_PLATFORM_CASE_SELECTION=native`: register every `only_on` case
//!   applicable to the current host and no target-independent cases. Required
//!   native CI uses this declarative selection.
//! - `differential_opt = true`: compile+run the case once per optimization
//!   level (`-O0`/`-O1`/`-O2`/`-O3`) and assert identical exit code AND stdout
//!   across all levels, catching optimizer miscompiles (RUE-236). The runner
//!   drives `-O`, so the case must not set its own `-O` in `args` and must not
//!   be `compile_fail`/`compile_only`. Give it exact `stdout`/`exit_code`.
//! - `requires_system_linker = true`: report the case as ignored when no `cc`
//!   driver is on `PATH`. For cases exercising the `--linker cc` symbolized
//!   profiling build (RUE-1173).
//! - `symbols_contain`: substrings that must each match at least one defined
//!   symbol name in the produced executable's symbol table (ELF `.symtab` /
//!   Mach-O `LC_SYMTAB`), verifying the symbolized profiling build workflow
//! - `no_symbol_table = true`: assert the produced executable carries no
//!   symbol table entries, pinning that default internal-linker output is
//!   unsymbolized (the documented motivation for the profiling workflow)
//!
//! # ICE detection
//!
//! Any compiler invocation that dies by signal or whose stderr contains a
//! Rust panic is reported as an INTERNAL COMPILER ERROR — a distinct, loud
//! failure class that a `known_bug` marker cannot turn into an expected pass.
//!
//! # Timeouts
//!
//! Both the COMPILE step and the compiled program run under phase-specific
//! wall-clock hang guards selected from the declarative `ordinary`, `slow`, and
//! `stress` profiles. Named contracts also move heavyweight cases into the
//! harness's bounded serial lane. These generous guards are correctness
//! backstops, not performance promises. If either phase runs long — an infinite loop
//! in generated code, or a compile-time hang in comptime evaluation — its whole
//! process group is killed and the diagnostic names the phase, elapsed time,
//! and budget. `compile_only = true` still skips the run entirely for sources
//! that are meant only to compile.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_target::{Arch, Target};
use rue_test_runner::{
    ExpectedFailureOutcome, KNOWN_TARGETS, PlatformCaseSelection, ShardSelector, TestFailure,
    TestResult, classify_expected_failure, compiler_command, find_dir, find_rue_binary,
    ice_message, run_with_timeout, validate_nonempty_case_corpus,
};
use serde::{Deserialize, Serialize};

use sharding::CliShardPlan;

mod sharding;

/// Build a tiny C-free static archive of pure machine code for `target`
/// (ADR-0064 C FFI proof). Each function is a leaf object produced with the
/// compiler's own `rue_linker::ObjectBuilder` (which emits one defined global
/// symbol), and the objects are wrapped in a GNU `ar` archive — the same static
/// archive shape the linker already resolves the runtime from. No C toolchain is
/// involved. The linker pulls only the members a program references, so the
/// unused members impose nothing on the `answer`-only P1 cases.
///
/// Members:
/// - `answer() -> 42` — the P1 smoke function.
/// - `ffi_i32_echo`/`ffi_i16_echo`/`ffi_u16_echo`/`ffi_u32_echo` — narrow-scalar
///   echoes for the P2 round-trips. Each returns its argument in the low bits of
///   the result register **with the bits above 32 deliberately dirtied**, so the
///   round-trip is a genuine conformance probe: only a caller that applies the
///   target-C narrow-integer extension (the `TargetCCallAbi` classifier) reads
///   the right value back.
/// - `ffi_bool_norm(int) -> _Bool` — a `_Bool` normalizer returning `x != 0` as a
///   1-byte 0/1 value, again with dirty high bits; the `false`/dirty case is the
///   load-bearing check that the boundary materializes exactly 0/1.
///
/// P3 aggregate members (ADR-0064 P3, RUE-1057) — each hand-assembled from
/// `llvm-mc` and following the target psABI exactly, so a *wrong* caller packing
/// produces WRONG OUTPUT rather than an accidental pass:
/// - `ffi_pair_combine(Pair{i32 a, i32 b}) -> i32` returns `a*1000 + b` — an
///   order-sensitive combination of a two-field struct passed by value in one
///   register (a in the low eightbyte half, b in the high half). A caller that
///   reused the native *reversed*-slot packing would swap a and b and compute a
///   different number.
/// - `ffi_triple_tail(Triple{i64 a, b, c}) -> i64` returns the TAIL field `c` of a
///   24-byte struct passed in memory (SysV byval-on-stack; AAPCS64 by reference to
///   a caller copy). Reading the tail specifically catches a wrong memory image or
///   a struct wrongly squeezed into registers.
/// - `ffi_make_pair(i32 x) -> Pair` returns `{a: x, b: x+1}` in the two result
///   registers (a low half, b high half) — the two-register aggregate return.
/// - `ffi_fill_triple(i64 x) -> Triple` returns `{a: x, b: x+1, c: x+2}` via sret:
///   the callee writes the caller storage (SysV echoes the pointer in rax; AAPCS64
///   uses the dedicated x8, no echo) — the large indirect aggregate return.
/// - `ffi_strlen(char*) -> size_t` — a `strlen`-shape length scan for the P6
///   `StrBuf`<->`char*` interop proof: it counts bytes before the NUL terminator
///   of the pointer argument, so a `StrBuf` exported via `std.c.owned_c_string`
///   round-trips its length through a real foreign call.
fn synthesize_answer_archive(target: Target) -> TestResult<Vec<u8>> {
    // A leaf function returning 42 (P1).
    let answer: Vec<u8> = match target.arch() {
        // mov eax, 42 ; ret
        Arch::X86_64 => vec![0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3],
        // movz w0, #42 ; ret
        Arch::Aarch64 => vec![0x40, 0x05, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6],
    };
    // A narrow-scalar echo: copy the low 32 bits of the argument into the result
    // register, then OR in a fixed dirty pattern in bits 32..63 so the high bits
    // no C callee is required to define are non-zero. Byte-for-byte from
    // `llvm-mc` (Intel/AArch64), see the assembly in the P2 cases' comments.
    let echo: Vec<u8> = match target.arch() {
        // mov eax, edi ; movabs r10, 0xF0F0F0F000000000 ; or rax, r10 ; ret
        Arch::X86_64 => vec![
            0x89, 0xF8, 0x49, 0xBA, 0x00, 0x00, 0x00, 0x00, 0xF0, 0xF0, 0xF0, 0xF0, 0x4C, 0x09,
            0xD0, 0xC3,
        ],
        // mov w0, w0 ; movz x10,#0xF0F0,lsl#48 ; movk x10,#0xF0F0,lsl#32
        //           ; orr x0,x0,x10 ; ret
        Arch::Aarch64 => vec![
            0xE0, 0x03, 0x00, 0x2A, 0x0A, 0x1E, 0xFE, 0xD2, 0x0A, 0x1E, 0xDE, 0xF2, 0x00, 0x00,
            0x0A, 0xAA, 0xC0, 0x03, 0x5F, 0xD6,
        ],
    };
    // A `strlen`-shape length scan over a NUL-terminated `char*` in the first
    // pointer argument (rdi / x0), returning the byte count before the `0` in the
    // result register (rax / x0). This is the P6 StrBuf<->char* interop proof
    // callee: a Rue `StrBuf` exported through `std.c.owned_c_string` is passed
    // here and its length is checked. Byte-for-byte a hand-assembled leaf loop;
    // no C toolchain is involved (matching the other members).
    let strlen: Vec<u8> = match target.arch() {
        // xor eax,eax ; L: cmp byte [rdi+rax],0 ; je +5 ; inc rax ; jmp L ; ret
        Arch::X86_64 => vec![
            0x31, 0xC0, 0x80, 0x3C, 0x07, 0x00, 0x74, 0x05, 0x48, 0xFF, 0xC0, 0xEB, 0xF5, 0xC3,
        ],
        // movz x2,#0 ; L: ldrb w3,[x0] ; cbz w3,done ; add x0,x0,#1 ; add x2,x2,#1
        //           ; b L ; done: mov x0,x2 ; ret
        Arch::Aarch64 => vec![
            0x02, 0x00, 0x80, 0xD2, 0x03, 0x00, 0x40, 0x39, 0x83, 0x00, 0x00, 0x34, 0x00, 0x04,
            0x00, 0x91, 0x42, 0x04, 0x00, 0x91, 0xFC, 0xFF, 0xFF, 0x17, 0xE0, 0x03, 0x02, 0xAA,
            0xC0, 0x03, 0x5F, 0xD6,
        ],
    };
    // A `_Bool` normalizer: al/w0 = (x != 0), then the same dirty-high pattern.
    let bool_norm: Vec<u8> = match target.arch() {
        // xor eax,eax ; test edi,edi ; setne al ; movabs r10,... ; or rax,r10 ; ret
        Arch::X86_64 => vec![
            0x31, 0xC0, 0x85, 0xFF, 0x0F, 0x95, 0xC0, 0x49, 0xBA, 0x00, 0x00, 0x00, 0x00, 0xF0,
            0xF0, 0xF0, 0xF0, 0x4C, 0x09, 0xD0, 0xC3,
        ],
        // cmp w0,#0 ; cset w0,ne ; movz x10,... ; movk x10,... ; orr x0,x0,x10 ; ret
        Arch::Aarch64 => vec![
            0x1F, 0x00, 0x00, 0x71, 0xE0, 0x07, 0x9F, 0x1A, 0x0A, 0x1E, 0xFE, 0xD2, 0x0A, 0x1E,
            0xDE, 0xF2, 0x00, 0x00, 0x0A, 0xAA, 0xC0, 0x03, 0x5F, 0xD6,
        ],
    };

    // One object (and thus one archive member) per exported symbol; the echo
    // members share their machine code but differ in symbol name so the P2
    // programs can declare a distinctly-typed signature for each.
    let echo_symbols = [
        "ffi_i32_echo",
        "ffi_i16_echo",
        "ffi_u16_echo",
        "ffi_u32_echo",
    ];
    let mut objects: Vec<(String, Vec<u8>)> = Vec::new();
    objects.push((
        "answer.o".to_string(),
        rue_linker::ObjectBuilder::new(target, "answer")
            .code(answer)
            .build(),
    ));
    for symbol in echo_symbols {
        objects.push((
            format!("{symbol}.o"),
            rue_linker::ObjectBuilder::new(target, symbol)
                .code(echo.clone())
                .build(),
        ));
    }
    objects.push((
        "ffi_bool_norm.o".to_string(),
        rue_linker::ObjectBuilder::new(target, "ffi_bool_norm")
            .code(bool_norm)
            .build(),
    ));
    objects.push((
        "ffi_strlen.o".to_string(),
        rue_linker::ObjectBuilder::new(target, "ffi_strlen")
            .code(strlen)
            .build(),
    ));

    // --- P3 aggregate members (ADR-0064 P3, RUE-1057) ------------------------

    // ffi_pair_combine(Pair{i32 a, i32 b}) -> i32 == a*1000 + b, order-sensitive.
    let pair_combine: Vec<u8> = match target.arch() {
        // mov eax,edi ; imul eax,eax,1000 ; mov rcx,rdi ; shr rcx,32 ; add eax,ecx ; ret
        Arch::X86_64 => vec![
            0x89, 0xF8, 0x69, 0xC0, 0xE8, 0x03, 0x00, 0x00, 0x48, 0x89, 0xF9, 0x48, 0xC1, 0xE9,
            0x20, 0x01, 0xC8, 0xC3,
        ],
        // mov w1,w0 ; mov w2,#1000 ; mul w1,w1,w2 ; lsr x0,x0,#32 ; add w0,w1,w0 ; ret
        Arch::Aarch64 => vec![
            0xE1, 0x03, 0x00, 0x2A, 0x02, 0x7D, 0x80, 0x52, 0x21, 0x7C, 0x02, 0x1B, 0x00, 0xFC,
            0x60, 0xD3, 0x20, 0x00, 0x00, 0x0B, 0xC0, 0x03, 0x5F, 0xD6,
        ],
    };
    // ffi_triple_tail(Triple{i64 a,b,c}) -> i64 == c (the tail).
    let triple_tail: Vec<u8> = match target.arch() {
        // SysV byval-on-stack: c is at [rsp+24] (return addr + a + b). mov rax,[rsp+24] ; ret
        Arch::X86_64 => vec![0x48, 0x8B, 0x44, 0x24, 0x18, 0xC3],
        // AAPCS64 by-reference: x0 = &Triple; ldr x0,[x0,#16] ; ret
        Arch::Aarch64 => vec![0x00, 0x08, 0x40, 0xF9, 0xC0, 0x03, 0x5F, 0xD6],
    };
    // ffi_make_pair(i32 x) -> Pair{a: x, b: x+1} in two result registers.
    let make_pair: Vec<u8> = match target.arch() {
        // mov eax,edi ; lea rcx,[rdi+1] ; shl rcx,32 ; or rax,rcx ; ret
        Arch::X86_64 => vec![
            0x89, 0xF8, 0x48, 0x8D, 0x4F, 0x01, 0x48, 0xC1, 0xE1, 0x20, 0x48, 0x09, 0xC8, 0xC3,
        ],
        // mov w1,w0 ; add w2,w0,#1 ; mov w0,w1 ; lsl x2,x2,#32 ; orr x0,x0,x2 ; ret
        Arch::Aarch64 => vec![
            0xE1, 0x03, 0x00, 0x2A, 0x02, 0x04, 0x00, 0x11, 0xE0, 0x03, 0x01, 0x2A, 0x42, 0x7C,
            0x60, 0xD3, 0x00, 0x00, 0x02, 0xAA, 0xC0, 0x03, 0x5F, 0xD6,
        ],
    };
    // ffi_fill_triple(i64 x) -> Triple{a: x, b: x+1, c: x+2} via sret.
    let fill_triple: Vec<u8> = match target.arch() {
        // SysV: rdi=sret ptr, rsi=x, echo rax=rdi.
        // mov rax,rdi ; mov [rdi],rsi ; lea rcx,[rsi+1] ; mov [rdi+8],rcx
        //             ; lea rcx,[rsi+2] ; mov [rdi+16],rcx ; ret
        Arch::X86_64 => vec![
            0x48, 0x89, 0xF8, 0x48, 0x89, 0x37, 0x48, 0x8D, 0x4E, 0x01, 0x48, 0x89, 0x4F, 0x08,
            0x48, 0x8D, 0x4E, 0x02, 0x48, 0x89, 0x4F, 0x10, 0xC3,
        ],
        // AAPCS64: x8=sret ptr (dedicated, no echo), x0=x.
        // str x0,[x8] ; add x1,x0,#1 ; str x1,[x8,#8] ; add x1,x0,#2 ; str x1,[x8,#16] ; ret
        Arch::Aarch64 => vec![
            0x00, 0x01, 0x00, 0xF9, 0x01, 0x04, 0x00, 0x91, 0x01, 0x05, 0x00, 0xF9, 0x01, 0x08,
            0x00, 0x91, 0x01, 0x09, 0x00, 0xF9, 0xC0, 0x03, 0x5F, 0xD6,
        ],
    };
    for (symbol, code) in [
        ("ffi_pair_combine", pair_combine),
        ("ffi_triple_tail", triple_tail),
        ("ffi_make_pair", make_pair),
        ("ffi_fill_triple", fill_triple),
    ] {
        objects.push((
            format!("{symbol}.o"),
            rue_linker::ObjectBuilder::new(target, symbol)
                .code(code)
                .build(),
        ));
    }

    // --- P4 export-thunk C-caller members (ADR-0064 P4, RUE-1058) ------------
    //
    // These are the *inverse* of every member above: instead of a leaf a Rue
    // program calls, each is a C-convention caller that invokes a Rue function
    // *exported* to C (`pub extern "C" fn`) through its C-ABI entry thunk. A P4
    // program imports one of these as an ordinary `extern "C"` leaf; the member
    // then calls back into the Rue export, proving a separately compiled C
    // caller reaches an exported Rue function across the target-C boundary. The
    // bytes are the `.text` of `cc`/`clang -O2` output for the callers below,
    // with the call sites left as relocations against the Rue export symbols
    // (resolved from the main program's own objects at link time), exactly as a
    // real C object references an external function.
    //
    //   long ffi_call_exports(void) {          // success driver
    //       long a = rue_add(3, 5);            // 8
    //       long b = rue_sub(10, 4);           // 6  (order-sensitive)
    //       return a * 100 + b;                // 806
    //   }
    //   long ffi_call_trap(void) {             // abort driver
    //       rue_trap(2000000000);              // overflows -> aborts (exit 101)
    //       return 999;                        // unreachable
    //   }
    let plt = |offset: u64, symbol: &str| rue_linker::CodeRelocation {
        offset,
        symbol: symbol.to_string(),
        rel_type: match target.arch() {
            Arch::X86_64 => rue_linker::RelocationType::Plt32,
            Arch::Aarch64 => rue_linker::RelocationType::Call26,
        },
        addend: match target.arch() {
            Arch::X86_64 => -4,
            Arch::Aarch64 => 0,
        },
    };
    let (call_exports_code, call_exports_relocs): (Vec<u8>, Vec<(u64, &str)>) = match target.arch()
    {
        Arch::X86_64 => (
            vec![
                0x53, 0xBE, 0x05, 0x00, 0x00, 0x00, 0xBF, 0x03, 0x00, 0x00, 0x00, 0xE8, 0x00, 0x00,
                0x00, 0x00, 0xBE, 0x04, 0x00, 0x00, 0x00, 0xBF, 0x0A, 0x00, 0x00, 0x00, 0x89, 0xC3,
                0xE8, 0x00, 0x00, 0x00, 0x00, 0x89, 0xC2, 0x48, 0x63, 0xC3, 0x5B, 0x48, 0x8D, 0x04,
                0x80, 0x48, 0x8D, 0x0C, 0x80, 0x48, 0x63, 0xC2, 0x48, 0x8D, 0x04, 0x88, 0xC3,
            ],
            vec![(12, "rue_add"), (29, "rue_sub")],
        ),
        Arch::Aarch64 => (
            vec![
                0xFD, 0x7B, 0xBE, 0xA9, 0xF3, 0x0B, 0x00, 0xF9, 0xFD, 0x03, 0x00, 0x91, 0x60, 0x00,
                0x80, 0x52, 0xA1, 0x00, 0x80, 0x52, 0x00, 0x00, 0x00, 0x94, 0xF3, 0x03, 0x00, 0x2A,
                0x40, 0x01, 0x80, 0x52, 0x81, 0x00, 0x80, 0x52, 0x00, 0x00, 0x00, 0x94, 0x09, 0x7C,
                0x40, 0x93, 0x88, 0x0C, 0x80, 0x52, 0x60, 0x26, 0x28, 0x9B, 0xF3, 0x0B, 0x40, 0xF9,
                0xFD, 0x7B, 0xC2, 0xA8, 0xC0, 0x03, 0x5F, 0xD6,
            ],
            vec![(20, "rue_add"), (36, "rue_sub")],
        ),
    };
    let (call_trap_code, call_trap_relocs): (Vec<u8>, Vec<(u64, &str)>) = match target.arch() {
        Arch::X86_64 => (
            vec![
                0x48, 0x83, 0xEC, 0x08, 0xBF, 0x00, 0x94, 0x35, 0x77, 0xE8, 0x00, 0x00, 0x00, 0x00,
                0xB8, 0xE7, 0x03, 0x00, 0x00, 0x48, 0x83, 0xC4, 0x08, 0xC3,
            ],
            vec![(10, "rue_trap")],
        ),
        Arch::Aarch64 => (
            vec![
                0xFD, 0x7B, 0xBF, 0xA9, 0xFD, 0x03, 0x00, 0x91, 0x00, 0x80, 0x92, 0x52, 0xA0, 0xE6,
                0xAE, 0x72, 0x00, 0x00, 0x00, 0x94, 0xE0, 0x7C, 0x80, 0x52, 0xFD, 0x7B, 0xC1, 0xA8,
                0xC0, 0x03, 0x5F, 0xD6,
            ],
            vec![(16, "rue_trap")],
        ),
    };
    let mut caller_objects: Vec<(String, Vec<u8>)> = Vec::new();
    for (symbol, code, relocs) in [
        ("ffi_call_exports", call_exports_code, call_exports_relocs),
        ("ffi_call_trap", call_trap_code, call_trap_relocs),
    ] {
        let mut builder = rue_linker::ObjectBuilder::new(target, symbol).code(code);
        for (offset, target_symbol) in relocs {
            builder = builder.relocation(plt(offset, target_symbol));
        }
        caller_objects.push((format!("{symbol}.o"), builder.build()));
    }
    objects.extend(caller_objects);

    let members: Vec<(&str, &[u8])> = objects
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    Ok(ar_archive(&members))
}

/// Assemble a minimal GNU `ar` archive from named object members. Only the
/// fields the internal linker's archive reader consumes are populated.
fn ar_archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"!<arch>\n");
    for (name, data) in members {
        // GNU short-name members terminate the name with `/`.
        let header_name = format!("{name}/");
        let mut header = [b' '; 60];
        let name_bytes = header_name.as_bytes();
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        write_ar_field(&mut header, 16, 12, b"0"); // mtime
        write_ar_field(&mut header, 28, 6, b"0"); // uid
        write_ar_field(&mut header, 34, 6, b"0"); // gid
        write_ar_field(&mut header, 40, 8, b"644"); // mode (octal)
        write_ar_field(&mut header, 48, 10, data.len().to_string().as_bytes()); // size
        header[58] = b'`';
        header[59] = b'\n';
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// Left-justify `value` into a fixed-width `ar` header field.
fn write_ar_field(header: &mut [u8; 60], offset: usize, width: usize, value: &[u8]) {
    let len = value.len().min(width);
    header[offset..offset + len].copy_from_slice(&value[..len]);
}
/// Possible paths for the cases directory.
const CASES_DIR_PATHS: &[&str] = &[
    "crates/rue-cli-tests/cases",
    "cases",
    "../rue-cli-tests/cases",
];

/// Possible paths for the repo's std library (for `${REAL_STD}` expansion).
const STD_DIR_PATHS: &[&str] = &["std", "../std", "../../std"];

/// Possible paths for the repo's `examples/` directory (RUE-48 smoke tests).
const EXAMPLES_DIR_PATHS: &[&str] = &["examples", "../../examples", "../examples"];

#[derive(Clone, Default)]
struct CaseTimingRecorder {
    writer: Option<Arc<Mutex<BufWriter<File>>>>,
}

#[derive(Serialize)]
struct CaseTiming<'a> {
    event: &'static str,
    name: &'a str,
    elapsed_s: f64,
}

impl CaseTimingRecorder {
    fn from_env() -> Result<Self, String> {
        let Some(path) = std::env::var_os("RUE_CLI_CASE_TIMINGS") else {
            return Ok(Self::default());
        };
        let path = PathBuf::from(path);
        let file = File::create(&path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        Ok(Self {
            writer: Some(Arc::new(Mutex::new(BufWriter::new(file)))),
        })
    }

    fn measure(
        &self,
        name: &str,
        run: impl FnOnce() -> Result<(), RunError>,
    ) -> Result<(), RunError> {
        let started = Instant::now();
        let result = run();
        if let Some(writer) = &self.writer {
            let timing = CaseTiming {
                event: "rue_cli_case_timing",
                name,
                elapsed_s: started.elapsed().as_secs_f64(),
            };
            let mut writer = writer
                .lock()
                .map_err(|_| RunError::fail("CLI timing output lock was poisoned"))?;
            serde_json::to_writer(&mut *writer, &timing)
                .map_err(|error| RunError::fail(format!("cannot write CLI timing: {error}")))?;
            writer
                .write_all(b"\n")
                .map_err(|error| RunError::fail(format!("cannot write CLI timing: {error}")))?;
            writer
                .flush()
                .map_err(|error| RunError::fail(format!("cannot flush CLI timing: {error}")))?;
        }
        result
    }
}

/// Expected outcome of compiling and running one `examples/**/*.rue` program.
///
/// Every file under `examples/` is compiled and run by the suite (see
/// [`example_trials`]); this table pins the *deterministic* ones to an exact
/// exit code and stdout so a regression that silently changes their output is
/// caught. An example NOT listed here is still smoke-tested — it must compile,
/// produce a binary, run, and exit *normally* (never die by SIGSEGV/SIGABRT) —
/// so dropping a new file into `examples/` is covered automatically without
/// editing this harness. Adding an entry here upgrades that smoke check into a
/// pinned exact-output regression test.
///
/// Exit codes are the low byte of `main()`'s return value (a process exit code
/// is 0-255), e.g. `power.rue` returns 5^6 = 15625, which exits as 15625 % 256
/// = 9.
struct ExampleExpectation {
    /// Path relative to `examples/`, using `/` separators.
    path: &'static str,
    /// Expected process exit code.
    exit_code: i32,
    /// Exact expected stdout.
    stdout: &'static str,
    /// Optional stdin to pipe to the example.
    stdin: Option<&'static str>,
}

const EXAMPLE_EXPECTATIONS: &[ExampleExpectation] = &[
    // The 2026-07-11 ambitious-dogfood programs: each is a self-checking
    // program that returns 42 exactly when every internal @assert passes.
    ExampleExpectation {
        path: "sudoku/main.rue",
        exit_code: 42,
        stdout: "19\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "bignum/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "calculator/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "tinydb/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "maze/main.rue",
        exit_code: 42,
        stdout: "125\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "dijkstra/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "hashmap/main.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    ExampleExpectation {
        path: "linear_pool.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    // Lattice, Meridian, and Caldera are large maintained Rue applications.
    // Lattice and Meridian keep their automatic smoke invocations intentionally
    // lightweight. Caldera's default invocation is its comprehensive self-test,
    // avoiding repeated compilation of the 100K-line module graph.
    ExampleExpectation {
        path: "lattice/main.rue",
        exit_code: 0,
        stdout: "usage: lattice [demo | run FILE | selftest | stress1 | stress2 | stress4 | benchmark | help]\nLattice compiles a workflow DSL, validates its graph, and simulates execution.\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "meridian/main.rue",
        exit_code: 0,
        stdout: "usage: meridian [demo | run FILE | selftest | stress1 | stress2 | stress4 | benchmark | help]\nMeridian parses SQL-like workloads and audits planning, execution, transactions, and recovery.\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "caldera/main.rue",
        exit_code: 0,
        stdout: "selftest checks=23\nfailures=0\nentities=8\nticks=4\naudit_passes=256\nsystems=192\nbehaviors=128\nrules=96\nreport_documents=64\nvm_steps=30\noracle_mismatches=0\ndeterministic_mismatches=0\ndigest=4530453528088162116\nvalid=true\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "arrays.rue",
        exit_code: 157,
        stdout: "157\n64\n12\n60\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "binary_search.rue",
        exit_code: 4,
        stdout: "4\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "collatz.rue",
        exit_code: 97,
        stdout: "27\n111\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "dbg.rue",
        exit_code: 0,
        stdout: "42\n-17\ntrue\nfalse\n70\ntrue\ntrue\n120\n0\n1\n2\n3\n4\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "fibonacci.rue",
        exit_code: 55,
        stdout: "0\n1\n1\n2\n3\n5\n8\n13\n21\n34\n55\n89\n144\n233\n377\n610\n987\n1597\n2584\n4181\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "first/option_try.rue",
        exit_code: 0,
        stdout: "triple: 12\ntriple: none\n",
        stdin: None,
    },
    // `?` propagation for Result (RUE-591, ADR-0038): Ok unwraps, Err
    // short-circuits. add_one_over(10,2)=Ok(6); add_one_over(10,0)=Err(1).
    ExampleExpectation {
        path: "first/result_try.rue",
        exit_code: 0,
        stdout: "ok: 6\nerr: 1\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "first/stats.rue",
        exit_code: 3,
        stdout: "count: 3\nsum: 13\nmax: 7\n",
        stdin: Some("7\n5\n1\n"),
    },
    ExampleExpectation {
        path: "fizzbuzz.rue",
        exit_code: 0,
        stdout: "1\n2\n1\n4\n2\n1\n7\n8\n1\n2\n11\n1\n13\n14\n3\n16\n17\n1\n19\n2\n1\n22\n23\n1\n2\n26\n1\n28\n29\n3\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "gcd.rue",
        exit_code: 21,
        stdout: "6\n1\n36\n",
        stdin: None,
    },
    // Generic fixed-capacity Stack(T, CAP) (RUE-586 dogfood): a user-defined
    // comptime generic instantiated at two types (i32 and bool) in one program —
    // specialization stress. Pops 7, true; sizes 1, 2; flag true -> 7+1+2 = 10.
    ExampleExpectation {
        path: "generic_stack.rue",
        exit_code: 10,
        stdout: "7\ntrue\n1\n2\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "generics.rue",
        exit_code: 72,
        stdout: "42\n20\n10\n100\n8\n17\n",
        stdin: None,
    },
    // Binary min-heap over ArrayBuf (RUE-586 dogfood): heapsort pops a scramble
    // in ascending order 1..9.
    ExampleExpectation {
        path: "heap.rue",
        exit_code: 9,
        stdout: "1\n2\n3\n4\n5\n6\n7\n8\n9\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "hello.rue",
        exit_code: 42,
        stdout: "",
        stdin: None,
    },
    // Conway's Game of Life (RUE-586 dogfood): a glider's population is 5 and
    // holds across all eight generations we print, so this pins both the
    // per-generation output and the final population (exit code).
    ExampleExpectation {
        path: "life.rue",
        exit_code: 5,
        stdout: "5\n5\n5\n5\n5\n5\n5\n5\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "match.rue",
        exit_code: 5,
        stdout: "5\n",
        stdin: None,
    },
    // Integer matrix library (RUE-586 dogfood): exercises a @copy struct with a
    // const-sized 2D array field + methods (the RUE-587 pattern). a * identity
    // == a, so trace(a*id) == 1+5+9 == 15; identity trace == 3.
    ExampleExpectation {
        path: "matrix.rue",
        exit_code: 15,
        stdout: "15\n3\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "power.rue",
        exit_code: 9,
        stdout: "1\n2\n1024\n243\n2401\n1024\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "primes.rue",
        exit_code: 25,
        stdout: "2\n3\n5\n7\n11\n13\n17\n19\n23\n29\n31\n37\n41\n43\n47\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "quicksort.rue",
        exit_code: 11,
        stdout: "0\n64\n34\n25\n12\n22\n11\n90\n42\n15\n77\n1\n11\n12\n15\n22\n25\n34\n42\n64\n77\n90\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "second/calculator.rue",
        exit_code: 0,
        stdout: "result: 11\n",
        stdin: Some("2 + 3 * (4 - 1)\n"),
    },
    // Tagged-union geometry (RUE-586 dogfood): enum payloads (single + multi-
    // field), match with tuple bindings, and an array of enum values. Areas
    // 12, 12, 25, 3 sum to 52.
    ExampleExpectation {
        path: "shapes.rue",
        exit_code: 52,
        stdout: "12\n12\n25\n3\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "sqrt.rue",
        exit_code: 12,
        stdout: "0\n1\n2\n2\n3\n3\n4\n10\n31\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "std/arraybuf_demo.rue",
        exit_code: 119,
        stdout: "3\n20\n99\n30\n2\n-1\n119\n",
        stdin: None,
    },
    ExampleExpectation {
        path: "structs.rue",
        exit_code: 50,
        stdout: "25\n50\n30\ntrue\n",
        stdin: None,
    },
    // The onboarding smoke test. README, CONTRIBUTING, and the tutorial's
    // installation page point new users at `scripts/rue exec examples/welcome.rue`
    // to verify their setup, so it MUST print recognizable output and exit 0 —
    // otherwise `set -e`, `&&` chains, and CI steps read a successful run as a
    // failure (RUE-517). Guarded further by test_welcome_example_is_zero_exit.
    ExampleExpectation {
        path: "welcome.rue",
        exit_code: 0,
        stdout: "1\n2\n3\n42\n",
        stdin: None,
    },
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFile {
    section: Section,
    /// Named execution contracts contributed by this file. Keeping these in
    /// the same declarative corpus as cases lets automatic examples and TOML
    /// cases share one scheduling and timeout policy.
    #[serde(default, rename = "contract")]
    contracts: HashMap<String, ExecutionContractDeclaration>,
    /// The one declarative authority for correctness hang guards. Contracts
    /// select a named profile instead of inventing raw deadlines.
    #[serde(default, rename = "timeout_profile")]
    timeout_profiles: HashMap<TimeoutProfile, HangTimeoutProfile>,
    /// Parsed here so unknown policy fields fail closed. The CI wrapper owns
    /// the whole-suite derivation from these values.
    #[serde(default)]
    timeout_policy: Option<TimeoutPolicy>,
    /// Contracts and tiers for recursively discovered examples, keyed by the
    /// path that also determines their `cli.examples::...` test name.
    #[serde(default)]
    automatic_example: Vec<AutomaticExampleContract>,
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
    /// Default execution contract for every case in this section. Individual
    /// cases may override it when only one scenario is heavyweight.
    #[serde(default)]
    contract: Option<String>,
    /// Logical execution tier for this section's explicit cases. Automatic
    /// examples declare their tier independently.
    #[serde(default)]
    tier: CliCaseTier,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CliCaseTier {
    #[default]
    Premerge,
    Slow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliCaseTierSelection {
    All,
    Premerge,
    Slow,
}

impl CliCaseTierSelection {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("") | Some("all") => Ok(Self::All),
            Some("premerge") => Ok(Self::Premerge),
            Some("slow") => Ok(Self::Slow),
            Some(other) => Err(format!(
                "unknown RUE_CLI_CASE_TIER {other:?} (expected all, premerge, or slow)"
            )),
        }
    }

    fn from_env() -> Result<Self, String> {
        match std::env::var("RUE_CLI_CASE_TIER") {
            Ok(value) => Self::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn includes(self, tier: CliCaseTier) -> bool {
        self == Self::All
            || matches!(
                (self, tier),
                (Self::Premerge, CliCaseTier::Premerge) | (Self::Slow, CliCaseTier::Slow)
            )
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExecutionClass {
    Ordinary,
    Heavyweight,
}

#[derive(Debug, Clone, Copy, Deserialize, Hash, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TimeoutProfile {
    Ordinary,
    Slow,
    Stress,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HangTimeoutProfile {
    compile_hang_timeout_ms: u64,
    runtime_hang_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TimeoutPolicy {
    expected_cost_multiplier_percent: u64,
    fixed_headroom_ms: u64,
    minimum_shard_timeout_ms: u64,
    minimum_monolith_timeout_ms: u64,
    minimum_slow_suite_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExecutionContractDeclaration {
    class: ExecutionClass,
    timeout_profile: TimeoutProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionContract {
    class: ExecutionClass,
    compile_timeout_ms: u64,
    runtime_timeout_ms: u64,
}

impl ExecutionContract {
    fn compile_timeout(&self) -> Duration {
        Duration::from_millis(self.compile_timeout_ms)
    }

    fn runtime_timeout(&self) -> Duration {
        Duration::from_millis(self.runtime_timeout_ms)
    }

    fn is_heavyweight(&self) -> bool {
        self.class == ExecutionClass::Heavyweight
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AutomaticExampleContract {
    path: String,
    contract: String,
    #[serde(default)]
    tier: CliCaseTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutomaticExampleMetadata {
    contract: ExecutionContract,
    tier: CliCaseTier,
}

#[derive(Debug)]
struct LoadedCorpus {
    files: Vec<(String, TestFile)>,
    contracts: HashMap<String, ExecutionContractDeclaration>,
    timeout_profiles: HashMap<TimeoutProfile, HangTimeoutProfile>,
    automatic_examples: Vec<AutomaticExampleContract>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    /// Human-readable explanation of what this case pins and why. Not used by
    /// the harness; it exists so case files can document intent inline.
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
    /// Named execution contract. Overrides the section default.
    #[serde(default)]
    contract: Option<String>,
    /// Files written to the temp directory before invoking the compiler.
    #[serde(default)]
    files: Vec<SourceFile>,
    /// Repo-root-relative source file to compile directly instead of copying
    /// inline source into the temp directory. Use this when a CLI case should
    /// pin a checked-in example/program rather than duplicating its source.
    #[serde(default)]
    source_path: Option<String>,
    /// Compiler arguments, relative to the temp dir (default: first file + `-o prog`).
    #[serde(default)]
    args: Option<Vec<String>>,
    /// Synthesize a tiny C-free static archive exporting `answer() -> 42` as
    /// pure machine code for the case's target, and substitute its path for the
    /// `${FFI_ARCHIVE}` token in `args` (ADR-0064 C FFI P1 proof program). The
    /// archive is produced with the compiler's own object machinery
    /// (`rue_linker::ObjectBuilder`), mirroring how the runtime archive is
    /// linked in — no C toolchain is required.
    #[serde(default)]
    ffi_answer_archive: bool,
    /// Name of the executable the compiler is expected to produce.
    #[serde(default)]
    output: Option<String>,
    /// Extra environment variables for the compiler invocation.
    #[serde(default)]
    env: HashMap<String, String>,
    /// Command-line arguments passed to the COMPILED PROGRAM when it runs
    /// (RUE-935). These become `argv[1..]`; `argv[0]` is the program path the
    /// harness invokes. Distinct from `args`, which are the compiler's flags.
    #[serde(default)]
    program_args: Vec<String>,
    /// Extra environment variables for the COMPILED PROGRAM's run (RUE-935),
    /// layered on top of the inherited environment. Distinct from `env`, which
    /// applies to the compiler invocation.
    #[serde(default)]
    program_env: HashMap<String, String>,
    /// Piped to the compiled program's stdin.
    #[serde(default)]
    stdin: Option<String>,
    /// Expect compilation to fail.
    #[serde(default)]
    compile_fail: bool,
    /// Substrings expected in the compiler's stderr when compilation fails.
    #[serde(default)]
    error_contains: Vec<String>,
    /// Compile but don't run the produced binary.
    #[serde(default)]
    compile_only: bool,
    /// Target whose executable structure must be validated after compilation.
    #[serde(default)]
    executable_target: Option<String>,
    /// Run a structurally validated executable only when it is native to this
    /// host. This lets one case cover every host-target compile pair without
    /// attempting to execute foreign machine code.
    #[serde(default)]
    execute_if_native: bool,
    /// Substrings expected in the compiler's stdout (e.g. `--emit` output).
    #[serde(default)]
    compile_stdout_contains: Vec<String>,
    /// Substrings that must NOT appear in the compiler's stdout.
    #[serde(default)]
    compile_stdout_not_contains: Vec<String>,
    /// Substrings that MUST appear in the compiler's stderr, regardless of
    /// whether compilation succeeds or fails. Use for warnings that must
    /// survive a successful compile (e.g. under `--emit`).
    #[serde(default)]
    compile_stderr_contains: Vec<String>,
    /// Substrings that must NOT appear in the compiler's stderr, regardless of
    /// whether compilation succeeds or fails. Use to guard against debug spew
    /// or leaked internal diagnostics (e.g. raw `DEBUG:` eprintln lines).
    #[serde(default)]
    compile_stderr_not_contains: Vec<String>,
    /// Exact expected program stdout.
    #[serde(default)]
    stdout: Option<String>,
    /// Substrings expected in the program's stdout.
    #[serde(default)]
    stdout_contains: Vec<String>,
    /// Substrings expected in the program's stderr (runtime panics).
    #[serde(default)]
    runtime_error_contains: Vec<String>,
    /// Expected program exit code (default 0).
    #[serde(default)]
    exit_code: Option<i32>,
    /// Expected failure: reference to the Linear issue tracking the bug.
    #[serde(default)]
    known_bug: Option<String>,
    /// Platforms the known_bug applies to (e.g. ["x86-64-linux"]). Empty
    /// means all platforms. On other platforms the case runs as a normal
    /// test. Useful for ABI bugs that manifest differently per target.
    #[serde(default)]
    known_bug_on: Vec<String>,
    /// Platforms this case runs on (e.g. ["x86-64-linux"]); elsewhere it is
    /// reported as ignored. Empty means all platforms. Use when the expected
    /// behavior itself depends on the host (e.g. `--target X` is a
    /// cross-compile on some hosts and a native compile on others).
    #[serde(default)]
    only_on: Vec<String>,
    /// Skip this case entirely.
    #[serde(default)]
    skip: bool,
    /// Opt-level differential test (RUE-236): compile+run this case once per
    /// optimization level (`-O0`, `-O1`, `-O2`, `-O3`) and assert IDENTICAL
    /// exit code AND stdout across all levels. A divergence fails the case,
    /// naming the level that differs. This catches optimizer passes that break
    /// semantics — the analogue, at the *program's* opt level, of the
    /// release-mode CI job (RUE-45) that catches `cfg(debug_assertions)`
    /// divergence in the *compiler*. `-O2`/`-O3` alias `-O1` today, so results
    /// match now; the net is set so a future divergence is caught.
    ///
    /// Marked cases must be plain compile-and-run cases: `compile_fail`,
    /// `compile_only`, and an explicit `-O` in `args` are rejected (the runner
    /// drives the opt level itself). Give the case exact `stdout` and
    /// `exit_code` so each level is also checked against the known-good result,
    /// not merely against the other levels.
    #[serde(default)]
    differential_opt: bool,
    /// Report the case as ignored when no system `cc` driver is on `PATH`.
    /// For cases exercising the `--linker cc` symbolized profiling build
    /// (RUE-1173): every supported CI host provides `cc`, but a minimal local
    /// environment without a C toolchain should skip rather than fail.
    #[serde(default)]
    requires_system_linker: bool,
    /// Substrings that must each match at least one defined symbol name in
    /// the produced executable's symbol table (ELF `.symtab` / Mach-O
    /// `LC_SYMTAB`). Verifies the symbolized profiling build keeps function
    /// symbols (RUE-1173). Incompatible with `compile_fail`.
    #[serde(default)]
    symbols_contain: Vec<String>,
    /// Assert the produced executable carries no symbol table entries. Pins
    /// that default internal-linker output is unsymbolized — the documented
    /// motivation for the `--linker cc` profiling workflow (RUE-1173).
    /// Incompatible with `compile_fail` and `symbols_contain`.
    #[serde(default)]
    no_symbol_table: bool,
}

/// What running one case produced: the compiled program's exit code and
/// stdout. For cases that don't run a program (`compile_fail`/`compile_only`),
/// `ran` is false and the other fields are empty. Used by the opt-level
/// differential runner to compare results across `-O` levels.
#[derive(Debug, Default, PartialEq, Eq)]
struct RunOutcome {
    ran: bool,
    exit_code: Option<i32>,
    stdout: String,
}

const MAX_EXECUTABLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_LOAD_ENTRIES: usize = 64;
const MAX_SECTIONS: usize = 4096;
const MAX_RELOCATIONS: usize = 1_000_000;

fn checked_region(offset: u64, size: u64, limit: usize, label: &str) -> Result<(), String> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| format!("{label} range overflows"))?;
    if end > limit as u64 {
        return Err(format!(
            "{label} range {offset:#x}..{end:#x} exceeds file size {limit:#x}"
        ));
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize, label: &str) -> Result<u16, String> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| format!("truncated {label}"))?;
    Ok(u16::from_le_bytes(raw.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32, String> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| format!("truncated {label}"))?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize, label: &str) -> Result<u64, String> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| format!("truncated {label}"))?;
    Ok(u64::from_le_bytes(raw.try_into().unwrap()))
}

fn validate_aarch64_entry_branch(
    bytes: &[u8],
    entry_offset: usize,
    entry_address: u64,
    executable_start: u64,
    executable_end: u64,
) -> Result<(), String> {
    if entry_address % 4 != 0 || entry_offset % 4 != 0 {
        return Err("AArch64 entry point is not 4-byte aligned".to_string());
    }
    for byte_offset in (0..64).step_by(4) {
        let instruction_offset = entry_offset
            .checked_add(byte_offset)
            .ok_or_else(|| "entry instruction offset overflows".to_string())?;
        let instruction = read_u32(bytes, instruction_offset, "AArch64 entry instruction")?;
        if instruction & 0xfc00_0000 != 0x9400_0000 {
            continue;
        }
        let displacement = (((instruction & 0x03ff_ffff) << 6) as i32 >> 4) as i64;
        let instruction_address = entry_address
            .checked_add(byte_offset as u64)
            .ok_or_else(|| "entry instruction address overflows".to_string())?;
        let target = instruction_address
            .checked_add_signed(displacement)
            .ok_or_else(|| "entry BL target overflows".to_string())?;
        if (executable_start..executable_end).contains(&target) {
            return Ok(());
        }
        return Err(format!(
            "AArch64 entry BL resolves outside executable code: {target:#x}"
        ));
    }
    Err("AArch64 entry contains no resolved BL instruction in its first 64 bytes".to_string())
}

fn validate_elf_executable(bytes: &[u8], target: Target) -> Result<(), String> {
    if bytes.len() < 64 || bytes.get(..4) != Some(b"\x7fELF") {
        return Err("output is not an ELF file".to_string());
    }
    if bytes[4..7] != [2, 1, 1] {
        return Err("ELF is not a little-endian ELF64 version 1 file".to_string());
    }
    if read_u16(bytes, 16, "ELF type")? != 2 {
        return Err("ELF output is not ET_EXEC".to_string());
    }
    let expected_machine = target
        .elf_machine()
        .ok_or_else(|| format!("{target} does not use ELF"))?;
    let machine = read_u16(bytes, 18, "ELF machine")?;
    if machine != expected_machine {
        return Err(format!(
            "ELF machine is {machine:#x}, expected {expected_machine:#x} for {target}"
        ));
    }
    if read_u32(bytes, 20, "ELF version")? != 1 {
        return Err("ELF header version is not 1".to_string());
    }

    let entry = read_u64(bytes, 24, "ELF entry")?;
    let phoff = read_u64(bytes, 32, "ELF program-header offset")?;
    let shoff = read_u64(bytes, 40, "ELF section-header offset")?;
    let phentsize = read_u16(bytes, 54, "ELF program-header size")? as u64;
    let phnum = read_u16(bytes, 56, "ELF program-header count")? as usize;
    let shentsize = read_u16(bytes, 58, "ELF section-header size")? as u64;
    let shnum = read_u16(bytes, 60, "ELF section-header count")? as usize;
    if phentsize != 56 || phnum == 0 || phnum > MAX_LOAD_ENTRIES {
        return Err(format!(
            "ELF has invalid bounded program-header layout: size={phentsize}, count={phnum}"
        ));
    }
    checked_region(
        phoff,
        phentsize * phnum as u64,
        bytes.len(),
        "ELF program headers",
    )?;

    let mut executable_load = None;
    let mut load_count = 0;
    for index in 0..phnum {
        let offset = (phoff + index as u64 * phentsize) as usize;
        if read_u32(bytes, offset, "ELF segment type")? != 1 {
            continue;
        }
        load_count += 1;
        let flags = read_u32(bytes, offset + 4, "ELF segment flags")?;
        let file_offset = read_u64(bytes, offset + 8, "ELF segment file offset")?;
        let virtual_address = read_u64(bytes, offset + 16, "ELF segment address")?;
        let file_size = read_u64(bytes, offset + 32, "ELF segment file size")?;
        let memory_size = read_u64(bytes, offset + 40, "ELF segment memory size")?;
        let alignment = read_u64(bytes, offset + 48, "ELF segment alignment")?;
        if file_size > memory_size {
            return Err(format!("ELF PT_LOAD {index} has filesz larger than memsz"));
        }
        checked_region(file_offset, file_size, bytes.len(), "ELF PT_LOAD")?;
        if alignment > 1
            && (!alignment.is_power_of_two()
                || virtual_address % alignment != file_offset % alignment)
        {
            return Err(format!("ELF PT_LOAD {index} has invalid alignment"));
        }
        let virtual_end = virtual_address
            .checked_add(file_size)
            .ok_or_else(|| "ELF PT_LOAD virtual range overflows".to_string())?;
        if flags & 1 != 0 && (virtual_address..virtual_end).contains(&entry) {
            let entry_offset = file_offset
                .checked_add(entry - virtual_address)
                .ok_or_else(|| "ELF entry file offset overflows".to_string())?;
            executable_load = Some((entry_offset as usize, virtual_address, virtual_end));
        }
    }
    if load_count == 0 || load_count > MAX_LOAD_ENTRIES {
        return Err(format!("ELF has invalid PT_LOAD count {load_count}"));
    }
    let (entry_offset, executable_start, executable_end) = executable_load
        .ok_or_else(|| "ELF entry is not backed by an executable PT_LOAD".to_string())?;

    if shnum > MAX_SECTIONS {
        return Err(format!(
            "ELF section count {shnum} exceeds limit {MAX_SECTIONS}"
        ));
    }
    if shnum > 0 {
        if shentsize < 64 {
            return Err("ELF section-header entries are too small".to_string());
        }
        checked_region(
            shoff,
            shentsize * shnum as u64,
            bytes.len(),
            "ELF section headers",
        )?;
        for index in 0..shnum {
            let offset = (shoff + index as u64 * shentsize) as usize;
            let kind = read_u32(bytes, offset + 4, "ELF section type")?;
            if kind == 4 || kind == 9 {
                let relocation_offset = read_u64(bytes, offset + 24, "ELF relocation offset")?;
                let relocation_size = read_u64(bytes, offset + 32, "ELF relocation size")?;
                let entry_size = read_u64(bytes, offset + 56, "ELF relocation entry size")?;
                if entry_size == 0
                    || relocation_size % entry_size != 0
                    || relocation_size / entry_size > MAX_RELOCATIONS as u64
                {
                    return Err(format!("ELF relocation section {index} is not bounded"));
                }
                checked_region(
                    relocation_offset,
                    relocation_size,
                    bytes.len(),
                    "ELF relocations",
                )?;
            }
        }
    }

    match target.arch() {
        Arch::X86_64 => {
            // Entry shim since RUE-935: `mov rdi, rsp` (capture argc/argv/envp
            // pointer) + `and rsp, -16` + `call` + `ud2`.
            let entry_bytes = bytes
                .get(entry_offset..entry_offset + 14)
                .ok_or_else(|| "truncated x86-64 entry point".to_string())?;
            if entry_bytes[..8] != [0x48, 0x89, 0xe7, 0x48, 0x83, 0xe4, 0xf0, 0xe8]
                || entry_bytes[12..] != [0x0f, 0x0b]
            {
                return Err(
                    "x86-64 entry does not contain the runtime capture/alignment/call/ud2 shim"
                        .to_string(),
                );
            }
            let displacement = i32::from_le_bytes(entry_bytes[8..12].try_into().unwrap()) as i64;
            let target_address = entry
                .checked_add(12)
                .and_then(|address| address.checked_add_signed(displacement))
                .ok_or_else(|| "x86-64 entry call target overflows".to_string())?;
            if !(executable_start..executable_end).contains(&target_address) {
                return Err(format!(
                    "x86-64 entry call resolves outside executable code: {target_address:#x}"
                ));
            }
        }
        Arch::Aarch64 => validate_aarch64_entry_branch(
            bytes,
            entry_offset,
            entry,
            executable_start,
            executable_end,
        )?,
    }
    Ok(())
}

fn validate_macho_executable(bytes: &[u8], target: Target) -> Result<(), String> {
    if target != Target::Aarch64Macos {
        return Err(format!("{target} does not use Mach-O"));
    }
    if bytes.len() < 32 || bytes.get(..4) != Some(&[0xcf, 0xfa, 0xed, 0xfe]) {
        return Err("output is not a little-endian 64-bit Mach-O file".to_string());
    }
    if read_u32(bytes, 4, "Mach-O CPU type")? != 0x0100_000c {
        return Err("Mach-O output is not arm64".to_string());
    }
    if read_u32(bytes, 12, "Mach-O file type")? != 2 {
        return Err("Mach-O output is not MH_EXECUTE".to_string());
    }
    let command_count = read_u32(bytes, 16, "Mach-O load-command count")? as usize;
    let command_bytes = read_u32(bytes, 20, "Mach-O load-command bytes")? as u64;
    if command_count == 0 || command_count > MAX_LOAD_ENTRIES {
        return Err(format!(
            "Mach-O load-command count {command_count} is not bounded"
        ));
    }
    checked_region(32, command_bytes, bytes.len(), "Mach-O load commands")?;

    let command_end = 32usize + command_bytes as usize;
    let mut command_offset = 32usize;
    let mut entry_offset = None;
    let mut executable_segment = None;
    let mut segment_count = 0;
    for index in 0..command_count {
        let command = read_u32(bytes, command_offset, "Mach-O load command")?;
        let command_size =
            read_u32(bytes, command_offset + 4, "Mach-O load-command size")? as usize;
        if command_size < 8 || command_offset + command_size > command_end {
            return Err(format!("Mach-O load command {index} has an invalid size"));
        }
        if command == 0x19 {
            segment_count += 1;
            if command_size < 72 {
                return Err("truncated Mach-O LC_SEGMENT_64".to_string());
            }
            let virtual_address = read_u64(bytes, command_offset + 24, "Mach-O segment address")?;
            let virtual_size = read_u64(bytes, command_offset + 32, "Mach-O segment size")?;
            let file_offset = read_u64(bytes, command_offset + 40, "Mach-O segment file offset")?;
            let file_size = read_u64(bytes, command_offset + 48, "Mach-O segment file size")?;
            let initial_protection =
                read_u32(bytes, command_offset + 60, "Mach-O segment protection")?;
            let section_count =
                read_u32(bytes, command_offset + 64, "Mach-O section count")? as usize;
            if file_size > virtual_size || section_count > MAX_SECTIONS {
                return Err(format!(
                    "Mach-O segment {index} has an invalid bounded layout"
                ));
            }
            checked_region(file_offset, file_size, bytes.len(), "Mach-O segment")?;
            let expected_command_size = 72usize
                .checked_add(
                    section_count
                        .checked_mul(80)
                        .ok_or_else(|| "Mach-O section table size overflows".to_string())?,
                )
                .ok_or_else(|| "Mach-O segment command size overflows".to_string())?;
            if expected_command_size > command_size {
                return Err(format!(
                    "Mach-O segment {index} has a truncated section table"
                ));
            }
            for section_index in 0..section_count {
                let section = command_offset + 72 + section_index * 80;
                let section_size = read_u64(bytes, section + 40, "Mach-O section size")?;
                let section_offset = read_u32(bytes, section + 48, "Mach-O section offset")? as u64;
                let relocation_offset =
                    read_u32(bytes, section + 56, "Mach-O relocation offset")? as u64;
                let relocation_count =
                    read_u32(bytes, section + 60, "Mach-O relocation count")? as usize;
                let flags = read_u32(bytes, section + 64, "Mach-O section flags")?;
                if flags & 0xff != 1 && section_size > 0 {
                    checked_region(section_offset, section_size, bytes.len(), "Mach-O section")?;
                }
                if relocation_count > MAX_RELOCATIONS {
                    return Err(format!(
                        "Mach-O section {section_index} has too many relocations"
                    ));
                }
                checked_region(
                    relocation_offset,
                    relocation_count as u64 * 8,
                    bytes.len(),
                    "Mach-O relocations",
                )?;
            }
            if initial_protection & 4 != 0 && file_size > 0 {
                let file_end = file_offset
                    .checked_add(file_size)
                    .ok_or_else(|| "Mach-O executable file range overflows".to_string())?;
                let virtual_end = virtual_address
                    .checked_add(file_size)
                    .ok_or_else(|| "Mach-O executable virtual range overflows".to_string())?;
                executable_segment = Some((file_offset, file_end, virtual_address, virtual_end));
            }
        } else if command == 0x8000_0028 {
            if command_size != 24 {
                return Err("Mach-O LC_MAIN has an invalid size".to_string());
            }
            entry_offset = Some(read_u64(bytes, command_offset + 8, "Mach-O entry offset")?);
        }
        command_offset += command_size;
    }
    if command_offset != command_end || segment_count == 0 || segment_count > MAX_LOAD_ENTRIES {
        return Err("Mach-O load-command layout is inconsistent".to_string());
    }
    let entry_offset = entry_offset.ok_or_else(|| "Mach-O output has no LC_MAIN".to_string())?;
    let (file_start, file_end, virtual_start, virtual_end) = executable_segment
        .ok_or_else(|| "Mach-O output has no file-backed executable segment".to_string())?;
    if !(file_start..file_end).contains(&entry_offset) {
        return Err("Mach-O LC_MAIN entry is outside executable code".to_string());
    }
    let entry_address = virtual_start
        .checked_add(entry_offset - file_start)
        .ok_or_else(|| "Mach-O entry virtual address overflows".to_string())?;
    validate_aarch64_entry_branch(
        bytes,
        entry_offset as usize,
        entry_address,
        virtual_start,
        virtual_end,
    )
}

fn validate_executable(path: &Path, target: Target) -> TestResult {
    let bytes = std::fs::read(path).map_err(|error| {
        TestFailure::assertion(format!(
            "failed to read produced executable '{}': {error}",
            path.display()
        ))
    })?;
    if bytes.len() > MAX_EXECUTABLE_BYTES {
        return Err(TestFailure::assertion(format!(
            "produced executable is {} bytes, exceeding the {} byte inspection limit",
            bytes.len(),
            MAX_EXECUTABLE_BYTES
        )));
    }
    let result = if target.is_elf() {
        validate_elf_executable(&bytes, target)
    } else {
        validate_macho_executable(&bytes, target)
    };
    result.map_err(|error| {
        TestFailure::assertion(format!(
            "produced executable failed {target} structural validation: {error}"
        ))
    })
}

/// Upper bound on symbol-table entries the harness will read, mirroring the
/// bounded-inspection posture of the structural validators above.
const MAX_SYMBOLS: usize = 1_000_000;

/// Read a NUL-terminated name out of a string table region.
fn read_strtab_name(bytes: &[u8], strtab: std::ops::Range<usize>, index: usize) -> Option<String> {
    let start = strtab.start.checked_add(index)?;
    if start >= strtab.end {
        return None;
    }
    let terminator = bytes[start..strtab.end]
        .iter()
        .position(|&byte| byte == 0)?;
    Some(String::from_utf8_lossy(&bytes[start..start + terminator]).into_owned())
}

/// Collect every defined symbol name from an ELF executable's `.symtab`.
/// An executable without section headers or without a symbol table yields an
/// empty list — exactly the internal linker's default output shape.
fn read_elf_symbol_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.len() < 64 || bytes.get(..4) != Some(b"\x7fELF") {
        return Err("output is not an ELF file".to_string());
    }
    let shoff = read_u64(bytes, 40, "ELF section-header offset")?;
    let shentsize = read_u16(bytes, 58, "ELF section-header size")? as u64;
    let shnum = read_u16(bytes, 60, "ELF section-header count")? as usize;
    if shnum == 0 {
        return Ok(Vec::new());
    }
    if shentsize < 64 || shnum > MAX_SECTIONS {
        return Err(format!(
            "ELF has invalid bounded section-header layout: size={shentsize}, count={shnum}"
        ));
    }
    checked_region(
        shoff,
        shentsize * shnum as u64,
        bytes.len(),
        "ELF section headers",
    )?;

    let section_header = |index: usize| (shoff + index as u64 * shentsize) as usize;
    let mut names = Vec::new();
    for index in 0..shnum {
        let header = section_header(index);
        // SHT_SYMTAB
        if read_u32(bytes, header + 4, "ELF section type")? != 2 {
            continue;
        }
        let symtab_offset = read_u64(bytes, header + 24, "ELF symtab offset")?;
        let symtab_size = read_u64(bytes, header + 32, "ELF symtab size")?;
        let strtab_index = read_u32(bytes, header + 40, "ELF symtab link")? as usize;
        let entry_size = read_u64(bytes, header + 56, "ELF symtab entry size")?;
        if entry_size < 24
            || symtab_size % entry_size != 0
            || symtab_size / entry_size > MAX_SYMBOLS as u64
        {
            return Err("ELF symbol table is not bounded".to_string());
        }
        checked_region(symtab_offset, symtab_size, bytes.len(), "ELF symtab")?;
        if strtab_index >= shnum {
            return Err("ELF symtab links to an out-of-range string table".to_string());
        }
        let strtab_header = section_header(strtab_index);
        let strtab_offset = read_u64(bytes, strtab_header + 24, "ELF strtab offset")?;
        let strtab_size = read_u64(bytes, strtab_header + 32, "ELF strtab size")?;
        checked_region(strtab_offset, strtab_size, bytes.len(), "ELF strtab")?;
        let strtab = strtab_offset as usize..(strtab_offset + strtab_size) as usize;

        for entry in 0..(symtab_size / entry_size) as usize {
            let symbol = (symtab_offset + entry as u64 * entry_size) as usize;
            let name_index = read_u32(bytes, symbol, "ELF symbol name")? as usize;
            let section = read_u16(bytes, symbol + 6, "ELF symbol section")?;
            // Skip SHN_UNDEF entries (imports and the null symbol): the
            // assertions speak about symbols the executable defines.
            if section == 0 {
                continue;
            }
            if let Some(name) = read_strtab_name(bytes, strtab.clone(), name_index) {
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }
    Ok(names)
}

/// Collect every defined symbol name from a Mach-O executable's `LC_SYMTAB`,
/// excluding debug stabs and undefined entries.
fn read_macho_symbol_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.len() < 32 || bytes.get(..4) != Some(&[0xcf, 0xfa, 0xed, 0xfe]) {
        return Err("output is not a little-endian 64-bit Mach-O file".to_string());
    }
    let command_count = read_u32(bytes, 16, "Mach-O load-command count")? as usize;
    let command_bytes = read_u32(bytes, 20, "Mach-O load-command bytes")? as u64;
    if command_count > MAX_LOAD_ENTRIES {
        return Err(format!(
            "Mach-O load-command count {command_count} is not bounded"
        ));
    }
    checked_region(32, command_bytes, bytes.len(), "Mach-O load commands")?;
    let command_end = 32usize + command_bytes as usize;

    let mut names = Vec::new();
    let mut command_offset = 32usize;
    for index in 0..command_count {
        let command = read_u32(bytes, command_offset, "Mach-O load command")?;
        let command_size =
            read_u32(bytes, command_offset + 4, "Mach-O load-command size")? as usize;
        if command_size < 8 || command_offset + command_size > command_end {
            return Err(format!("Mach-O load command {index} has an invalid size"));
        }
        // LC_SYMTAB
        if command == 0x2 {
            if command_size < 24 {
                return Err("truncated Mach-O LC_SYMTAB".to_string());
            }
            let symtab_offset = read_u32(bytes, command_offset + 8, "Mach-O symtab offset")? as u64;
            let symbol_count =
                read_u32(bytes, command_offset + 12, "Mach-O symbol count")? as usize;
            let strtab_offset =
                read_u32(bytes, command_offset + 16, "Mach-O strtab offset")? as u64;
            let strtab_size = read_u32(bytes, command_offset + 20, "Mach-O strtab size")? as u64;
            if symbol_count > MAX_SYMBOLS {
                return Err("Mach-O symbol table is not bounded".to_string());
            }
            checked_region(
                symtab_offset,
                symbol_count as u64 * 16,
                bytes.len(),
                "Mach-O symtab",
            )?;
            checked_region(strtab_offset, strtab_size, bytes.len(), "Mach-O strtab")?;
            let strtab = strtab_offset as usize..(strtab_offset + strtab_size) as usize;

            for entry in 0..symbol_count {
                let symbol = (symtab_offset + entry as u64 * 16) as usize;
                let name_index = read_u32(bytes, symbol, "Mach-O symbol name")? as usize;
                let n_type = bytes[symbol + 4];
                // Skip N_STAB debug entries and undefined (N_UNDF) entries.
                if n_type & 0xe0 != 0 || n_type & 0x0e == 0 {
                    continue;
                }
                if let Some(name) = read_strtab_name(bytes, strtab.clone(), name_index) {
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        }
        command_offset += command_size;
    }
    Ok(names)
}

/// Enforce a case's `symbols_contain` / `no_symbol_table` expectations against
/// the produced executable (RUE-1173).
fn check_symbol_expectations(path: &Path, target: Target, case: &Case) -> TestResult {
    let bytes = std::fs::read(path).map_err(|error| {
        TestFailure::assertion(format!(
            "failed to read produced executable '{}': {error}",
            path.display()
        ))
    })?;
    if bytes.len() > MAX_EXECUTABLE_BYTES {
        return Err(TestFailure::assertion(format!(
            "produced executable is {} bytes, exceeding the {} byte inspection limit",
            bytes.len(),
            MAX_EXECUTABLE_BYTES
        )));
    }
    let names = if target.is_elf() {
        read_elf_symbol_names(&bytes)
    } else {
        read_macho_symbol_names(&bytes)
    }
    .map_err(|error| {
        TestFailure::assertion(format!(
            "failed to read {target} executable symbol table: {error}"
        ))
    })?;

    if case.no_symbol_table {
        if !names.is_empty() {
            return Err(TestFailure::assertion(format!(
                "expected no symbol table entries, but found {} (first: {}). If the \
                 internal linker now emits symbols, update docs/process/profiling.md \
                 and this case together.",
                names.len(),
                names[0]
            )));
        }
        return Ok(());
    }

    for expected in &case.symbols_contain {
        if !names.iter().any(|name| name.contains(expected)) {
            let mut listing = names.clone();
            listing.sort();
            return Err(TestFailure::assertion(format!(
                "no defined symbol contains `{expected}`\n--- defined symbols ({}) ---\n{}",
                listing.len(),
                listing.join("\n")
            )));
        }
    }
    Ok(())
}

/// Whether a system `cc` driver is available on `PATH`, probed once per
/// process. Gates `requires_system_linker` cases (RUE-1173).
fn system_linker_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("cc")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Expand `${REAL_STD}` in env values to the absolute path of the repo's std/.
fn expand_env_value(value: &str, real_std: &Path) -> String {
    value.replace("${REAL_STD}", &real_std.to_string_lossy())
}

fn apply_case_environment(
    command: &mut Command,
    environment: &HashMap<String, String>,
    real_std: &Path,
) {
    for (key, value) in environment {
        command.env(key, expand_env_value(value, real_std));
    }
}

fn case_compiler_command(
    binary: &Path,
    args: &[String],
    directory: &Path,
    environment: &HashMap<String, String>,
    real_std: &Path,
) -> Command {
    let mut command = compiler_command(binary);
    command.args(args).current_dir(directory);
    apply_case_environment(&mut command, environment, real_std);
    command
}

fn find_repo_root(cases_dir: &Path, real_std: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("RUE_REPO_DIR") {
        return PathBuf::from(path);
    }

    let mut candidates = Vec::new();
    if let Ok(path) = cases_dir.canonicalize() {
        candidates.push(path);
    }
    if let Ok(path) = std::env::current_dir() {
        candidates.push(path);
    }
    if let Some(parent) = real_std.parent() {
        candidates.push(parent.to_path_buf());
    }

    for candidate in candidates {
        for ancestor in candidate.ancestors() {
            if ancestor.join("crates/rue-cli-tests/cases").is_dir()
                && ancestor.join("std/_std.rue").is_file()
            {
                return ancestor.to_path_buf();
            }
        }
    }

    PathBuf::from(".")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessPhase {
    Compiler,
    ProducedProgram,
}

impl ProcessPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Compiler => "compiler phase",
            Self::ProducedProgram => "produced-program phase",
        }
    }
}

/// Run one subprocess phase and replace the runner's intentionally generic
/// timeout with a phase-aware diagnostic. The measured elapsed time includes
/// process-group termination and pipe drainage, while `budget` is the exact
/// contract applied to this phase.
fn run_phase_with_timeout(
    command: Command,
    phase: ProcessPhase,
    budget: Duration,
    stdin: Option<&str>,
) -> TestResult<Output> {
    let started = Instant::now();
    run_with_timeout(command, budget, stdin).map_err(|error| {
        if error.starts_with(rue_test_runner::TIMEOUT_PREFIX) {
            TestFailure::fatal(format!(
                "{} {} timed out (elapsed={} ms, budget={} ms; process group killed)",
                rue_test_runner::TIMEOUT_PREFIX,
                phase.name(),
                started.elapsed().as_millis(),
                budget.as_millis(),
            ))
        } else {
            error.with_context(phase.name())
        }
    })
}

fn resolve_source_path(path: &str, real_std: &Path, repo_root: &Path) -> PathBuf {
    let source_path = Path::new(path);
    if source_path.is_absolute() {
        return source_path.to_path_buf();
    }

    if let Some(rest) = path.strip_prefix("examples/") {
        return find_dir("RUE_EXAMPLES_DIR", EXAMPLES_DIR_PATHS, "examples").join(rest);
    }
    if let Some(rest) = path.strip_prefix("std/") {
        return real_std.join(rest);
    }
    repo_root.join(source_path)
}

/// Compile and run one case, returning the program's outcome.
///
/// When `opt_level` is `Some("-O2")` etc., that flag is appended to the
/// compiler args so the same case can be driven across optimization levels
/// (see [`run_case_differential`]); when `None`, args are used verbatim.
fn run_case(
    case: &Case,
    contract: &ExecutionContract,
    rue_binary: &Path,
    real_std: &Path,
    repo_root: &Path,
    opt_level: Option<&str>,
) -> TestResult<RunOutcome> {
    let temp_dir = tempfile::tempdir()
        .map_err(|e| TestFailure::fatal(format!("failed to create temp dir: {}", e)))?;
    let dir = temp_dir.path();
    let source_path = case
        .source_path
        .as_ref()
        .map(|path| resolve_source_path(path, real_std, repo_root));

    // Write the case's files to disk, creating subdirectories as needed.
    for file in &case.files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                TestFailure::fatal(format!("failed to create dir for {}: {}", file.path, e))
            })?;
        }
        std::fs::write(&path, &file.source)
            .map_err(|e| TestFailure::fatal(format!("failed to write {}: {}", file.path, e)))?;
    }

    let output_name = case.output.clone().unwrap_or_else(|| "prog".to_string());

    // Default invocation mirrors what a user types: `rue main.rue -o prog`,
    // with RELATIVE paths and the temp dir as the working directory.
    let mut args: Vec<String> = match &case.args {
        Some(args) => args.clone(),
        None => {
            let first = match &source_path {
                Some(path) => path.display().to_string(),
                None => case
                    .files
                    .first()
                    .ok_or_else(|| TestFailure::assertion("case has no files"))?
                    .path
                    .clone(),
            };
            vec![first, "-o".to_string(), output_name.clone()]
        }
    };
    if let Some(source_path) = &source_path {
        let source_path = source_path.display().to_string();
        for argument in &mut args {
            if argument == "${SOURCE}" {
                *argument = source_path.clone();
            }
        }
    }
    // Synthesize the C FFI proof archive (ADR-0064 P1) and substitute its path
    // for the `${FFI_ARCHIVE}` token. The archive target matches the case's
    // `executable_target` (default host) so the emitted machine code is valid.
    if case.ffi_answer_archive {
        let archive_target = match &case.executable_target {
            Some(name) => name.parse::<Target>().map_err(|_| {
                TestFailure::assertion(format!("invalid executable_target `{name}`"))
            })?,
            None => Target::host()
                .ok_or_else(|| TestFailure::assertion("no host target for ffi_answer_archive"))?,
        };
        let archive_path = dir.join("libanswer.a");
        let archive_bytes = synthesize_answer_archive(archive_target)?;
        std::fs::write(&archive_path, &archive_bytes)
            .map_err(|e| TestFailure::fatal(format!("failed to write ffi answer archive: {e}")))?;
        let archive_path = archive_path.display().to_string();
        for argument in &mut args {
            if argument == "${FFI_ARCHIVE}" {
                *argument = archive_path.clone();
            }
        }
    }
    // For an opt-level differential run, append the level flag. Flag position
    // is irrelevant to the CLI, so this composes with either default or
    // explicit args (differential cases are validated to not carry their own
    // `-O`, so there's no conflict).
    if let Some(level) = opt_level {
        args.push(level.to_string());
    }

    let cmd = case_compiler_command(rue_binary, &args, dir, &case.env, real_std);
    // A compile-time hang (comptime/parser loop) must fail this one case rather
    // than wedge the suite. Compilation and execution intentionally have
    // separate contract budgets and diagnostics.
    let compile_output = run_phase_with_timeout(
        cmd,
        ProcessPhase::Compiler,
        contract.compile_timeout(),
        None,
    )?;

    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    let compile_stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();

    // ICE detection comes first: a compiler panic is never acceptable output,
    // even for compile_fail cases.
    if let Some(ice) = ice_message(&compile_output.status, &compile_stderr) {
        return Err(ice);
    }

    // Debug-spew / leaked-diagnostics guard runs regardless of compile outcome.
    for expected in &case.compile_stderr_contains {
        if !compile_stderr.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "compiler stderr missing expected substring: {}\n--- actual stderr ---\n{}",
                expected, compile_stderr
            )));
        }
    }

    for forbidden in &case.compile_stderr_not_contains {
        if compile_stderr.contains(forbidden) {
            return Err(TestFailure::assertion(format!(
                "compiler stderr contained forbidden substring: {}\n--- actual stderr ---\n{}",
                forbidden, compile_stderr
            )));
        }
    }

    for expected in &case.compile_stdout_contains {
        if !compile_stdout.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "compiler stdout mismatch:\n  expected to contain: {}\n--- actual stdout ---\n{}",
                expected, compile_stdout
            )));
        }
    }

    for forbidden in &case.compile_stdout_not_contains {
        if compile_stdout.contains(forbidden) {
            return Err(TestFailure::assertion(format!(
                "compiler stdout contained forbidden substring: {}\n--- actual stdout ---\n{}",
                forbidden, compile_stdout
            )));
        }
    }

    let compile_succeeded = compile_output.status.success();

    if case.compile_fail {
        if compile_succeeded {
            return Err(TestFailure::assertion(
                "expected compilation to fail, but it succeeded",
            ));
        }
        for expected in &case.error_contains {
            if !compile_stderr.contains(expected) {
                return Err(TestFailure::assertion(format!(
                    "compiler error mismatch:\n  expected stderr to contain: {}\n--- actual stderr ---\n{}",
                    expected, compile_stderr
                )));
            }
        }
        return Ok(RunOutcome::default());
    }

    if !compile_succeeded {
        return Err(TestFailure::assertion(format!(
            "compilation failed (exit: {:?}):\n--- compiler stdout ---\n{}\n--- compiler stderr ---\n{}",
            compile_output.status.code(),
            compile_stdout,
            compile_stderr
        )));
    }

    let program = dir.join(&output_name);
    if (case.executable_target.is_some() || !case.compile_only) && !program.exists() {
        return Err(TestFailure::assertion(format!(
            "compiler reported success but did not produce '{}'",
            output_name
        )));
    }

    let executable_target = case
        .executable_target
        .as_deref()
        .map(str::parse::<Target>)
        .transpose()
        .map_err(|error| TestFailure::assertion(error.to_string()))?;
    if let Some(target) = executable_target {
        validate_executable(&program, target)?;
    }

    // Symbol-table expectations (RUE-1173). The format is the case's declared
    // executable target when present, otherwise the native host the compile
    // defaulted to.
    if !case.symbols_contain.is_empty() || case.no_symbol_table {
        let symbol_target = executable_target.or_else(Target::host).ok_or_else(|| {
            TestFailure::assertion("no target available for symbol-table inspection")
        })?;
        check_symbol_expectations(&program, symbol_target, case)?;
    }

    if case.compile_only || (case.execute_if_native && executable_target != Target::host()) {
        return Ok(RunOutcome::default());
    }

    // Run the produced binary from the temp dir.

    // Run the produced binary under a per-case wall-clock timeout. An infinite
    // loop in generated code must fail this one case (as a distinct TIMEOUT
    // class), not hang the whole suite. `run_with_timeout` puts the program in
    // its own process group and kills the group on timeout.
    let mut run_cmd = Command::new(&program);
    run_cmd.current_dir(dir);
    // Runtime argv/env for the compiled program (RUE-935): `program_args`
    // become `argv[1..]` and `program_env` layers over the inherited
    // environment, so a case can exercise `std.env` against known input.
    run_cmd.args(&case.program_args);
    for (key, value) in &case.program_env {
        run_cmd.env(key, value);
    }
    let run_output = run_phase_with_timeout(
        run_cmd,
        ProcessPhase::ProducedProgram,
        contract.runtime_timeout(),
        case.stdin.as_deref(),
    )?;

    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();

    if run_output.status.code().is_none() {
        return Err(TestFailure::fatal(format!(
            "TEST PROGRAM CRASH: process killed by signal ({:?})\n--- program stderr ---\n{}",
            run_output.status, run_stderr
        )));
    }

    let expected_exit = case.exit_code.unwrap_or(0);
    let actual_exit = run_output.status.code();
    if actual_exit != Some(expected_exit) {
        return Err(TestFailure::assertion(format!(
            "program exit code mismatch:\n  expected: {}\n  actual: {:?}\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
            expected_exit, actual_exit, run_stdout, run_stderr
        )));
    }

    if let Some(expected) = &case.stdout {
        if &run_stdout != expected {
            return Err(TestFailure::assertion(format!(
                "program stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                expected, run_stdout
            )));
        }
    }

    for expected in &case.stdout_contains {
        if !run_stdout.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "program stdout mismatch:\n  expected to contain: {}\n--- actual stdout ---\n{}",
                expected, run_stdout
            )));
        }
    }

    for expected in &case.runtime_error_contains {
        if !run_stderr.contains(expected) {
            return Err(TestFailure::assertion(format!(
                "program stderr mismatch:\n  expected to contain: {}\n--- actual stderr ---\n{}",
                expected, run_stderr
            )));
        }
    }

    Ok(RunOutcome {
        ran: true,
        exit_code: actual_exit,
        stdout: run_stdout,
    })
}

/// Opt-level differential runner (RUE-236): compile+run a marked case at every
/// optimization level and assert identical exit code AND stdout across all of
/// them, so an optimizer pass that miscompiles is caught.
///
/// Each level runs through the full [`run_case`] machinery, so its declared
/// `exit_code`/`stdout` are checked at *every* level (a correctness anchor),
/// and then the levels' outcomes are cross-checked against the first level so a
/// divergence that still matches no declared value can't slip through. On a
/// mismatch the error names the diverging level.
fn run_case_differential(
    case: &Case,
    contract: &ExecutionContract,
    rue_binary: &Path,
    real_std: &Path,
    repo_root: &Path,
) -> TestResult {
    // Levels to compare. -O2/-O3 alias -O1 today; the net catches a future
    // divergence.
    const OPT_LEVELS: &[&str] = &["-O0", "-O1", "-O2", "-O3"];

    // Guard against misuse: the runner drives the opt level, so these would be
    // ambiguous or meaningless.
    if case.compile_fail || case.compile_only {
        return Err(TestFailure::assertion(
            "differential_opt case must be a compile-and-run case (not compile_fail/compile_only)",
        ));
    }
    if let Some(args) = &case.args {
        if args.iter().any(|a| a.starts_with("-O")) {
            return Err(TestFailure::assertion(
                "differential_opt case must not set its own -O flag in args (the runner drives it)",
            ));
        }
    }

    let mut baseline: Option<(&str, RunOutcome)> = None;
    for level in OPT_LEVELS {
        let outcome = run_case(case, contract, rue_binary, real_std, repo_root, Some(level))
            .map_err(|error| error.with_context(format!("at {level}")))?;
        match &baseline {
            None => baseline = Some((level, outcome)),
            Some((base_level, base)) => {
                if &outcome != base {
                    return Err(TestFailure::assertion(format!(
                        "opt-level divergence: {} produced (exit={:?}, stdout={:?}) but {} produced (exit={:?}, stdout={:?})",
                        base_level,
                        base.exit_code,
                        base.stdout,
                        level,
                        outcome.exit_code,
                        outcome.stdout
                    )));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum KnownBugDisposition {
    Ignore(String),
    Fail(String),
}

fn known_bug_disposition(bug: &str, result: TestResult) -> KnownBugDisposition {
    match classify_expected_failure(result) {
        ExpectedFailureOutcome::ExpectedFailure(error) => {
            let first_line = error.lines().next().unwrap_or("");
            KnownBugDisposition::Ignore(format!(
                "known bug {} (expected failure): {}",
                bug, first_line
            ))
        }
        ExpectedFailureOutcome::FatalFailure(error) => KnownBugDisposition::Fail(error.to_string()),
        ExpectedFailureOutcome::UnexpectedPass => KnownBugDisposition::Fail(format!(
            "test PASSED but is marked known_bug = \"{}\". If the bug is fixed, \
             remove the known_bug marker so this becomes a regression test.",
            bug
        )),
    }
}

/// Wrapper handling skip and known_bug (xfail) semantics.
fn run_case_wrapper(
    case: &Case,
    contract: &ExecutionContract,
    rue_binary: &Path,
    real_std: &Path,
    repo_root: &Path,
    ctx: RunContext<'_>,
) -> Result<(), RunError> {
    if case.skip {
        return ctx.ignore_for("marked as skip");
    }

    // The `--linker cc` profiling workflow needs a system toolchain; a host
    // without one skips rather than fails (RUE-1173).
    if case.requires_system_linker && !system_linker_available() {
        return ctx.ignore_for("no system `cc` linker driver on PATH");
    }

    // Host-dependent cases: only run on the listed platforms.
    if !case.only_on.is_empty()
        && !case
            .only_on
            .iter()
            .any(|p| p == rue_test_runner::get_host_target())
    {
        return ctx.ignore_for(format!(
            "not applicable on this host (only_on = {:?})",
            case.only_on
        ));
    }

    let result = if case.differential_opt {
        run_case_differential(case, contract, rue_binary, real_std, repo_root)
    } else {
        run_case(case, contract, rue_binary, real_std, repo_root, None).map(|_| ())
    };

    // A known_bug scoped to other platforms doesn't apply here: the case
    // runs as a normal test on this host.
    let bug_applies_here = case.known_bug.is_some()
        && (case.known_bug_on.is_empty()
            || case
                .known_bug_on
                .iter()
                .any(|p| p == rue_test_runner::get_host_target()));
    let known_bug = if bug_applies_here {
        &case.known_bug
    } else {
        &None
    };

    match (known_bug, result) {
        // Normal case.
        (None, Ok(())) => Ok(()),
        (None, Err(error)) => Err(RunError::fail(error.to_string())),
        (Some(bug), result) => match known_bug_disposition(bug, result) {
            KnownBugDisposition::Ignore(reason) => ctx.ignore_for(reason),
            KnownBugDisposition::Fail(reason) => Err(RunError::fail(reason)),
        },
    }
}

/// A `differential_opt` case's cross-level check is only meaningful if each opt
/// level is also pinned to a known-good result, so it must declare both an
/// explicit `stdout` and `exit_code`. Returns true when the case is
/// `differential_opt` but missing either — a load-time error (RUE-132).
fn differential_opt_missing_pin(case: &Case) -> bool {
    case.differential_opt && (case.stdout.is_none() || case.exit_code.is_none())
}

/// A `compile_fail` case that pins nothing about *why* compilation fails passes
/// on ANY rejection — a diagnostic for an unrelated reason, or (before ICE
/// detection) even a compiler crash. Require it to assert on the compiler's
/// output via `error_contains` or `compile_stderr_contains`, so it verifies the
/// specific error it is testing (RUE-132). Returns true when the case is
/// `compile_fail` but carries neither assertion — a load-time error.
fn compile_fail_missing_assertion(case: &Case) -> bool {
    case.compile_fail && case.error_contains.is_empty() && case.compile_stderr_contains.is_empty()
}

fn compile_fail_has_exit_code(case: &Case) -> bool {
    case.compile_fail && case.exit_code.is_some()
}

fn invalid_execute_if_native(case: &Case) -> bool {
    case.execute_if_native && (case.compile_only || case.executable_target.is_none())
}

fn invalid_executable_target(case: &Case) -> Option<&str> {
    case.executable_target
        .as_deref()
        .filter(|target| target.parse::<Target>().is_err())
}

/// Symbol-table expectations require a produced executable, so they cannot
/// accompany `compile_fail`, and asserting an empty table contradicts
/// asserting its contents (RUE-1173). Returns the load-time error, if any.
fn invalid_symbol_expectations(case: &Case) -> Option<&'static str> {
    let has_expectations = !case.symbols_contain.is_empty() || case.no_symbol_table;
    if case.compile_fail && has_expectations {
        return Some(
            "`symbols_contain`/`no_symbol_table` inspect a produced executable \
             and are incompatible with `compile_fail`",
        );
    }
    if case.no_symbol_table && !case.symbols_contain.is_empty() {
        return Some("`no_symbol_table` contradicts `symbols_contain`");
    }
    None
}

fn unknown_only_on_targets(case: &Case) -> Vec<&str> {
    case.only_on
        .iter()
        .map(String::as_str)
        .filter(|platform| !KNOWN_TARGETS.contains(platform))
        .collect()
}

fn load_cases(cases_dir: &Path) -> LoadedCorpus {
    let toml_files = rue_test_runner::discover_files(cases_dir, "toml").unwrap_or_else(|error| {
        eprintln!(
            "error: failed to discover CLI test files under {}: {error}",
            cases_dir.display()
        );
        std::process::exit(1);
    });

    let mut out = Vec::new();
    let mut contracts = HashMap::new();
    let mut contract_origins = HashMap::<String, String>::new();
    let mut timeout_profiles = HashMap::new();
    let mut timeout_profile_origins = HashMap::<TimeoutProfile, String>::new();
    let mut timeout_policy_origin = None::<String>;
    let mut automatic_examples = Vec::new();
    for path in toml_files {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: failed to read {}: {}", path.display(), e);
                std::process::exit(1);
            }
        };
        match toml::from_str::<TestFile>(&content) {
            Ok(tf) => {
                // A differential_opt case's cross-level check is trivially green
                // today (-O2/-O3 alias -O1), so it only catches a *consistently*
                // wrong program if each level is also pinned to a known-good
                // result. Require both an explicit `stdout` and `exit_code` at
                // load time so the doc comment's promise is enforced, not merely
                // documented (RUE-132).
                for case in &tf.cases {
                    if let Some(target) = invalid_executable_target(case) {
                        eprintln!(
                            "error: {}: case '{}' has unknown executable_target '{}' (known: {})",
                            path.display(),
                            case.name,
                            target,
                            Target::all_names()
                        );
                        std::process::exit(1);
                    }
                    if invalid_execute_if_native(case) {
                        eprintln!(
                            concat!(
                                "error: {}: case '{}' uses `execute_if_native`, which requires ",
                                "`executable_target` and is incompatible with `compile_only`"
                            ),
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                    if let Some(reason) = invalid_symbol_expectations(case) {
                        eprintln!(
                            "error: {}: case '{}': {}",
                            path.display(),
                            case.name,
                            reason
                        );
                        std::process::exit(1);
                    }
                    let unknown_platforms = unknown_only_on_targets(case);
                    if !unknown_platforms.is_empty() {
                        eprintln!(
                            "error: {}: case '{}' has unknown only_on platform(s): {} (known: {})",
                            path.display(),
                            case.name,
                            unknown_platforms.join(", "),
                            KNOWN_TARGETS.join(", ")
                        );
                        std::process::exit(1);
                    }
                    if compile_fail_has_exit_code(case) {
                        eprintln!(
                            concat!(
                                "error: {}: compile_fail case '{}' also declares `exit_code`, ",
                                "but no program runs for a compile failure. Remove `exit_code`."
                            ),
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                    if differential_opt_missing_pin(case) {
                        eprintln!(
                            "error: {}: differential_opt case '{}' must declare both an explicit \
                             `stdout` and `exit_code` (each opt level is checked against them, so \
                             the cross-level compare can't pass a consistently-wrong program)",
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                    // A compile_fail case with no error assertion passes on any
                    // rejection, verifying nothing about why it failed (RUE-132).
                    if compile_fail_missing_assertion(case) {
                        eprintln!(
                            "error: {}: compile_fail case '{}' declares neither `error_contains` \
                             nor `compile_stderr_contains`, so it would pass on ANY rejection. \
                             Add an assertion pinning the specific error.",
                            path.display(),
                            case.name
                        );
                        std::process::exit(1);
                    }
                }

                for (name, contract) in &tf.contracts {
                    if let Some(previous) =
                        contract_origins.insert(name.clone(), path.display().to_string())
                    {
                        eprintln!(
                            "error: {}: duplicate contract '{}' (already defined in {})",
                            path.display(),
                            name,
                            previous,
                        );
                        std::process::exit(1);
                    }
                    contracts.insert(name.clone(), contract.clone());
                }
                for (name, profile) in &tf.timeout_profiles {
                    if profile.compile_hang_timeout_ms == 0 || profile.runtime_hang_timeout_ms == 0
                    {
                        eprintln!(
                            "error: {}: timeout profile '{name:?}' must use non-zero hang guards",
                            path.display(),
                        );
                        std::process::exit(1);
                    }
                    if let Some(previous) =
                        timeout_profile_origins.insert(*name, path.display().to_string())
                    {
                        eprintln!(
                            "error: {}: duplicate timeout profile '{name:?}' (already defined in {previous})",
                            path.display(),
                        );
                        std::process::exit(1);
                    }
                    timeout_profiles.insert(*name, profile.clone());
                }
                if let Some(policy) = &tf.timeout_policy {
                    if let Some(previous) =
                        timeout_policy_origin.replace(path.display().to_string())
                    {
                        eprintln!(
                            "error: {}: duplicate timeout_policy (already defined in {previous})",
                            path.display(),
                        );
                        std::process::exit(1);
                    }
                    if policy.expected_cost_multiplier_percent < 100
                        || policy.fixed_headroom_ms == 0
                        || policy.minimum_shard_timeout_ms == 0
                        || policy.minimum_monolith_timeout_ms == 0
                        || policy.minimum_slow_suite_timeout_ms == 0
                    {
                        eprintln!(
                            "error: {}: invalid zero or sub-100 timeout_policy value",
                            path.display()
                        );
                        std::process::exit(1);
                    }
                }
                automatic_examples.extend(tf.automatic_example.iter().cloned());
                out.push((path.display().to_string(), tf));
            }
            Err(e) => {
                eprintln!("error: failed to parse {}: {}", path.display(), e);
                std::process::exit(1);
            }
        }
    }
    LoadedCorpus {
        files: out,
        contracts,
        timeout_profiles,
        automatic_examples,
    }
}

fn case_contract_name<'a>(section: &'a Section, case: &'a Case) -> Option<&'a str> {
    case.contract.as_deref().or(section.contract.as_deref())
}

fn resolve_contract(
    contracts: &HashMap<String, ExecutionContractDeclaration>,
    timeout_profiles: &HashMap<TimeoutProfile, HangTimeoutProfile>,
    name: Option<&str>,
) -> ExecutionContract {
    let declaration = name.map_or(
        ExecutionContractDeclaration {
            class: ExecutionClass::Ordinary,
            timeout_profile: TimeoutProfile::Ordinary,
        },
        |name| {
            contracts
                .get(name)
                .unwrap_or_else(|| panic!("contract '{name}' was not validated"))
                .clone()
        },
    );
    let profile = timeout_profiles
        .get(&declaration.timeout_profile)
        .unwrap_or_else(|| panic!("timeout profile was not validated"));
    ExecutionContract {
        class: declaration.class,
        compile_timeout_ms: profile.compile_hang_timeout_ms,
        runtime_timeout_ms: profile.runtime_hang_timeout_ms,
    }
}

/// Validate the contract graph against the complete, unfiltered inventory.
/// This happens before `libtest2` sees command-line filters, so a focused run
/// cannot silently lose a budget or scheduling classification.
fn validate_contract_metadata(
    corpus: &LoadedCorpus,
    discovered_examples: &HashSet<String>,
) -> Result<HashMap<String, AutomaticExampleMetadata>, String> {
    for required in [
        TimeoutProfile::Ordinary,
        TimeoutProfile::Slow,
        TimeoutProfile::Stress,
    ] {
        if !corpus.timeout_profiles.contains_key(&required) {
            return Err(format!("missing required timeout profile '{required:?}'"));
        }
    }
    for (name, contract) in &corpus.contracts {
        if !corpus
            .timeout_profiles
            .contains_key(&contract.timeout_profile)
        {
            return Err(format!(
                "execution contract '{name}' references missing timeout profile '{:?}'",
                contract.timeout_profile
            ));
        }
    }
    let mut used_contracts = HashSet::new();

    for (source, file) in &corpus.files {
        for case in &file.cases {
            if let Some(name) = case_contract_name(&file.section, case) {
                if !corpus.contracts.contains_key(name) {
                    return Err(format!(
                        "{source}: case '{}::{}' references unknown contract '{name}'",
                        file.section.id, case.name,
                    ));
                }
                used_contracts.insert(name.to_string());
            }
        }
    }

    let mut automatic_contracts = HashMap::new();
    for entry in &corpus.automatic_examples {
        if !discovered_examples.contains(&entry.path) {
            return Err(format!(
                "automatic example contract names '{}', but that example was not discovered",
                entry.path,
            ));
        }
        if !corpus.contracts.contains_key(&entry.contract) {
            return Err(format!(
                "automatic example '{}' references unknown contract '{}'",
                entry.path, entry.contract,
            ));
        }
        let contract = resolve_contract(
            &corpus.contracts,
            &corpus.timeout_profiles,
            Some(&entry.contract),
        );
        if automatic_contracts
            .insert(
                entry.path.clone(),
                AutomaticExampleMetadata {
                    contract,
                    tier: entry.tier,
                },
            )
            .is_some()
        {
            return Err(format!(
                "automatic example '{}' has more than one execution contract",
                entry.path,
            ));
        }
        used_contracts.insert(entry.contract.clone());
    }

    let mut unused = corpus
        .contracts
        .keys()
        .filter(|name| !used_contracts.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    unused.sort();
    if !unused.is_empty() {
        return Err(format!(
            "execution contract(s) are not attached to any discovered case: {}",
            unused.join(", "),
        ));
    }

    Ok(automatic_contracts)
}

/// Compile and run one `examples/**/*.rue` program end to end (RUE-48).
///
/// The real example file is compiled with the real driver (`rue <file> -o
/// prog`). The produced binary lives in a temp dir, but the compiler sees the
/// source at its repository path, so relative imports are exercised exactly as
/// they are written. A compiler panic is an ICE; a compile failure fails the
/// case. The produced binary is then run under the standard wall-clock timeout,
/// and — crucially — must exit *normally*: a program killed by a signal
/// (SIGSEGV/SIGABRT show up as a `None` exit code on unix) fails as a crash. If
/// the example is in [`EXAMPLE_EXPECTATIONS`], its exact exit code and stdout
/// are asserted; otherwise passing this "no crash" bar is enough
/// (self-maintaining smoke coverage for newly added examples).
fn run_example(
    path: &Path,
    expectation: Option<&ExampleExpectation>,
    rue_binary: &Path,
    real_std: &Path,
    contract: &ExecutionContract,
) -> TestResult {
    let temp_dir = tempfile::tempdir()
        .map_err(|e| TestFailure::fatal(format!("failed to create temp dir: {}", e)))?;
    let dir = temp_dir.path();

    let mut cmd = compiler_command(rue_binary);
    cmd.arg(path).args(["-o", "prog"]).current_dir(dir);
    cmd.env("RUE_STD_PATH", real_std);
    let compile_output = run_phase_with_timeout(
        cmd,
        ProcessPhase::Compiler,
        contract.compile_timeout(),
        None,
    )?;
    let compile_stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
    let compile_stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();

    if let Some(ice) = ice_message(&compile_output.status, &compile_stderr) {
        return Err(ice);
    }
    if !compile_output.status.success() {
        return Err(TestFailure::assertion(format!(
            "example failed to compile (exit: {:?}):\n--- compiler stdout ---\n{}\n--- compiler stderr ---\n{}",
            compile_output.status.code(),
            compile_stdout,
            compile_stderr
        )));
    }

    let program = dir.join("prog");
    if !program.exists() {
        return Err(TestFailure::assertion(
            "compiler reported success but produced no 'prog' binary",
        ));
    }

    let mut run_cmd = Command::new(&program);
    run_cmd.current_dir(dir);
    let run_output = run_phase_with_timeout(
        run_cmd,
        ProcessPhase::ProducedProgram,
        contract.runtime_timeout(),
        expectation.and_then(|exp| exp.stdin),
    )?;
    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();

    // "Runs without crashing": a normal exit yields Some(code); death by
    // signal (SIGSEGV/SIGABRT) yields None on unix.
    let actual_exit = match run_output.status.code() {
        Some(code) => code,
        None => {
            return Err(TestFailure::fatal(format!(
                "example crashed (killed by signal, status {:?})\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
                run_output.status, run_stdout, run_stderr
            )));
        }
    };

    if let Some(exp) = expectation {
        if actual_exit != exp.exit_code {
            return Err(TestFailure::assertion(format!(
                "example exit code mismatch:\n  expected: {}\n  actual: {}\n--- program stdout ---\n{}\n--- program stderr ---\n{}",
                exp.exit_code, actual_exit, run_stdout, run_stderr
            )));
        }
        if run_stdout != exp.stdout {
            return Err(TestFailure::assertion(format!(
                "example stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}",
                exp.stdout, run_stdout
            )));
        }
    }

    Ok(())
}

fn collect_example_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    // A directory that directly contains a `main.rue` is ONE root-module
    // example (RUE-424): its other `.rue` files are modules reached through
    // `@import` from that root, not standalone programs — compiling them
    // alone would fail on the missing `main`. Emit the root and stop
    // recursing into the program's subtree.
    let root = dir.join("main.rue");
    if root.is_file() {
        out.push(root);
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read examples directory '{}': {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("cannot read examples entry: {}", e))?;
        let path = entry.path();
        if path.is_dir() {
            collect_example_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rue") {
            out.push(path);
        }
    }
    Ok(())
}

fn example_relative_path(examples_dir: &Path, path: &Path) -> String {
    path.strip_prefix(examples_dir)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn example_test_name(relative_path: &str) -> String {
    let name = relative_path
        .strip_suffix(".rue")
        .unwrap_or(relative_path)
        .replace('/', "::");
    format!("cli.examples::{name}")
}

#[derive(Debug)]
struct DiscoveredExample {
    path: PathBuf,
    relative_path: String,
}

/// Discover the complete automatic-example inventory before filtering. This
/// lets execution-contract metadata be checked against reality even for a
/// focused test invocation.
fn discover_examples() -> Result<Vec<DiscoveredExample>, String> {
    let examples_dir = find_dir("RUE_EXAMPLES_DIR", EXAMPLES_DIR_PATHS, "examples");

    let mut example_files = Vec::new();
    collect_example_files(&examples_dir, &mut example_files).map_err(|error| {
        format!(
            "{} (set RUE_EXAMPLES_DIR to override '{}')",
            error,
            examples_dir.display(),
        )
    })?;
    example_files.sort();

    if example_files.is_empty() {
        return Err(format!(
            "no *.rue examples found in '{}' — RUE-48 smoke coverage is empty",
            examples_dir.display()
        ));
    }

    Ok(example_files
        .into_iter()
        .map(|path| DiscoveredExample {
            relative_path: example_relative_path(&examples_dir, &path),
            path,
        })
        .collect())
}

/// Build one automatic smoke-test trial per discovered example (RUE-48).
fn example_trials(
    examples: Vec<DiscoveredExample>,
    automatic_contracts: &HashMap<String, AutomaticExampleMetadata>,
    timeout_profiles: &HashMap<TimeoutProfile, HangTimeoutProfile>,
    case_tier: CliCaseTierSelection,
    rue_binary: &Path,
    real_std: &Path,
    shard: Option<&CliShardPlan>,
    timings: &CaseTimingRecorder,
) -> Vec<Trial> {
    let mut trials = Vec::with_capacity(examples.len());
    for example in examples {
        let path = example.path;
        let relative_path = example.relative_path;
        let metadata = automatic_contracts.get(&relative_path);
        let tier = metadata.map_or(CliCaseTier::Premerge, |metadata| metadata.tier);
        if !case_tier.includes(tier) {
            continue;
        }
        let contract = metadata
            .map(|metadata| metadata.contract.clone())
            .unwrap_or_else(|| resolve_contract(&HashMap::new(), timeout_profiles, None));
        let heavyweight = contract.is_heavyweight();
        let rue_binary = rue_binary.to_path_buf();
        let real_std = real_std.to_path_buf();
        let test_name = example_test_name(&relative_path);
        // RUE-1116: keep only the examples this shard owns (unset shard = all).
        if let Some(shard) = shard {
            if !shard.includes(&test_name) {
                continue;
            }
        }
        let timing_name = test_name.clone();
        let timings = timings.clone();
        let trial = Trial::test(test_name, move |_ctx| {
            timings.measure(&timing_name, || {
                let expectation = EXAMPLE_EXPECTATIONS
                    .iter()
                    .find(|e| e.path == relative_path);
                run_example(&path, expectation, &rue_binary, &real_std, &contract)
                    .map_err(RunError::fail)
            })
        });
        trials.push(if heavyweight {
            trial.exclusive()
        } else {
            trial
        });
    }
    trials
}

fn main() {
    // An optional `INDEX/COUNT` spec selects one cost-balanced slice of the
    // corpus so CI can fan the work across parallel runners. Unset (the
    // default, and every local/filtered run) means the whole corpus.
    let shard_selector = ShardSelector::from_env("RUE_CLI_TEST_SHARD").unwrap_or_else(|error| {
        eprintln!("error: invalid RUE_CLI_TEST_SHARD: {error}");
        std::process::exit(2);
    });
    let case_tier = CliCaseTierSelection::from_env().unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(2);
    });
    let platform_selection = PlatformCaseSelection::from_env().unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(2);
    });
    if shard_selector.is_some() && case_tier != CliCaseTierSelection::Premerge {
        eprintln!(
            "error: RUE_CLI_TEST_SHARD requires RUE_CLI_CASE_TIER=premerge \
             so shards partition exactly the required CLI inventory"
        );
        std::process::exit(2);
    }
    let timings = CaseTimingRecorder::from_env().unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(2);
    });

    // The compiler is invoked with the test's temp dir as cwd, so the binary
    // path must be absolute (find_rue_binary may return a relative path).
    let rue_binary = find_rue_binary();
    let rue_binary = rue_binary.canonicalize().unwrap_or_else(|e| {
        eprintln!(
            "error: cannot resolve rue binary '{}': {}",
            rue_binary.display(),
            e
        );
        std::process::exit(1);
    });
    let cases_dir = find_dir("RUE_CLI_CASES", CASES_DIR_PATHS, "cases");
    let real_std = find_dir("RUE_STD_DIR", STD_DIR_PATHS, "std");
    let real_std = real_std.canonicalize().unwrap_or(real_std);
    let repo_root = find_repo_root(&cases_dir, &real_std);

    let corpus = load_cases(&cases_dir);
    let examples = discover_examples().unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    });
    let discovered_example_paths = examples
        .iter()
        .map(|example| example.relative_path.clone())
        .collect::<HashSet<_>>();
    let automatic_contracts = validate_contract_metadata(&corpus, &discovered_example_paths)
        .unwrap_or_else(|error| {
            eprintln!("error: invalid CLI execution contract metadata: {error}");
            std::process::exit(1);
        });

    let mut discovered_names = corpus
        .files
        .iter()
        .flat_map(|(_, file)| {
            file.cases
                .iter()
                .filter(|case| {
                    case_tier.includes(file.section.tier)
                        && platform_selection.includes(&case.only_on)
                })
                .map(|case| format!("{}::{}", file.section.id, case.name))
        })
        .collect::<Vec<_>>();
    if platform_selection == PlatformCaseSelection::All {
        discovered_names.extend(examples.iter().filter_map(|example| {
            let tier = automatic_contracts
                .get(&example.relative_path)
                .map_or(CliCaseTier::Premerge, |metadata| metadata.tier);
            case_tier
                .includes(tier)
                .then(|| example_test_name(&example.relative_path))
        }));
    }
    let selected_discovered = discovered_names.len();
    let shard = shard_selector.map(|selector| {
        let weights_path = std::env::var_os("RUE_CLI_SHARD_WEIGHTS").unwrap_or_else(|| {
            eprintln!("error: RUE_CLI_SHARD_WEIGHTS is required when RUE_CLI_TEST_SHARD is set");
            std::process::exit(2);
        });
        CliShardPlan::load(selector, discovered_names, Path::new(&weights_path)).unwrap_or_else(
            |error| {
                eprintln!("error: invalid CLI shard plan: {error}");
                std::process::exit(2);
            },
        )
    });

    let total: usize = corpus.files.iter().map(|(_, f)| f.cases.len()).sum();
    if let Err(error) = validate_nonempty_case_corpus(&cases_dir, total, "CLI") {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
    let mut tests: Vec<Trial> = Vec::with_capacity(total);
    let timeout_profiles = corpus.timeout_profiles.clone();

    for (_, file) in corpus.files {
        if !case_tier.includes(file.section.tier) {
            continue;
        }
        let section_id = file.section.id.clone();
        for case in file.cases {
            if !platform_selection.includes(&case.only_on) {
                continue;
            }
            let contract_name = case_contract_name(&file.section, &case);
            let contract = resolve_contract(&corpus.contracts, &timeout_profiles, contract_name);
            let heavyweight = contract.is_heavyweight();
            let test_name = format!("{}::{}", section_id, case.name);
            // RUE-1116: keep only the cases this shard owns (unset shard = all).
            if let Some(shard) = &shard {
                if !shard.includes(&test_name) {
                    continue;
                }
            }
            let rue_binary = rue_binary.clone();
            let real_std = real_std.clone();
            let repo_root = repo_root.clone();
            let timing_name = test_name.clone();
            let case_timings = timings.clone();
            let trial = Trial::test(test_name, move |ctx| {
                case_timings.measure(&timing_name, || {
                    run_case_wrapper(&case, &contract, &rue_binary, &real_std, &repo_root, ctx)
                })
            });
            tests.push(if heavyweight {
                trial.exclusive()
            } else {
                trial
            });
        }
    }

    // RUE-48: compile+run every examples/**/*.rue through the real driver, so a
    // regression that breaks a shipped example (or an example referencing a
    // removed flag) can't slip past CI unnoticed.
    if platform_selection == PlatformCaseSelection::All {
        tests.extend(example_trials(
            examples,
            &automatic_contracts,
            &timeout_profiles,
            case_tier,
            &rue_binary,
            &real_std,
            shard.as_ref(),
            &timings,
        ));
    }

    if tests.is_empty() {
        eprintln!(
            "error: platform case selection {platform_selection:?} selected no CLI cases on {}",
            rue_test_runner::get_host_target()
        );
        std::process::exit(1);
    }

    if let Some(shard) = &shard {
        eprintln!(
            "cli-tests: shard {}/{} ({}) — running {} of {} discovered cases; estimated {} ms across {} assigned cases",
            shard_selector.unwrap().index(),
            shard_selector.unwrap().count(),
            shard.platform(),
            tests.len(),
            selected_discovered,
            shard.selected_estimated_load_ms(),
            shard.selected_case_count(),
        );
    }

    Harness::with_env().discover(tests).main();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_compiler(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary fake compiler directory");
        let binary = directory.path().join("rue");
        std::fs::write(&binary, script).expect("write fake compiler");
        let mut permissions = std::fs::metadata(&binary)
            .expect("fake compiler metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("make fake compiler executable");
        (directory, binary)
    }

    #[test]
    fn test_welcome_example_is_zero_exit() {
        // RUE-517: the onboarding docs (README, CONTRIBUTING, and the tutorial's
        // installation page) use `scripts/rue exec examples/welcome.rue` as the
        // "did my setup work?" health check. Because `exec` propagates the
        // program's exit code, welcome MUST exit 0 and print recognizable output
        // — otherwise a successful setup looks like a failure under `set -e`,
        // `&&` chains, or CI. Pin the invariant so the canonical onboarding
        // example can't silently regress to a nonzero exit.
        let welcome = EXAMPLE_EXPECTATIONS
            .iter()
            .find(|e| e.path == "welcome.rue")
            .expect("welcome.rue must be a pinned example expectation");
        assert_eq!(
            welcome.exit_code, 0,
            "the onboarding example must exit 0 (RUE-517)"
        );
        assert!(
            !welcome.stdout.is_empty(),
            "the onboarding example must print recognizable output (RUE-517)"
        );
    }

    fn corpus_from_toml(source: &str) -> LoadedCorpus {
        let profiles = r#"
            [timeout_profile.ordinary]
            compile_hang_timeout_ms = 180000
            runtime_hang_timeout_ms = 120000
            [timeout_profile.slow]
            compile_hang_timeout_ms = 900000
            runtime_hang_timeout_ms = 300000
            [timeout_profile.stress]
            compile_hang_timeout_ms = 1800000
            runtime_hang_timeout_ms = 600000
        "#;
        let source = format!("{profiles}\n{source}");
        let file = toml::from_str::<TestFile>(&source).expect("valid test corpus TOML");
        LoadedCorpus {
            contracts: file.contracts.clone(),
            timeout_profiles: file.timeout_profiles.clone(),
            automatic_examples: file.automatic_example.clone(),
            files: vec![("inline.toml".to_string(), file)],
        }
    }

    #[test]
    fn cli_case_tier_selection_is_explicit_and_fail_closed() {
        let all = CliCaseTierSelection::parse(None).unwrap();
        assert!(all.includes(CliCaseTier::Premerge));
        assert!(all.includes(CliCaseTier::Slow));

        let premerge = CliCaseTierSelection::parse(Some("premerge")).unwrap();
        assert!(premerge.includes(CliCaseTier::Premerge));
        assert!(!premerge.includes(CliCaseTier::Slow));

        let slow = CliCaseTierSelection::parse(Some("slow")).unwrap();
        assert!(!slow.includes(CliCaseTier::Premerge));
        assert!(slow.includes(CliCaseTier::Slow));

        assert!(CliCaseTierSelection::parse(Some("slwo")).is_err());
    }

    #[test]
    fn maintained_large_example_sections_share_their_automatic_contract() {
        for (section_id, example_path, tier_line, expected_tier) in [
            (
                "cli.examples_mosaic",
                "mosaic/main.rue",
                r#"tier = "slow""#,
                CliCaseTier::Slow,
            ),
            (
                "cli.examples_rill",
                "rill/main.rue",
                "",
                CliCaseTier::Premerge,
            ),
        ] {
            let corpus = corpus_from_toml(&format!(
                r#"
                    [section]
                    id = "{section_id}"
                    name = "maintained large example"
                    contract = "heavyweight"

                    [contract.heavyweight]
                    class = "heavyweight"
                    timeout_profile = "slow"

                    [[automatic_example]]
                    path = "{example_path}"
                    contract = "heavyweight"
                    {tier_line}

                    [[case]]
                    name = "ordinary_behavior"

                    [[case]]
                    name = "scaling_behavior"
                "#,
            ));
            let discovered = HashSet::from([example_path.to_string()]);
            let automatic = validate_contract_metadata(&corpus, &discovered).unwrap();
            let file = &corpus.files[0].1;
            for case in &file.cases {
                assert!(case.contract.is_none(), "case contract must stay inherited");
                let explicit = resolve_contract(
                    &corpus.contracts,
                    &corpus.timeout_profiles,
                    case_contract_name(&file.section, case),
                );
                assert_eq!(automatic[example_path].contract, explicit);
                assert_eq!(automatic[example_path].tier, expected_tier);
                assert!(explicit.is_heavyweight());
                assert_eq!(explicit.compile_timeout(), Duration::from_secs(900));
                assert_eq!(explicit.runtime_timeout(), Duration::from_secs(300));
            }
        }
    }

    #[test]
    fn ordinary_profile_has_generous_correctness_headroom() {
        let corpus = corpus_from_toml(
            r#"
                [section]
                id = "cli.contracts"
                name = "contracts"
            "#,
        );
        let contract = resolve_contract(&corpus.contracts, &corpus.timeout_profiles, None);
        assert!(!contract.is_heavyweight());
        assert_eq!(contract.compile_timeout(), Duration::from_secs(180));
        assert_eq!(contract.runtime_timeout(), Duration::from_secs(120));
    }

    #[test]
    fn stale_automatic_example_contract_is_rejected_before_filtering() {
        let corpus = corpus_from_toml(
            r#"
                [section]
                id = "cli.contracts"
                name = "contracts"

                [contract.heavy]
                class = "heavyweight"
                timeout_profile = "ordinary"

                [[automatic_example]]
                path = "renamed/main.rue"
                contract = "heavy"
            "#,
        );
        let error = validate_contract_metadata(&corpus, &HashSet::new()).unwrap_err();
        assert!(error.contains("renamed/main.rue"));
        assert!(error.contains("was not discovered"));
    }

    #[cfg(unix)]
    #[test]
    fn compiler_and_program_timeouts_name_distinct_phases_and_budgets() {
        let source_case = || Case {
            name: "phase_timeout".to_string(),
            files: vec![SourceFile {
                path: "main.rue".to_string(),
                source: "fn main() -> i32 { 0 }".to_string(),
            }],
            ..Default::default()
        };

        let (_directory, compiler) = fake_compiler("#!/bin/sh\nwhile :; do :; done\n");
        let compile_contract = ExecutionContract {
            class: ExecutionClass::Ordinary,
            compile_timeout_ms: 10,
            runtime_timeout_ms: 1_000,
        };
        let compile_error = run_case(
            &source_case(),
            &compile_contract,
            &compiler,
            Path::new("std"),
            Path::new("."),
            None,
        )
        .unwrap_err();
        assert!(
            compile_error.contains("TIMEOUT: compiler phase timed out"),
            "actual error: {compile_error}",
        );
        assert!(
            compile_error.contains("elapsed="),
            "actual error: {compile_error}",
        );
        assert!(
            compile_error.contains("budget=10 ms"),
            "actual error: {compile_error}",
        );

        let (_directory, compiler) = fake_compiler(
            "#!/bin/sh\nprintf '#!/bin/sh\\nwhile :; do :; done\\n' > prog\nchmod +x prog\n",
        );
        let runtime_contract = ExecutionContract {
            class: ExecutionClass::Ordinary,
            // Keep fixture setup comfortably outside the forced runtime budget
            // so loaded hosts cannot turn this into a compiler-phase timeout.
            compile_timeout_ms: rue_test_runner::DEFAULT_TIMEOUT_MS,
            runtime_timeout_ms: 10,
        };
        let runtime_error = run_case(
            &source_case(),
            &runtime_contract,
            &compiler,
            Path::new("std"),
            Path::new("."),
            None,
        )
        .unwrap_err();
        assert!(
            runtime_error.contains("TIMEOUT: produced-program phase timed out"),
            "actual error: {runtime_error}",
        );
        assert!(
            runtime_error.contains("elapsed="),
            "actual error: {runtime_error}",
        );
        assert!(
            runtime_error.contains("budget=10 ms"),
            "actual error: {runtime_error}",
        );
    }

    #[test]
    fn differential_opt_requires_stdout_and_exit_code() {
        // Missing both -> rejected.
        let mut case = Case {
            name: "c".to_string(),
            differential_opt: true,
            ..Default::default()
        };
        assert!(differential_opt_missing_pin(&case));

        // Only stdout -> still rejected (exit_code missing).
        case.stdout = Some("59\n".to_string());
        assert!(differential_opt_missing_pin(&case));

        // Only exit_code -> still rejected (stdout missing).
        case.stdout = None;
        case.exit_code = Some(59);
        assert!(differential_opt_missing_pin(&case));

        // Both present -> accepted.
        case.stdout = Some("59\n".to_string());
        assert!(!differential_opt_missing_pin(&case));
    }

    #[test]
    fn non_differential_case_not_required_to_pin() {
        // A plain case missing stdout/exit_code is fine — the requirement only
        // applies to differential_opt cases.
        let case = Case {
            name: "c".to_string(),
            differential_opt: false,
            ..Default::default()
        };
        assert!(!differential_opt_missing_pin(&case));
    }

    #[test]
    fn compile_fail_case_must_pin_an_error() {
        // Bare compile_fail -> rejected.
        let mut case = Case {
            name: "c".to_string(),
            compile_fail: true,
            ..Default::default()
        };
        assert!(compile_fail_missing_assertion(&case));

        // error_contains satisfies the guard.
        case.error_contains = vec!["[E0206]".to_string()];
        assert!(!compile_fail_missing_assertion(&case));

        // compile_stderr_contains also satisfies it (pins stderr content).
        case.error_contains = vec![];
        case.compile_stderr_contains = vec!["bad.rue:".to_string()];
        assert!(!compile_fail_missing_assertion(&case));
    }

    #[test]
    fn non_compile_fail_case_not_required_to_pin_error() {
        // A compile-and-run case needs no compile-error assertion.
        let case = Case {
            name: "c".to_string(),
            compile_fail: false,
            ..Default::default()
        };
        assert!(!compile_fail_missing_assertion(&case));
    }

    #[test]
    fn unknown_only_on_target_is_rejected() {
        let case = Case {
            name: "platform_typo".to_string(),
            only_on: vec!["x86_64-linux".to_string()],
            ..Default::default()
        };

        assert_eq!(unknown_only_on_targets(&case), vec!["x86_64-linux"]);
    }

    #[test]
    fn known_only_on_targets_are_accepted() {
        let case = Case {
            name: "known_platforms".to_string(),
            only_on: KNOWN_TARGETS
                .iter()
                .map(|target| (*target).to_string())
                .collect(),
            ..Default::default()
        };

        assert!(unknown_only_on_targets(&case).is_empty());
    }

    #[test]
    fn compile_fail_case_rejects_runtime_exit_code() {
        let case = Case {
            name: "ignored_exit".to_string(),
            compile_fail: true,
            exit_code: Some(1),
            ..Default::default()
        };

        assert!(compile_fail_has_exit_code(&case));
    }

    #[test]
    fn symbol_expectations_reject_contradictory_configurations() {
        // Symbol assertions need a produced executable.
        let with_compile_fail = Case {
            name: "symbols_vs_compile_fail".to_string(),
            compile_fail: true,
            symbols_contain: vec!["main".to_string()],
            ..Default::default()
        };
        assert!(invalid_symbol_expectations(&with_compile_fail).is_some());

        // Asserting emptiness and contents at once is contradictory.
        let contradictory = Case {
            name: "empty_vs_contents".to_string(),
            no_symbol_table: true,
            symbols_contain: vec!["main".to_string()],
            ..Default::default()
        };
        assert!(invalid_symbol_expectations(&contradictory).is_some());

        let symbolized = Case {
            name: "symbolized".to_string(),
            symbols_contain: vec!["main".to_string()],
            ..Default::default()
        };
        assert!(invalid_symbol_expectations(&symbolized).is_none());

        let unsymbolized = Case {
            name: "unsymbolized".to_string(),
            no_symbol_table: true,
            ..Default::default()
        };
        assert!(invalid_symbol_expectations(&unsymbolized).is_none());
    }

    #[test]
    fn elf_symbol_reader_finds_defined_symbols() {
        // An ObjectBuilder object carries exactly one defined global symbol in
        // an ELF `.symtab`; the reader must surface it and nothing spurious.
        let object = rue_linker::ObjectBuilder::new(Target::X86_64Linux, "profiling_probe")
            .code(vec![0; 4])
            .build();
        let names = read_elf_symbol_names(&object).expect("readable ELF symtab");
        assert!(
            names.iter().any(|name| name == "profiling_probe"),
            "defined symbol missing from {names:?}"
        );
    }

    #[test]
    fn macho_symbol_reader_finds_defined_symbols() {
        // Mach-O emission prepends exactly one `_` to the symbol name.
        let object = rue_linker::ObjectBuilder::new(Target::Aarch64Macos, "profiling_probe")
            .code(vec![0; 4])
            .build();
        let names = read_macho_symbol_names(&object).expect("readable Mach-O symtab");
        assert!(
            names.iter().any(|name| name == "_profiling_probe"),
            "defined symbol missing from {names:?}"
        );
    }

    #[test]
    fn elf_symbol_reader_reports_empty_for_sectionless_executables() {
        // The internal linker's default ELF output has no section table at
        // all; the reader must report "no symbols" rather than an error.
        let mut minimal = vec![0u8; 64];
        minimal[..4].copy_from_slice(b"\x7fELF");
        assert_eq!(
            read_elf_symbol_names(&minimal).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn case_compiler_command_applies_explicit_environment_after_sanitizing() {
        let real_std = Path::new("/repo/std");
        let environment = HashMap::from([
            ("RUE_STD_PATH".to_string(), "${REAL_STD}".to_string()),
            ("RUST_LOG".to_string(), "trace".to_string()),
        ]);
        let command = case_compiler_command(
            Path::new("rue"),
            &["main.rue".to_string()],
            Path::new("/case"),
            &environment,
            real_std,
        );
        let environments: HashMap<_, _> = command.get_envs().collect();

        assert_eq!(
            environments.get(std::ffi::OsStr::new("RUE_STD_PATH")),
            Some(&Some(std::ffi::OsStr::new("/repo/std")))
        );
        assert_eq!(
            environments.get(std::ffi::OsStr::new("RUST_LOG")),
            Some(&Some(std::ffi::OsStr::new("trace")))
        );
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["main.rue"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/case")));
    }

    #[cfg(unix)]
    #[test]
    fn known_bug_cannot_absorb_fake_compiler_panic() {
        let (_directory, binary) =
            fake_compiler("#!/bin/sh\nprintf 'panicked at fake CLI compiler' >&2\nexit 101\n");
        let case = Case {
            name: "known_bug_panic".to_string(),
            files: vec![SourceFile {
                path: "main.rue".to_string(),
                source: "fn main() -> i32 { 0 }".to_string(),
            }],
            known_bug: Some("RUE-PROBE".to_string()),
            differential_opt: true,
            stdout: Some(String::new()),
            exit_code: Some(0),
            ..Default::default()
        };

        let result = run_case_differential(
            &case,
            &ExecutionContract {
                class: ExecutionClass::Ordinary,
                compile_timeout_ms: rue_test_runner::DEFAULT_TIMEOUT_MS,
                runtime_timeout_ms: rue_test_runner::DEFAULT_TIMEOUT_MS,
            },
            &binary,
            Path::new("std"),
            Path::new("."),
        );
        let error = result.expect_err("compiler panic must fail an xfail");
        assert!(error.is_fatal());
        assert!(error.contains("at -O0"));
        assert!(matches!(
            known_bug_disposition("RUE-PROBE", Err(error)),
            KnownBugDisposition::Fail(message) if message.contains("INTERNAL COMPILER ERROR")
        ));
    }

    #[test]
    fn known_bug_disposition_ignores_only_ordinary_failures_and_rejects_xpass() {
        assert!(matches!(
            known_bug_disposition(
                "RUE-PROBE",
                Err(TestFailure::assertion("wrong output\nmore detail"))
            ),
            KnownBugDisposition::Ignore(message)
                if message == "known bug RUE-PROBE (expected failure): wrong output"
        ));
        assert!(matches!(
            known_bug_disposition("RUE-PROBE", Ok(())),
            KnownBugDisposition::Fail(message) if message.contains("test PASSED")
        ));
    }
}
