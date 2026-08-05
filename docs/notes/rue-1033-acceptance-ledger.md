# RUE-1033 Phase 12 acceptance ledger

This note records the final structural and latency witnesses for ADR-0063's
fresh-link implementation. The executable gates remain tests; this ledger fixes
the measurement context so future runs can be compared without turning one
host's timing into a compiler guarantee.

## Warm single-function edit baseline

Measured 2026-08-04 from the RUE-1033 working tree based on `cb9812cd`, using an
optimized compiler test binary:

```text
./buck2 run --target-platforms //platforms:release \
  //crates/rue-compiler:rue-compiler-test -- \
  rue_1033_warm_single_function_latency_witness --ignored --nocapture
```

| Context | Value |
| --- | --- |
| Host | MacBook Pro `Mac17,2`; Apple M5; 10 logical CPUs |
| OS | macOS 26.5.2 (25F84), Darwin 25.5.0 ARM64 |
| Compiler target | `aarch64-macos` |
| Query workers | 4 |
| Sampling | 11 independent two-revision sessions; median is the sixth sorted sample |
| Edit-to-CodegenUnit | 300 µs median; samples 272, 279, 292, 296, 298, 300, 303, 306, 306, 318, 359 µs |
| Edit-to-runnable fresh link | 1,618 µs median; samples 1,573, 1,593, 1,598, 1,599, 1,610, 1,618, 1,622, 1,629, 1,677, 1,694, 1,741 µs |

Each sample prepares the same cold baseline before the clock begins. The timed
interval starts immediately before publishing the edited snapshot. The first
endpoint is completion of rooted CodegenUnit collection; the second is
completion of the deliberately fresh internal link. Source-string construction,
executing the resulting program, and macOS test signing are outside the timed
interval. Every linked result is executed after measurement.

Every sample asserts this exact warm work:

- one lexer invocation, one parser invocation, and one reparsed module;
- one computed, one reused, and one invalidated body analysis;
- one computed CFG;
- one computed replacement CodegenUnit and one reused CodegenUnit; and
- one intentionally fresh link.

The ignored witness has no latency threshold. Structural assertions run every
time it is invoked; release engineering may record new host-specific rows when
tracking performance changes.

## Executable schedule and locality gates

The ordinary compiler unit suite also proves the Phase 12 schedules through
final executable bytes:

- `joined_codegen_schedule_matches_fresh_linked_executable` forces an exact-key
  CodegenUnit join inside the registered production evaluator, then verifies
  that the ordinary image/fresh-link adapter matches a fresh session.
- `canceled_codegen_waiter_schedule_matches_fresh_linked_executable` cancels
  only a joined waiter while its owner remains live, then verifies the owner's
  terminal through the same fresh-link comparison.
- `query_native_rooted_demand_warm_edit_locality_through_fresh_link` publishes
  two revisions through the rooted import-input protocol. Editing the reached
  imported function computes one body, CFG, and replacement CodegenUnit;
  editing an unreachable imported function computes none. Both cases compare
  fresh-linked executable bytes with a fresh session.

The wider Phase 12 acceptance suite retains the remaining schedule and edit
coverage. The differential oracle compares cold, reused, recovered-failure, and
bounded-eviction histories with stepwise fresh sessions. Position-free trivia
reuse is covered by the revisioned-query tests; reachability edge deletion by
the body-closure and scaling-harness gates; and one-worker/many-worker linked
executable parity by
`one_and_many_query_workers_produce_identical_linked_executables`. The query
runtime suite owns the underlying adversarial claim/join/cancellation and
deterministic retention-budget schedules.

## Remaining linker delta

Phase 12 ends at a `ProgramImagePlan` consumed by a deterministic fresh internal
link. Stateful incremental linking remains a separate design: placement and
slack policy, stable-address growth, reverse-relocation patching, runtime
archive changes, compaction/fallback, atomic publication, and target-specific
signing all require the follow-up incremental-linker ADR described by ADR-0063.
