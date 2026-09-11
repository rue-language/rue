---
id: 0088
title: Panic termination without unwinding or recovery
status: accepted
tags: [semantics, runtime, ownership, principle]
feature-flag: null
created: 2026-09-10
accepted: 2026-09-10
implemented:
spec-sections: ["8.0:2"]
superseded-by:
relates: ["RUE-1902"]
---

# ADR-0088: Panic termination without unwinding or recovery

## Status

Accepted on 2026-09-10 by Steve.

## Summary

Rue panics are process-terminating control flow. A panic abandons the current
evaluation immediately, runs no language-level cleanup, and cannot be observed,
caught, unwound, recovered, or resumed by Rue code.

## Context

Rue already defines one termination discipline for `@panic`, failed
assertions, arithmetic traps, bounds violations, and division or remainder by
zero: the runtime reports the failure and exits with status 101. Destructors
and `?` introduce two distinct concerns that must remain explicit. A panic
must not turn into an implicit cleanup path, and recoverable failures need a
typed representation rather than an implicit handler mechanism.

## Decision

Every Rue panic terminates the process immediately. The panicking callee, its
callers, and surrounding scopes do not run destructors or other language-level
cleanup, and no user code after the trap executes. Rue has no handler, catch,
unwind, recovery, or resume construct for a panic; no language expression can
consume a panic as a value.

This principle covers explicit `@panic`, failed `@assert`, `@assert_eq`, and
`@assert_ne`, integer overflow, array bounds violations, division by zero,
remainder by zero, allocation failure, and other runtime traps defined by the
specification. The runtime's status-101 and diagnostic details remain owned by
the applicable runtime-behavior rules.

Recoverable errors use typed values: `Result`, `Option`, or an application
enum, propagated with `?` as specified by ADR-0038. A host that must continue
after a panic isolates the work in a subprocess, following ADR-0083's test
execution model.

This ADR does not decide effect tracking (RUE-225) or broken-pipe policy
(RUE-510); those concerns are separate from panic termination.

## Consequences

### Positive

- Panic behavior is deterministic and cannot accidentally run user cleanup.
- The language has no hidden in-process recovery surface to constrain codegen.
- Recoverable failures remain visible in types and ordinary control flow.

### Negative

- A Rue process cannot recover from a panic in place.
- Hosts that need fault isolation pay the cost of a subprocess boundary.

## References

- [ADR-0036: Behavior classification preference](0036-behavior-classification-preference.md)
- [ADR-0038: Error handling: sum types, Result/Option, and must-check via linearity](0038-error-handling-sum-types-result-must-check.md)
- [ADR-0083: `rue test` MVP](0083-rue-test-mvp.md)
- [Runtime Behavior, §8](../spec/src/08-runtime-behavior/_index.md)
