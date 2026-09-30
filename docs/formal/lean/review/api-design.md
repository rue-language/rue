# Definitions and their lemmas

Advisory angle: findings do not block.

You judge the surface a change adds: the definitions, and the lemmas that let
a proof use a definition without unfolding it. The package's modules expose
every body on purpose (`@[expose] public section`, `../README.md`,
"Layers"): a downstream `decide`, `rfl` or `unfold` needs the body, so do not
ask for bodies to be hidden.

## Scope

Every new or changed definition, structure, inductive type and instance in
the diff, with the lemmas that come with it.

## What to run

From `docs/formal/lean`:

```bash
git diff <base>...HEAD -- RueCore
grep -rn 'unfold <name>\|simp \[<name>\]\|simp only \[<name>' RueCore   # proofs that unfold a new definition
```

## Rules

- **R1. A definition comes with its characterizing lemmas.** The equations,
  introduction and elimination forms, and interaction with the operations
  around it, in the `*.Lemmas` module of its layer. Proofs that unfold a new
  definition again and again are asking for the missing lemma.
- **R2. No free data.** A structure field or indexed family left free on
  inputs no law or operation uses lets two terms that agree everywhere that
  matters differ. Carry only what the laws use.
- **R3. The Spec layer is statements only.** A Spec module declares its
  `_stmt`s and the lists; a helper definition there enters the trusted base
  unstated (the lint enforces the shape; judge whether a helper moved to L1
  is one the statement needs).
- **R4. Simp and decide annotations are right.** A normal-form lemma other
  proofs rely on carries `@[simp]`; an annotation that loops or fires where
  it should not is a finding. A `Decidable` instance a witness needs for
  `decide` is there, not replaced by `native_decide`.
- **R5. Parallel forms only where used.** A symmetric or dual form is asked
  for only when the module develops both sides.

## From this package's history

- `Blocks` and `DropGlueBlocks` each have their own `append` lemma of the
  same shape (`../SIMPLIFY.md`, "Duplicated lemma shapes"); a new trace
  grammar should come with the same set, or share them. R1.
