# Body analysis and CFG incrementality audit

Status: RUE-720 trunk audit after RUE-719. This note records the live ownership
and identity boundaries before any body or CFG artifact is retained. It does
not authorize reuse.

## Conclusion

The canonical frontend has one semantic path, but its body-analysis result is
not a per-definition query result. `BoundSema::analyze_all_bodies` consumes one
request-local semantic epoch and runs a joint, program-wide reachability and
specialization fixed point. `build_functions_and_cfgs` then consumes the whole
`SemaOutput`, synthesizes glue, remaps function-local strings into one global
table, sorts every emitted function, builds every CFG in parallel, and
optimizes every successful CFG.

The first independently gated RUE-720 implementation PR should therefore add
structural work accounting at the existing seams. The present counters record
selected declaration lookups and dependency events, but cannot prove how many
bodies, specialization candidates, string entries, glue functions, or CFGs
were attempted, completed, discarded, or optimized. Retaining AIR before that
ledger exists would make apparent reuse indistinguishable from hidden work.

Stable ordinary-body identity can build on `StableDefinitionKey` plus the
already separated declaration, signature, and body fingerprints. A reusable
body output cannot be today's `AnalyzedFunction`: it contains request-local
AIR symbol, type, string, span/FileId, and global-pool identities. Durable
specialization and CFG representations remain later design steps.

## Live phase and ownership map

The production session reaches this path through `CanonicalFrontendSession`:

1. Canonical parse and merge create `CanonicalMergedProgram`, its
   `DefinitionSnapshot`, and parser-authored signature/body partitions.
2. Canonical RIR construction creates one revision-local `Rir` and
   `SourceRevision`.
3. `prepare_canonical_declarations` creates one `Sema`, its RIR declaration
   index, declaration shells, and stable definition records.
4. The ordinary path resolves the shells. The reuse path atomically installs
   projected durable declaration payloads or falls back to an entirely
   ordinary binder. Both produce one `BoundSema` that owns the mutable semantic
   epoch consumed below.
5. `finish_canonical_analysis` calls `BoundSema::analyze_all_bodies` exactly
   once. `rue_air::sema::analysis::analyze_function_bodies_lazy` owns the
   reachable-body worklists and the outer fixed point.
6. The driver roots `main`, named destructors, and dynamically registered
   anonymous destructors. It analyzes reachable ordinary free functions,
   anonymous methods, named methods, and named destructors into
   `AnalyzedFunction` values. Calls discovered by each body feed the same
   deterministic worklists.
7. One `Specializer` persists across the outer fixed point. It scans newly
   appended AIR, deduplicates request-local `SpecializationKey` values, rewrites
   `CallGeneric` instructions in place, reanalyzes generic source bodies for
   pending substitutions, and returns newly discovered ordinary callees to the
   outer worklists. Its 64-round budget is program-wide.
8. Finalization concatenates function-local strings into one request-global
   table and rewrites AIR string indices. It also orders warnings and dependency
   observations and returns one `SemaOutput` with the request-global type pool.
9. `build_functions_and_cfgs` consumes that entire output. It synthesizes drop
   glue from the complete type pool, removes comptime-only functions, combines
   and sorts all machine symbols, then uses `CfgBuilder::build` for every
   function. Malformed AIR fails before optimization; otherwise the CFG is
   optimized with the request's `OptLevel`.
10. CFG lowering also discovers implicit named-destructor dependencies. The
    driver joins request-local `StructId` targets through the global type pool,
    merges CFG warnings in sorted function order, and returns
    `FunctionWithCfg` values coupled to the same type pool and string table.

There is no second parser, lowerer, binder, or body-analysis entry point in this
path. The declaration-import parity helper deliberately runs ordinary and
installed binders separately in tests; it is not a production reuse path.

## Current prospective query owners

| Body class | Current selection identity | Current analysis owner | Reuse disposition |
| --- | --- | --- | --- |
| Non-generic free function | request-local `Spur`, then indexed `InstRef` | outer sema worklist | first supported candidate after durable projection exists |
| Generic free function | `SpecializationKey { Spur, Vec<Type>, Vec<ConstValue> }` | persistent program-wide `Specializer` | unsupported until specialization arguments and output are durable |
| Named method/associated function | request-local `(StructId, Spur)` and private `InstRef` | outer sema worklist | possible only after owner/method stable keys are threaded through dispatch |
| Anonymous method/destructor | request-local `StructId`, captured substitution maps | outer sema worklist | fail closed; no stable definition endpoint |
| Named destructor | declaration-index record plus request-local `StructId` | implicit-root pass | later supported candidate; it needs explicit root and drop-dependency provenance |
| Synthesized drop glue | request-local type-pool traversal | CFG frontend tail | not a source body; later artifact keyed by durable nominal type and destructor/layout inputs |

Named methods with comptime parameters currently still produce one runtime
body; they do not have a method-specialization table. That surface must fail
closed rather than being described as specialized reuse.

## Request-local identity inventory

### Body dispatch and semantic inputs

- `InstRef` identifies RIR declarations and bodies only inside the current RIR.
- `Spur` identifies functions, methods, parameters, calls, and function-valued
  constants only inside the current interner.
- `FileId` and raw `Span` values appear in lookup records, diagnostics,
  warnings, specialization call sites, and dependency observations. They are
  useful for projecting a current result, but are not durable identity.
- `StructId`, `EnumId`, array/pointer intern IDs, and `Type::as_u32` values are
  indices into one `TypeInternPool`. Even primitive-looking `Type` values must
  be treated as belonging to the representation that produced them.
- `InferenceContext`, `ParamArena`, captured comptime maps, method tables,
  declaration tables, module registry state, and named-declaration indices are
  owned by the consumed `Sema` epoch.
- `ConstValue::Type` embeds `Type`; `ConstValue::Function` embeds `Spur`.

### Current body output

`AnalyzedFunction` is not durable:

- `name` is a display/machine symbol, not sufficient source identity;
- `Air` instructions contain interned symbols, pool-local types, AIR-local
  instruction/extra-array offsets, string indices, and source spans;
- `implicit_drop_source` has stable-capable names but still uses FileId indices
  that require exact-revision translation;
- parameter slot counts/modes and allow flags are values and can be retained
  only alongside their complete semantic provenance;
- warnings retain current spans and their final order is program-wide;
- local strings are rewritten into a request-global string table only during
  body-analysis finalization.

The returned `SemaOutput` further couples all bodies to one `TypeInternPool`,
one global string table, program-wide dependency completeness flags, and the
fixed-point result set.

### Specialization

`SpecializationKey` is wholly request-local: its base is a `Spur`, type
arguments are pool-local `Type` values, and value arguments can contain either.
The specialized machine name is derived from those values and interned again.
The specializer also owns a request-global scan frontier, warning-dedup set,
round counter, and first-call-site span. Consequently an ordinary body result
that still contains `CallGeneric` cannot be imported independently, and a
post-rewrite body cannot be imported without the exact specialization map that
performed the rewrite.

The existing `SpecializedFreeFunctionOrigin` is an observation, not a durable
identity. Its FileId index and encoded `Type::as_u32`, `ConstValue::Type`, and
`ConstValue::Function` words are revision-local.

### CFG

`Cfg` contains block/value/instruction identities local to one build. Its
instructions retain pool-local `Type`, `StructId`, symbols/string provenance,
and source spans. `CfgOutput` additionally exposes request-local `StructId`
destructor targets. Final warning and function order is assembled outside the
builder. CFG reuse therefore requires an atomic import that remaps every type,
symbol, string, span, and destructor target before any artifact becomes
visible.

## Exact dependency and invalidation inputs

An ordinary per-definition body query must conservatively include or depend on:

- the owner's `StableDefinitionKey`;
- its body fingerprint and exact signature fingerprint;
- target and preview-feature semantic inputs;
- stable declaration semantics for the owner;
- every resolved value, type, const, module, method, destructor, and callable
  type-head dependency selected during analysis, including negative or
  ambiguous resolution where relevant;
- inferred nominal layouts and function signatures used by type checking;
- function-level warning allowances and all diagnostic-producing source spans;
- for methods, the stable nominal owner and receiver mode;
- implicit destructor obligations discovered during later CFG elaboration.

Optimization level is not a body-analysis input today and must not reduce body
reuse. It is a CFG key. A future CFG query additionally needs the durable body
artifact identity, optimization level, complete type/layout inputs, global
string remapping provenance, machine-symbol order/identity, drop-glue inputs,
and warning policy.

Compilation root and reverse dependency closure determine which cached body
requests may be considered. They are not by themselves artifact keys or proof
that a retained result can be projected. If root choice changes a body-local
observable, that specific provenance must become an explicit input. A missing
stable endpoint, incomplete dependency surface, failed projection, or
unsupported body class must select the ordinary path with zero partial
installation.

## Current fallback and failure boundaries

- Declaration projection is read-only and can fall back using the same shells.
- Declaration installation consumes shells; a failure starts a wholly fresh
  semantic epoch before ordinary binding.
- Body analysis has no import attempt or body-level fallback yet. Any body
  error fails the single request after other queued work may already have run.
- Specialization failure occurs inside the joint fixed point. The partially
  rewritten and appended AIR is discarded with the failed `SemaOutput`.
- CFG builders run in parallel. Each malformed-AIR result records errors and
  skips optimization, but other parallel builders may already have completed.
  Collection returns the first ordered failure and discards the request output.
- There is no persistent body/CFG baseline to poison. A future session must
  publish a new baseline only after body projection, specialization,
  string/type remapping, CFG construction, warning assembly, and parity checks
  have all succeeded.

## Structural work ledger audit

`BodyAnalysisWork` currently exposes dependency-event counts; free-function,
named-method, and anonymous-method record lookups; named-destructor records
visited; and two expected-zero RIR scan counters. The session benchmark exports
only part of that structure. Declaration reuse has a substantially more exact
attempt/fallback/epoch ledger.

Missing body-analysis counters:

- roots selected and queue items popped, deduplicated, absent, attempted,
  succeeded, and failed, split by ordinary function, named method, anonymous
  method/destructor, and named destructor;
- AIR instructions and local strings produced or remapped;
- specialization AIR instructions scanned, generic calls observed, unique and
  duplicate requests, rewrite attempts, outer waves, internal rounds,
  specialized bodies attempted/succeeded/failed, and ordinary references
  returned;
- warning candidates produced and specialization-warning deduplications;
- comptime-only bodies filtered after analysis;
- body import comparisons, projection attempts, remapped entities, reuse,
  rejection reasons, fallbacks, and ordinary analyses skipped once reuse exists.

Missing CFG-tail counters:

- drop-glue candidates visited and functions synthesized;
- functions considered and comptime-only functions filtered;
- CFG builds attempted/succeeded/failed and AIR instructions consumed;
- optimization attempts/completions, split by level if the benchmark aggregates
  unlike requests;
- warnings and implicit destructor targets emitted;
- type, string, symbol, span, and destructor-target remaps attempted/completed;
- CFG import comparisons, reuse, rejection reasons, fallbacks, and builds
  skipped once reuse exists.

Every attempt must be counted before the fallible operation. Parallel CFG work
must use per-result value counters reduced in deterministic function order, not
a shared timing-dependent atomic. Failed requests must return their work record
to the benchmark harness where the API permits; otherwise failure scenarios
cannot prove that discarded work occurred.

## Proposed ordinary body-query boundary

The first reusable semantic unit should be a supported, non-generic named
definition, not a whole `SemaOutput` and not a CFG. The conceptual durable
input is:

```text
StableBodyInput {
    owner: StableDefinitionKey,
    signature: StableDefinitionFingerprint,
    body: StableDefinitionFingerprint,
    semantic_input: { target, preview_features },
    dependencies: ordered stable keys plus exact input fingerprints,
}
```

The root and reverse closure select which inputs are requested; root membership
need not be duplicated in an artifact that is otherwise semantically identical.
If root choice changes observable warning policy or another body-local result,
that fact must instead become an explicit input.

The conceptual durable output is an owned, request-independent typed-body
record containing a stable owner, canonical types, owned symbol/string values,
source-relative diagnostic anchors, parameter ABI metadata, warning records,
direct stable dependency observations, and an instruction graph whose references
are record-local rather than AIR-epoch offsets. Projection into fresh AIR must
be atomic and return either a complete current-epoch `AnalyzedFunction` plus
local strings and dependencies, or no mutation.

This shape is a design constraint, not approval to introduce a second IR now.
The implementation should first test whether existing AIR can gain an explicit
export/import representation without becoming a parallel analyzer or lowerer.

Initial support must exclude generic free functions, named methods with an
unresolved generic surface, anonymous owners, bodies with untranslatable
function-valued comptime data, and any dependency observation lacking a stable
endpoint. Those bodies run the ordinary worklist.

## Smallest independently gated first PR

Add value-only body, specialization, and CFG structural counters at the live
operations above, thread them through `SemaOutput`, `CanonicalSemanticWork`, and
the session benchmark JSON, and hard-gate cold, exact-noop, body-edit, failure,
and recovery scenarios. Do not add a cache, durable AIR, or CFG reuse.

Required tests for that PR:

1. A small non-generic call graph pins exact body attempts and completions.
2. Recursion and duplicate references pin queue deduplication separately from
   analysis attempts.
3. A generic chain pins specialization scans, unique/duplicate requests,
   rounds, bodies, and rewrites.
4. A semantic body failure counts the attempt and no completion.
5. Malformed AIR in a focused CFG test counts a build failure and zero
   optimization attempts for that function.
6. O0 and optimized CFG tests count the same builds and the appropriate
   optimization work.
7. The N=4 benchmark smoke asserts cold and declaration-reuse revisions still
   perform identical body/CFG work today; this is the pre-reuse baseline.
8. The benchmark schema and process document enumerate every new field and do
   not infer work from elapsed time.

Only after this PR lands should the next PR thread `StableDefinitionKey` through
ordinary body dispatch and translate direct dependency observations at the
point they are produced. That second PR should still retain no AIR: its gate is
an exact, complete per-owner dependency/input manifest with unsupported owners
failing closed. Durable body export/import is the following slice.

## Later sequence

1. Land the complete structural ledger.
2. Thread stable owners through ordinary body analysis and emit exact per-owner
   dependency/input records without a second RIR scan.
3. Define and test atomic durable ordinary-body export/import.
4. Demonstrate unchanged-body reuse and exact fresh-session parity, including
   changed reachable bodies and reverse caller closure.
5. Define canonical specialization type/value/function arguments and a durable
   specialization identity before retaining specialized bodies.
6. Extend the body boundary to supported named methods and destructors.
7. Define stable CFG artifacts and atomic remapping only after semantic body
   reuse is sound; include optimization, layout, string, symbol, and drop
   provenance in the key.

Persistent storage, watchers, an LSP, parallel compiler entry points, and weaker
semantic keys remain out of scope.
