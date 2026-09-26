# Agent tooling for the Lean package: the lean4 plugin, calibrated

The lean4-skills plugin ([cameronfreer/lean4-skills](https://github.com/cameronfreer/lean4-skills))
packages Lean 4 workflows for coding agents: drafting, guided proving, review,
axiom checking, proof golfing, blocked-goal triage and environment diagnosis,
with helper scripts, hooks and command guardrails. This page (RUE-2455) records
how it behaves on this package, what it found when calibrated on one module,
`RueCore/TraceOrder.lean`, and which parts the project's loop uses for which
role. Nothing in the build, the checks in `README.md`, or CI depends on it: it
is an aid to the agent writing or reviewing a proof, and the package's own
gates (`lake exe ruecore-lint`, `TRUST.md`, `leanchecker`, Comparator) stay the
authority on what a proof rests on.

Calibrated with plugin version 4.11.1 on trunk `e832831c4` (2026-09-26),
Lean 4.33.1, macOS arm64.

## Install and scope

The plugin is installed at user scope, not in the repository: a maintainer runs
`/plugin marketplace add cameronfreer/lean4-skills` and `/plugin install lean4`
once, in Claude Code. User scope keeps the repository tool-neutral
(`AGENTS.md`): nothing under version control names the plugin, and an agent
that does not have it loses nothing the gates rely on. The plugin also ships a
native Codex adapter (`.codex-plugin/`, `hooks/codex-hooks.json`); see
[Other agents](#other-agents-codex).

## How it fits this package

The plugin assumes a Mathlib project with the lean-lsp MCP server attached.
This package has neither, and uses the module system (`module`, `public
import`, `@[expose] public section`). What that changes:

| Assumption | Here | Effect |
| --- | --- | --- |
| Lean LSP tools (`lean_goal`, `lean_multi_attempt`, `lean_diagnostic_messages`, the search ladder) are the normative first pass | not attached in the loop's sessions | Every workflow falls back to its scripts plus `lake env lean <file>` for per-edit checks. Golf drops to syntactic patterns by its own rule; candidate testing becomes "edit a scratch copy, recompile" (about 1 s for TraceOrder). Run `lake env lean` with `docs/formal/lean` as the working directory: from the repository root (`lake -d …`) elan can pick another toolchain and the build fails with `incompatible header`, which `compilation-errors.md` does not list. A scratch file outside the package uses a plain `import`, not `module` (RUE-2478). |
| Mathlib under `.lake/packages/mathlib` | absent | `search_mathlib.sh` and `smart_search.sh --source=mathlib` exit with "mathlib not found". Library search is Lean core's `exact?`/`apply?` only. |
| Mathlib style (100-character lines, naming, docstring rules) | the package has its own doc-comment convention (`README.md`) and no width rule | The review's style section and its Layer 2 (mathlib review) are advisory here: `lean4-skills-project-context` classifies the package `other-lean`, `contributing_upstream: no`, so Layer 2 runs advisory by the plugin's own precedence. |
| Standard axioms are `propext`, `Quot.sound`, `Classical.choice` | the policy is exactly `propext` and `Quot.sound` (`TRUST.md`) | The plugin's axiom checker passes `Classical.choice`. It is not our axiom gate. |
| Plain `import` headers | module-system headers | `minimize_imports.py` reports "No imports found" on a `public import` file. `diagnose`'s module-system error table does apply. |

Preflight, run from `docs/formal/lean`:

- `lean4-skills-preflight --codex` (checks the installed tree and wrappers):
  passes.
- `lean4-skills-preflight` (checks the session's `LEAN4_PLUGIN_ROOT`,
  `LEAN4_SCRIPTS`, `LEAN4_REFS`): fails in a subagent's shell, which does not
  inherit the variables the SessionStart hook persists, and passes once they
  are set to the plugin's `lib/scripts` and `skills/lean4/references`. The
  `lean4-skills-*` wrappers are on `PATH` either way, and they are all the
  workflows below need.
- `lean4-skills-project-context --from .`: `other-lean`, toolchain
  `leanprover/lean4:v4.33.1`, `mk_all_declared: false`.
- `lean`, `lake`, `python3`, `git` present. The plugin's scripts that shell out
  to `rg` need a ripgrep binary on `PATH`; a shell function named `rg` (as some
  agent shells define) is not seen by a `bash` subprocess, and without it
  `unused_declarations.sh` refuses to run (macOS `grep` has no `-P`).

## The workflows

Slash commands (`/lean4:review` and the rest) are Claude Code entry points. In
a delegated agent they are not invocable, so the loop follows a command's
procedure by hand: its `commands/<name>.md` names the scripts and the order.

| Workflow | What it is | Here |
| --- | --- | --- |
| review | Read-only report: build status, sorry audit, axiom check, style, golfing opportunities, proof-size metrics; a second, mathlib-review layer; a stuck mode that names the top three blockers | Runs by hand from its scripts. Style and Layer 2 advisory. The axiom step must be ours (below). |
| axiom check (`check_axioms_inline.sh`) | Appends `#print axioms` for every top-level declaration, compiles, restores the file | Unreliable on this package (below). Replaced by `lake exe ruecore-lint` and `TRUST.md`. |
| golf | Improves compiling proofs, scored by directness, then inference burden, then performance, then length; hard rejects (no new naked `;`, no climb up the tactic ladder for one line, no statement change) | Runs without LSP as a syntactic pass plus recompile-per-edit. Needs our axiom gate after every lemma replacement. |
| prove / autoprove | The cycle engine: plan, work per `sorry`, checkpoint, review, replan; stuck detection; deep mode with a header fence (no statement changes) | Its interactive prompts and commit policy do not fit a delegated lane. Its rules do (below). |
| blocked-goal triage | `skills/lean4/references/sorry-filling.md`: make the blocker concrete, classify it (seven classes), test at most three candidates, search before adding structure, stop guessing when it repeats | Adopted as is; tool-neutral. |
| diagnose | Environment check, and error triage against `compilation-errors.md` before the generic "lake clean" remedy | `diagnose env` is the preflight above. Error triage applies, including the module-system rows. |
| checkpoint | Per-file compile, project build, axiom scan, sorry count, commit of the touched files only | Superseded by the loop's own commit rules and `bin/chain.sh`. |
| draft, formalize, autoformalize, disprove, refactor, learn | Statement drafting, counterexample search, strategy-level simplification, teaching | Not calibrated. Statement work here goes through the Spec layer and its review (`README.md`, "The statement layer"). |

## Calibration on `RueCore/TraceOrder.lean`

### The module before

| Measure | Value |
| --- | --- |
| Lines | 1439 |
| Declarations | 74 (73 theorems, 1 definition), all in `namespace RueCore`, six `mutual` blocks |
| `sorry` | 0 (`lean4-skills-sorry-analyzer`) |
| Options (`set_option`, `maxHeartbeats`) | none |
| Rebuild, `lake build RueCore.TraceOrder` with its outputs removed, three runs | 1.42 s, 1.29 s, 1.43 s |
| Profile, `lake env lean -Dprofiler=true` (cumulative, three runs) | tactic execution 1.67 to 1.69 s, simp 0.48 to 0.49 s, type checking 0.28 s, elaboration 0.35 s |
| Heartbeat floor (`-DmaxHeartbeats=N` on the whole file) | builds at 6000, fails at 5000 (in `step_ordered`); the default is 200000 |
| Longest proofs (lines to the next declaration, doc-comment included) | `eval_glue_blocks` 248, `step_ordered` 63, `step_nested` 56, `step_lifo` 49, `step_drop_order` 43 |

### What the review found

- Build: passes. Sorry audit: none.
- Axiom check: the plugin's script resolved 4 of the 74 declarations, marked
  the file unverified and exited 1. The cause is in the script: its scope
  tracker treats the `end` that closes a `mutual` block as closing the
  enclosing `namespace RueCore`, so every declaration after the first `mutual`
  block is looked up without its namespace and not found. An independent
  `#print axioms` over all 74 fully qualified names: 51 use `propext`, 21 use
  `propext` and `Quot.sound`, 2 use none, matching `TRUST.md`. Even when it
  resolves, the script counts `Classical.choice` as standard.
- Style (advisory): 14 lines over 100 characters; about 55 lines that join
  tactics with `;`, which the golf policy counts as separate lines.
- Golfing opportunities: `lean4-skills-find-golfable` found none in
  TraceOrder (18 across the package, in `Soundness`, `TraceExact`, `Float`,
  `Float/Lemmas`, `Statics/Lemmas` and `Adequacy`).
  `lean4-skills-find-exact-candidates` found 12 anchors (1 high, 11 low). It
  names the high one `range`; the declaration is `range'_increasing`.
- Layer 2 (advisory): nothing that is not already a package convention.

The review found no proof issue: no hole, no axiom outside the policy, no
heartbeat override.

### Golf, on a scratch copy

Following `commands/golf.md` and the `proof-golfer` agent's no-LSP fallback,
each candidate was applied to a copy of the package, recompiled with
`lake env lean`, and kept only if the file compiled; the whole package was
rebuilt at the end. Eleven edits were accepted:

| Where | Change |
| --- | --- |
| `Lifo.newer` | a two-line `have := ...; simpa using this` becomes `by simpa using hs.subset ...` |
| `dropEvents_allCopy` | a `cases`-on-`Bool` block becomes `Bool.eq_false_iff.mpr fun hsd => ...`; its identical `.enum` and `.array` branches are merged, and the `simp only [dropEvents]` before `exact` dropped |
| `Config.Ordered.keep`, `Config.Ordered.push` | `rcases` blocks become `Or.elim` terms |
| `dropLocs_dropEvents` | `by simp only [dropEvents]; exact t` becomes `t` |
| `step_drop_order` | the identical `loopIter`/`brk` and `callReturn`/`ret` cases each merged into one `case` with two tags; an `rcases` becomes `Or.elim` |
| `step_ordered` | `loopEnter`'s `rcases` becomes a term; a single-use `have` inlined |

| Measure | Before | After |
| --- | --- | --- |
| Lines | 1439 | 1414 (25 fewer, 1.7%) |
| Rebuild, three runs | 1.42, 1.29, 1.43 s | 1.29, 1.30, 1.39 s |
| Tactic execution (profile) | 1.67 to 1.69 s | 1.70 to 1.73 s |
| Heartbeat floor | 6000 builds, 5000 fails | unchanged |
| Options | none | none |
| Axioms | 51 `propext`, 21 `propext` + `Quot.sound`, 2 none | unchanged |

The time and heartbeat differences are noise: the module is cheap, and golf
helped readability, not speed. Six of the eleven edits sit at
`find-exact-candidates` anchors; the other five were found by reading the
case analyses the scripts do not inspect. The edits are not in this change:
proof simplification belongs to RUE-2471.

One candidate was rejected, and it is the calibration's main lesson. Replacing
the proof of `range'_increasing` by core's `List.pairwise_lt_range'` compiles,
is shorter and more direct, and passes every golf rule. It also adds
`Classical.choice` to `range'_increasing`, `reachable_ordered` and
`drop_order`, which the declaration's own doc-comment warns about. Golf has no
axiom step, and the plugin's axiom checker would pass the result. So every
golf or lemma replacement in this package is followed by our axiom gate.

Beyond golf, the anchors at `reachable_ordered` and `reachable_nested` point
at a refactor: both, and `steps_live` in `Retire.lean`, prove "a property every
`Step` preserves holds along `Steps`" by the same six-line induction. One
shared lemma beside `Steps` would replace the three (RUE-2471's scope).

## What the loop adopts, by role

| Role | Uses | Instead of the plugin's |
| --- | --- | --- |
| Implementer | blocked-goal triage (`sorry-filling.md`) for a goal that resists; prove's rules: no statement or header change (route it back to the coordinator), at most three candidates per attempt, a goal is stuck after the same failure twice, stage only the touched files; `diagnose`'s error table (`compilation-errors.md`) before any `lake clean` | interactive cycle prompts, `--commit=ask`, deep mode, checkpoint commits |
| Reviewer | the review procedure by hand: `lean4-skills-sorry-analyzer <files> --format=json --report-only`, `lean4-skills-find-golfable <files> --filter-false-positives`, `lean4-skills-find-exact-candidates <files>`, proof-size metrics; then our axiom gate | the plugin's axiom checker and its standard-axiom list |
| Simplification (RUE-2471 and later) | the golf policy and scoring order, one edit at a time on a compiling tree, recompile after each; our axiom gate after any lemma replacement; before-and-after line count, rebuild time and heartbeat floor as above | parallel golfer subagents, bulk `sed` rewrites |

Our axiom gate is `lake exe ruecore-lint` (every declaration, by allow-list)
and a regenerated `TRUST.md` diffed against the committed copy, both run by
`bin/chain.sh`; for a quick look at one module, a scratch file that imports it
and runs `#print axioms` on each fully qualified name.

## What we decline, and why

- The plugin's axiom checker as a gate: misses every declaration after a
  `mutual` block, and allows `Classical.choice`.
- Mathlib search, Layer 2 at full strictness, the mathlib naming and width
  rules: not a Mathlib project.
- `autoprove`, `autoformalize`, deep mode and parallel golfer subagents: the
  loop already bounds agents (two lanes) and reviews every change; statement
  work goes through the Spec layer.
- `checkpoint`: the loop's commit rules and `bin/chain.sh` cover it, with a
  stronger axiom check and the kernel re-check.
- The lean-lsp MCP server: optional. The workflows run without it here; add it
  later if goal-state inspection in the implementer becomes the bottleneck.

## Guardrails

The plugin's `PreToolUse` hook screens each shell command. It activates only
when the command's working directory is inside a Lean project (a directory with
`lakefile.lean`, `lakefile.toml` or `lean-toolchain` at or above it), so here
only when the shell's directory is `docs/formal/lean` or below. A command run
from the repository root that reaches the package through `git -C`, an
absolute path or a `cd` inside the command is not screened. Verified by feeding the hook recorded payloads:

| Command | Inside `docs/formal/lean` |
| --- | --- |
| `git push --force`, `--force-with-lease`, `--mirror`, `--delete` | blocked, not bypassable |
| `git reset --hard`, `git clean -f`, `git checkout .`, `git restore .` | blocked, not bypassable |
| `git checkout -- <path>`, `git checkout --theirs <path>`, `git restore <path>` | blocked unless prefixed with `LEAN4_GUARDRAILS_BYPASS=1` |
| a `lean4-skills-*` script with stderr sent to `/dev/null` | blocked, not bypassable (`lake` itself is not screened) |
| `git push`, `git commit --amend`, `gh pr create` | allowed (default policy `host` defers to the agent's own permission rules) |
| `git rebase`, `git reset --keep`, `git stash` | allowed |

How they meet ours:

- No `sorry`, no `native_decide`, axioms exactly `propext` and `Quot.sound`:
  the plugin enforces none of these (it reports `sorry` and treats
  `Classical.choice` as standard). Our gates stay in force; nothing conflicts.
- Never check out over uncommitted work, resolve rebase hunks rather than whole
  files: the guardrails enforce the same rule on paths.
- A rebased branch needs a force push (`--force-with-lease`). The hook reads
  `LEAN4_GUARDRAILS_DISABLE` from its own environment, not from the command, so
  the `LEAN4_GUARDRAILS_DISABLE=1 git push ...` prefix its message suggests
  is still blocked when the hook is active. Run the push, after a rebase only,
  from a shell whose directory is the worktree root (outside the package),
  with `git -C <worktree>`; there the hook does not activate.

## Other agents (Codex)

The plugin's scripts are plain Python 3 and Bash; they need no Claude Code, no
LSP and no Mathlib, and run standalone from `docs/formal/lean`, either through
the `bin/lean4-skills-*` wrappers or as `lib/scripts/*` in a clone of the
plugin repository:

| Script | Standalone here |
| --- | --- |
| `sorry_analyzer.py` | yes |
| `find_golfable.py`, `find_exact_candidates.py`, `analyze_let_usage.py` | yes |
| `project_context.py`, `preflight_env.sh --codex` | yes |
| `find_usages.sh` | yes (plain `grep` without ripgrep) |
| `check_axioms_inline.sh` | runs, but see above; use `ruecore-lint` and `TRUST.md` |
| `unused_declarations.sh` | needs a ripgrep binary |
| `minimize_imports.py` | does not read module-system imports |
| `search_mathlib.sh`, `smart_search.sh` | no (need Mathlib or network search services) |

The workflow procedures are Markdown (`commands/*.md`,
`skills/lean4/references/*.md`) and read the same for any agent. Codex can also
install the plugin natively; its hooks are advisory there. A Codex checkpoint
reviewer runs the review procedure by hand, as above, and the package's gates.

## Re-running the calibration

From `docs/formal/lean`, on a copy of the package for anything that edits:

```bash
lean4-skills-sorry-analyzer RueCore/TraceOrder.lean --format=json --report-only
lean4-skills-find-golfable RueCore/TraceOrder.lean --filter-false-positives
lean4-skills-find-exact-candidates RueCore/TraceOrder.lean
lake env lean -Dprofiler=true RueCore/TraceOrder.lean
lake env lean -DmaxHeartbeats=6000 RueCore/TraceOrder.lean
lake exe ruecore-lint
```
