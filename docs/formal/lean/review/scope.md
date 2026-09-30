# Scope: the issue, and one topic

Advisory angle: findings do not block.

Does the change do what its issue asks, and only that, as one unit? The
project's roadmap is its Linear issues and milestone ladder
(`../../../notes/lean-mechanization-spike.md`, the project outline); a lane
serves one issue.

## Scope

The whole diff, against the issue's text and the description.

## What to run

```bash
git diff <base>...HEAD --stat
git log --oneline <base>..HEAD
```

## Rules

- **R1. The issue's deliverables are there.** Each deliverable the issue
  lists is in the change, or the description says which part it is and what
  follows.
- **R2. Nothing the issue did not ask for.** An opportunistic refactor, a
  second topic, a new construct or theorem outside the issue. Ask for a split
  or a follow-up issue. A change to a spine statement the issue did not ask
  for is `spine-change` R4's, and blocks there.
- **R3. The order holds.** Work for a later milestone while the milestone it
  depends on is not merged is premature; name the missing step.
- **R4. Refactors of merged material are in scope.** Reworking, renaming,
  relocating or re-proving what is already on trunk needs no fresh
  justification, as one topic.

## From this package's history

- The tooling calibration on `TraceOrder.lean` found a refactor (the shared `Steps`
  induction) and eleven golf edits; neither was made in that change, because
  proof simplification belonged to RUE-2471. R2, done right (`../TOOLING.md`).
