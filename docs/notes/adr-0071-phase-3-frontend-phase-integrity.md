# ADR-0071 Phase 3 frontend phase-integrity ledger

This note records the current production frontend re-entry points and the
replacement contract for Phase 3. Source and tests remain authoritative; rows
marked complete describe paths already removed by the bounded Phase 3 slices.

## Deletion ledger

| Re-entry | Production owner and consumers | Displaced work | Required deletion proof |
| --- | --- | --- | --- |
| Declaration signatures | `compiler.semantic-nucleus` owns a request-local projection from the exact canonical parsed declaration; specialization typing and call ABI consume its resolved signature, while cheap named-body classification uses parser-indexed shell facts | Deletes body-free source reconstruction, signature lexing/parsing, and the additive retained `compiler.declaration-signature-projection` family | Complete: no production call or definition of `parse_semantic_signature`, no raw-signature parser locator/materializer or peer query family, and a cold signature request performs no body AstGen work |
| Runtime and specialized bodies | `compiler.declaration-body-plan-artifacts` owns one packed candidate artifact; the ordinary-definition and free-function-specialization arms of `BodyTransactionEvaluator` consume it through the transient resolver | Deletes concatenated signature/body text, synthetic snapshots, body-local lex/parse/AstGen, synthetic span remapping, and specialization-multiplied frontend work | Complete: both transaction arms consume the same candidate-keyed structured body plan; no production call or definition of `lower_owned_body_input`; specialization count does not increase parsing or AstGen lowering |
| Anonymous members | `compiler.declaration-body-plan-artifacts` owns the ultimate named/constant producer candidate; the anonymous-member arm recursively selects the exact nested declaration by producer chain, indexed owner anchor, name, and member kind | Deletes destructor spelling rewrites, fake named owners, synthetic source assembly, lex/parse/AstGen, and the second RIR remap/index path | Complete: no production call or definition of `lower_anonymous_member_body_input` or its explicit-anchor lowering seam; nested and constant-produced members directly observe the producer candidate artifact, and member demand adds no candidate AstGen work |
| Comptime bodies | `parse_semantic_body` in `semantic_query_nucleus.rs`; called by the semantic-nucleus comptime-call evaluator | Wraps exact body text in a fake function, lexes and parses it, rediscovers imports, transports anonymous anchors, then evaluates the cloned AST | Comptime evaluation consumes the same declaration artifact as runtime analysis; no production call or definition of `parse_semantic_body` |
| Constants | `parse_semantic_const` in `semantic_query_nucleus.rs`; called by the semantic-nucleus const evaluator | Reconstructs a fake const declaration, lexes and parses it, rediscovers imports, transports anonymous anchors, then evaluates the cloned AST | Constant evaluation consumes the declaration artifact directly; no production call or definition of `parse_semantic_const` |
| Well-known option body scan | `PackedValidatedRir::fallible_intrinsics`; consumed by `compiler.body-toolchain-demands` through `DeclarationBodyPlanArtifacts` | Derives the typed five-kind set during the one canonical packed-RIR traversal; production performs no second lexer pass over retained body text | Complete: the packed header owns the stable typed set and the old lexical scanner remains only as an independent `cfg(test)` oracle |
| Structured type syntax | `AstGen::intern_type` in `rue-rir/src/astgen.rs` renders arrays, pointers, calls, qualified paths, and integer arguments into interned text; `rue-air` and compiler consumers then use parse helpers, prefix/slice tests, qualified-name splitting, and literal `"type"` comparisons to recover structure | Turns parser structure into a peer string grammar and repeatedly reconstructs it during inference, semantic type resolution, binding-manifest construction, provider analysis, and comptime classification | RIR carries dense structured type-syntax references; semantic consumers traverse them; no production code parses, splits, trims, or prefix-tests rendered compound type text. Leaf-name lookup and presentation-only formatting remain allowed; the parser-to-RIR-to-sema conformance round-trip test is removed |
| Semantic-nucleus type tokenization | `SemanticNucleusTypeProvider` in `crates/rue-compiler/src/revisioned_query_database.rs`, including the handwritten split/decomposition route around lines 10054–10129 and the `parse_type_call_syntax` route beginning around line 10551 | A second handwritten type grammar reconstructs declaration types while binding semantic signatures and constants | The provider consumes the artifact's dense type-syntax nodes; neither the handwritten tokenizer nor a production call to `parse_type_call_syntax` remains in the provider |
| Warning-only static-call discovery | `warning_static_call_heads` and `WarningStaticCallCollector` in `revisioned_query_database.rs` independently walk canonical AST bodies, implement lexical scopes and static aliases/import paths, and discover value/type call heads for warning reachability | Maintains a peer body-discovery and partial name-resolution path even though it does not reparse text | The canonical candidate/artifact boundary publishes one structured body-reference projection; warning reachability is a thin consumer and the peer collector/scoping resolver is deleted |

## Implementation checkpoint: named-body frontend cutover

The first vertical slice deletes the production `lower_owned_body_input` route
for ordinary named bodies and free-function specializations. One
`compiler.declaration-body-plan-artifacts` terminal owns the candidate's
canonical packed envelope: typed logical RIR, declaration-relative span basis,
complete dense spelling table, anchors, declaration root, and optional method
owner share one `Arc<[u8]>`. Both transaction arms consume that same terminal.
The producer lowers the exact AST
node selected by the parsed module's O(1) private locator; it neither assembles
source nor reparses. Canonical RIR presentation transiently composes these same
candidate artifacts with parser-owned recipes, so no registered module-RIR or
peer module-wide AstGen path remains.

This checkpoint intentionally does **not** claim the complete artifact/view
contract below or acceptance items 3 and 12. Rue-air still requires an owned
current-coordinate `ValidatedRir` and mutable symbol interner for each semantic
transaction. The request-local adapter consequently performs one O(body)
direct packed decode, span projection, symbol-table reconstruction, RIR validation, and
`BodyRirIndex` build per ordinary/specialized transaction. Timing keeps the
deleted assembly, lex/parse, and body-local RIR-lowering intervals at zero and
publishes plan-materialization, base-symbol, instruction/payload, and index work
separately. The next slice is the immutable-base/projected rue-air view that
removes those remaining per-transaction remap, validation, index, and interner
rebuilds. Candidate-local construction is independently demandable for indexed
named declarations, and anonymous members now reuse the ultimate producer
candidate as described below. This checkpoint still does **not** claim complete
frontend phase integrity or acceptance items 3 and 12 because the request-local
AIR adapter and candidate-granularity sibling construction remain.

The candidate producer still observes the module parse terminal. A sibling-only
module revision therefore reevaluates AstGen once for each reached candidate,
then publishes the prior artifact stamp when canonical packed bytes compare
equal; transactions, CFGs, and codegen remain green. Work is not
multiplied by consumers or specializations, but eliminating this remaining
producer reevaluation requires a finer parser-owned candidate input stamp and
is deferred and measured separately.

## Implementation checkpoint: anonymous-member frontend cutover

Anonymous method, associated-function, and destructor transactions no longer
retain or reconstruct method source. The producer's durable anonymous shape
retains only the member signature plus a body-availability bit. At execution,
the transaction requests the packed artifact of the named function, method, or
constant that ultimately owns the producer chain, decodes that artifact once,
and recursively locates the exact member declaration. Each hop is identified
by its indexed structural owner anchor and exact member name/kind; method edges
are never rediscovered by source position or a name-only scan.

The selected declaration is passed directly to the existing provider-backed
semantic analyzer. Its current RIR body span is converted to the producer
body's relative coordinate before the transaction is retained, so a prefix or
sibling relocation keeps the artifact and transaction stamps green while
warning and diagnostic presentation uses the latest `FileId` and absolute
origin. Internal member trivia remains part of the candidate-relative span
basis and correctly dirties the artifact and transaction.

The displaced route and its support code are deleted: no fake struct owner,
synthetic `SourceSnapshot`, anonymous lexer/parser/AstGen, explicit anonymous
anchor override, module-RIR rematerialization helper, or raw anonymous member
body carrier remains in production. A nested member producer and a member of a
constant-produced anonymous type both resolve back to the same ultimate
candidate artifact. Materialization cancellation publishes no terminal and an
uncanceled retry succeeds; malformed member kind and producer artifact failure
publish deterministic typed failures rather than cancellation.

This slice removes frontend work from the anonymous-member critical path but
does not yet remove the one request-local packed decode, current-span
projection, symbol reconstruction, RIR validation, and index build performed
for each anonymous transaction. Those costs remain visible under the same AIR
view deferral as ordinary and specialized bodies.

### Isolated performance evidence

The expected time effect is direct: each reached anonymous runtime member used
to assemble a synthetic source snapshot and repeat lexing, parsing, AstGen, span
remapping, validation, and index construction. The replacement selects the
member from its already-demanded producer artifact. It still decodes the
producer artifact for the request-local AIR adapter, but it removes the second
frontend traversal from the one-worker critical path rather than merely moving
retained ownership.

On the frozen Lattice workload, a release thin-LTO x86-64 compiler was measured
against its exact parent in six alternating one-worker pairs after one warmup
per binary. All twelve native outputs were 1,413,120 bytes with SHA-256
`b893e76cfabed737b149d0e8c4d8527077dedd17da78418db20a28a7d30885e5`.
The absolute median compiler-root time fell from 746.141 ms to 673.888 ms
(-72.253 ms, -9.68%); every paired prototype run was faster and the paired
median delta was -43.513 ms. Median external peak RSS fell from 387,817,472 to
374,939,648 bytes (-12,877,824 bytes, -12.28 MiB), while the benchmark's
internal peak fell from 387,555,328 to 374,718,464 bytes. The paired median
external RSS delta was -13,484,032 bytes.

The phase counters support the intended cause. Median attributed body-input
work fell from 32.486 ms to 10.980 ms: synthetic assembly (4.239 ms), lex/parse
(16.700 ms), and body-local RIR lowering (9.395 ms) all fell to zero, leaving
only the packed decode/current-coordinate projection. Query claims and memo
nodes each fell by 2,731. The request-local decoded instruction count rose
because an anonymous transaction now decodes its ultimate producer fragment;
that remaining work is the explicitly deferred borrowed/projected AIR-view
slice rather than a hidden frontend fallback.

## Retained declaration artifact contract

The canonical parse/index boundary publishes compact candidate projections, and
a new definition/candidate-keyed declaration-plan query publishes the one
complete analysis `DeclarationArtifact`. The plan query is not keyed by a
`BodyQueryKey` specialization and is not hidden inside the raw-body query. It is
immutable, independent of `FileId`, absolute offsets, and prefix relocation,
and safe to retain across source revisions. Equality is exact packed-byte
equality and intentionally includes the candidate-relative diagnostic basis,
so internal trivia that moves diagnostic endpoints dirties the artifact:

- a canonical typed-logical packed RIR for exactly the selected top-level or
  named-member declaration candidate. Arena allocation order, orphan payload
  words, and replacement holes do not enter durable identity. Constant
  initializers use their own candidate artifacts; anonymous member bodies are
  nested declarations selected from the artifact of their ultimate producer.
  A member request never invokes a second AstGen pass, although candidate-local
  construction still includes every nested declaration owned by that candidate;
- an artifact-owned dense string table. RIR symbol indexes refer only to this
  table, never to a parser `Spur` or parser resolver;
- dense structured type-syntax nodes referenced by declarations and
  instructions, never source spelling used as a semantic protocol;
- declaration-relative span coordinates stored in the same packed envelope.
  Current `FileId`,
  physical path, and absolute offsets remain in the independently stamped
  source/diagnostic basis and are applied only when a consumer materializes a
  request-local view;
- definition-relative anonymous structural anchors minted from the canonical
  AST producer root and preserved verbatim;
- ordered declaration-local import sites and accessor facts needed by semantic
  consumers.

The semantic-nucleus signature, raw body, and raw constant projections remain
independently comparable and independently stamped. Signature projection is
request-local inside the authoritative semantic-nucleus signature evaluator;
there is no second retained signature family. Raw-body and raw-constant query
families retain their separate stamps and demand edges. The complete declaration plan's
structural equality/digest, however, covers every semantically relevant part of
the declaration: parameters, result, comptime and parameter modes, directives,
accessor-only state, body structure, and declaration category. Thus a
signature-only edit may keep the raw-body stamp stable while correctly changing
the declaration plan and body transaction that depends on it. Sharing
storage never couples an unchanged raw projection to a sibling edit.

Those projections are also independently demandable and schedulable. One
schema/storage owner is not one eager computation node: a cold semantic-nucleus
signature request does no body AstGen or instruction work; requesting one member does no sibling
member work; and cancellation, retained charge, and eviction apply to the exact
projection constructed. Different declaration candidates share no mutable
arena or lock while their projections run in parallel.

Candidate selection retains the exact module, category, owner, name, and
duplicate discriminator. The canonical index also retains an O(1),
parser-private locator for top-level and member declarations; plan construction
must not linearly rescan the AST by span (which becomes quadratic across a
module). No name-only or span-scan lookup may select the artifact.

The artifact contains no `ParsedModule`, raw AST node, parser resolver, parser
`Spur`, current `FileId`, or absolute source offset. Producer lowering uses a
fixed synthetic provenance token. Artifact equality excludes the request-local
source/diagnostic basis, so identical content assigned a different `FileId` or
source-table slot shares the same plan and structural index while diagnostics
relocate to the current source. Method and constant lookup likewise uses a
file-independent declaration-relative structural index, with current `FileId`
supplied only by a request-local locator facade.

`compiler.declaration-body-plan-artifacts` remains the candidate-keyed lowering
authority, and `compiler.body-transaction` directly observes that terminal plus
the independently stamped current source basis through one transient resolver.
There is no specialization-keyed body-input memo. A specialization observes the
exact same artifact `Arc` as its definition; substitutions are applied only by
semantic analysis. Pointer identity of that artifact is testable, and
specialization evaluation adds no parser or candidate AstGen work. The current
AIR adapter still performs request-local instruction/payload copying, symbol
rebuilding, span projection, validation, and index construction; eliminating
those costs remains deferred. Any request-local symbol facade is constructed
from canonical spellings owned by the plan and must reject mismatched ordering;
opaque symbols can never be rebound merely because two interners have the same
length.

Current diagnostic spans are reconstructed from relative coordinates at the
one transaction adapter. That adapter still copies the candidate RIR into a
request-local current-coordinate arena; replacing that copy with a borrowed
projection is explicitly deferred with the other rue-air view work above.
Every diagnostic export crosses this coordinate boundary, including syntax,
type, call-resolution, and borrow failures. Anonymous-member source
positions are presentation data only and never participate in member identity
or body-plan equality. Indexed anonymous structural sites are installed and
cross-checked during root lowering; no post-index AST walk may remint them.
There is no feature switch, syntax whitelist, compatibility evaluator, or
fallback.

The current body-plan artifact retains exactly one packed-envelope allocation;
its charge is the exact `Arc<[u8]>` pointee length plus ordinary outer query
value/container overhead. The envelope covers every reachable instruction,
logical payload, spelling, span-basis slot, anchor, root, and owner field. It is ordinary bounded
query storage: eviction drops it, and a later demand deterministically derives
it again from the canonical parse. Construction uses no global arena or lock
and remains safe on the parallel ready frontier.

Plan construction is cancellation-aware and publishes atomically only after
lowering, validation, indexing, and charge calculation succeed. Cancellation
publishes neither a terminal nor retained bytes; retry derives the plan
normally. Typed producer failures, including foreign-symbol and invalid-anchor
failures, survive unchanged instead of being erased by `.ok()?` into a generic
`ParserCapabilityMismatch`. No partial plan, index, or charge may become
observable after either cancellation or failure.

## Acceptance evidence

Each completed vertical slice must add source-inventory assertions banning its
deleted entry point, plus behavioral coverage proving:

1. all body kinds admitted by the slice produce native x86-64 and AArch64
   output through the existing query graph;
2. an unchanged body retains its declaration artifact, body-transaction, CFG,
   optimized CFG, and codegen terminal stamps after a sibling-only edit;
3. multiple specializations share one structural body-plan terminal and do not
   add parser, AstGen, instruction/payload clone, or span-remap work;
4. syntax, type, call-resolution, and borrow diagnostics in both ordinary and
   generic bodies retain exact current files, spans, provenance, and ordering
   after prefix, sibling, and source-table/FileId reassignment edits, with no
   synthetic coordinates escaping;
5. anonymous nominal identities retain their indexed frontend structural
   anchors with no post-index remint walk, while member positions affect only
   presentation;
6. identical source content under different `FileId` assignments shares the
   same plan and declaration-relative index, including method and constant
   lookup, while diagnostics relocate through the current locator;
7. parameter, result, comptime, parameter-mode, directive, and accessor-only
   edits each invalidate the complete declaration plan and body transaction as
   appropriate while preserving independent signature/body/constant
   projection stamps;
8. an equal-length but differently ordered symbol interner fails closed, while
   the plan-owned spelling table recreates an exact local facade when needed;
9. an index-heavy artifact is fully charged, can be evicted under budget, and
   is deterministically rederived with equal contents and no global lock;
10. cancellation during a large lower publishes no terminal and charges zero;
    retry succeeds, while foreign-symbol and invalid-anchor producer errors
    retain their typed identity;
11. candidate lookup remains O(1), scheduling continues across the parallel
    ready frontier, and exact duplicate/category/owner identity is preserved;
12. a cold signature-only demand performs zero body AstGen/instruction work,
    demanding one member performs zero sibling-member work, and independently
    ready candidates share no mutable arena or lock;
13. warning reachability agrees with canonical body references across
    shadowing, aliases, imports, qualified calls, anonymous methods, type calls,
    and malformed bodies; and
14. a fault in the replacement path fails the real compile instead of selecting
    another implementation.
