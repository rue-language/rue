# No spine-statement change without Comparator

Integrity angle: any finding blocks.

The spine is what the mechanization claims: the statements
`RueCore.Spec.spine`, `RueCore.Spec.witnesses` and `RueCore.Spec.sharpness`
list (`../README.md`, "The statement layer"). Comparator checks that each proof
has its Spec statement and uses no axiom outside the policy, but it does not
freeze the claim: its challenge is generated from the Spec layer, so a
statement weakened together with its proof, and the challenge regenerated,
passes. What makes such a change visible is `spine-fingerprints.txt`, one hash
of each statement's elaborated body, which is never regenerated except on
purpose. This angle checks that any change to the claim is deliberate,
stated, and checked by Comparator.

## Scope

- `docs/formal/lean/spine-fingerprints.txt`, `docs/formal/lean/SPINE.md`,
  `docs/formal/lean/comparator/Challenge.lean`,
  `docs/formal/lean/comparator/config.json`;
- `docs/formal/lean/RueCore/Spec.lean` and `docs/formal/lean/RueCore/Spec/*.lean`;
- any L0 or L1 definition a Spec statement unfolds to: changing it changes a
  statement's elaborated body, and so its fingerprint.

## What to run

From `docs/formal/lean`, after `lake build` and building the executables
(`lake build ruecore-digest ruecore-lint`):

```bash
git diff <base>...HEAD -- spine-fingerprints.txt RueCore/Spec.lean RueCore/Spec comparator SPINE.md
lake exe ruecore-digest --fingerprint > "$SCRATCH/fingerprints.txt"
diff spine-fingerprints.txt "$SCRATCH/fingerprints.txt"      # must be empty: the committed hashes are the head's
lake exe ruecore-digest --spine > "$SCRATCH/SPINE.md" && diff SPINE.md "$SCRATCH/SPINE.md"
lake exe ruecore-digest --challenge > "$SCRATCH/Challenge.lean" && diff comparator/Challenge.lean "$SCRATCH/Challenge.lean"
lake exe ruecore-digest --comparator-config > "$SCRATCH/config.json" && diff comparator/config.json "$SCRATCH/config.json"
lake exe ruecore-lint
```

When any fingerprint differs between `<base>` and the head, run Comparator:

```bash
comparator/run.sh                  # on Linux with Landlock active: sandboxed
comparator/run.sh --unsandboxed    # elsewhere: every check but the sandbox
```

It passes with `Your solution is okay!` as its last line. It builds Comparator
and `lean4export` into `.lake/comparator` on first use (network access), or
uses a prebuilt one named by `COMPARATOR_HOME`. If your environment cannot run
it, say so in "Notes" and give the verdict `block`: the angle is not
satisfied until someone runs it.

## Rules

- **R1. The committed hashes are the head's.** The regenerated fingerprints,
  `SPINE.md`, challenge and configuration equal the committed copies. A stale
  copy means the claim the pull request shows is not the claim it proves.
- **R2. A changed fingerprint is stated.** For every line of
  `spine-fingerprints.txt` added, removed or changed since `<base>`, the
  description names the statement and says what changed in it: a rename, a
  statement strengthened, weakened or re-expressed, a definition it unfolds
  to changed. A rename is a removal and an addition; it too is stated.
- **R3. A changed statement passes Comparator.** When any fingerprint
  changed, Comparator runs on the head and passes.
- **R4. The issue asked for it.** A spine statement weakened, removed or
  re-expressed is within the issue's text or a maintainer's recorded
  decision. A change the issue does not cover, however small, is a finding;
  a simplification or refactoring lane changes no fingerprint
  (`../SIMPLIFY.md`, "Invariants").
- **R5. The diff shows the new statement.** The Spec diff, and the
  regenerated `SPINE.md` entry, let a reader see the old and the new
  statement side by side; a change made only through a definition two
  layers down is stated in the description with the statements it moved.

## What counts as a finding

A regenerated file that differs from the committed one; a fingerprint change
the description does not state; a fingerprint change with no Comparator pass;
a statement change the issue did not ask for. The repro is the `diff` output,
the fingerprint lines, or Comparator's last lines.

Not a finding here: whether the new statement is faithful (`faithfulness`) or
non-vacuous (`non-vacuity`); give those reviewers the list of changed
statements in "Notes".

## Severity

Every finding blocks.

## From this package's history

- Comparator was configured first (RUE-2460); review of that change found it
  passes a statement weakened together with its proof, since the challenge is
  generated. The fingerprints were added in its fix round, and a changed line
  now stops the lane until reviewed (`../README.md`, "The statement layer").
- The terminology audit (RUE-2466) renamed spine statements and definitions
  they name in three steps, each regenerating `spine-fingerprints.txt` in
  its own commit that names the renames. R2 for a rename: the change is
  stated, and the statements' bodies are otherwise unchanged.
- Proof simplification (RUE-2471) keeps every fingerprint: its invariant is
  that the Spec layer is not edited. R4.
