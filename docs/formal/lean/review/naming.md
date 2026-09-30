# Naming and notation

Advisory angle: findings do not block.

You judge the Lean names and notation a change introduces. Prose terms are
`glossary`'s. This is not a Mathlib project, so Mathlib's naming guide is not
the reference (`../TOOLING.md`, "What we decline"); the package's own
neighbouring names are, and [`../../FIELD.md`](../../FIELD.md) for the
field's words.

## Scope

Every new or renamed declaration, namespace and notation in the diff.

## What to run

From `docs/formal/lean`:

```bash
git diff <base>...HEAD -- RueCore | grep -n '^+.*\(theorem\|lemma\|def\|structure\|inductive\|abbrev\|notation\) '
grep -rn '<neighbouring name pattern>' RueCore
```

## Rules

- **R1. A name describes its conclusion.** Read from the conclusion outward,
  in the terms the package already uses for that notion. Consistency with
  adjacent declarations beats a better name in isolation.
- **R2. A name is no stronger than its statement.** No `_iff` with one
  direction, no `_eq` proving an inequality, no `exactly_once` for an upper
  bound.
- **R3. The field's name where it has one.** A spine statement whose notion
  the field names (FIELD.md) uses the field's name, or its doc-comment gives
  it; a name the field uses for a different notion is not reused.
- **R4. Namespaces.** Everything under `RueCore`; a declaration about a
  package type is in that type's namespace so dot notation works.
- **R5. Notation sparingly.** New notation is scoped and follows the
  calculus's own symbols.

## From this package's history

- `step_safeAt` reads as the syntactic subject-reduction lemma but states a
  semantic invariant; the statement was kept and its `SPINE.md` row says so
  (RUE-2423). R2, R3.
- The terminology audit (RUE-2466) renamed spine statements, witnesses and
  L1 definitions to the field's words, with no aliases left behind. R3, and
  `_common.md`'s "No compatibility layer".
