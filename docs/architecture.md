# Rue compiler architecture

Rue is a native compiler written in Rust. It uses Buck2, emits machine code
directly rather than using LLVM, and keeps each major representation
inspectable through the CLI.

## Pipeline

```text
source files
    │
    ├─ lexer ───────────── tokens
    ├─ parser ──────────── AST
    ├─ AST generation ──── RIR (untyped, per-file)
    ├─ semantic analysis ─ AIR (typed, program-wide)
    ├─ CFG construction ── CFG
    ├─ instruction select─ target MIR
    ├─ liveness / regalloc / scheduling
    ├─ machine emission ── code and relocations
    └─ object emission / linking ─ executable
```

Use `rue --emit <stage>` to inspect `tokens`, `ast`, `rir`, `air`, `cfg`,
`lowering`, `mir`, `liveness`, `regalloc`, `asm`, or `stackframe`. The option
can be repeated to request several views.

## Frontend and semantic IRs

- **`rue-lexer`** tokenizes source and records byte spans and interned names.
- **`rue-parser`** constructs a syntax tree without resolving names or types.
- **`rue-rir`** lowers syntax into a dense, index-addressed, untyped IR. Each
  source file is parsed into a self-contained `ParsedModule`. A canonical
  `ParsedProgram` orders those modules by stable identity, and
  `CanonicalMergedProgram` validates definition candidates while retaining
  their module provenance. RIR lowering traverses those canonical module views
  directly; there is no shared-interner parse representation or concatenated
  compatibility AST.
- **`rue-air`** performs declaration collection, name resolution, inference,
  type checking, ownership/exclusivity analysis, comptime evaluation, and
  production of typed AIR.
- **`rue-cfg`** turns AIR into explicit control flow, inserts drop behavior,
  verifies invariants, and runs target-independent optimizations such as
  constant folding, propagation, and dead-code elimination.

Instructions and types are generally stored densely and referenced by small
integer IDs. This avoids self-referential lifetimes, makes equality and lookup
cheap, and keeps IR dumps deterministic. Verifiers and typed index wrappers
provide the corresponding invariant checks.

## Native backends

`rue-codegen` contains separate x86-64 and AArch64 machine IRs. Each backend
performs instruction selection, liveness analysis, register allocation,
peephole optimization, scheduling, stack-frame construction, verification, and
byte emission. Common code owns architecture-neutral concerns such as aggregate
slots, by-reference arguments, and virtual-register bookkeeping.

The supported targets are:

| Target | Code generation | Executable linking |
| --- | --- | --- |
| `x86-64-linux` | direct x86-64 emission | internal ELF linker |
| `aarch64-linux` | direct AArch64 emission | internal ELF linker |
| `aarch64-macos` | direct AArch64 emission | Mach-O/system-tool path |

Cross-target `--emit` views work independently of whether the host can link or
run the resulting executable.

## Runtime, built-ins, and linking

- **`rue-runtime`** supplies startup, allocation, memory, strings, I/O,
  parsing, random data, and target-specific syscall support.
- **`rue-builtins`** describes compiler-visible built-in types, enums,
  functions, methods, and operators. Built-in aggregate types are injected as
  synthetic declarations so they use ordinary semantic paths where possible.
- **`rue-linker`** reads and writes ELF and Mach-O objects, archives,
  relocations, and symbols. Linux executables normally use the internal linker;
  the CLI can also delegate to a system linker.
- **`rue-target`** centralizes target triples, ABI properties, pointer/page
  sizes, and object-format selection.

## Driver and diagnostics

`rue-compiler` orchestrates compilation and exposes intermediate states used by
the CLI and tests. The `rue` crate implements source discovery, module loading,
file processing, target and optimization selection, emit modes,
linking, timing, and diagnostic rendering.

Embedding semantic compilation uses the same canonical boundary as the CLI:
construct a `SourceSnapshot` whose `SourceMetadata` explicitly identifies the
root, physical and logical module paths, update a `CanonicalFrontendSession`,
and query it with `CompileOptions`. Parser `Ast` values remain available for
syntax inspection and presentation, but are not semantic compiler inputs; this
prevents source, module, target, preview-feature, and optimization identity from
being inferred or split across unrelated arguments.

Long-lived frontend sessions use explicit bounded historical ownership. A
session strongly retains at most 16 diagnostic snapshots, evicting in insertion
order while protecting the latest attempted query, latest successful query,
and last successful semantic query. Syntax and semantic failures therefore do
not displace the last-good semantic diagnostics. Callers that need older
diagnostics clone the returned `Arc`; that caller-owned pin remains valid after
the session evicts its entry. Invalidation planning strongly retains the eight
most recently inserted plans and both manifests for each plan, evicting the
oldest plan first. It deliberately uses no weak-key fallback: every retained
plan continues to own the complete dependency evidence from which it was
computed. `CanonicalFrontendSessionWork::retention` reports the current
session-owned diagnostic entries, distinct source attempts and bytes, unique
dependency manifests, and invalidation plans.

Syntax output uses an explicit `ParsedAstPresentation` adapter. It walks the
`SourceSnapshot` in caller-selected order so diagnostics and `--emit ast`
retain presentation order without constructing a second parsed or merged
program. Duplicate, cross-kind, and program-wide `main` legality is implemented
once by canonical merge candidate validation.

`rue-error` defines stable error categories, suggestions, warnings, preview
features, and internal-compiler-error reporting. `rue-span` maps byte spans to
files and line/column positions.

## Testing architecture

Rue deliberately tests at several boundaries:

- Rust unit tests validate local algorithms and IR/backend invariants.
- Specification cases tie observable behavior to normative spec paragraphs.
- UI cases pin diagnostics and warnings that are not language semantics.
- CLI cases exercise the real compiler, filesystem, linker, ABI, and runtime.
- `rue-oracle` independently evaluates a modeled subset of the language;
  differential tests compare it with compiled programs.
- `rue-fuzz` targets the lexer, parser, semantic analysis, full compiler, and
  emitters with mutation and property-based testing.
- Sanitizer and benchmark workflows cover runtime memory safety and performance
  regressions.

The full suite and specification traceability gate are run by `scripts/rue
test`. See [development.md](development.md) for commands and
[designs/](designs/) for the rationale behind major choices.
