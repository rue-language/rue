---
id: 0051
title: CanonicalImportGraph as the sole import-resolution authority
status: accepted
tags: [architecture, compiler, modules, incremental, tooling]
feature-flag: null
created: 2026-07-13
accepted: 2026-07-13
implemented:
spec-sections: []
superseded-by:
amended-by: [0063]
relates: ["ADR-0026", "ADR-0047", "ADR-0050", "RUE-806", "RUE-807", "RUE-808", "RUE-809", "RUE-810"]
---

# ADR-0051: CanonicalImportGraph as the sole import-resolution authority

## Status

Accepted under RUE-806 after adversarial review, 2026-07-13. RUE-807 through
RUE-810 implement it. This is an internal compiler and tooling design. It does
not change import syntax or language semantics, so it has no preview feature and
requires no specification change. [ADR-0063](0063-parallel-demand-driven-incremental-compilation.md)
amends the closed-whole-graph staging boundary into batched, rooted demand
fulfillment while retaining this ADR's compiler authority, host read policy,
candidate precedence, provenance, typed observations, and ledger contracts.

## Summary

`CompilerSession::CanonicalImportGraph` is the sole authority for import
resolution outcomes and canonical module identity. Parser-owned import sites are
the sole recognition source. The compiler derives ordered filesystem discovery
requests from those sites and explicit resolution inputs; discovery performs the
requested filesystem operations but owns no resolution policy. Semantic analysis
consumes resolved import records and never re-resolves paths or reads the
environment. One compiler diagnostic projection maps canonical outcomes back to
parser-owned occurrences and spans. Dependency output is a presentation of the
canonical graph plus separate observation and accepted-read ledgers.

The graph is authoritative only after discovery reaches a closed source snapshot.
Intermediate parse/discovery iterations are update work, not semantic artifacts.

## Context

The current implementation has one emerging canonical path and several competing
paths:

- `crates/rue-compiler/src/parsed_modules.rs` walks the parser AST and retains
  valid, exactly-one-string-literal import occurrences with stable `ModuleId`,
  source offset, and specifier. `CompilerSession::import_graph` in `session.rs`
  resolves those records into an occurrence-independent
  `CanonicalImportGraph`.
- `crates/rue/src/main.rs::discover_and_load_imports` independently recognizes
  imports with a five-token window, derives candidate groups, reads
  `RUE_STD_PATH`, probes and reads files, records missing imports, and constructs
  a separate `DependencyGraph`.
- AIR still resolves imports from paths. `sema/file_paths.rs`,
  `sema/module_path.rs`, `sema/analysis/intrinsics.rs`, and declaration/body
  consumers match loaded paths, derive candidates, canonicalize path spellings,
  and in some cases read `RUE_STD_PATH` again.

These paths can disagree about which syntax is an import, candidate precedence,
standard-library context, ambiguity, physical aliases, diagnostics, and
dependency output. Keeping a shared helper for some path operations does not
solve the ownership problem: each caller still decides when and why to invoke
the policy.

The current canonical graph already has the right durable shape: logical
`ModuleId` values, normalized specifiers, and `Resolved`, `Missing`, or
`Ambiguous` outcomes. Its validation reports missing and ambiguous outcomes as
problems and reports strongly connected components separately. Cycles are legal
Rue topology; they are not graph-construction failures. The current durable
records intentionally collapse repeated occurrences, so diagnostics additionally
need a parser-owned occurrence-to-record projection rather than source positions
inside the durable graph.

ADR-0026 defines import semantics, ADR-0047 separates the semantic import graph
from declared build inputs, and ADR-0050 makes canonical imports an explicit
semantic dependency. This ADR assigns one owner to the computation those designs
depend on.

## Decision

### Four distinct responsibilities

Import handling is divided into four boundaries:

1. **Recognition — parser-owned.** The parsed module artifact is the only source
   of import occurrences. It records a valid-request site or an invalid-shape
   site. A valid site contains the importer `ModuleId`, exact decoded specifier,
   source offset/span provenance, and a stable occurrence key for projection;
   invalid shape retains the intrinsic diagnostic operands/span but never yields
   a discovery request. Token windows, RIR rescans, and semantic rediscovery are
   not import-recognition paths.
2. **Candidate enumeration and resolution — compiler-owned.** A canonical import
   query in `CompilerSession` derives ordered candidate groups from recognized
   sites, the semantic root, the current logical-module/physical-location input
   map, and the captured standard-library context. Before closure it emits an
   immutable discovery plan. After closure it alone produces
   `CanonicalImportGraph` outcomes and module identities.
3. **Physical I/O — discovery-owned.** The CLI or embedding host executes the
   compiler's discovery plan. It applies the source-read policy, performs only
   the requested metadata probes and reads, and returns typed physical
   observations and accepted bytes. Compiler-owned assembly performs identity
   deduplication and creates `SourceFile` inputs and accepted-read-manifest
   entries. The host does not recognize imports, invent candidates, choose
   precedence, assign logical identity, or declare an import resolved.
4. **Diagnostic presentation — compiler-owned.** One canonical diagnostic query
   joins graph records and discovery failures back to parser-owned occurrences.
   The CLI only renders those diagnostics. AIR does not create competing missing,
   ambiguity, or standard-library diagnostics.

This separation keeps filesystem access outside the pure compiler query while
preventing “discovery” from becoming a fourth resolution-policy implementation.

### Canonical discovery plan

For every valid parsed occurrence, the compiler emits ordered candidate groups
using the language's one resolution policy. A request identifies:

- the importer logical `ModuleId` and parser occurrence key;
- the importer and root physical anchors supplied as explicit inputs;
- the exact and normalized specifier;
- the ordered group and candidate position;
- the expected candidate role (exact file, file module, directory facade, or
  standard-library facade); and
- the captured normalized standard-library root, when applicable.

The plan, not the filesystem executor, determines importer-before-root
precedence, file-versus-directory grouping, and standard-library precedence.
Discovery executes groups in order and returns a policy-decision or physical
observation event for every request needed to decide the first applicable group.
Within an ambiguity group it must observe and, when allowed, load every present
candidate; it cannot stop after the first file. It need not execute later groups
after an earlier group has been conclusively established.

Candidate request results distinguish at least absent, present-readable,
present-unreadable, denied by the source-read policy, and invalid physical type.
They retain normalized requested spelling and, after a successful probe,
canonical physical identity. Request events and physical observations are values
supplied back to the compiler, not resolution outcomes. The closed-snapshot graph
reduces them and the loaded module inputs to `Resolved`, `Missing`, or
`Ambiguous`.

### Discovery fixed point

Parser-owned recognition requires parsing the modules already present in a
snapshot. Transitive discovery therefore follows this fixed-point protocol:

1. Capture immutable invocation inputs: root source, source-read policy,
   normalized `RUE_STD_PATH` context, target-independent module resolution
   policy version, explicit identity roots, and any already supplied source
   modules. This creates a discovery epoch and a staging session revision; it
   does not change the committed last-good revision.
2. Assemble and parse the current immutable staging `SourceSnapshot` through
   `CompilerSession`. The current production parser is all-or-nothing: if any
   module fails lexing or parsing, abort the attempt with syntax diagnostics.
   There is no partial import-site artifact and no discovery plan from a failed
   parse.
3. Ask the compiler for the canonical discovery plan derived from that parsed
   snapshot. Malformed import calls are absent from the plan.
4. Execute outstanding requests as observation transactions. Return bytes and
   physical observations to compiler-owned source-input assembly; the host does
   not assign logical module identity. The compiler recovers or assigns the
   stable identity and appends newly accepted modules to a new staging snapshot.
5. If modules were added, publish only to staging and repeat from parsing. Sites
   in newly discovered modules can add requests; completed observations may be
   reused only within this immutable discovery epoch.
6. When an iteration adds no module and every required request has a conclusive
   observation, build and validate the `CanonicalImportGraph` for that closed
   staging snapshot. Atomically adopt the staging revision only if the graph is
   valid; otherwise retain it as a revision-labeled attempted artifact.

An executor refusal, cancellation, or inability to produce every required
observation is non-closure and fails the attempt. Physical deduplication makes
cycles terminate: a back-edge reuses the already loaded physical file, while the
final graph retains the logical cycle.

### Artifact and session states

`CompilerSession` must distinguish a staging revision from the committed
last-good revision. Import processing has exactly three artifact states:

1. **Open discovery iteration.** Parsing succeeded for the current staging
   snapshot, but requests remain or newly read modules require another parse.
   Plans and observations are staging work only. Sema, canonical import
   diagnostics, and dependency topology cannot consume this state.
2. **Closed attempted graph with resolution problems.** Discovery reached a
   fixed point, so the revision-labeled graph can contain canonical `Missing` or
   `Ambiguous` records and can be joined with observations and sites. Publish
   this attempted graph for canonical diagnostics and explicit attempted
   deps/tooling inspection, but block sema. It never replaces the last-good
   successful graph or successful dependency artifacts.
3. **Closed valid graph.** Discovery is closed and validation has no resolution
   problem. Adopt the staging snapshot, graph, site projection, observation
   ledger, and accepted read manifest atomically as the new committed
   semantic/dependency head.

Parse failure, denied or failed I/O, invalid identity, and non-closure have no
closed graph; they publish only revision-labeled attempted diagnostics and
available request/observation events. Batch compilation selects the attempted
revision and fails. An editor may display those attempted diagnostics while
continuing to query explicitly labeled last-good semantic artifacts. Neither
mode may confuse last-good results with the attempted revision.

Structural graph invalidity such as a foreign importer/target, conflicting
duplicate record, root mismatch, or identity collision is an input/compiler
failure and has no publishable closed attempted graph. The attempted-graph state
is reserved for a structurally sound closed graph whose only validation problems
are canonical resolution outcomes such as `Missing` or `Ambiguous`.

### Identity and provenance

The durable compiler `rue_compiler::ModuleId` is a normalized logical-path
string. It is not `rue_air::types::ModuleId`, which is a compact request-local
`u32`. Physical paths, canonical filesystem keys, and `FileId` are also separate.

Compiler-owned source-input assembly owns the durable mapping. The host returns
candidate bytes and physical observations only. Assembly classifies an accepted
source using the immutable invocation identity roots:

- **Project.** The root source fixes the project root at its lexically normalized
  parent. The root and project candidates use their normalized lexical path
  relative to that root, preserving the current relocation-stable identity
  scheme. This root is an identity anchor, not a new containment restriction:
  current legal `..` imports can produce normalized project-relative paths with
  leading `..`, subject to the read policy. A result on an incompatible volume
  or otherwise lacking one unique normalized relative logical path is a typed
  input failure.
- **Standard library.** A candidate under the captured normalized std root uses
  its relative path under the reserved, disjoint `"\0rue-std"` namespace. No
  environment read participates in this classification.
- **Explicit supplied module or named root.** An explicitly supplied module
  carries a durable logical ID and physical input that assembly validates.
  Future named roots must be explicit invocation inputs with unique normalized
  names and normalized physical roots; they are never inferred by discovery.
  Their logical IDs use the disjoint reserved
  `"\0rue-root/<name>/<root-relative-path>"` namespace; invalid names, overlap,
  or ambiguous classification are typed failures. A source that cannot receive
  the current project-relative identity and is outside std/named mappings is a
  typed failure, not an ad hoc path identity.

Classification is deterministic: validate an explicit supplied mapping first;
otherwise a physical source under the std root is std, one under a named root is
named, and every source for which the project anchor yields a unique lexical
relative path is project. Std and named roots must not overlap each other, and
explicit mappings must agree with this derived role. A second route that claims
an already canonicalized physical file under a different role or logical ID is a
typed conflict rather than a new module.

The logical relative component is computed from the normalized accepted
candidate spelling and its lexical identity root, never from the canonical
physical target; this preserves relocation-stable current behavior. Canonical
physical identity is used only for post-policy validation, alias deduplication,
and conflict detection. If the lexical role and canonical-target role disagree,
the observation is an escape/identity conflict rather than a silent remapping.

The resulting `SourceMetadata`/`SourceSnapshot` mapping is the one input to
parsing, `CanonicalImportGraph`, stable definitions, and generated symbol
identity. Symbol derivation consumes this exact mapping; it cannot reread the
environment or derive a parallel logical path. Logical-path collisions, one
logical ID naming different canonical physical files, or incompatible explicit
and derived mappings fail the attempt.

Canonical physical identity deduplicates aliases during the discovery epoch. If
an alias reaches an already accepted file, assembly reuses its existing durable
ID. Reaching the same physical file under incompatible project, std, or named
root roles is a typed identity conflict; no role wins by discovery order. Two
distinct allowed physical candidates in the same ambiguity group receive
distinct durable IDs and produce `Ambiguous`.

For each semantic epoch the compiler creates a deterministic bijection from
durable compiler IDs to compact AIR IDs by sorting the closed module set in
canonical compiler-ID order and allocating `rue_air::types::ModuleId` indices in
that order. AIR's `ModuleRegistry` is populated from this bridge before analysis;
path-keyed or insertion-order `get_or_create` allocation is prohibited. The
bridge supplies `Type::Module`, `ModuleDef`, member binding, and the joins from a
local AIR module ID to current `FileId`/physical provenance. This preserves AIR's
compact indices without treating them as durable identity.

A `FileId` and span identify source text only for the current parser/diagnostic
epoch and never enter durable graph equality. The occurrence projection retains
enough parser provenance to map each durable record, including repeated sites
with the same importer/specifier, back to every current span.

### Observation transaction and manifests

For each candidate request the host executes this ordered transaction:

1. lexically normalize the requested path and apply the declared source-read
   policy without filesystem I/O;
2. if permitted, canonicalize and stat the candidate as one metadata observation;
3. reapply the source-read policy to the canonical target, rejecting symlink or
   alias escape before content access;
4. require a regular source file and read its bytes; and
5. return the accepted bytes with a content fingerprint plus the distinct
   metadata identity/fingerprint observed for that transaction.

The accepted bytes and their content fingerprint are authoritative for the
staging `SourceFile`; metadata identity is only physical provenance and change
detection. This sequence narrows time-of-check/time-of-use exposure and prevents
a permitted lexical path from granting access through an escaping symlink. A
host that detects the target changing during canonicalize/stat/read returns a
typed unstable-read failure and retries only in a new transaction; it never
combines metadata from one file state with bytes from another.

There are three separate manifest values:

- the **declared source manifest/read policy**, which grants or denies requested
  operations but never semantic scope;
- the **observation ledger**, which records every request and status, including
  lexical-policy denial, metadata absence, unreadable/non-file results, and
  successful observations; and
- the **accepted read manifest**, which records only roots and regular source
  contents actually read and accepted, with canonical physical identity and
  content fingerprint.

A denial is a request-policy event, not an executed probe. A combined serialized
ledger may carry all operation/status variants, but it must preserve that
distinction and cannot call denied or absent candidates source reads.

### Fail-closed behavior

Import processing fails closed as follows:

- **Malformed syntax or intrinsic shape.** Parse errors remain parser
  diagnostics and abort the staging parse without a partial discovery plan. A
  successfully parsed `@import` with the wrong arity or a non-string argument
  remains the intrinsic shape error owned by the intrinsic diagnostic contract.
  It never becomes a discovery request, a missing-module record, or a filesystem
  read.
- **Missing.** Conclusive absence across every candidate in every applicable
  group produces `Missing`. The graph retains this outcome for diagnostics and
  tooling, but graph validation prevents semantic success. A missing std facade
  projects to the canonical std diagnostic rather than an environment-dependent
  AIR decision.
- **Unreadable or invalid candidates.** A candidate that exists but cannot be
  read as a regular Rue source is not absence. Discovery returns its typed I/O,
  encoding, or file-type failure; the update fails and cannot publish a closed
  graph that pretends another candidate won.
- **Source manifests and escape.** A source manifest/read policy can only remove
  permission to execute a compiler-generated request. It cannot add a search
  root, candidate, module, or name to semantic scope. Candidate paths are
  lexically normalized before a no-I/O policy check, and the canonical target is
  checked again before reading. A request or canonical target outside the
  permitted roots or declared source set is denied without content access; if
  that event is needed to decide an import, the update fails as
  undeclared/escape rather than treating the candidate as absent. Legitimate
  `..` spellings remain governed by the current language resolution policy;
  manifest membership cannot authorize a path that policy did not enumerate.
- **Ambiguity.** Every permitted present candidate in the winning group is read
  and appended before closure. The graph records both logical module identities
  in `Ambiguous`, validation fails closed, and the diagnostic projection reports
  every parser occurrence.
- **Cycles.** Cycles are legal topology. Reads are deduplicated by canonical
  physical identity, the fixed point terminates, and graph validation reports
  stable cycle components separately from problems. No diagnostic is emitted
  merely because a cycle exists unless the language later adopts an independent
  semantic restriction.
- **`RUE_STD_PATH`.** The host reads it exactly once at invocation/update
  capture, validates and lexically normalizes it, and passes it as an explicit
  optional input. Discovery, compiler consumers, AIR, diagnostics, symbol
  identity, and dependency emission never read the environment independently.

### Diagnostic precedence and projection

The compiler emits import diagnostics in this order:

1. parser syntax and intrinsic shape diagnostics;
2. discovery policy-denial, unstable-read, file-type, encoding, and I/O failures
   at each affected occurrence; and
3. closed-graph `Missing`, std-missing, and `Ambiguous` diagnostics in parser
   occurrence order.

A discovery failure for a site suppresses a synthetic `Missing` for that site.
Any import error blocks sema, so unrelated declaration/body errors cannot mask or
reorder import failures. Cycles alone emit no diagnostic. Repeated sites each
receive a projected diagnostic even though the durable graph record is
occurrence-independent. Primary diagnostics retain the user's exact specifier;
candidate notes use the canonical ordered list from the compiler discovery plan.

### Determinism, caching, and updates

Import occurrences are ordered by logical importer, source offset, then exact
specifier. Canonical graph records are ordered by logical importer, normalized
specifier, then outcome; occurrence deduplication in the durable graph does not
discard the separately ordered site projection. Candidate groups and request
events retain policy order. The observation ledger follows canonical plan order;
the accepted read manifest is ordered by durable module ID then canonical
physical identity. Neither uses filesystem enumeration or hash-map order.

The discovery-plan cache key contains the parsed `SourceRevision`, root and
module logical-to-physical mapping, identity roots, normalized captured std
context, source-read policy/manifests, and resolution-policy/schema version. The
closed graph cache key contains the closed source/module revision, plan, relevant
observations, and std context.

Filesystem observations and accepted reads may be reused only inside one
immutable discovery epoch. A new update epoch re-executes them unless the host
supplies a trustworthy filesystem/read-policy revision or watch token whose
contract proves the candidate metadata, permissions, and contents unchanged;
that token then participates in every observation key. A normalized candidate,
operation kind, policy revision, and metadata identity/fingerprint key the
physical observation. Accepted source reuse additionally requires the content
fingerprint. Metadata identity and content fingerprint are never interchangeable.

Any change to source text, parsed import sites, root, module set, logical or
physical identity mapping, std context, declared manifest/read policy, candidate
metadata, accepted bytes, readability, or observation outcome invalidates the
affected plan/observation and every dependent graph/semantic artifact. Pure
parsed modules and plans may be reused by their exact immutable keys; physical
observations obey the epoch rule above. Discovery adding a module creates a new
staging source revision before another plan is requested; it never mutates either
the staging or committed snapshot in place.

### Consumer contract

AIR receives a resolved-import table keyed by durable compiler `ModuleId` plus
normalized specifier (or an equivalent stable record handle), together with the
deterministic compiler-ID-to-AIR-ID bridge. Binding an import selects the
record's target through that bridge. `Type::Module`, `ModuleDef`, member access,
visibility, and current `FileId` provenance use compact AIR IDs internally. AIR
receives neither candidate paths nor std/environment context and cannot resolve,
canonicalize, probe, or allocate modules by path.

Semantic dependency capture consumes the same `CanonicalImportGraph` records.
For every closed revision, the versioned `--emit deps` envelope and future build
integrations present three revision-aligned components:

1. canonical topology and resolution outcomes from the closed graph;
2. the complete observation ledger; and
3. the accepted read manifest.

They do not use a peer CLI `DependencyGraph`, rerun sema, or infer semantic edges
from files that happened to be probed or read. A closed valid revision emits a
`complete` envelope with fully resolved topology and exits successfully. A
closed attempted graph with `Missing` or `Ambiguous` emits the same envelope
with `incomplete` status, canonical unresolved records, the complete observation
ledger, and accepted staging reads; it also prints canonical diagnostics and
exits unsuccessfully. The incomplete envelope is revision-labeled and cannot
replace or masquerade as a successful dependency artifact. Parse,
discovery-I/O, identity, and non-closure failures have no canonical topology to
emit, but their request/observation events remain available to diagnostics and
tooling.

### Current-path disposition

| Current path or concern | Disposition | Required end state |
| --- | --- | --- |
| `parsed_modules.rs` AST walk and parsed import sites | **Keep and strengthen** | Sole recognition path; retain occurrence/span projection and feed the compiler discovery plan. |
| `CompilerSession::import_graph` and `CanonicalImportGraph` | **Keep and change** | Sole candidate-policy, final resolution, validation, durable module-identity, and topology authority; add staging/committed state, the pre-closure discovery-plan query, and explicit observations. |
| `SourceMetadata`/`SourceSnapshot` assembly and symbol paths | **Change** | Compiler-owned assembly assigns/reuses the one durable logical mapping from explicit identity roots; parsing, graph, and symbols consume it unchanged. |
| `import_graph.rs::ModulePath`/candidate helpers | **Change/relocate** | One compiler-owned pure policy implementation used to create plans and final outcomes; no driver or AIR policy calls. |
| CLI five-token-window recognition in `discover_and_load_imports` | **Remove** | RUE-807 replaces it with execution of parser/compiler-owned requests. |
| CLI candidate enumeration and resolution decisions | **Remove** | The CLI checks policy and performs requested metadata/read operations only. |
| CLI `DependencyGraph` | **Remove/replace** | Both complete and incomplete envelopes contain canonical topology/outcomes, the observation ledger, and accepted reads; incomplete resolution exits unsuccessfully and never replaces a successful artifact. |
| CLI `SourceManifest` | **Keep and change** | Explicit read-policy input and cache key; constrains requested physical operations, never adds modules or scope, and reports denied/escape outcomes. |
| CLI/std symbol derivation reads of `RUE_STD_PATH` | **Change** | Capture and normalize once with all invocation inputs; pass the value through identity, discovery, graph, and symbol-path queries. |
| `rue-air` `sema/file_paths.rs::resolve_import_path` | **Remove** | Sema consumes resolved records. File/source path lookup remains only for diagnostics, visibility, symbol metadata, and current-revision joins where physical provenance is intrinsically required. |
| `rue-air` `sema/analysis/intrinsics.rs` import/std resolvers | **Remove** | Parser/compiler preflight owns import shape diagnostics; successful import binding uses a canonical resolved record and never reads `RUE_STD_PATH`. |
| `rue-air` `module_path.rs` candidate/resolution policy | **Remove from AIR** | Candidate policy belongs to the compiler graph query; retain only unrelated path utilities if still needed. |
| `rue-air/src/module_registry.rs` | **Change** | Prepopulate a deterministic durable-compiler-ID-to-local-AIR-ID bijection in canonical order; remove path-keyed/insertion-order semantic creation. |
| `rue-air/src/types.rs::ModuleId` | **Keep as local** | Remains a compact semantic-epoch `u32` used by `Type::Module`/`ModuleDef`; never serves as durable compiler identity or a cache key across epochs. |
| AIR `canonical_file_id` and module physical-path joins | **Change** | Join local AIR ID through the deterministic bridge to durable ID and current provenance; retain physical paths only as thin current-revision metadata adapters, not resolution. |
| Declaration, comptime, type-check, member-access, and body import consumers | **Change** | All consume the same resolved records; none accepts path/env inputs or creates import diagnostics. |
| Revisioned semantic module-input edges | **Keep and tighten** | Consume `CanonicalImportGraph` through canonical per-key queries; invalid/non-closed graphs fail closed and no import rediscovery occurs. ADR-0063 retired ADR-0050's whole-program manifest projection. |
| Missing/ambiguous/std diagnostic creation across CLI and AIR | **Remove/centralize** | One compiler diagnostic projection from outcomes/observations to every parser-owned site. |
| Intrinsic wrong-arity/non-string diagnostics | **Keep behavior, change owner** | Parser-owned invalid-shape sites project the existing intrinsic diagnostics before sema and never initiate discovery. |
| Declared source manifest, observation ledger, and accepted read manifest | **Keep distinct** | Permission, request/operation outcomes, accepted content reads, and semantic topology are separate revisioned values; denial is not a probe and a probe is not scope. |

## Implementation Phases

- [ ] **Phase 1: Parser-owned discovery protocol** — RUE-807. Add the
  compiler-owned discovery-plan/observation fixed point, staging/committed
  revision states, durable source-input identity assembly, transactional
  observation ledgers, and the minimum site projection that preserves existing
  shape/missing/ambiguous/std diagnostics. Capture std/read-policy inputs once
  and replace CLI token-window and candidate-policy discovery. RUE-807's issue
  scope and acceptance criteria must be updated to include these requirements
  before implementation, or its work must be split without changing this phase's
  architectural boundary.
- [ ] **Phase 2: Semantic resolved-record consumption** — RUE-808. Thread
  canonical resolved records and the deterministic durable-to-local ID bridge
  into AIR, then remove AIR path/environment resolution and path-keyed module
  creation. It depends on RUE-807's minimum diagnostic projection; every import
  problem must block sema, so removal cannot create a diagnostic gap or change
  missing/ambiguous/std behavior.
- [ ] **Phase 3: Canonical diagnostics** — RUE-809. Generalize the RUE-807
  projection into the complete precedence, ordering, repeated-site,
  discovery-failure suppression, and attempted-revision presentation contract.
  Remove remaining peer CLI/AIR import diagnostic paths. It depends on RUE-807
  and RUE-808.
- [ ] **Phase 4: Dependency/read-manifest presentation** — RUE-810. Replace the
  CLI dependency graph with deterministic envelopes containing canonical
  topology/outcomes, observations, and accepted reads; emit closed attempted
  graphs with explicit incomplete status, and ensure failed `--emit deps` never
  overwrites successful dependency artifacts. It depends on the RUE-807
  ledgers/state model and RUE-809 failure projection.

The implementation order is RUE-807, RUE-808, RUE-809, then RUE-810. These
issues implement the decisions above; they require no further architecture
choice about recognition, resolution, I/O, identity, diagnostics, or dependency
ownership.

## Consequences

### Positive

- One immutable graph determines every import outcome and downstream module
  identity.
- Parser syntax, filesystem I/O, semantic consumption, and diagnostic rendering
  have testable non-overlapping boundaries.
- Malformed calls cannot accidentally load files, and missing or ambiguous
  imports cannot be silently healed by a different phase's policy.
- Standard-library resolution, manifests, incremental invalidation, and deps
  output become explicit and deterministic.
- Legal cycles terminate discovery without being misclassified as validation
  errors.

### Negative

- Discovery becomes an iterative compiler/host protocol rather than a single CLI
  scan.
- The compiler needs explicit discovery-plan, observation, site-projection, and
  read-manifest value types.
- Embedders must preserve transactional revisions and execute compiler requests
  faithfully instead of supplying an arbitrary bag of files.

### Neutral

- This ADR does not change `@import` syntax, candidate precedence, directory
  facade semantics, visibility, or the one-root compilation model.
- It does not make physical paths durable semantic identity.
- It does not prohibit future virtual filesystems or package maps; those hosts
  must implement the same request/observation contract and provide explicit
  resolution inputs.

## References

- ADR-0026: Module System
- ADR-0047: Root-module compilation units and build-system inputs
- ADR-0050: Stable semantic dependency manifests
- RUE-806: Make `CanonicalImportGraph` the sole import-resolution authority
- RUE-807: CLI parser-owned discovery
- RUE-808: Sema consumes resolved records
- RUE-809: Canonical import diagnostics
- RUE-810: Dependency graph and read-manifest presentation
