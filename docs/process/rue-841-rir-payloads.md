# RUE-841 typed RIR payload publication record

This record accompanies the RIR portion of
[ADR-0056](../designs/0056-typed-ir-payload-schemas.md). It records the
structural and focused allocation evidence owned by RUE-841; the paired
whole-compiler benchmark matrix remains the integration gate owned by RUE-843.

## Published representation

- Every variable RIR payload is represented by a family-specific opaque range.
- Each stored range has compile-time size and alignment assertions: exactly two
  `u32` words, aligned as `u32`; marker families occupy no storage.
- Range values are neither `Copy` nor `Clone`. Semantic metadata retains the
  owning declaration `InstRef` and borrows its parameter range on demand.
- `RirEditor` is the mutable construction form. `ValidatedRir::finish` consumes
  the real owner (not an alias), runs structural plus canonical interner/source
  validation, and is the type stored by
  `CanonicalRirOutput` and therefore published by `CompilerSession`.
- Every payload-bearing instruction is created by one transactional editor
  operation that rolls back both stores on failure. The editor exposes no
  mutable dereference and AstGen never constructs a range separately from its
  owning instruction.
- Empty ranges use only `(0, 0)`. Fixed-width payloads retain their prior word
  width. Nonempty match-arm and directive envelopes add exactly one count word.

## Construction and validation evidence

Builders stage complete payloads before reserving and appending to the owner.
Every length conversion, multiplication, addition, and reservation is checked.
Failures are categorized as `ResourceLimitExceeded`, `CapacityFailure`, or
`InvalidBuilderInput`; AstGen retains the first failure and `try_finish()`
returns it without publishing the partially lowered editor.

The publication validator checks canonical empty ranges, checked range ends,
fixed record widths, variable record counts and trailing words, argument and
parameter modes, comptime booleans, match scalar tags, enum payload cardinality,
schema-local symbol representability before any infallible view, all canonical
symbol and instruction handles, and every source span. Fixed families and
variable match/directive/enum families share their checked schema descriptors
across sizing, validation, and iteration. Errors include
the family, physical range, record index where applicable, and stable reason.

## Focused allocation and correctness evidence

`inst::typed_payload_tests` covers every migrated family, canonical empty
payloads, deterministic malformed scalar/context data, and noncanonical empty
ranges. A counting allocator traverses every migrated borrowing view and records
exactly zero heap allocations. A separate all-family builder probe records
allocation calls and bytes alongside logical and retained-capacity bytes for
each of the 17 families independently, and asserts the storage relationships
without timing-sensitive constants. It deliberately does not report a staging
peak: the system allocator reports requested allocation bytes, but cannot
separate simultaneously live staging capacity from word-store growth without
builder instrumentation that would perturb the measured path. RUE-843 owns
that narrowly deferred peak-live-staging measurement as part of its
instrumented paired whole-compiler wall-time/RSS matrix.

Publication checks run from the repository root:

```text
scripts/rue fmt
./buck2 test //crates/rue-rir:rue-rir-test //crates/rue-air:rue-air-test
./buck2 build //crates/rue:rue
scripts/rue quick
```

On the implementation worktree these checks passed with 39 RIR tests, 440 AIR
tests, the full compiler build, and the repository quick suite. The focused
allocation assertion is exact rather than timing-sensitive. No claimed
whole-compiler wall-time or RSS measurement is invented here; RUE-843 performs
the alternating baseline/candidate series required by ADR-0056 after RIR, AIR,
and CFG migrations are integrated together.
