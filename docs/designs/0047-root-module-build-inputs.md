---
id: 0047
title: Root-module compilation units and build-system inputs
status: accepted
tags: [modules, compiler, build-system, packages, cli, language-shape]
feature-flag:
created: 2026-07-05
accepted: 2026-07-05
implemented:
spec-sections: ["10.5 (program composition)"]
relates: ["ADR-0023", "ADR-0026", "ADR-0045", "ADR-0046", "RUE-430", "RUE-434"]
---

# ADR-0047: Root-Module Compilation Units and Build-System Inputs

## Status

Accepted — Steve, 2026-07-05. This ADR records the compilation-unit model that
falls out of ADR-0046's flat-mode deletion and the build-system concerns raised
while discussing RUE-430/RUE-434. It is a design decision, not an implementation.
Concrete CLI, manifest, dependency-reporting, and package-resolution work should
be tracked as separate Linear issues.

**Amendment (RUE-920/RUE-921).** The single-root-module model here is the
authority for entry-point selection: the program entry point is the root
module's `main` (spec 6.1:38). An interim guard added under RUE-582 rejected a
second top-level `main` in *any* loaded module program-wide, a transitional
approximation from before top-level names became module-scoped. RUE-920 retired
that guard: a `main` in a non-root module is now an ordinary namespaced function,
which is exactly what this ADR's root-module model implies. Same-file duplicate
names remain per-file conflicts (spec 10.5:1).

## Summary

A Rue compilation target has exactly one **semantic root module**. The program's
semantic compilation unit is the transitive module graph reachable from that root
through explicit imports. Build systems may still pass a complete, declared set
of source inputs for hermeticity, sandboxing, caching, and incremental rebuilds,
but those declared inputs are **not semantic roots** and never create a flat
namespace. They constrain and validate import resolution; they do not change what
names are in scope.

This gives Buck-like build systems the thing they need — a complete action-input
set — without bringing back C-style translation units or Rue's old flat multi-file
mode.

## Context

ADR-0046 deletes flat multi-file mode: listing `a.rue b.rue main.rue` on the
command line must not make every `pub` declaration visible by bare name. That
leaves an important question unanswered: if extra files no longer affect scope,
why would a compiler invocation or build rule ever pass them?

The answer is that there are two different sets that should not be conflated:

1. **The semantic module graph**: what the program can reach from its entry/root
   module through imports and references.
2. **The build action input set**: the files a build system declares as possible
   inputs to the compiler action, so the action is hermetic, cacheable, and
   sandboxable.

C conflates these in a way Rue should not copy: each source file is its own
translation unit, and headers are textual inputs discovered along the way. Zig is
a better comparison for Rue's language model: files are modules, one artifact is
compiled from a root module, and imported modules form a graph. Zig also ships an
integrated build system, but Rue should not make that part of the compiler's
semantic contract. The recent Zig direction of moving package-management behavior
out of the compiler executable and into the build-system process reinforces this
boundary: the compiler should compile resolved inputs; the build system or package
manager should fetch, resolve, watch, and orchestrate them.

Rue therefore needs a crisp rule:

- Language semantics are rooted in imports.
- Build-system integration is rooted in explicit input manifests and dependency
  reports.
- Package management is outside the compiler.

## Decision

### One root module per compilation target

Each buildable Rue target has one semantic root module: for an executable, the
module containing `main`; for future libraries/tests, the target's declared root.
The semantic compilation unit is the module graph reachable from that root.

```text
target root module
└─ explicit @import graph
   └─ referenced declarations and comptime instantiations
```

Extra source files supplied by a build system are only candidate inputs. They do
not become additional semantic roots, and declarations in them do not enter the
root module's scope unless reached through an explicit import.

### Source input manifests constrain imports; they do not define scope

Build systems may invoke the compiler with an explicit source manifest. The exact
flag and schema are future work, but the intended shape is:

```bash
rue main.rue \
    --source-manifest rue-sources.json \
    --deps-manifest rue-deps.json \
    -o prog
```

The source manifest's job is to describe files the compiler is allowed to read for
this action. It supports hermetic builds, remote execution, deterministic cache
keys, and early diagnostics for imports that escape the build rule's declared
inputs.

It does **not**:

- auto-import every listed file;
- make sibling declarations visible by bare name;
- change duplicate-name rules outside the actual module graph;
- force every listed file to be analyzed in the default mode.

In other words, manifest membership means "available to import," not "in scope."

### Positional multi-file CLI remains legacy

After ADR-0046, a command such as:

```bash
rue main.rue helper.rue -o prog
```

has no good long-term meaning. If `helper.rue` does not affect scope, then the
human-facing CLI should prefer:

```bash
rue main.rue -o prog
```

and build-system-facing invocations should use a manifest instead of an ambiguous
list of positional `.rue` files.

RUE-434 should decide the exact migration path, but the preferred end state is:

- one positional root source for normal compilation;
- explicit flags for build manifests and dependency manifests;
- diagnostics or removal for legacy extra positional source files.

### Compiler emits actual dependency information

The compiler should eventually be able to report the files and package modules it
actually read while compiling the root module. This is distinct from the source
manifest:

- the manifest is the build system's declared allowed input set;
- the emitted dependency information is the compiler's observed read/import set.

The observed set enables build-system validation, depfile-style integrations,
incremental recompilation, and editor tooling. It must not become another way to
define language semantics.

### Packages provide named module roots, not flat visibility

Future package support follows the same model. A package is a collection of source
modules plus metadata, targets, and resolved dependencies. A target inside a
package still has one semantic root module.

Dependency packages provide named module roots or exported module namespaces. A
future spelling might look like `@import("pkg:foo")`, `@import("foo")` through a
dependency map, or something else; this ADR deliberately does not choose syntax.
The invariant is the important part: importing a package gives the current module
an explicit module value/namespace. It does not inject that package's declarations
into the current scope.

The package/build graph and language graph layer like this:

```text
workspace / package graph
└─ target graph
   └─ one root module per target
      └─ explicit import graph
         └─ referenced declarations / comptime instantiations
```

### Package management stays outside the compiler

The Rue compiler should consume already-resolved package/dependency information.
It should not own network fetching, version solving, registry protocols, Git,
TLS, compression formats, lockfile updates, or package cache mutation.

Those responsibilities belong to a build system, package manager, or future Rue
tooling process that invokes the compiler with resolved manifests. This keeps the
compiler smaller, easier to embed, easier to test, and compatible with external
build systems such as Buck.

### Interaction with lazy analysis and `check-all`

ADR-0045's lazy semantic analysis still applies. The default compiler analyzes
what is reachable from the root module and its referenced declarations. A source
manifest may contain files that are never imported or declarations that are never
referenced; those need not be analyzed by default.

A future `check-all` or library-validation mode may intentionally expand the set
of roots to validate all public declarations in a package or all files in a
manifest. That mode is a tool/checking mode. It must not alter normal program
semantics or make unimported names visible.

## Implementation Phases

- [ ] **Phase 1: Spec and CLI wording** — Update spec 10.5 and user docs to say
      compilation has one semantic root module, and that extra source inputs do
      not define scope.
- [ ] **Phase 2: Positional multi-file cleanup** — Implement the RUE-434
      migration away from ambiguous extra positional `.rue` files.
- [ ] **Phase 3: Source manifest input** — Add a manifest flag that constrains
      source resolution to a declared set of files.
- [ ] **Phase 4: Dependency reporting** — Emit the compiler's observed import/read
      graph for build systems, CI validation, and future incremental work.
- [ ] **Phase 5: Package/dependency manifest** — Design and implement the
      resolved-package input format once package syntax and stdlib layout are
      ready.

## Consequences

### Positive

- Build systems can still declare all source inputs up front.
- Deleting flat mode does not make Buck-style hermetic builds impossible.
- The human CLI stays simple: compile one root source.
- The compiler gets a narrow responsibility: compile resolved inputs.
- Future package support composes with imports instead of reintroducing global
  visibility.
- Lazy analysis remains meaningful because declared input files are not
  automatically semantic roots.

### Negative

- Rue will need at least one manifest format and likely a dependency-reporting
  format, which is extra tooling surface area.
- Build integrations must model Rue targets explicitly instead of passing an
  arbitrary bag of `.rue` files and expecting the compiler to decide semantics.
- The exact migration from legacy positional multi-file invocations needs care so
  existing examples and tests remain understandable.

### Neutral

- This ADR does not decide package import syntax.
- This ADR does not decide whether the future package/build tool is named `rue`,
  `rue build`, a Buck rule, or something else.
- This ADR does not require a bundled build system. It only requires that the
  compiler expose enough protocol surface for build systems to be first-class.

## Open Questions

- What is the exact schema for the source manifest?
- Should unused declared source inputs be accepted, warned on, or rejected in
  strict build-system mode?
- Should dependency reporting use a Make-style depfile, JSON, or both?
- What is the package import spelling and how does it interact with `@import`'s
  existing filesystem resolution?
- How do manifests represent generated Rue sources and virtual/module-map style
  inputs?
- Which command owns package fetching and lockfile updates if Rue eventually
  ships first-party package tooling?

## References

- ADR-0023: Multi-File Compilation
- ADR-0026: Module System
- ADR-0045: Lazy semantic analysis
- ADR-0046: Delete flat multi-file mode
- RUE-430: compilation-unit / build-system semantics discussion
- RUE-434: positional multi-file CLI cleanup
