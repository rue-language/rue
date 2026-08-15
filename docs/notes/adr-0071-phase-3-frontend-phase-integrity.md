# ADR-0071 Phase 3 frontend phase-integrity ledger

This note records the current production frontend re-entry points and the
replacement contract for Phase 3. Source and tests remain authoritative; rows
marked complete describe paths already removed by the bounded Phase 3 slices.

## Deletion ledger

| Re-entry | Production owner and consumers | Displaced work | Required deletion proof |
| --- | --- | --- | --- |
| Declaration signatures | `compiler.semantic-nucleus` owns a declaration-local dense type-syntax arena projected directly from the exact canonical parsed declaration; specialization typing and call ABI traverse its nodes, while cheap named-body classification uses parser-indexed shell facts | Deletes body-free source reconstruction, signature lexing/parsing, the punctuation-splitting type tokenizer, and the additive retained `compiler.declaration-signature-projection` family | Complete: no production call or definition of `parse_semantic_signature`, no raw-signature or raw-constant parser locator/materializer or peer query family, semantic signature resolution calls only the structured resolver, and a cold signature request performs no body AstGen work |
| Runtime and specialized bodies | `compiler.declaration-body-plan-artifacts` owns one packed candidate artifact; the ordinary-definition and free-function-specialization arms of `BodyTransactionEvaluator` consume it through the transient resolver | Deletes concatenated signature/body text, synthetic snapshots, body-local lex/parse/AstGen, synthetic span remapping, and specialization-multiplied frontend work | Complete: both transaction arms consume the same candidate-keyed structured body plan; no production call or definition of `lower_owned_body_input`; specialization count does not increase parsing or AstGen lowering |
| Anonymous members | `compiler.declaration-body-plan-artifacts` owns the ultimate named/constant producer candidate; the anonymous-member arm recursively selects the exact nested declaration by producer chain, indexed owner anchor, name, and member kind | Deletes destructor spelling rewrites, fake named owners, synthetic source assembly, lex/parse/AstGen, and the second RIR remap/index path | Complete: no production call or definition of `lower_anonymous_member_body_input` or its explicit-anchor lowering seam; nested and constant-produced members directly observe the producer candidate artifact, and member demand adds no candidate AstGen work |
| Comptime bodies | The semantic-nucleus comptime-call evaluator directly decodes the exact `compiler.declaration-body-plan-artifacts` terminal into its request-local semantic evaluator | Deletes fake-function source assembly, body lexing/parsing, duplicate import discovery, cloned AST evaluation, and a second anonymous-anchor transport | Complete: comptime evaluation consumes the same candidate artifact as runtime analysis; no production call or definition of `parse_semantic_body`, and declaration discovery hands the exact artifact terminals to body closure without a second AstGen pass |
| Constants | The semantic-nucleus const evaluator directly decodes the exact `compiler.declaration-body-plan-artifacts` terminal into its request-local semantic evaluator | Deletes fake-const source reconstruction, lexing/parsing, duplicate import discovery, cloned AST evaluation, and a second anonymous-anchor transport | Complete: constant evaluation consumes the declaration artifact directly; no production call or definition of `parse_semantic_const`, and the evaluator resolves packed ordinal spellings without reconstructing an interner |
| Well-known option body scan | `PackedValidatedRir::fallible_intrinsics`; consumed by `compiler.body-toolchain-demands` through `DeclarationBodyPlanArtifacts` | Derives the typed five-kind set during the one canonical packed-RIR traversal; production performs no second lexer pass over retained body text | Complete: the packed header owns the stable typed set and the old lexical scanner remains only as an independent `cfg(test)` oracle |
| Structured type syntax | Candidate RIR owns one declaration-local `RirTypeSyntaxArena<Spur>` projected directly from parser `TypeExpr`; the canonical packed declaration artifact stores that arena in its versioned type section, and ordinary, specialized, anonymous, constant, comptime, and canonical-RIR consumers decode the same nodes | Deletes `AstGen` compound-type rendering/interning and the RIR-to-sema string grammar for arrays, pointers, calls, qualified paths, anonymous aggregates, and integer/value arguments | Complete for every RIR type operand: instruction and payload schemas carry `RirTypeSyntaxRef`, packed encode/decode is exhaustive and fail-closed, semantic consumers traverse structured nodes, and source inventory forbids a rendered-type adapter at the RIR intake. Simple leaf-name lookup plus diagnostic or `--emit rir` formatting remain presentation policy rather than a semantic transport |
| Semantic-nucleus type tokenization | `SemanticNucleusTypeProvider` consumes the structured type/value nodes projected from the exact parsed declaration and packed candidate artifact | Deletes the last provider-local `parse_type_call_syntax` branch that could reinterpret a rendered array-length value with a second handwritten grammar | Complete: provider inventory forbids the rendered parser, lexer/parser entry points, and split/trim grammars; comptime type/value calls resolve only from typed nodes |
| Warning-only static-call discovery | The parser-owned module projection collects imports and declaration-local warning call/type heads during one syntax traversal; `compiler.warning-call-head-projection` is an O(1) candidate projection used solely to retain body-local stamps, and `compiler.warning-body-references` resolves its indexed import occurrences | Deletes the independent `warning_static_call_heads` / `WarningStaticCallCollector` AST walk and its peer lexical-scope/name-resolution implementation | Complete: warning queries contain no AST traversal, raw-body request, RIR/AstGen request, or fallback; shadowing, aliases, imports, named methods, nested anonymous methods, type calls, sibling invalidation, and exact failure behavior are covered |

## Implementation checkpoint: parser-owned signature type syntax

The semantic-signature projection now preserves parser `TypeExpr` structure in
one dense declaration-local arena. Named and qualified paths, unit/never,
arrays and their literal/name/call lengths, slices, pointers, type/value calls,
and integer arguments are emitted once in postorder with a deduplicated spelling
table. `compiler.semantic-nucleus` traverses those nodes through the shared
semantic type-resolution policy. It no longer slices type fragments from source,
reparses them, or runs the former punctuation-splitting dependency tokenizer.
Exact candidate selection, duplicate discriminators, sibling/body independence,
and every parser-legal annotation shape have direct query tests. A source
inventory fails if either the projection or semantic-signature resolver regains
a lexer, parser, split/trim grammar, rendered-type adapter, `Span`, or `FileId`.

This checkpoint by itself did **not** claim the full structured-type row.
The later RIR structured-type cutover extends the same arena through candidate
RIR and replaces the body/constant slots, while the final frontend-integrity
closure below deletes the remaining provider-local `parse_type_call_syntax`
branch.

The one-worker frozen Lattice measurement isolates why this slice should improve
time. The parent rebuilt semantic type structure from compact signature text and
ran a speculative punctuation tokenizer that issued name queries for every
type-looking token. The replacement projects parser nodes once and resolves only
the resulting typed paths. Against exact parent `986b3a51`, six alternating
release-thin-LTO x86-64 pairs reduced query claims and memo nodes by exactly 322.
Declaration-nucleus time fell from a 28.006 ms median to 26.437 ms (-1.569 ms,
-5.6%), semantic time from 296.877 ms to 291.658 ms (-5.219 ms), and compiler
root time from 660.199 ms to 652.051 ms (-8.148 ms, -1.23%). Five of six paired
root observations improved; the median paired effect was -3.671 ms, so the
smaller paired result remains the conservative reading on this interactive
host. External peak RSS fell from 376,799,232 to 367,222,784 bytes by marginal
medians (median paired effect -8,953,856 bytes), although two individual pairs
were positive and the memory result is therefore treated as a non-regression,
not a precise allocation attribution. Every warm and measured executable was
1,662,976 bytes with SHA-256
`45784ce7c7cde992d7ea820912ca1692c05dc7582a367210d017b3765a9a89e7`.

## Implementation checkpoint: RIR structured type-syntax cutover

Candidate lowering now projects every parser type expression directly into the
RIR owner's dense `RirTypeSyntaxArena`. Declaration signatures, struct fields,
enum payloads, body type constants, casts, and anonymous aggregate syntax carry
checked `RirTypeSyntaxRef` operands instead of interned compound spellings. The
candidate's single packed envelope owns a versioned structured-type section;
ordinary, specialized, anonymous, constant, comptime, and canonical-RIR
consumers decode those same nodes and remap only their declaration-local symbol
indexes. No consumer on that route renders a type merely to split or parse it
again. Rendering remains available only for diagnostics, tests, and human RIR
presentation, while simple identifier lookup remains ordinary name resolution.

The packed codec covers every structured node and variable-width payload,
rejects invalid tags, ranges, symbols, forward references, truncation, and
trailing bytes, checkpoints large owners, and rolls back atomically on failure
or cancellation. Its retained charge includes the structured section in the
same `Arc<[u8]>` pointee as the instruction, payload, spelling, anchor, and span
basis data. Parser-to-RIR intake tests cover named and qualified paths,
unit/never, arrays with literal/name/call lengths, slices, pointers, type/value
calls, fixed strings, integers, and anonymous struct/enum declarations. The
specialization tests additionally prove that both type-parameter and
value-parameter dependent annotations retain exact syntax: `[i32; N]` resolves
to `[i32; 3]` rather than leaking the semantic `type` placeholder.

This replaces duplicate text rendering and parsing on the critical path, but it
also introduces structured-node packing and request-local decoding. The first
prototype therefore retired about 38.9 million more instructions on frozen
Lattice and was 2.9 ms slower by absolute medians. Before accepting the slice,
the producer stopped remapping exact signature syntax for concrete callables,
successful semantic lookup stopped eagerly rendering diagnostics, packed
encoding stopped revalidating an already validated type arena, and named types
began resolving by their existing `Spur` rather than allocating and reinterning
a string. Small declaration-local type grammars now avoid allocating a symbol
hash table, while qualified paths resolve their existing symbols directly
instead of copying every segment into a fresh string. Those changes recovered
most of the replacement work without adding another cache or compiler route.

The final exact-parent gate used one warmup and six alternating release-thin-LTO
x86-64 pairs with one query worker. Median process time was 660.438 ms for the
parent and 667.037 ms for the prototype; the paired median was +2.693 ms with a
5.013 ms median absolute deviation. That result is inside host noise and is
recorded as no measurable wall-time change, not a speedup. The paired median
instruction delta was +12,834,157 (about +0.10%), down from the first
prototype's +38.9 million and identifying the remaining structured
encode/decode work without showing a stable elapsed-time regression. External
peak RSS changed by +958,464 bytes by absolute medians and +2,048,000 bytes by
paired medians, below the 3,555,328-byte paired median absolute deviation; two
prototype samples entered a roughly 18 MiB allocator high-water mode. This is
recorded as no measurable RSS change. A rejected experiment that batched codec
checkpoints produced a stable +15.4 MiB paired RSS increase, so the accepted
implementation retains per-node bounded cancellation checks. All twelve native
outputs were 1,662,976 bytes with SHA-256
`45784ce7c7cde992d7ea820912ca1692c05dc7582a367210d017b3765a9a89e7`.
Measurement artifacts are in
`/private/tmp/rue-phase-rir-types-optimized-paired.sLUgdn` on the measuring
host.

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

## Implementation checkpoint: constant and comptime frontend cutover

Constant and comptime semantic-nucleus evaluation now decodes the same packed
candidate artifact used by runtime bodies. The evaluator receives a borrowed
dense spelling view keyed by packed ordinals, so it neither reconstructs a
`ThreadedRodeo` nor parses a synthetic const/function. The old raw-constant
query family, payload/failure algebra, initializer/type-span locator, fragment
materializer, and lifecycle tests are deleted. The raw-body query and fragment
materializer remain `cfg(test)` only as a deleted-route oracle; neither is
registered in production.

Declaration discovery and body closure are separate rooted requests. The
candidate artifact family deliberately keeps only a small unrooted history, so
dropping the declaration request previously let validation evict and lower the
same const/comptime candidate again before body closure consumed it. The
declaration publication now hands off leases for exactly the candidate-artifact
family observed by that request. It does not retain or walk the much larger
declaration dependency cone. Body closure validates its other dependencies
normally and atomically replaces this temporary bridge with the published
closure root. A query-runtime regression proves the family filter excludes
unrelated observed terminals, and the real compiler regression proves one
AstGen evaluation per reached candidate rather than one per root or
specialization.

The expected wall-time effect is therefore causal rather than inferred from a
smaller query count: synthetic lex/parse/AstGen and mutable-interner rebuilds
leave the one-worker declaration critical path, and the narrow handoff avoids
paying those costs again in body closure without adding a full-cone validation
walk.

The frozen Lattice workload was measured against the exact parent with release
thin-LTO x86-64 compilers in twelve alternating one-worker pairs after warming
both binaries. All 24 outputs were 1,413,120 bytes with SHA-256
`b893e76cfabed737b149d0e8c4d8527077dedd17da78418db20a28a7d30885e5`.
Absolute median compiler-root time fell from 670.637 ms to 663.632 ms
(-7.005 ms); the noisier paired median delta was -2.507 ms and 7 of 12 pairs
were faster. The narrower declaration-graph phase, where the duplicate work
was removed, fell from 37.594 ms to 35.881 ms; its paired median delta was
-1.893 ms and 11 of 12 pairs improved. Median external peak RSS fell from
376,365,056 to 374,661,120 bytes (-1,703,936 bytes), while the benchmark's
internal peak fell from 376,143,872 to 374,407,168 bytes (-1,736,704 bytes).
The paired median external and internal RSS deltas were -1,687,552 and
-1,769,472 bytes. Query claims and memo nodes each fell by three; the
consistent local-phase reduction and smaller, noisier root reduction match the
expected effect of removing duplicate producer work from one part of the full
compile rather than deleting a large bookkeeping-node population.

## Implementation checkpoint: direct request materialization

The request-local body adapter now decodes a packed candidate directly into a
fresh validated RIR owner. The typed packed decoder already checks every
instruction, payload, reference, symbol ordinal, and span slot while appending,
so a fresh-owner decode no longer populates a duplicate-symbol hash map and then
walks the completed arena again through `ValidatedRir::finish`. The dense local
symbol interner is still rebuilt once because AIR consumes ordinary `Spur`
values, but ordinal identity is checked during that one construction rather
than copied into a second remap vector. Generic module composition retains its
full append-boundary validation because it relocates into a nonempty arena.

Local semantic materialization also no longer clones and freezes the complete
request-local type universe merely to enumerate aggregate handles before
freezing the original universe for publication. It snapshots only the compact
`Type` handles, preserving shared-base/overlay order and exact aggregate export
while avoiding the duplicate definitions, lookup indexes, and containment
metadata.

Both removals are expected to improve wall time: the frozen workload performs
1,263 body decodes and 1,280 local semantic materializations, so the displaced
linear scans and type-universe copies were on the one-worker critical path, not
merely retained bookkeeping. Two independent runs of twelve alternating
baseline/prototype pairs used release thin-LTO x86-64 compilers and the frozen
one-worker Lattice command. All 48 outputs were 1,413,120 bytes with SHA-256
`b893e76cfabed737b149d0e8c4d8527077dedd17da78418db20a28a7d30885e5`.
Across all 24 pairs, 20 prototype runs were faster and the paired median
compiler-root delta was -7.686 ms. Absolute median root time fell from
677.577 ms to 666.053 ms. The two directly affected phases improved
consistently: paired median body-input lowering fell by 2.137 ms and semantic
materialization by 3.105 ms.

This slice adds no query terminal or retained artifact. Claims, memo nodes, and
memo display-identity bytes were byte-for-byte unchanged. Peak RSS was
effectively flat but slightly positive in the combined median: +155,648 bytes
externally and +221,184 bytes in the compiler report (absolute medians
374,923,264 to 375,504,896 and 374,652,928 to 375,160,832 bytes). That sub-MiB
movement is reported as measurement noise, not a memory improvement; the
wall-time claim rests on the repeatable phase-local reductions and the 20/24
paired root wins.

## Implementation checkpoint: parser-owned warning projection closure

The parser's one module syntax projection now discovers declaration-local
warning call/type heads while it discovers imports. Lexical locals and static
aliases use parser-local `Spur` identities during that traversal; only the
sorted, deduplicated retained heads are converted to owned spellings. Imported
heads carry the exact declaration-local import occurrence and specifier, so
`compiler.warning-body-references` continues to resolve imports through the
canonical declaration-import query rather than performing path resolution
itself.

A thin candidate-keyed `compiler.warning-call-head-projection` terminal performs
one O(1) lookup into the parsed definition index. It retains no AST handle and
does no syntax walk. The terminal is intentionally kept because its exact
equality preserves body-local warning stamps when a sibling body changes; the
downstream warning-reference query therefore does not need a broad module stamp
or a second AST pass. The former `warning_static_call_heads`, candidate span
search, `WarningStaticCallCollector`, and peer scope/alias resolver are deleted.

The semantic-nucleus provider also no longer interprets a rendered array-length
value with `parse_type_call_syntax`. Structured comptime calls arrive through
the declaration/candidate type-syntax arenas; simple names continue through
ordinary substitution and const lookup. Source inventory forbids the removed
parser branch and every lexer/parser or split/trim grammar inside that provider.

The expected critical-path improvement is modest: the replacement removes one
whole declaration-body AST traversal and temporary scope/name allocations, but
it preserves the small candidate projection terminal required for exact
invalidation and it adds warning-head collection to the already-required parse
pass. The rendered-type deletion is a correctness/deletion proof and was not a
reachable production cost.

The exact-parent gate used one warmup followed by twelve order-balanced
release-thin-LTO x86-64 pairs with one query worker. Median compiler-root time
was 664.504 ms for the parent and 664.125 ms for the replacement; the paired
median was +1.850 ms with a 5.252 ms median absolute deviation, and 5 of 12
prototype runs were faster. That is recorded as no measurable wall-time change,
not a speedup. The displaced work is nevertheless observable: all twelve
prototype runs retired fewer instructions, with a paired median reduction of
15,800,058, while query claims, memo nodes, and memo display-identity bytes were
exactly unchanged. Source discovery and parsing increased by a paired median
1.138 ms because it now owns the projection; the independent later warning AST
walk and its attribution disappeared.

Peak RSS was allocator-bimodal on this interactive macOS host. External paired
medians moved +6,914,048 bytes with a 10,002,432-byte paired MAD, and exactly six
of twelve prototype runs used less memory. A separate counting-allocator run
showed the replacement made 8,607 fewer allocations and requested 5,085,156
fewer bytes. The RSS result is therefore recorded as inconclusive/no claimed
memory improvement rather than as a stable ownership regression. All 24 native
outputs were 1,662,976 bytes with SHA-256
`45784ce7c7cde992d7ea820912ca1692c05dc7582a367210d017b3765a9a89e7`.
Measurement artifacts are in
`/private/tmp/rue-phase-warning-paired.gsJc0W` on the measuring host.

## Implementation checkpoint: direct body-transaction projections

Body reachability already requests and inspects each exact `BodyTransaction`.
It now reads that transaction's immutable `BodyReferences` directly instead of
requesting a registered `compiler.body-references` projection which queried the
same transaction and cloned the same Arc. The rooted closure's existing
`compiler.body-analysis-bundle` continues to own the transaction/producer
lifecycle boundary, but it no longer requests a registered
`compiler.canonical-body` projection solely to recover the `CanonicalBody` Arc
already stored in its transaction. Inventory tests forbid both deleted family
names.

The expected critical-path effect is bounded but real: every one of Lattice's
1,263 reached bodies avoids two memo claims, two dependency validations, and
two terminal envelopes. The replacement work is an immutable field read in
reachability and the existing closure-bundle transaction clone; body analysis,
producer scheduling, bundle invalidation, diagnostics, and CFG input assembly
are unchanged.

The exact-parent gate used one warmup followed by twelve order-balanced
release-thin-LTO x86-64 pairs with one query worker. The interactive host was
heavily loaded, so wall time was not statistically distinguishable: the paired
median moved -6.925 ms with a 230.962 ms paired MAD and 6 of 12 prototype runs
were faster. That is recorded as no measured wall-time speedup. The displaced
CPU work was stable: 11 of 12 prototype runs retired fewer instructions, with
a paired median reduction of 75,605,727 instructions and a 19,344,008 paired
MAD.

Query claims and memo nodes each fell by exactly 2,526, from 36,433 to 33,907
and 35,720 to 33,194. Memo display-identity storage fell from 8,969,675 to
8,035,081 bytes (-934,594). External peak RSS had a paired median reduction of
1,302,528 bytes (8 of 12 pairs lower), while compiler-reported peak RSS fell by
a paired median 1,245,184 bytes (9 of 12 lower). All 24 native outputs were
1,662,976 bytes with SHA-256
`45784ce7c7cde992d7ea820912ca1692c05dc7582a367210d017b3765a9a89e7`.
Measurement artifacts are in
`/private/tmp/rue-body-two-projection-paired.kZHG3f` on the measuring host.

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

Signature projection remains independently demandable inside the authoritative
semantic-nucleus evaluator; there is no second retained signature family.
The raw-constant query family and source-fragment materializer are deleted;
the raw-body family remains a `cfg(test)` deleted-route oracle only. Production
constant, comptime, runtime, specialized, and anonymous evaluation all consume
the candidate artifact. The
complete declaration plan's structural equality covers every semantically
relevant part of the declaration: parameters, result, comptime and parameter
modes, directives, accessor-only state, body structure, declaration category,
and declaration-relative diagnostic basis. A signature-only edit can therefore
leave unrelated body terminals green while correctly changing the candidate
artifact and every transaction or semantic-nucleus projection that observes
that signature. Shared storage never couples an unchanged candidate to a
sibling edit.

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
   edits each invalidate the complete declaration plan, body transaction, and
   const/comptime semantic projection as appropriate while preserving
   independent unrelated signature and body stamps;
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
