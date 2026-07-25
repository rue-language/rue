# Request-scoped immutable declaration base

Status: implemented (RUE-1135 — RUE-1091 repair option 2, request-scoped
variant). This is a production boundary, unlike the RUE-1133 clone-from-template
probe it is measured against.

## What changed

`analyze_body_query` rebuilt the whole declaration epoch for every reached body:
`prepare_query_declaration_shells`, `project_durable_declaration_semantics`, the
durable install, and the stable endpoint installs. Those stages take only
revision-scoped inputs and produce the same epoch every time. The only reason
they repeated is that the epoch they produce — a `Sema`/`BoundSema` — is mutated
during body analysis.

A rooted attempt now builds one bound declaration epoch and derives each reached
body's epoch from it. The base lives in `SharedDeclarationBase`
(`crates/rue-compiler/src/canonical_semantic.rs`), created inside one rooted
attempt and dropped with it.

## Deriving is not copying

`BoundSema::derive_body_epoch` (`crates/rue-air/src/sema/declaration_base.rs`)
splits the epoch's state three ways. The split is the whole design; each group
is what it is for a checkable reason.

### Shared, never written after the base is built

The closed declaration namespace, the stable definition/module/body-owner/
const-candidate endpoint tables, the module registry, the RIR declaration index,
and the file/symbol path maps. Each sits behind an `Arc`, so a derivation bumps a
refcount instead of copying O(declarations) entries.

This is checked by the compiler, not asserted in prose:

* `SourceDeclarations` — the declaration-namespace phase body analysis runs in —
  has no `DerefMut`. `MutableDeclarations` is the only namespace state that
  does, and it is consumed at the phase transition. A body cannot reach the
  shared namespace mutably.
* Every other member of this group is `Arc<_>`, so any write is an
  `Arc::make_mut` that has to be written down. Today all of them are at install
  time, while the base is being built.

These are the two families RUE-1133 measured as the epoch's structural bulk
(roughly 50/50 between them), so this is where most of the copy went.

### Layered: true base plus append-only overlay

The canonical type pool (`TypeInternPool`) and the parameter arena
(`ParamArena`) are the two structures a body genuinely extends. Both gain an
immutable `base` plus a local layer numbered from `base_len` upwards:

* Pool indices below `base_len` are read straight out of the base. A canonical
  `Type` therefore means the same thing in the base and in every epoch layered
  on it, so nothing has to be remapped.
* Interning appends to the local layer. A body that interns a type copies
  nothing the base already holds.
* A parameter `ParamRange` never straddles the boundary, because each range is
  allocated in a single call into a single layer.

This is deliberately *not* copy-on-write of the inner store. Every body interns
types, so copy-on-write would copy the whole pool per body and recover nothing —
which is exactly the hazard RUE-1091 named for `TypeInternPool`.

Soundness needs one more piece. A phase that rewrites an already-interned
entry — destructor assignment, containment finalization — must not write through
to a base shared with sibling bodies. `TypeInternPoolInner::try_entry_mut`
promotes that one entry into a private `overrides` map first. Those maps are
empty on the body path, so reads check them only when non-empty and the hot
`entry(index)` path stays a single branch.

### Copied per body

The small per-epoch owned state: generated-struct and generated-enum overlays,
the anonymous-nominal identity maps, the dependency-observation vectors, and the
body-analysis work counters. These are sized by the epoch's anonymous universe
rather than by its declarations.

## What the counters say

`PerBodyDeclarationContextWork` gains four fields, each charged at the operation
it measures:

| counter | meaning |
| -- | -- |
| `declaration_bases_built` | bases derived from scratch; one per request |
| `body_epochs_derived` | bodies served from a base |
| `declaration_units_shared` | units a derivation read from the base |
| `body_local_units_copied` | units a derivation actually copied |

They exist because the base changes what the *existing* counters describe.
`cold_body_preparations`, `shells_prepared`, `semantics_installed`, and
`endpoints_installed` are all charged by stages that now run once per request, so
they fall by construction. The new counters account for that drop rather than
leaving it as an unexplained improvement: the work moved into sharing, and the
shared total is reported in full beside it.

`unstable::declaration_base_metrics` exposes the same accounting per session,
plus the parity results below. Unlike the RUE-1133 probe meter it is charged by
every compile, because the base *is* the production path.

The scaling gate (`crates/rue-compiler/src/scaling_harness.rs`) follows the same
change of shape RUE-1132 made for the projection: the prepare, install, and
endpoint rows are now TOTAL rows against one universe rather than per-body rows,
which is strictly stronger — a regression to per-body construction lands at
`universe · cold_bodies` and fails immediately instead of hiding inside an
integer-division quotient. A hard row pins one declaration prefix per request.

## Measured

Cold semantic phase, `rue-scaling-bench --mode timing`, release build, on a
shared 4-core container. Raw evidence, every sample retained:
[`../benchmarks/rue-1135-declaration-base.jsonl`](../benchmarks/rue-1135-declaration-base.jsonl).
Treat the ratios rather than the absolute milliseconds as the result — the host
is not quiet.

| bodies × decls | before (`ef75a7a`) | after (`9e790ff`) | speedup |
| -- | --: | --: | --: |
| 100 × 100 | 159.3 ms | 42.1 ms | 3.8× |
| 200 × 200 | 585.3 ms | 96.5 ms | 6.1× |
| 200 × 400 | 911.8 ms | 122.9 ms | 7.4× |
| 400 × 400 | 2 314.4 ms | 297.5 ms | 7.8× |

The speedup grows with the declaration universe, which is the shape to expect:
what the base removes is the `O(bodies × declarations)` epoch rebuild, so the
larger the universe each body used to re-derive, the more of it goes away. The
larger points land in the range RUE-1133 predicted for a shared base (~6.5×,
against a 5.75× measured clone arm).

This is a coefficient measurement, not a value-audit row. The value-audit
protocol in `benchmarks/value-audit/manifest.toml` needs three role binaries, a
historical baseline, and its paired median/MAD sampling policy on a quiet host;
it has not been run for this change.

## Why this is sound

Three properties carry the change.

**Publication parity.** `unstable::enable_shared_declaration_base_differential`
analyzes every reached body twice — once inside a freshly built, independently
derived epoch and once inside a base-derived epoch — and compares the published
`BodyTransaction`s. `transaction_equal` compares the canonical body, its
references, its produced anonymous nominals, and the rendered diagnostic stream
including order. The base-derived arm is the one that publishes, so the oracle
observes production rather than replacing it. It is off by default and roughly
doubles semantic work when on.

**Schedule-order independence.** This is the risk a shared base introduces and
the one RUE-1133's deep copy could not test: a deep copy is trivially
independent, so its 1 785-body parity result says nothing about a share. The
pre-cutover batch compiler shared one epoch *sequentially*, and a body could
observe what an earlier body materialized. Two tests attack it directly. The same
bodies analyzed through one base forward and in reverse must publish identical
transactions, and analyzing bodies must never grow the base's own type-pool or
parameter universe. Both run over a corpus that includes named structs and
methods, a `-> type` producer that materializes anonymous nominals into the
epoch, and a failing body — the families RUE-1133's `fn bN() -> i32 { N }` corpus
made invisible.

**Failure containment.** The base is created inside one rooted attempt and
dropped with it. It publishes no artifact, holds no lease, and is not reachable
outside the body bridge, so a failed or canceled request cannot leave one behind.
A body whose per-body epoch inputs — the materialized anonymous nominals and the
well-known `Option` registry, both installed *into* the epoch — differ from the
retained base's rebuilds rather than being served an epoch that was never built
for it; `base_input_misses` records when that happens.

## What this does not do

It does not make cold work flat. RUE-1133 measured that relative to a per-body
copy, sharing the base removes at most the copy itself, which was ~2% of today's
cold work; ~83% of what remains once derivation is gone is an
`O(bodies × declarations)` term that lives outside the declaration epoch
entirely. Cold work stays `O(bodies × declarations)` with a smaller coefficient.
Score this against the coefficient, not the shape.

It does not narrow invalidation. An unrelated declaration edit invalidates the
base and therefore every body. That costs nothing against current behaviour —
PR #1961 measured that under rooted-demand discovery a leaf edit already
recomputes every body — but it is not exact invalidation, and RUE-1091's
completion criterion and RUE-1093's gate stay open. Narrowing is RUE-1134 (B1)
and the provider path.

It does not remove the epoch's compact identities. The base still owns the type
pool and the stable endpoint tables that issue the `Type`, `StructId`,
`SemanticDefinitionToken`, and `SemanticModuleToken` values bodies consume, so
bodies of one request share an issuer rather than each minting their own through
`BodySemanticOverlay`. That is the identity trade-off RUE-1135 flags as
load-bearing for whether this step is on RUE-1092's path or lateral to it.
Removing it requires the provider-driven analyzer, which this step deliberately
does not build. What this step does supply toward it is the ownership discipline:
the base is a private, request-scoped data cache with no query authority and no
public surface, so RUE-1092 can delete it outright.
