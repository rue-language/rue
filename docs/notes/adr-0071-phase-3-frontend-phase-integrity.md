# ADR-0071 Phase 3 frontend phase-integrity ledger

This note records the current production frontend re-entry points and the
replacement contract for Phase 3. Source and tests remain authoritative. Every
row below is present at trunk revision `2865800c`.

## Deletion ledger

| Re-entry | Production owner and consumers | Displaced work | Required deletion proof |
| --- | --- | --- | --- |
| Declaration signatures | `parse_semantic_signature` in `semantic_query_nucleus.rs`; called by the semantic-nucleus signature evaluator and transient named-body prerequisite classification | Reconstructs a declaration string, lexes and parses it, then copies parameter, result, field, variant, directive, receiver, and accessor facts back out | Deferred: no production call or definition of `parse_semantic_signature`; signature evaluation consumes the declaration artifact directly |
| Runtime and specialized bodies | `lower_owned_body_input` in `revisioned_query_database.rs`; called by both the ordinary-definition and free-function-specialization arms of `BodyTransactionEvaluator` | Concatenates retained signature/body text, builds a synthetic snapshot, lexes, parses, lowers with `AstGen`, builds a body RIR index, and remaps synthetic spans for every body transaction and specialization | Both transaction arms consume the same candidate-keyed structured body plan; no production call or definition of `lower_owned_body_input`; specialization count does not increase parsing or AstGen lowering |
| Anonymous members | `lower_anonymous_member_body_input` in `revisioned_query_database.rs`; called by the anonymous-member arm of `BodyTransactionEvaluator` | Rewrites destructor spelling, wraps the member in a fake named struct, lexes, parses, lowers, indexes, and remaps it | Anonymous-member transactions consume a producer-published structured member plan; no production call or definition of `lower_anonymous_member_body_input` |
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
named declarations, but anonymous members retain their separately listed
synthetic reparse route until their producer-published structured member plan
lands. Accordingly this checkpoint still does **not** claim complete frontend
phase integrity or acceptance items 3 and 12.

The candidate producer still observes the module parse terminal. A sibling-only
module revision therefore reevaluates AstGen once for each reached candidate,
then publishes the prior artifact stamp when canonical packed bytes compare
equal; transactions, CFGs, and codegen remain green. Work is not
multiplied by consumers or specializations, but eliminating this remaining
producer reevaluation requires a finer parser-owned candidate input stamp and
is deferred and measured separately.

## Retained declaration artifact contract

The canonical parse/index boundary publishes compact candidate projections, and
a new definition/candidate-keyed declaration-plan query publishes the one
complete analysis `DeclarationArtifact`. The plan query is not keyed by a
`BodyQueryKey` specialization and is not hidden inside the raw-body query. It is
immutable, independent of `FileId`, absolute offsets, and prefix relocation,
and safe to retain across source revisions. Equality is exact packed-byte
equality and intentionally includes the candidate-relative diagnostic basis,
so internal trivia that moves diagnostic endpoints dirties the artifact:

- a canonical typed-logical packed RIR for exactly the selected declaration
  body. Arena allocation order, orphan payload words, and replacement holes do
  not enter durable identity. Constant initializers and anonymous member bodies use independently
  demandable/retained subplans under the same schema; requesting one never
  eagerly constructs its siblings;
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

Raw signature, body, and constant projections remain independently comparable
and independently stamped. Existing signature/body/constant query families
retain their separate stamps and demand edges. The complete declaration plan's
structural equality/digest, however, covers every semantically relevant part of
the declaration: parameters, result, comptime and parameter modes, directives,
accessor-only state, body structure, and declaration category. Thus a
signature-only edit may keep the raw-body stamp stable while correctly changing
the declaration plan and body transaction that depends on it. Sharing
storage never couples an unchanged raw projection to a sibling edit.

Those projections are also independently demandable and schedulable. One
schema/storage owner is not one eager computation node: a cold signature query
does no body AstGen or instruction work; requesting one member does no sibling
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
   appropriate while preserving independent raw signature/body/constant
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
