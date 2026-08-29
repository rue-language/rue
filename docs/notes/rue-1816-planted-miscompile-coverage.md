# RUE-1816 planted-miscompile coverage ledger

This note records a measured answer to a narrow question: which current Rue
test nets detect three silent compiler defects when those defects are faithfully
reintroduced? It is evidence about these three defects and these recorded runs,
not a claim that every future miscompile will be detected.

## Isolation and reproduction

The plants are unified source patches under
`crates/rue-planted-miscompiles/patches/`. The runner creates a tracked-source
snapshot with `git archive HEAD`, applies exactly one patch, and compiles that
temporary tree. No plant, flag, environment hook, or alternate phase path is
compiled into `//crates/rue:rue`; `//:planted-miscompile-isolation-validation`
checks each contextual patch against an exact reviewed source preimage, rejects
file operations outside its allowlisted target, scans the complete Rust and
BUCK surfaces of the four compiler-owning crates for activation markers, and
exercises the runner's fail-closed classification and timeout paths.

Run one fail-closed study from a clean commit with:

```bash
scripts/planted-miscompile-study.py --defect RUE-348 --output /tmp/rue-1816-RUE-348
scripts/planted-miscompile-study.py --defect RUE-914 --output /tmp/rue-1816-RUE-914
scripts/planted-miscompile-study.py --defect RUE-1758 --output /tmp/rue-1816-RUE-1758
```

Each output directory contains `build.log`, one log per net, the one-input
`rue-fuzz` corpus, any oracle-fuzz findings, copied runner/patch/repro inputs,
`ledger.json`, and `SHA256SUMS`. The ledger binds each command, selected safe
environment, exit status, outer timeout, log hash, compiler hash, input hash,
and host identity. Study mode refuses a dirty or untracked source tree; each
ledger records the clean base commit from which it can be reconstructed. The
recorded measurements used macOS arm64, oracle-fuzz seeds 0 through 15, a
ten-second timeout for each generated compiler/execution phase, and explicit
outer timeouts for every build and harness process.

The plants are:

- **RUE-348:** allow constant folding of equal payload-carrying enum variants
  from their tag alone, reversing the payload-length guard in
  `fold_enum_comparison`.
- **RUE-914:** classify an address-taken by-value parameter as never written,
  reversing the CSE exclusion that prevents a post-`@ptr_write` read from
  reusing the stale pre-write value.
- **RUE-1758:** visit CFG blocks in arena order during CFG-to-MIR lowering,
  reversing the dominator-respecting `block_lowering_order` choice.

## Measured ledger

`Caught` means the net failed with an observable disagreement naming the
historical replay. `Missed` means the applicable bounded net completed cleanly.
`N/A` means its configured optimization level cannot activate that plant.
`Harness gap` means the endpoint ran the source but its contract does not
compare executable behavior.

| Net | RUE-348 | RUE-914 | RUE-1758 | Reason |
| --- | --- | --- | --- | --- |
| Oracle CLI corpus, O1 | Caught | N/A | N/A | RUE-348 is in constant folding at O1; CSE and inlining/lowering plants begin at O2. |
| Oracle CLI corpus, O2 | Caught | Caught | Caught | The corpus contains verified historical replays and compares optimized native observations with the interpreter. |
| Oracle CLI corpus, O3 | Caught | Caught | Caught | Same mechanism and replay inventory as O2. |
| Oracle generated fuzz, seeds 0--15 | Missed | Missed | Missed | The deterministic generated window did not produce any of the three required shapes. |
| CLI `differential_opt` filter | Caught | Caught | Missed | It selects the `cli.differential_opt` section by test name. RUE-1758's differential cases live in `cli.inlined_continuation_lowering_order`, outside that bounded release-smoke filter. |
| Focused historical CLI section/case | Caught | Caught | Caught | Every replay compares O0 through O3; RUE-1758 failed `present_and_absent_keys_both_answer_correctly` at O2 in the recorded run. |
| Focused specification case | N/A | N/A | N/A | The RUE-348 enum-equality and RUE-914 pointer-read/write semantic baselines run at O0, where their plants are inactive. No specification endpoint maps to RUE-1758's backend block-order defect; its focused CLI replay supplies the O0 source baseline and optimized native comparison together. |
| `rue-fuzz compiler_x86_64_o1`, one checked-in repro | Harness gap | N/A | N/A | The only optimized whole-compiler endpoint is O1 and asserts compile/ICE safety, not native behavior. It compiles the RUE-348 repro but cannot observe the wrong exit code; RUE-914/RUE-1758 require O2. |

Concrete oracle observations were:

- RUE-348: `enum_payload_equality_across_opt_levels` printed `1\n1\n0\n`
  instead of `0\n1\n0\n` at each of O1, O2, and O3.
- RUE-914: `param_raw_mut_write_reread_across_opt_levels` printed `12\n`
  instead of `1006\n` at O2 and O3.
- RUE-1758: the O2/O3 corpus lanes reported disagreements in the inlined
  continuation replay; the focused CLI run also sent absent/present values down
  the wrong match arms.

## Accepted gaps

The bounded generated-fuzz misses are accepted sampling gaps: deterministic
reproduction matters here, and 16 seeds are not evidence that longer nightly
fuzzing cannot find the shapes. The corpus lanes carry the deterministic
regressions.

The `differential_opt` release-smoke filter's RUE-1758 miss is accepted because
that lane is intentionally a bounded named section; the ordinary CLI corpus and
the focused `inlined_continuation_lowering_order` section own the replay, while
all O2/O3 oracle corpus actions also catch it.

The `rue-fuzz` row is a contract gap, not a failure of its stated purpose. Its
registered compiler targets hunt crashes and ICEs, and there is no O2/O3 native
execution comparator. This study does not expand that endpoint into the
semantic-oracle or fuzz-infrastructure work tracked separately.
