---
id: 0098
title: Structured concurrency and thread transfer
status: accepted
tags: [language, concurrency, runtime, ownership, abi]
feature-flag: concurrency
created: 2026-09-20
accepted: 2026-09-20
implemented:
spec-sections: ["6.9"]
superseded-by:
relates: ["RUE-2272", "RUE-2274", "ADR-0037", "ADR-0084", "ADR-0088", "ADR-0096"]
---

# ADR-0098: Structured concurrency and thread transfer

## Status

Accepted under Steve's delegated design authority on 2026-09-20. In the
concurrency design discussion he approved implementation and publication and
explicitly delegated the remaining design choices. This ADR records those
choices. Implementation is tracked by RUE-2272; the language surface remains
behind `--preview concurrency` until its specified validation is complete.

The first public slice is scoped exclusive fork/join. Owned consuming tasks,
shared borrows, parallel algorithms, and communication extend that foundation
in separately reviewable slices. Scalable I/O receives a separate measured ADR;
this decision does not introduce algebraic effects or select a coroutine model.

## Summary

Concurrent work belongs to a structured operation. The operation cannot return
until every child has finished and its runtime resources have been reclaimed.
Rue checks access and transferability before admitting concurrent calls, keeps
callbacks second-class, and continues to terminate the entire process on a
panic. The initial runtime uses OS threads provided by pthreads on the three
supported targets. Thread-requiring executables use a hosted runtime built from
the same canonical source; other executables retain freestanding linking.

The first library operation is `std.parallel.join_inout`: two callbacks receive
exclusive access to two distinct contexts and finish before the call returns.
No handle or loan escapes. This is deliberately named for its access mode;
the later consuming `join` accepts owned inputs and returns owned results.

## Context

Rue already has affine and linear ownership, second-class `borrow`/`inout`
access, statically enforced root exclusivity, and named second-class callbacks.
ADR-0096's callbacks are implemented and stable. Capturing closures remain the
separate RUE-1885 workstream and are not a prerequisite for explicit contexts.

The allocator in `crates/rue-allocator/src/lib.rs` already serializes its
mutable state and requires a concurrently callable page mapper. Replacing the
allocator is not a concurrency prerequisite. The runtime provides these foundations before safe threads are public:

- Every process-exit path terminates all threads. Linux uses `exit_group`.
- Assertion locations and complete failure reports are caller-owned records,
  and one private parking gate serializes each ordinary terminal report across
  its complete frame, stderr, and process exit. Signal and explicit raw-exit
  paths bypass that gate and may truncate a best-effort final record.
- Signal disposition is process-wide. Each worker has a dedicated alternate
  signal stack and a registered stack window retained through its join.
- Process arguments and environment are published immutably during startup.
- Hosted standard-input contention parks while another thread performs a
  blocking read.

The compiler's own query workers are an implementation of the compiler, not a
Rue program runtime. Program concurrency neither imports that scheduler nor
adds a second compiler phase machine.

## Decision

### 1. Structured completion and failure

A concurrent operation retains every context, callback, and output location
until all children have completed. Successful return includes joining the
children and reclaiming worker resources. It does not promise termination:
user code may loop forever, block on I/O, or deadlock.

There is no implicit detach, asynchronous thread kill, or catch-unwind. A panic
in either a parent or child terminates the process with the existing runtime
contract, without language cleanup (ADR-0088). Process exit and ordinary worker
completion are distinct runtime operations.

Recoverable errors remain values. A failed thread launch must not lose an
affine owner or discharge a linear obligation by dropping it. The scoped API
leaves ownership with its caller. The later consuming API must return unstarted
inputs in its failure outcome. It must never disguise a partially executed
operation as one whose inputs are unchanged.

### 2. The first safe operation

The library surface is conceptually:

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

`Result` denotes the ordinary standard-library type. `SpawnError` reports a
launch/resource error in a stable library representation; OS-specific numeric
codes are diagnostic details, not a portable enumeration promised by the API.

Both `A` and `B` must be transferable. The callbacks obey ADR-0096: named
monomorphic functions, exact signatures, ordinary forwarding, no stored or
escaping function values. The callback result is unit. The existing prohibition
on moving out of an `inout` parameter still applies. A context may contain a
linear value, but the operation does not consume that obligation.

On success each callback is invoked exactly once. One runs on a child OS
thread and the other on the caller. Their relative execution order is not
specified. Both mutations are visible when the operation returns.

On a recoverable launch or worker-initialization failure, neither callback is
invoked and both contexts are unchanged. The runtime enforces this with an
initialization handshake, not an assumption that `pthread_create` waits for the
child. The state sequence is:

1. Create a child with a private, stable context owned by the operation.
2. The child initializes its signal stack and runtime bookkeeping without
   running user code, then publishes ready or failed.
3. Failure: join the child, reclaim resources, and return the launch error.
4. Ready: allow user callbacks to run, execute the caller callback, then join.
5. Publish successful completion only after cleanup.

Ready/Failed is published with release semantics and observed with acquire
semantics. The parent releases a distinct Start state after observing Ready;
the child acquires Start before invoking user code. Waiting parks rather than
spinning while another thread initializes or executes a callback.

Failure before `pthread_create` succeeds also returns with unchanged contexts.
Failure of `pthread_join`, or any invariant failure after user code may have
started, aborts the process unless completion is independently proven. It must
never end loans while a worker might still access their places.

The operation has genuine concurrent progress; there is no silent serial
fallback when thread resources are exhausted. Parallel algorithms may later
define a weaker, serially executable scheduling contract under different APIs.

### 3. Access and transfer are independent

Ordinary call checking accounts for the two exclusive contexts across the
entire operation. The same root cannot be passed twice, including through two
field or element projections. The initial implementation introduces no dynamic
exclusivity checks and no special disjoint-slice exception.

Transferability answers whether an exclusively held value may be accessed,
mutated, and destroyed on another thread. It is independent of Copy, affine,
and linear multiplicity. Both contexts are checked even though one callback
currently runs on the caller: the public contract must not depend on which
operand the implementation chooses to move to a worker.

The canonical type-fact computation uses these rules:

- Primitive numeric values, booleans, and unit are transferable.
- Arrays require a transferable element type. Enums require transferable
  payload types in every variant. Structs require transferable fields.
- An ordinary raw pointer is not transferable. A callback is not a storable
  transferable value; passing the operation's second-class callback parameters
  is the narrowly bounded runtime bridge described below.
- A struct with its own destructor is not automatically transferable. An
  ordinary generated field-drop sequence does not independently disqualify an
  otherwise transferable aggregate.
- Canonical immutable builtin string representations receive their audited
  transfer property by identity, never by a user-spellable type name.

`@thread_bound` on a struct prohibits transfer and propagates through containing
fields, arrays, enum payloads, and pointer-pointee checks. This is necessary
even for a pointer-free type: an integer may identify a resource valid only on
its creating thread. The marker is not a runtime restriction on ordinary local
use.

`@unchecked_transfer("reason")` on a struct is an unchecked assertion that its
own resource and destructor contract tolerates transfer. The nonempty reason
documents the safety argument. In particular, the assertion covers ownership,
aliasing, retained addresses, thread affinity, and destruction; it is not merely
a promise that a pointer can be freed on another thread.

The assertion does not exempt ordinary fields from recursive checking. For a
direct raw-pointer field it asserts the necessary ownership/aliasing invariant
and requires that pointer's pointee type to be transferable. It cannot override
a nested `@thread_bound` type. Conflicting or duplicate markers are rejected.
Recursive pointer ownership is checked with a cycle-aware canonical query; an
explicit negative fact is not hidden by visiting a cycle.

Both annotations have the same rules on named structs and anonymous structs
returned by type factories. A transferable pointee alone never proves pointer
ownership: that obligation belongs to the unchecked assertion.

This makes an audited `RawBuf(T)` conditional on `T` at the transfer boundary.
It does not put an unconditional `@require_transferable(T)` in the type factory,
which would wrongly forbid constructing a local `ArrayBuf` of thread-bound
elements. `RawBuf`, `ArrayBuf`, and `StrBuf` each require their own audit where
their own destructor or pointer fields need the assertion. No ownership type
receives a special exemption by spelling.

Diagnostics name the argument and the field/pointee path that prevents
transfer. Attributes and their semantic effects participate in canonical type
metadata, identity, and incremental invalidation. Consumers do not maintain
separate transferability walkers.

Shareability is a separate property for future concurrent `borrow` access.
Transferability alone never authorizes shared access or interior mutation.

### 4. Memory and observable behavior

Initialization before starting a child happens-before its user callback. A
child's writes and completion happen-before the enclosing operation returns.
The runtime's synchronization must provide those edges on AArch64 as well as
x86-64. Optimization and runtime-call effects must preserve them.

Safe code cannot create overlapping unsynchronized mutable accesses across
tasks. The unchecked transfer assertion and existing unchecked pointer/foreign
operations carry the obligations the compiler cannot prove.

A data race is conflicting access from different threads to overlapping memory,
with at least one write, where the accesses are not ordered by synchronization
and are not both accesses through an appropriately defined atomic operation.
Creating one by violating an unchecked assertion or pointer/foreign contract is
unchecked undefined behavior. A race expressible through the safe API is a
compiler/runtime defect, not permission for that API to have undefined behavior.

There is no promised ordering between concurrent side effects. Structured
completion is not deterministic scheduling. Future `parallel_map` returns
results in input order; reductions specify their grouping rather than allowing
the scheduler to select floating-point association. Channels and synchronization
will explicitly expose scheduling-dependent behavior.

Public atomics and user-selectable memory orders are not required for this
slice. Internal runtime atomics implement the start/join contract.

### 5. Runtime and linking

Use pthreads on x86-64 Linux, AArch64 Linux, and AArch64 macOS. The runtime stays
`no_std`; hosted OS-thread support does not require the Rust standard library.
The same source implements common allocation, diagnostics, process inventory,
and thread lifecycle. A hosted archive supplies the OS-thread imports and
startup integration without making ordinary freestanding images depend on
libc.

Linux's hosted entry routes through `__libc_start_main` before Rue user code.
It preserves the original entry-stack observation used by the existing process
capture and fault setup. macOS retains the existing dyld/`LC_MAIN` startup and
libSystem initialization. The system C linker driver resolves pthread/libc
imports; the internal linker does not pretend to support dynamic bindings it
cannot emit.

Thread-requiring runtime helpers carry a typed runtime requirement. Reachable
codegen units feed that requirement into the canonical `ProgramImagePlan`,
which selects the hosted archive and system-link route. The requirement and
archive identity participate in plan invalidation and output fingerprints.
An explicit incompatible linker selection receives a diagnostic. Assembly and
object emission remain available without executing a host linker. Cross-linking
uses an explicitly suitable toolchain or reports its absence.

Automatic linker choice is represented separately from an explicit internal
linker request. The compiler daemon's internal-link-only admission contract must
also be preserved: a thread requirement discovered at the image-plan boundary
causes a typed fallback to a permitted client-side link path, or a diagnostic,
instead of silently launching a system linker inside the daemon.

Do not use the preview flag as a permanent runtime selector, scan source text
for thread calls, or add a parallel phase machine. A program with no reached
thread requirement keeps its existing runtime/link path. Archive validation
accounts for each variant's declared exports; unresolved pthread imports must
not leak into nonthreaded images through coarse archive extraction.

The runtime helper accepts the operation's native callback code pointer and
the address of its exclusive context. A Rue `fn(inout T) -> ()` has exactly one
pointer argument and a unit result under ADR-0084. This narrowly specified
internal bridge is audited on both backends and wrapped by a normal pthread
C-ABI entry trampoline. It does not expose a general Rue-to-C callback
conversion or permit callbacks to escape their enclosing operation.

### 6. Runtime safety prerequisites

Before the public operation is reachable:

- Every process-exit path uses process-wide termination, including traps,
  normal main completion, and explicit exit. Returning from a pthread entry
  is ordinary worker completion.
- Assertion source-location records are caller-owned or otherwise isolated
  per thread; a pointer and length must never come from different failures.
  Structured failure frames remain parseable under concurrent failures.
- Each worker has its own alternate signal stack and accurate stack bounds.
  Signal handlers use only async-signal-safe operations and stable metadata.
  Worker metadata and stacks are reclaimed only after it cannot execute.
- Immutable startup inventory is published before children access it.
- Blocking standard-input contention parks rather than occupying a CPU in a
  spin loop for the duration of another thread's read.
- Allocation/free across threads obeys the existing allocator contract.

### 7. Follow-on surfaces

The consuming `join` takes `fn(A) -> R` tasks and returns both owned results.
Its adapters use canonical callable lowering and must handle all existing
native argument/result shapes, including hidden aggregate results. They do
not hand-roll a concurrency-specific calling convention.

Shared borrows, explicit-capture closures, mutable partitioning, bounded
parallel algorithms, channels, scoped mutex access, and cooperative cancellation
extend the same lifetime discipline. They do not arrive accidentally through
an unrestricted spawn primitive. Their detailed acceptance work is tracked
separately; the initial operation has neither cancellation nor escaping handles.

Scalable I/O is decided by a separate comparison of explicit execution/I/O
contexts, stackful tasks, and compiler-generated state machines. Monomorphization
and second-class loans are not a proof of movable suspension frames. That
decision must establish address stability, cancellation cleanup, FFI interaction,
and memory costs before claiming no-Pin or no-allocation async.

## Implementation Phases

- [x] **Process-wide Linux termination** — RUE-2273.
- [x] **This decision and adversarial design review** — RUE-2274.
- [x] **Pthread lifecycle, runtime safety, and hosted integration** — RUE-2275.
- [x] **Canonical transferability and explicit thread affinity** — RUE-2276.
- [ ] **Scoped `join_inout`, native validation, and first workload** — RUE-2277.
- [ ] **Consuming owned fork/join adapters** — RUE-2278.
- [ ] **Scoped sharing and bounded parallel algorithms** — RUE-2279.
- [ ] **Bounded communication and cooperative cancellation** — RUE-2280.
- [ ] **Scalable I/O evaluation and separate decision** — RUE-2281.

## Validation and Preview Exit

The first public slice is complete only when all three native targets pass:

- genuine simultaneous progress and joined visibility;
- generic/forwarded callbacks and zero-sized/multi-slot contexts;
- same-root, signature, preview, and transfer rejection cases;
- nested joins and resource exhaustion without silent serial fallback;
- injected pre-start failure with neither callback invoked and both contexts
  unchanged;
- parent and child traps terminating the whole process;
- child stack overflow and ordinary invalid-pointer fault classification;
- concurrent assertion reporting without mixed records;
- cross-thread heap allocation/free and repeated joins without resource leaks;
- no change to freestanding linking for programs without a thread requirement.

Spec, grammar, diagnostic/UI, CLI, durable-query, runtime archive, and oracle
coverage follow the normal repository contracts. The oracle may sequentially
interpret order-independent structured operations; programs observing actual
parallel execution receive an explicit model-gap classification rather than
an unexplained harness failure.

A maintained CPU workload must demonstrate useful parallel execution and stable
assembled output. Report elapsed time and memory/thread costs; no speedup is
promised for tiny tasks. Scheduler or allocator redesign follows measurements.

## Consequences

### Positive

- Completion and ownership are visible at one operation boundary.
- Named callbacks and explicit contexts provide useful concurrency before
  escaping closures, lifetimes, coroutine frames, or a general scheduler.
- Conditional transfer checking admits audited owners without preventing their
  ordinary single-threaded use.
- Pthreads provide established platform lifecycle and TLS behavior.
- Hosted requirements flow through the existing compiler graph and image plan.

### Negative

- Threaded programs require platform threading libraries and a suitable system
  linker toolchain. Their startup/link dependencies differ from freestanding
  programs, and both archive variants need validation.
- One OS thread per fork has stack and launch costs; fine-grained parallelism
  will need measured scheduling work.
- `join_inout` cannot consume its contexts into unrelated result types.
- Structured completion can wait forever for blocked or nonterminating work.
- Incorrect unchecked transfer assertions can break memory safety.

## References

- [ADR-0037: Access-point-based exclusivity](0037-exclusivity-model-access-point-based.md)
- [ADR-0084: Native calling convention](0084-native-calling-convention.md)
- [ADR-0088: Panic termination](0088-panic-termination.md)
- [ADR-0096: Second-class function parameters](0096-second-class-function-parameters.md)
- [Rust scoped threads](https://doc.rust-lang.org/std/thread/fn.scope.html)
- [POSIX pthread creation](https://pubs.opengroup.org/onlinepubs/9799919799/functions/pthread_create.html)
- [POSIX pthread join](https://pubs.opengroup.org/onlinepubs/9799919799/functions/pthread_join.html)
