---
id: 0046
title: "Delete flat multi-file mode (all cross-file references go through @import)"
status: accepted
tags: [modules, semantics, cli, language-shape, ergonomics]
created: 2026-07-03
accepted: 2026-07-03
implemented:
spec-sections: ["10.3 (visibility)", "10.5 (program composition)"]
supersedes:
relates: ["ADR-0023", "ADR-0026", "RUE-180", "RUE-181", "RUE-116"]
---

# ADR-0046: Delete Flat Multi-File Mode

## Status

Accepted — Steve, 2026-07-02 ("i want to delete flat mode entirely"), tracked by
RUE-181. This ADR designs **how** the deletion happens; it does not implement it.
Implementation is broken into tracked tasks after this ADR lands.

## Summary

Today `rue a.rue b.rue -o prog` loads every listed file into **one flat global
namespace**, so a function in `a.rue` can call a `pub` function in `b.rue` by its
bare name with no `@import` (spec 10.5:2, marked *transitional*). This ADR deletes
that behavior. After the deletion, **the command-line file list only seeds the
compilation unit / module graph — it never creates a shared namespace.** Every
cross-file reference must be reached through an `@import` module binding (ADR-0026),
exactly as if the file had not been listed on the command line. A bare name that
today resolves to another file becomes an "unresolved name" diagnostic. The change
ships as a **warn-then-error** deprecation so existing multi-file programs get one
release of migration lead time.

## Context

### Where flat mode came from

ADR-0023 introduced multi-file compilation with a flat global namespace as an
explicit stepping stone ("The UX is admittedly awkward … this is a stepping stone,
not the final design"). ADR-0026 then introduced the real module system
(`@import`, file-as-struct, directory modules, `pub`/private) and **superseded**
the flat namespace on paper — but the flat namespace was never removed. Both
mechanisms are live simultaneously today:

- `@import("b")` returns a module value; `b.foo()` is a qualified reference. This
  is the ADR-0026 model and the intended future.
- Bare `foo()` resolving to a `pub fn foo` in another listed file is flat mode.
  Spec 10.3:8 and 10.5:2 keep this alive as "transitional."

### What RUE-180 already did

The privacy-everywhere rule (RUE-180, merged 2026-06-11, spec 10.3:7) made
visibility *uniform*: an item is usable outside its directory iff it is `pub`,
whether its file was `@import`ed or merely listed (E0460 for an unqualified
private reference; E0706 through a module). That closed flat mode's one genuine
soundness anomaly — a shared namespace *without* a visibility gate.

The consequence for this ADR is important: **flat mode's only remaining special
case is "name resolution without an import."** It no longer has a privacy
dimension. Deleting it is therefore a *resolution-only* change — we remove one
name-lookup fallback (the cross-file scan of listed files) and keep every
visibility rule exactly as RUE-180 left it. Nothing about `pub`, E0460, or E0706
changes semantically; those rules simply stop having an unqualified-reference case
to apply to, because unqualified cross-file references cease to exist.

### Why delete it at all

1. **Two ways to do one thing.** A `pub fn` in a sibling file is reachable two
   ways (bare and `mod.foo`), which is exactly the redundancy ADR-0026 set out to
   remove. New users can't tell which is idiomatic.
2. **The flat namespace is program-global.** Names collide across the entire
   compilation (spec 10.5:1, E0436), even between files that never reference each
   other. That is a scaling dead-end and directly contradicts the per-module
   scoping ADR-0026 promises. Flat mode is the reason 10.5:1 has to exist as a
   collision rule at all.
3. **The driver secretly changes semantics.** Whether `foo()` resolves depends on
   whether some *other* file happened to be on the command line — spooky action at
   a distance that `@import` makes explicit and local.
4. **It blocks the module-scoping revision.** 10.5:2 explicitly says the flat
   namespace is expected to be replaced by per-module scoping. That revision can't
   land while bare cross-file resolution is a supported guarantee.

## Decision

### The rule

> The list of source files on the `rue` command line **seeds the compilation
> unit** (the set of files the driver will parse and make available to
> `@import`). It does **not** introduce any name into any file's scope. A name
> that is not declared in the current file, imported via `@import`, or provided
> by the prelude is unresolved.

Concretely, after deletion:

```rue
// a.rue
pub fn helper() -> i32 { 42 }

// main.rue
fn main() -> i32 {
    helper()                       // ERROR (post-deletion): unresolved name `helper`
    let a = @import("a");          // the one supported path:
    a.helper()                     // qualified reference through a module binding
}
```

`rue main.rue a.rue -o prog` and `rue main.rue -o prog` become **semantically
identical** for `main.rue`: listing `a.rue` no longer changes what names
`main.rue` can see. Listing files remains meaningful only as a way to hand the
driver a file set (and, transitionally, for whole-program checking — see below).

### What stays exactly as-is

- `@import` resolution, directory modules, facade files, `pub const` re-exports
  (ADR-0026) — untouched.
- The visibility rules of spec 10.3 (RUE-180) — untouched. E0460/E0706 keep their
  meaning; they simply no longer fire on cross-file *unqualified* references,
  because those are now unresolved-name errors, not privacy errors.
- Duplicate-definition detection **within a directory / module** (E0436) — a
  directory is still one module (ADR-0026), and two files in the same directory
  defining the same top-level name still collide. What goes away is
  *program-global* collision across unrelated directories, which was purely an
  artifact of the flat namespace.
- Single-file compilation (`rue main.rue -o prog`) — unaffected; it has no
  cross-file references.

### Migration sequence (warn → error)

The compiler can and should warn before it errors. There is already a
`WarningKind` / `CompileWarning` channel in `rue-error` (`UnusedVariable`,
`UnreachableCode`, …); this adds one variant.

- **Phase A — Warn (one release).** Bare cross-file resolution still *works*, but
  every use emits a deprecation warning: *"`helper` is resolved through the
  transitional flat namespace and will require an explicit `@import` in a future
  release; add `const a = @import(\"a\"); … a.helper()`."* The warning is emitted
  at the *use* site (where the bare name resolves to another file), carries a
  machine-applicable suggestion where the fix is unambiguous, and is on by default
  (not gated behind a flag) so it is visible to everyone still relying on the
  mode. Programs that already use `@import` for all cross-file references see no
  warning — the warning is a precise detector of flat-mode reliance.
- **Phase B — Error.** The cross-file fallback in name resolution is deleted. A
  bare name that used to resolve to another file now produces a normal
  unresolved-name diagnostic. A **new diagnostic code** is allocated at
  implementation time (E0461 is already taken by the type-definition graph; the
  implementer picks the next free code in the module/name-resolution band and,
  ideally, points the message at the `@import` fix). The transitional paragraphs
  10.3:8 and 10.5:2 are struck from the spec, and 10.5:1's *program-global*
  collision rule narrows to *intra-module* collision.

Phase A is optional-but-recommended: because RUE-180 already made the behavior
uniform, we *could* delete flat mode in a single step. The warn phase is cheap
(one `WarningKind` + one resolver hook) and buys goodwill and a clean migration of
our own test corpus (see blast radius), so this ADR recommends shipping it.

### Ergonomics: the boilerplate gap and how we answer it

Deleting flat mode means every file that references a sibling pays `@import`
boilerplate. The honest worry: does this make small multi-file programs annoying?
Three candidate mitigations, and this ADR's recommendation:

1. **Prelude.** A small implicit prelude (ADR-0042 / RUE-315 territory) covers
   *std* names, not *user* cross-file names, so it does **not** address this gap.
   Out of scope here; it does not block deletion.
2. **Project file (`rue.toml`) / directory auto-import.** A build-manifest or a
   rule like "every file in a directory is implicitly in scope for its siblings"
   would remove the boilerplate — but directory-auto-import is *precisely flat
   mode scoped to a directory*, and re-introduces the "resolution depends on what
   else is in the folder" spookiness we are deleting. If we ever want it, it
   should be a **separate, deliberate ADR** with its own opt-in, not a silent
   consolation prize bundled into this deletion.
3. **Explicit `@import` only.** Accept the one-line-per-dependency cost.

**Recommendation: ship the deletion with explicit `@import` only; do not block it
on a prelude or a project file.** Rationale: (a) the boilerplate is one
`const x = @import("x");` line per *distinct* sibling a file actually uses, which
is proportionate and local; (b) it is exactly the Zig model ADR-0026 already chose,
so it is not a new tax, just the removal of an undocumented shortcut; (c) bundling
an auto-import mechanism into the deletion would re-create the very coupling we are
removing. A prelude for *std* (ADR-0042) and a possible future `rue.toml` are
tracked independently and can land whenever their own designs are ready — neither
is a prerequisite for RUE-181.

### `main.rue` and sibling discovery post-deletion

Question: once bare cross-file names are gone, how does a program with
`main.rue` + siblings hold together — does the driver auto-import the directory,
or must `main.rue` `@import` each sibling?

**Decision: `main.rue` must `@import` what it uses; the driver does not
auto-import siblings.** The entry file is an ordinary module (ADR-0026:
file-as-struct); it reaches siblings the same way any file does. The command-line
file list seeds *which files are available to load*, but availability ≠ scope.
This keeps one rule for all files (no special "the entry directory is magic" case)
and matches ADR-0026's "filesystem is the source of truth, imports are explicit."

The facade convention generalizes cleanly for the "many small siblings" case: a
directory module's `_dir.rue` facade `pub const`-re-exports its submodules, so a
consumer writes one `@import("dir")` and reaches everything the facade chooses to
expose — the intended replacement for "throw all the files on the command line."

Transitional note on the file list: today the driver both (a) discovers `main()`
among the listed files and (b) makes all listed files mutually visible. After
deletion only (a) survives, plus the whole-program *checking* extent (spec 10.5:4
currently analyzes every loaded file eagerly). Whether a listed-but-never-imported
file should still be *analyzed* (dead-correct checking) or dropped is an
ADR-0026-lazy-analysis question (RUE-134) and is **out of scope** here; this ADR
only removes cross-file *name resolution*, not the extent-of-analysis rule.

### Interaction with privacy-everywhere (RUE-180)

Already covered above, restated as the key invariant: **RUE-180 is the reason this
deletion is small.** Because visibility is already uniform, removing flat mode
removes *only* the unqualified cross-file resolution path — no visibility rule
changes. Post-deletion, `pub` regains a single, honest meaning: "reachable
**through an `@import`** from another directory." E0460 (unqualified private
reference) loses its cross-file trigger entirely and becomes reachable only in the
narrowing window before Phase B; after Phase B, a private *or* public sibling name
used unqualified is uniformly an unresolved-name error, and privacy is enforced
solely at the qualified `mod.item` access point (E0706). This is strictly simpler
than today's split where the *same* cross-file reference is either E0460 (private)
or silently-OK (pub) depending on visibility.

## Implementation Phases

Implementation is deferred; these phases are the task breakdown for RUE-181's
follow-up issues (filed after this ADR lands, not before).

- [ ] **Phase A: Deprecation warning** — add `WarningKind::FlatNamespaceReference`
      (name + suggestion), emit it at the resolver site where a bare name resolves
      to another file, on by default; UI test cases pinning the message. — RUE-181
      follow-up
- [ ] **Phase B: Delete resolution fallback** — remove the cross-file scan from
      name resolution; allocate the new unresolved-cross-file diagnostic code;
      narrow E0436 to intra-module. — RUE-181 follow-up
- [ ] **Phase C: Spec update** — strike 10.3:8 and 10.5:2; rewrite 10.5:1 to
      intra-module collision; adjust 10.3:7/10.5:4 wording; keep traceability 100%
      (retarget or replace the flat-namespace example cases). — RUE-181 follow-up
- [ ] **Phase D: Test-corpus migration** — add `@import` to the flat-mode cases in
      the harnesses (see blast radius). — RUE-181 follow-up
- [ ] **Phase E: Docs** — update CLAUDE.md "Multi-File Compilation" section and
      ADR-0023/0026 cross-references to describe the file list as a module-graph
      seed, not a shared namespace. — RUE-181 follow-up

## Blast Radius (measured on trunk @ this ADR)

Estimated by scanning the test corpora for multi-file cases that reach a sibling
**without** an `@import` (i.e. genuinely rely on flat mode):

| Harness | Flat-mode-reliant cases | Where |
|---------|------------------------|-------|
| CLI (`crates/rue-cli-tests/cases/`) | ~36 cases across 9 files | `basics`, `modules`, `multifile_errors`, `assoc_fn_privacy`, `arraybuf_library`, `const_init`, `emit_pipeline`, `output_guard`, `recursive_value_type` |
| Spec (`crates/rue-spec/cases/`) | ~18 cases (of 29 `pass_aux_files=true`) | `modules/intra_directory` (15), `modules/composition` (2), `modules/bindings` (1) |

Notes for the implementer:

- The **spec `modules/composition` cases** directly *test the flat namespace's
  collision behavior* (10.5:1/10.5:2). Those aren't "add an `@import`" migrations —
  they are the cases whose *rule* changes in Phase C, so they get rewritten (or
  moved to intra-module collision), not merely patched.
- The `intra_directory` cases test RUE-180 visibility using bare references; most
  migrate mechanically by wrapping the cross-file call in `@import` + qualified
  access, and several are already redundant with the qualified-access cases.
- CLI cases assert exact stdout; migrations must preserve the asserted output, so
  each is a real (small) code edit, not a find-replace.
- No compiler-crate error codes are allocated by this ADR; Phase B picks the code.

This is a **bounded, mechanical** migration (~54 cases, no runtime-behavior
changes for correctly-`@import`ing programs), which is why the warn phase is worth
having: Phase A lets us migrate the corpus under a green warning before Phase B
turns it red.

## Consequences

### Positive

- **One way to reference another file** — `@import`, matching ADR-0026's whole
  premise; removes the redundant bare-name path.
- **Name resolution is local and explicit** — a file's meaning no longer depends
  on its command-line neighbors.
- **Unblocks per-module name scoping** — 10.5:1's program-global collision rule
  can narrow to intra-module, the direction 10.5:2 always pointed at.
- **`pub` gets a single honest meaning** — "reachable through an import," full
  stop; privacy enforced only at the `mod.item` access point.
- **Smaller spec** — two transitional paragraphs deleted, one collision rule
  narrowed.

### Negative

- **Per-file import boilerplate** — one `@import` line per distinct sibling used.
  Mitigated by facade re-exports; explicitly *not* mitigated by auto-import (which
  we reject as re-introducing flat mode).
- **Corpus migration cost** — ~54 test cases need `@import` added or rules
  rewritten (bounded, mechanical; see blast radius).
- **Breaking change for any external multi-file program** — mitigated by the
  warn→error sequence and the fact that the fix is purely additive (`@import`).

### Neutral

- **Extent-of-analysis unchanged here** — whether a listed-but-unimported file is
  still analyzed is left to the lazy-analysis work (RUE-134), not decided here.
- **Prelude and `rue.toml` remain independent** — neither is required by this ADR;
  both can land on their own timelines (ADR-0042 / RUE-315).

## Open Questions

1. **Skip the warn phase?** RUE-180 makes a single-step deletion sound. This ADR
   recommends the warn phase for corpus-migration hygiene and external goodwill,
   but Steve may choose to collapse A+B into one change.
2. **Fate of listed-but-unimported files** — analyze eagerly (dead-correct) or
   drop them? Deferred to RUE-134 / ADR-0026 lazy analysis; not blocking.

## Future Work

- **Per-module name scoping** — the 10.5:2 revision this deletion unblocks.
- **Directory auto-import** — if ever wanted, a *separate* opt-in ADR, not a
  silent rider on this one.
- **`rue.toml` project file** — dependency and file-set declaration; independent.

## References

- [ADR-0023: Multi-File Compilation](0023-multi-file-compilation.md) — introduced
  the flat namespace as a stepping stone.
- [ADR-0026: Module System](0026-module-system.md) — `@import`, file-as-struct,
  directory modules; superseded the flat namespace on paper.
- [ADR-0042: Standard-library availability model](0042-std-availability-model.md) —
  prelude vs. explicit std, independent of this deletion.
- RUE-181 — this design's tracking issue ("delete flat mode entirely").
- RUE-180 — privacy everywhere; made this deletion resolution-only.
- RUE-116 — module-system epic; end-to-end `@import` correctness.
- Spec chapter 10 (`docs/spec/src/10-modules/`) — 10.3 visibility, 10.5 program
  composition; 10.3:8 and 10.5:2 are the paragraphs this deletion strikes.
