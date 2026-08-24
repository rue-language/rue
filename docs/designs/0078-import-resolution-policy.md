---
id: 0078
title: "Single-candidate import resolution and program-anchored std"
status: accepted
tags: [language, modules, spec, compiler]
feature-flag: null
created: 2026-08-18
accepted: 2026-08-18
implemented:
spec-sections: ["10.1:5", "10.2:1", "10.2:2", "10.2:4", "10.2:6", "4.13:89"]
superseded-by:
amends: [0026]
relates: ["ADR-0051", "ADR-0063", "ADR-0075", "RUE-1127", "RUE-266", "RUE-1100", "RUE-1023", "RUE-1586"]
---

# ADR-0078: Single-candidate import resolution and program-anchored std

## Status

Accepted under RUE-1127, 2026-08-18. This changes language semantics: it
narrows which files an `@import` path can resolve to, and it removes an
ambiguity surface along with its diagnostic. It is a source-breaking change
for extensionless imports of file modules, so it ships with a migration
diagnostic (Phase 4).

## Summary

An `@import` specifier resolves to exactly one candidate path. Extensionless
relative specifiers name a directory facade only; a file module is spelled
with its `.rue` extension. Relative candidates are searched against the
importing file's directory only — the root-file fallback is removed.
Project-relative module identity becomes a total function, so an import may
not escape the project root. The reserved specifier `std` is exempt from
relative search entirely: it anchors to the program, resolving to a vendored
`{root}/std/_std.rue`, then `$RUE_STD_PATH`.

## Context

Spec 10.2:1–2 currently specifies a four-candidate search for an
extensionless relative import: `{P}.rue` and `{P}/_{basename}.rue`, each
tried first against the importing file's directory and then against the root
file's directory, with an intra-group tie rejected as ambiguous (E0708) and a
cross-group tie resolved silently in favor of the nearer file.

RUE-266 fixed a genuine bug here — the search had been program-global rather
than importer-relative — but fixing the implementation did not settle whether
the resulting policy is the one Rue wants. RUE-1127 asks that question. The
policy is implementable incrementally and its candidate set is bounded, but
it is nonlocal in two ways that were reproduced against the current compiler
before this ADR was written:

- **Adding a file silently retargets an unchanged import.** A `sub/mod.rue`
  importing `"helper"` resolves to the root `helper.rue`. Creating
  `sub/helper.rue` — touching no existing source — silently rebinds that
  import to the new file, with no diagnostic.
- **Adding the alternate form turns a working program into an error.** With
  `sub/helper.rue` resolving, creating `sub/helper/_helper.rue` makes the
  same unchanged import E0708.

The same search applies to `std`, which produces a sharper version of the
first hazard: with `RUE_STD_PATH` unset, a `std/_std.rue` placed beside any
importing file captures the standard library for that file. Nonlocal
retargeting of unchanged code, aimed at the standard library.

Separately, `lexical_relative_path` emits `..` components rather than
rejecting an escaping path, so `@import("../outside.rue")` yields the logical
module identity `"../outside.rue"`. "Normalized project-root-relative path"
is therefore not a total function today. Canonical identity across differing
spellings is carried by physical identity (`PhysicalFileIdentity`, a
volume/inode pair) rather than by path normalization, which is why the
hard-link alias tests in `import_discovery.rs` exist.

### Why the root-relative fallback cannot simply be deleted

An earlier form of this ruling proposed "importer-relative only" without
qualification. Checking that against the compiler showed it would break
vendored standard libraries. With `RUE_STD_PATH` unset, a project root
containing `std/_std.rue` and a nested `sub/mod.rue` importing `"std"`
resolves — per `--emit deps` — to `std/_std.rue`, and it reaches that file
*only* because the base-directory list includes the root directory. Deleting
the fallback wholesale would silently remove vendoring for every importer not
sitting in the root directory.

Vendoring is a use case Rue intends to keep. There is no compiled-in default
standard-library location: `crates/rue/src/main.rs` reads `RUE_STD_PATH` and
nothing else, so for a toolchain that does not set it, the filesystem
fallback is the only way `std` resolves at all. The fallback is load-bearing,
not vestigial.

The resolution is to separate the two things the fallback was doing. Relative
specifiers lose it. The reserved name `std` keeps a program-anchored
location, which is what vendoring actually needs.

### Why `std` precedence is not an ordinary env-var override

"Environment variables let you override things" is a sound principle for
*settings*. A vendored standard library is not a setting: it is source that
is compiled into the artifact. Allowing an ambient `RUE_STD_PATH` — commonly
set by a CI image, a nix shell, direnv, or a toolchain wrapper — to replace a
standard library the program shipped is a substitution of program content by
ambient environment, and the person running the build is usually not the
person who vendored.

Making a conflict an error instead was considered and rejected. Because a
toolchain that works without vendoring must set `RUE_STD_PATH`, "both
present" is the *normal* state of the vendoring workflow rather than a rare
mistake, so the error would fire on the happy path and require a flag to
un-break correct code. It would also make creating a file break a
previously-working build — reintroducing at the `std` slot exactly the
nonlocality this ADR removes elsewhere.

A dedicated override flag (`--std-path`) was considered and deferred: no
present workflow needs it — every harness in this repository overrides
through the environment against non-vendoring programs — and CLI surface
shipped now is surface the RUE-1586 distribution design must later honor or
break. Adding a flag later is additive; it is deferred, not rejected. The
supply-chain posture is served by `--source-manifest`, which already governs
`std`: a std path the manifest does not declare is a hermetic denial with no
probe.

## Decision

### 1. Extensionless means facade only

Spec 10.2:1 interpretation 3 resolves an extensionless specifier `P` to the
directory facade `{P}/_{basename}.rue` and nothing else. Interpretation 2
already resolves a `.rue`-suffixed specifier exactly, so a file module is
spelled `@import("math.rue")`.

This *deletes* the file-versus-facade ambiguity rather than diagnosing it.
Spec 10.1:5 and rule 4.13:89 are reworded under their stable IDs (spec
paragraph IDs are permanent), and E0708 is retired.

### 2. Relative specifiers are importer-relative only

Spec 10.2:2 searches one base directory: the directory containing the
importing file. The root-file fallback and its precedence rule are removed.
Every relative specifier therefore has exactly one candidate path.

### 3. Project-relative identity is total; escaping the root is rejected

An import whose normalized path leaves the project root is rejected with a
new diagnostic. This makes "normalized project-root-relative path" a total
function and supplies the canonical identity 10.2:4 assumes but never
specifies. Two spellings denote the same module exactly when they normalize
to the same project-root-relative path.

Physical-identity deduplication is retained as a distinct mechanism: it
reconciles hard links and symlinks that reach one file by different paths,
and it continues to reject incompatible aliases. Path normalization decides
*identity*; physical identity decides *aliasing*.

The standard library is unaffected by this rule. Std modules already carry
their own `\0rue-std/` logical namespace and are not project-root-relative.

### 4. `std` anchors to the program

`std` is a reserved specifier, not a relative path — it is already an exact
match that never competes with a user file named `std.rue`. It resolves
against a fixed precedence chain, taking the first that exists:

| Order | Source | Meaning |
|-------|--------|---------|
| 1 | `{root}/std/_std.rue` | the program vendored its own |
| 2 | `$RUE_STD_PATH` | toolchain installation default |
| — | otherwise | E0705 |

Each is a single candidate. `std` is never searched importer-relative, which
removes the capture hazard. Vendoring works from any depth, because the
vendored location is anchored to the program root rather than to the
importer.

The governing property: **ambient environment can never replace a standard
library the program shipped.** Replacing a vendored standard library means
editing the program's tree.

Under `--source-manifest`, the manifest is the authority on which standard
library is in the build. A candidate the manifest does not declare is
skipped and the walk continues: declare the vendored copy and it wins,
omit it and the declared toolchain facade resolves.

This narrows an earlier form of this decision, which required an existing
but undeclared vendored std to fail closed. That rule is not implementable
as stated. Hermetic denial is *lexical and takes no probe* — by design, so
that a hermetic build never touches an undeclared path — so the compiler
cannot distinguish "absent" from "present but undeclared". Treating the
denial as conclusive fails every hermetic build whose program does not
vendor std at all: the vendored candidate is probed first and denied even
when nothing is there. The repository's own reproducibility fixture is
exactly that shape.

Preserving the stricter rule would require the host to stat undeclared
candidates to separate absence from denial, weakening the no-probe
guarantee to sharpen a case a correct manifest never reaches. The
generator that produces a manifest sees a vendored `std/` and declares it;
omitting it is a build-system defect, and its failure mode is the declared
toolchain std rather than a silent read of an undeclared file.

Because source-manifest denials are policy outcomes (with lexical denial taking
no probe), only `DeniedLexical` and `DeniedCanonical` observations are skipped
and allow the walk to continue. A probed candidate that is present but unusable —
`PresentUnreadable`, `InvalidPhysicalType`, or `UnstableRead` — is conclusive:
it is reported immediately and cannot be replaced by a later candidate. Under
policy v2 only `std` has more than one candidate, so this distinction is
confined to the std chain: every relative specifier still fails on its single
candidate's failure. Denied and absent remain distinguishable typed
observations, satisfying ADR-0063; they are merely both non-resolutions for
precedence purposes. Cancellation is not a candidate outcome: it is a
non-closing terminal state for the current read transaction and does not
advance precedence or permit a later candidate to win.

### 5. Nonlocality, stated

With the above, the answer to "may adding a new file retarget an unchanged
import?" is **no** for relative specifiers: each has one candidate, so a new
file either satisfies a previously-failing import or is irrelevant to it. The
only permitted positive→positive transition is at the `std` slot, where
adding a vendored `{root}/std/_std.rue` takes precedence over
`$RUE_STD_PATH`. That transition is deliberate, is confined to a single
well-known path, and is reported in the dependency record.

The negative observations an incremental compiler must retain are exactly the
absent candidates it probed: one per relative specifier, and for `std` the
prefix of the precedence chain above the entry that resolved. A retained
terminal is invalid when any of those absences becomes a presence. This is
the existing absent-leaf invalidation rule of ADR-0063 section 2; this ADR
only shrinks the set it applies to.

## Implementation Phases

- [ ] **Phase 1: Spec amendment** — rewrite 10.2:1–2, restate 10.2:4's
      canonical identity, replace 10.2:6 with the precedence chain, reword
      10.1:5 and 4.13:89 under their stable IDs, retire E0708.
- [ ] **Phase 2: Candidate-policy collapse** — one candidate per relative
      specifier and one base directory in `discovery_candidate_groups` /
      `discovery_groups_for_occurrence`; std resolves through the precedence
      chain; a test pins the undeclared-vendored-std hermetic denial.
- [ ] **Phase 3: Total root-relative identity** — reject escaping imports
      with a new diagnostic; keep physical-identity aliasing.
- [ ] **Phase 4: Migration diagnostics** — when an extensionless import finds
      no facade but a sibling `{P}.rue` exists, suggest the extensioned
      spelling. This probe is diagnostic-only and contributes no dependency
      edge and no retained observation.
- [ ] **Phase 5: Locality tests and fixture migration** — the four RUE-1127
      cases plus the ~280 extensionless fixture sites.

## Consequences

### Positive

- One spelling per target and one candidate path per import. Resolution
  becomes a function of the specifier and the importing file alone.
- Both nonlocal transitions are eliminated for relative specifiers.
- The stdlib capture hazard is removed while vendoring is preserved and, for
  nested importers, made to work independently of a fallback that was never
  designed for it.
- Ambient environment can no longer substitute program content.
- Canonical module identity is specified rather than emergent.
- Whatever RUE-1551 is eventually measured against is a smaller and more
  clearly specified policy.

### Negative

- Source-breaking for extensionless imports of file modules. Real impact is
  small — three occurrences across all `.rue` files in the repository, all in
  `examples/`, against 6,775 explicitly-extensioned imports — but roughly 280
  test-fixture sites exercise the current policy deliberately and must be
  migrated.
- Importer-relative-only leaves distant imports spelled
  `../../../core/registry`. The fix is a named-module namespace, which
  belongs to Packages; this ADR accepts the ergonomic gap in the interim.
- `std` now follows a different anchoring rule from relative specifiers. This
  is stated in the spec as a property of reserved names rather than left
  implicit, but it is a second rule where there had been one.

## Open Questions

None. The `--source-manifest` interaction and the override-flag question
were both resolved after acceptance and folded into Decision 4: undeclared
vendored std fails closed, and the explicit override flag is deferred to
RUE-1586.

## Future Work

The `std` precedence chain settles the immediate policy question; it does not
design the standard library's distribution model. The current behavior was
grown rather than designed, and the questions it touches — how a standard
library is versioned, how vendoring interacts with a package graph, how
third-party modules are named and downloaded, whether `std` remains a
hardcoded exact match or becomes an instance of a general named-module
mechanism — belong with Packages. Tracked as RUE-1586; this ADR should be revisited
once that design exists.

RUE-1551 proposes replacing filesystem-driven imports with an explicit module
manifest. This ADR takes no position on it. It narrows the current semantics
so that any such proposal is evaluated against Rue's strongest present
design rather than its weakest.

## References

- RUE-1127 — the decision issue, including the maintainer ruling this ADR records
- RUE-1586 — the deferred standard-library distribution design
- RUE-266 — the importer-relative correctness fix that preceded this policy question
- [ADR-0051](0051-canonical-import-resolution-authority.md) — compiler-owned
  candidate policy; this ADR changes the policy, not its ownership
- [ADR-0063](0063-parallel-demand-driven-incremental-compilation.md) §2 —
  absent-leaf invalidation
- [ADR-0075](0075-wave-granular-import-discovery.md) — wave-granular discovery
- Prior art: Zig requires explicit extensions and declined this fallback
  (ziglang/zig#216, #744); Rust rejects the two-candidate case (E0761) rather
  than ranking it; Go's importer-nearest vendor rule required amendment so an
  empty intermediate directory could not hide a package
