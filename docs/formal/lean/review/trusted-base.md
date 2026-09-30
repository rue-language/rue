# The trusted-base lint is clean

Integrity angle: any finding blocks.

The trust bar is zero `sorry`, no axiom but `propext` and `Quot.sound`, no
`native_decide`, and no construct that makes the code `#eval` runs differ from
the definition the kernel reasons about (`../README.md`, "The trusted-base
lint"). `TRUST.md` is the generated evidence. No CI lane builds this package
yet (RUE-2241), so this reviewer runs the package's own gates, and reads what
they report beyond pass or fail.

## Scope

Every Lean file the change touches, `lakefile.toml`, `lean-toolchain`,
`TRUST.md`, and the layer table (`RueCore/Layers.lean`). A change that touches
no Lean file still needs the lint run once: a stale `TRUST.md` is a finding.

## What to run

From `docs/formal/lean`:

```bash
lake build
lake build ruecore-corpus ruecore-explain ruecore-digest ruecore-layers ruecore-lint
lake exe ruecore-layers                                  # last line: "…, no upward import; …"
lake exe ruecore-lint                                    # exits non-zero on a violation, naming each on stderr
lake env leanchecker $(lake exe ruecore-layers --closure)   # the kernel re-check of every package module
lake exe ruecore-digest --trust > "$SCRATCH/TRUST.md"
diff TRUST.md "$SCRATCH/TRUST.md"                        # must be empty
git diff <base>...HEAD -- TRUST.md                       # what the change did to the trust report
```

Use only these for axioms. A generic axiom script, or one that appends
`#print axioms` to a file, is not a check here: one such script resolved 4 of
74 declarations in `TraceOrder.lean`, because it lost the namespace after a
`mutual` block, and counts `Classical.choice` as standard (`../TOOLING.md`,
"What the review found"). For a quick look at one declaration, a probe in
`$SCRATCH` that imports its module and runs `#print axioms` on the fully
qualified name is fine, as a supplement.

## Rules

- **R1. Every gate passes.** `lake build`, `ruecore-layers`, `ruecore-lint` and
  `leanchecker` each exit 0.
- **R2. `TRUST.md` is current.** The regenerated report equals the committed
  one.
- **R3. The axioms did not widen.** In `git diff <base>...HEAD -- TRUST.md`,
  no theorem gained an axiom outside the policy, and a theorem that gained
  `Quot.sound` or `propext` did so through a proof change the description
  mentions. A lemma replaced by a library lemma is the usual cause.
- **R4. The trusted base grew only on purpose.** A definition entering the
  "Trusted base" section (what the Spec statements unfold to) is one the
  change means to add to the claim, and the description says so. A helper
  definition that entered because a statement now mentions it is a finding.
- **R5. Every listed exception is justified.** Each new entry the lint lists
  rather than fails (an L3 definition reaching `Classical.choice`, an L3
  `partial def` or `@[implemented_by]`, a bounded `set_option` override) is
  in L3 or is the scoped override of one lemma, and the change needs it.
  `set_option maxHeartbeats` is scoped to one declaration with `in`, never
  file-wide, never `0`.
- **R6. No escape around the lint.** No `set_option` through a macro, no
  `debug.skipKernelTC`, no new `[[lean_lib]]` or dependency in
  `lakefile.toml`, no module left out of the layer table: the lint's scan of
  options is best-effort, and the kernel re-check is the guarantee, so a
  change that would keep a declaration from `leanchecker` is a finding even
  if every gate passes.

## What counts as a finding

A gate that fails; a stale `TRUST.md`; an axiom, trusted-base entry or listed
exception the change added without saying why; a construct that sidesteps the
lint. The repro is the gate's output, the `diff` lines, or the source line.

## Severity

Every finding blocks.

## From this package's history

- Replacing the proof of `range'_increasing` by core's
  `List.pairwise_lt_range'` compiled, was shorter, and passed every golf rule;
  it added `Classical.choice` to `range'_increasing`, `reachable_ordered` and
  `drop_order`. Neither the golf procedure nor the generic axiom checker
  would have caught it. R3 (`../TOOLING.md`, "Golf, on a scratch copy").
- Since Lean 4.29 each `native_decide` adds its own axiom,
  `foo._native.native_decide.ax_1_1`, which names no compiler primitive; a lint
  matching `Lean.ofReduceBool` by name would pass it. The lint fails any
  axiom not on the allow-list, whatever it is called. R1, R6.
- The float witness's rounding reaches `2^1076`, past the elaborator's
  evaluation threshold; its proof uses `decide +kernel`, never
  `native_decide` (`../README.md`, "Non-vacuity witnesses"). The pattern R6
  expects.
