# Body analysis and CFG incrementality audit

Status: RUE-720 completion audit. `CompilerSession` retains and reuses supported
per-definition body and CFG artifacts through the canonical semantic query.
Unsupported surfaces fail closed to the existing analysis/build path.

## Canonical ownership

The live pipeline remains singular:

1. parse, import validation, merge, and RIR create the current revision;
2. canonical declaration preparation creates one fresh AIR semantic epoch;
3. durable declaration payloads install atomically when their exact inputs
   match, otherwise the ordinary binder runs;
4. the existing reachability/specialization fixed point imports eligible
   durable bodies or analyzes current source bodies;
5. finalization assembles current strings, warnings, dependencies, and the type
   pool;
6. `build_functions_and_cfgs` imports eligible CFGs or invokes `CfgBuilder`,
   then returns canonical `FunctionWithCfg` values.

There is no peer parser, binder, body analyzer, phase machine, or CFG builder.
Durable records are compiler-owned exchange artifacts projected into the same
current-revision AIR and CFG types consumed by cold compilation.

## Durable body boundary

Supported ordinary free functions, named methods, associated functions, named
destructors, and stable free-function specializations use a durable input made
from:

- the owner's `StableDefinitionKey` (or stable specialization identity);
- exact signature and body fingerprints;
- target and preview-feature inputs;
- sorted direct dependency records and their exact current fingerprints;
- source-relative body anchors and warning/dependency completeness evidence.

The output owns canonical `DurableType` and comptime values, symbols and
strings, record-local instruction/place references, parameter ABI data, and
stable dependency observations. It contains no request-local `Spur`, `InstRef`,
`FileId`, raw `Span`, `Type`, nominal ID, AIR offset, or string-pool index.

Candidate comparison happens before ordinary dispatch. Projection resolves all
stable endpoints and anchors without mutation. Atomic import then remaps the
complete body into the fresh AIR epoch; any missing, ambiguous, stale,
wrong-kind, malformed, unsupported, or fingerprint-mismatched value discards
the candidate and schedules the ordinary body. Reused bodies still participate
in the same reachability and specialization fixed point, so root selection and
reverse dependency closure remain authoritative.

Specialized free functions are keyed by the stable generic base plus canonical
type and comptime-value arguments. Imported ordinary callers receive the
current specialized symbol mapping before installation. Named methods with
comptime parameters currently have one runtime body rather than a method
specialization mechanism, so the named declaration is their stable identity.

Anonymous structural methods/destructors, incomplete dependency evidence,
warning-producing bodies, unresolved generic calls, and untranslatable
comptime/function values remain ordinary fallbacks. These are supported
compilation paths, but they do not claim incremental reuse.

The session publishes a new body baseline only after the full semantic request
succeeds. Syntax, declaration, body, specialization, or CFG failure leaves the
previous successful baseline intact.

## Durable CFG boundary

A durable CFG artifact is keyed by its exact stable body provenance,
optimization level, target, and every nominal layout actually consumed by CFG
operations. It retains a cloned CFG plus an explicit projection of all domains
that must be remapped into the current semantic output:

- types and named struct/enum identities;
- callable and intrinsic symbols;
- string-table entries;
- source-relative spans;
- implicit destructor targets and warning provenance.

Block and value IDs stay local to the cloned CFG and are remapped atomically as
one internally consistent record. Layout dependency closure is transitive for
nominal fields/payloads. Pointer pointees are not layout dependencies unless an
operation such as pointer read/write/offset consumes the pointee layout.

Import requires an exact join for every domain and validates the remapped CFG
before publication. Target or optimization changes select a different key.
Missing symbols/layouts, malformed schemas, unsupported synthetic/drop-glue
provenance, warning/destructor surfaces that cannot be reproduced, or any
remap failure rebuild that function's CFG. A body may be reanalyzed while its
unchanged resulting CFG is reused; the durable CFG key is based on artifact
provenance, not merely on whether body analysis ran.

The session publishes CFG candidates only after the complete semantic/CFG
request succeeds. Failed requests cannot poison the last-good CFG baseline.

## Structural evidence

`CanonicalSemanticWork` records each body candidate comparison, stable
specialization-map operation, export/conversion/finalization/projection/import,
installed entity, reuse, fallback, atomic discard, and skipped ordinary body
analysis. `CfgConstructionWork` records construction/optimization plus each CFG
candidate, import, reuse, fallback, reused warning/destructor target, and
durable export. Attempts are counted before fallible work; parallel CFG values
are reduced deterministically rather than mutated through shared timing state.

The schema-11 session benchmark hard-gates supported N=128 workloads:

- exact no-op performs whole-query reuse;
- an unrelated edit imports all 128 bodies and CFGs and performs zero body
  analyses, CFG builds, or optimizations;
- a reachable leaf edit reanalyzes exactly the leaf and its direct reverse
  caller while rebuilding only the changed CFG;
- a chain edit invalidates the exact transitive reverse closure;
- O0 and O1 both import all eligible CFGs with zero construction/optimization;
- stable specialization imports avoid specialized reanalysis;
- semantic failure records discarded work and recovery imports from the prior
  successful baseline;
- cold analysis populates declaration, body, and CFG caches from its one
  canonical pass, without duplicate cache-population analysis.

Those hard gates measure a single-file corpus committed through the staged
discovery protocol. `rue-compiler-session-bench --module-axis` holds the corpus
and edit fixed and varies only the protocol: under the rooted import-demand
protocol that every multi-module program must use, the same leaf edit reanalyzes
every reached body rather than the leaf and its direct reverse caller. Module
count is not the variable — one module and eight behave identically. See
[`module-axis-locality-findings.md`](module-axis-locality-findings.md). The
scenarios below remain accurate about the corpus they measure; they are not
evidence of exact reverse invalidation for programs with imports.

Every reused scenario is compared against a fresh session for exact public
semantic/CFG artifacts, ordered type-pool entries, strings, warnings, stable
identities, specialized durable-body payloads, durable ordinary bodies exposed
by the manifest, dependency/completeness records, diagnostics, and
byte-identical emitted output. The private retained durable-CFG cache has no
comparison accessor, so its parity is asserted through imported public CFGs,
exact work counters, and emitted bytes. See
[`session-invalidation-benchmark.md`](../process/session-invalidation-benchmark.md)
for the field-level contract.

## Remaining boundaries

The current implementation is intentionally in-process and last-successful
only. Persistent cache serialization, cross-process compatibility, filesystem
watching, editor protocols, and stable position/reference indexes are separate
projects. Programs containing unsupported reusable body/CFG surfaces compile
normally and fail closed per artifact.

RUE-813 tracks a general reusable cold-versus-reused differential oracle beyond
the bounded completion workload. RUE-901 tracks representative multi-module
projects, wall-time baselines, and longer-running performance analysis. Neither
is required to establish the sound per-function boundary implemented here.
