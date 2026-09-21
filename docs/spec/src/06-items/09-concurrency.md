+++
title = "Structured concurrency"
weight = 9
template = "spec/page.html"
+++

# Structured concurrency

The first structured-concurrency operation is the standard-library function
`std.parallel.join_inout`. It is a preview feature and uses the ordinary call,
loan, callback, and transferability rules.

{{ rule(id="6.9:1", cat="normative") }}

Programs that use `std.parallel.join_inout` **MUST** be compiled with
`--preview concurrency`. The operation is unavailable without that flag.

{{ rule(id="6.9:2", cat="syntax") }}

Its public signature is:

```rue
pub fn join_inout(
    comptime A: type,
    comptime B: type,
    inout left: A,
    inout right: B,
    left_fn: fn(inout A),
    right_fn: fn(inout B),
) -> Result((), SpawnError)
```

{{ rule(id="6.9:3", cat="legality-rule") }}

`A` and `B` **MUST** be transferable according to the canonical
`@require_transferable` type fact. The two context arguments **MUST** be
explicit `inout` places and **MUST** have distinct roots under the ordinary
exclusivity rules. A transferability assertion does not permit aliasing and
does not change local ownership or drop behavior.

{{ rule(id="6.9:4", cat="legality-rule") }}

Each callback **MUST** be a named, monomorphic second-class function with the
exact corresponding `fn(inout A)` or `fn(inout B)` signature. The callbacks
cannot be stored, captured, or returned. Their bodies and transitive runtime
requirements remain reachable for the operation.

{{ rule(id="6.9:5", cat="dynamic-semantics") }}

On success, each callback runs exactly once, both callbacks finish before the
function returns, and mutations are visible through `left` and `right`. Their
relative execution order is unspecified. A recoverable launch or worker
initialization failure returns `Err(SpawnError.ResourceExhausted)` or
`Err(SpawnError.InitializationFailed)` without invoking either callback and
without changing either context. An unexpected runtime status is a process
failure, not a recoverable `Result` value.

{{ rule(id="6.9:6", cat="legality-rule") }}

An intrinsic argument may carry the same `inout` or `borrow` mode prefix as an
ordinary call argument. Intrinsics that do not explicitly accept a mode **MUST**
reject non-`Normal` arguments; `std.parallel.join_inout` is the bounded
exception for its two context arguments.
