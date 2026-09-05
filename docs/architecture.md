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

The `stackframe` view reports each frame's byte-based layout — per-slot offsets
and sizes, callee-saved registers, and the 16-byte-aligned total — from the
single frame-layout authority (`rue-codegen`'s `frame_layout`, RUE-975), the
same byte product both backends' prologue/epilogue and spill allocators consume,
rather than re-deriving `slot * 8` arithmetic per backend. That authority also
records whether the function establishes a frame pointer at all: a leaf with no
frame slots and no calls needs neither a frame pointer nor a slot region, so its
prologue is at most the callee-saved pushes and the view reports its saved
registers relative to the entry stack pointer (RUE-1171). Physical stack layout
is one representation produced by the ADR-0052 canonical layout authority; the
internal value decomposition stays slot-shaped, so under the default layout the
reported offsets are unchanged.

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

Semantic analysis owns the mutable `TypeInternPool`. Its reachable-body fixed
point includes generic specialization plus anonymous type and destructor
discovery; only after that work completes is the pool consumed into
`FrozenTypeInternPool`. CFG construction, optimization, and both native
backends accept only this immutable view. Their nominal lookups borrow complete
definitions directly and their type iteration requires neither locking nor a
temporary ID allocation. Destructor names cross this boundary as stable
strings; each CFG/codegen request interns them into its own symbol universe, so
request-local `Spur` values never become durable type metadata. Private
semantic-name indexes remain available for stable collision handling, but the
frozen public API neither accepts nor exposes their `Spur` values or raw type
records.

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

Which convention governs a call boundary is one value type,
`rue_target::CallingConvention`, and every psABI rule the compiler needs from a
C row is data on `CConventionSpec` beside it — roster sizes, where the hidden
indirect-result pointer travels and whether it is echoed, stack alignment and
shadow space, outgoing-argument packing, narrow-integer extension, and which
aggregate rule applies. `rue_air::lower_c_signature` is the one function that
reads that data against a type's facts and answers where every argument and the
result of a `"C"` signature lives. All three C crossing sites consume its
`LoweredSignature`: the `extern "C"` import planner (`foreign_call`), the
`pub extern "C" fn` export thunk (`export_thunk`), and the stable query plane's
`compiler.call-abi`. The backends contribute only physical leaves — mapping a
roster index to a register name, and emitting the loads, stores, and calls.
Calls between Rue functions use the separate native convention, whose classifier
is `rue_air::NativeCallAbi`.

The supported targets are:

| Target | Code generation | Executable linking |
| --- | --- | --- |
| `x86-64-linux` | direct x86-64 emission | internal ELF linker |
| `aarch64-linux` | direct AArch64 emission | internal ELF linker |
| `aarch64-macos` | direct AArch64 emission | Mach-O/system-tool path |

Cross-target `--emit` views work independently of whether the host can link or
run the resulting executable.

## Runtime, built-ins, and linking

- **`rue-allocator`** owns dependency-free heap policy and bookkeeping over a
  consumer-supplied page mapper. **`rue-runtime`** supplies that mapper plus
  startup, memory, strings, I/O, parsing, random data, and target-specific
  syscall support.
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

Embedding semantic compilation uses the same session boundary as the CLI:
construct a `SourceSnapshot` whose `SourceMetadata` explicitly identifies the
root, physical and logical module paths, update a `CompilerSession`,
and query it with `CompileOptions`. Parser `Ast` values remain available for
syntax inspection and presentation, but are not semantic compiler inputs; this
prevents source, module, target, preview-feature, and optimization identity from
being inferred or split across unrelated arguments.

Import discovery is the single exception to the compiler's otherwise pure
snapshot queries because a host must observe the filesystem. Parsing produces
one canonical `ImportDirective` occurrence representation. The compiler turns
those occurrences and an explicit discovery context (including the captured
`RUE_STD_PATH` value, when present) into ordered requests; the source loader is
the only component that probes or reads candidate files. It reports typed
observations back to the session, which commits one `CanonicalImportGraph`.
Semantic analysis, dependency reporting, and code generation consume that
committed graph and never repeat path resolution from loaded file names.

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
computed. `CompilerSessionWork::retention` reports the current
session-owned diagnostic entries, distinct source attempts and bytes, unique
dependency manifests, and invalidation plans.

Syntax output uses an explicit `ParsedAstPresentation` adapter. It walks the
`SourceSnapshot` in caller-selected order so diagnostics and `--emit ast`
retain presentation order without constructing a second parsed or merged
program. Duplicate, cross-kind, and program-wide `main` legality is implemented
once by canonical merge candidate validation.

The parser retains at most 100 detailed diagnostics per source file across
grammar recovery and post-parse directive validation. Exact duplicates (the
same error kind and complete primary span) are removed in first-occurrence
order before that budget is applied; rich labels, notes, helps, and suggestions
do not distinguish parser duplicates. If unique parser diagnostics exceed the
budget, one E0103 summary is appended at the first omitted diagnostic's span.
Each file gets a fresh budget, and snapshot parsing continues with later files.
Lexer errors come from the preceding lexer phase and retain their separate
phase-specific behavior.

The `rue-compiler` facade mirrors implementation ownership rather than a
monolithic driver:

- `session` owns publication, invalidation, diagnostics, and immutable query
  artifacts;
- `queries` owns supported options, source inputs, work records, and the thin
  `compile_snapshot` adapter;
- `backend` owns CFG-to-machine lowering and emit presentation;
- `linking` owns runtime selection, object linking, and executable output.

Public consumers do not call individual phase implementations. The facade
exports source/session/query artifacts, diagnostics, presentation helpers, and
the one-shot executable adapter; structural API inventory tests prevent retired
drivers and duplicate `compile_*` families from returning.

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
- Sanitizer workflows cover runtime memory safety.

The full suite and specification traceability gate are run by `scripts/rue
test`. See [development.md](development.md) for commands and
[designs/](designs/) for the rationale behind major choices.
