# Compiler Facade Changes

`rue-compiler` exposes one session-centered public facade. Its supported root
contains owned requests, `CompilerSession`, the one-shot `compile_snapshot`
adapter, and immutable artifact views. Query records, durable payloads,
interners, parser and raw IR owners, backend state, and alternate phase entry
points are implementation details.

The reviewed inventory lives in
`crates/rue-compiler/src/supported_api_inventory.rs`. Every root export and
every public `CompilerSession` or `CompilerSessionUpdate` signature has one
line with this form:

```text
stability|owner|class|approved-consumer|symbol|canonical-signature
```

The compiler unit tests mechanically extract the current surface and compare
it with that file. They also reject root glob exports and aliases, public
additions without a classification, forbidden implementation categories, and
additional one-shot compiler entry points. An intentional API change therefore
produces a small inventory diff that reviewers can assess semantically.

## Requesting a tooling view

Start from the structured fact the consumer needs and find the canonical
session artifact that already owns it. Extend an existing immutable view, or
add a new owner-retaining view, when the fact is a durable compiler concept for
embedders or machine tooling. Views must use checked references, opaque stable
identities, and borrowed iterators; they must not reveal owner-indexed storage
or permit compiler state to be installed or mutated.

Presentation text, metrics, tracing adapters, benchmark data, differential
oracle hooks, and driver-specific operations belong under
`rue_compiler::unstable`. An unstable adapter still consumes canonical session
artifacts and must not create a peer parser, semantic path, or backend path.
Specialized in-tree phase tools may depend directly on the crate that owns a
raw phase type instead of making `rue-compiler` an umbrella crate.

Import discovery follows the same split: the stable `ImportDiscoveryView`
reports closure status, source revision, and structured diagnostics, while
debug renderings of accepted reads, graph inputs, ledgers, and source-assembly
policy live under `rue_compiler::unstable`.

For a stable addition, document why an existing view cannot answer the request,
name the owning module and approved consumers, and update the classification in
`api_inventory.rs` together with the exact reviewed inventory line. Signature
changes and removals receive the same review as root export changes. Do not
weaken the scanner or add a compatibility wrapper that computes the artifact a
second way.

Run the focused guard while iterating:

```bash
scripts/rue unit compiler api_inventory::semantic_api_inventory_matches_every_root_export_and_session_signature --exact --nocapture
```

Before publication, run `scripts/rue fmt`, the full compiler API-inventory unit
group, the affected consumer tests, and `scripts/rue quick`. Cross-cutting
facade changes also require the repository's full serialized test suite.

The compatibility and ownership rationale is recorded in
[ADR-0061](../designs/0061-supported-compiler-facade.md).
