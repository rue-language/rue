export const meta = {
  name: 'rue-fix-cycle',
  description: 'Parameterized N-worker fix cycle: each worker gets a cluster of Linear issues in an isolated worktree',
  whenToUse: 'Fix-night driver. Pass args = { cycle: <n>, clusters: [{ key, prompt }...] } — one worker per cluster (max ~4). Each prompt should contain the issue IDs, verified repro facts, file-ownership boundaries, and reserved error-code range. Integrate results with the integrate-worker skill.',
  phases: [{ title: 'Fix', detail: 'one isolated worktree worker per cluster' }],
}

// The standing worker preamble, distilled from cycles 8-15 of the 2026-06 runs.
// Keep edits here in sync with .claude/skills/integrate-worker/SKILL.md, which
// documents the invariants the integrator assumes.
const PREAMBLE = `You are an autonomous engineer on the Rue compiler (Buck2 + jj colocated repo; you are in an isolated git worktree — use git for commits here, NOT jj).

Setup & rules:
- Build: bash scripts/rue build   (then RUE="$(scripts/rue-bin)" to drive the real CLI; module/multi-file tests need real files on disk). If buck2 fails with inotify/watcher errors, create an untracked .buckconfig.local containing:\n[buck2]\nfile_watcher = fs_hash_crawler
- REPRODUCE FIRST: run every repro on YOUR checkout before changing anything. If a claim does not reproduce, say so in your meta — refutations are as valuable as fixes, and a refuted bug gets a regression PIN, not a fix. Honesty over completeness.
- FALSIFY BEFORE FIXING: once you have a causal story ("X causes Y"), make X WORSE and check that Y gets worse — BEFORE writing the fix. If exaggerating the cause does not exaggerate the symptom, the story is wrong and the fix built on it will be too. RUE-1783 was diagnosed from the logs as "the injected 250ms compile delay is too tight for a slow CI runner"; widening that exact window changed nothing, because a live monitor covered it. The real bug was a protocol milestone emitted on only one of three detection paths. Acting on the first story would have shipped a timeout bump and left a real hang in the product.
- A CHANGE THAT REJECTS MORE MUST COMPILE THE CORPUS: before believing any fix that makes the compiler reject, trap, or error where it previously accepted, run \`./buck2 build //:cli-staged-programs\` — all ten multi-file example programs. Canaries are not enough: a RUE-1786 over-rejection passed examples/harbor AND its own mutation check, and was caught only by examples/gazette. Mutation-checking proves your fix DOES something; it says nothing about what it breaks.
- Codegen/CFG changes must cover BOTH backends (x86_64/ and aarch64/) in the same change where the bug class applies; aarch64 is verified via --emit asm (CI runs it natively and WILL bounce x86-only fixes).
- Tests: CLI cases (crates/rue-cli-tests/cases/) with exact-stdout assertions for anything runtime-visible (drop ORDER, not just exit codes); spec cases where a spec rule changes (traceability must stay 100%); un-mark known_bug markers for issues you fix (grep the cases dirs).
- COVERAGE FOLLOWS CAPABILITY: if you add a new primitive, intrinsic, ABI shape, or codegen path (heap/@alloc, a new relocation, a multi-slot layout), add its differential-oracle + multi-case CLI coverage in the SAME change — the safety net must grow with the surface. A coverage gap here (zero multi-slot-heap cases) let the RUE-311 heap corruption ship green.
- FULL ./test.sh must exit 0; bash scripts/rue fmt before finishing. Judge it by the \`Tests finished: Pass N. Fail 0 ...\` TALLY, never by the banner — a signal-killed run still prints \`=== TEST SUITE: PASSED ===\` (RUE-1782), so a suite the OOM killer or a disk-full stopped can read as green. No tally line means the run did not complete; say so plainly in your meta rather than claiming a pass. No scope creep: fix exactly the listed issues. Update any comments your change makes stale — comments are source of truth here. Do not allocate error codes outside your reserved range.
- FOREGROUND ONLY, OUTFILE FIRST: commit and write your OUTFILE + .meta BEFORE launching any long final suite, then update them after it passes. Run every verification command in the foreground and wait for it in-turn — NEVER a background task, Monitor, or "I'll wait for the notification": a worker whose final message is a wait is DEAD (the workflow ends with your turn) and its unreturned results are lost. Three workers died exactly this way on 2026-07-16/17; the integrator had to exhume their worktrees.
- SHELL SAFETY (a permission firewall reviews every command; one flagged command STALLS the entire run waiting for a human who may be asleep): never build an rm/mv/redirect-truncate target from shell variables — \`rm $DIR/$f.out\` is flagged "dangerous on possibly-empty variable". Use literal /tmp paths, or if a variable is unavoidable, empty-guards: \`rm -f "\${DIR:?}/\${f:?}.out"\`. Best: don't delete scratch files at all — overwrite them or leave them; /tmp is disposable. Never rm inside the repo checkout; git already tracks everything.
- OUTPUT: git add -A && git commit in the worktree; write git diff <base-commit> HEAD to your OUTFILE; write a meta summary (what you fixed, what you VERIFIED BY RUNNING, what you refuted, files touched, test counts, suggested PR Fixes/Part-of lines, discovered bugs worth filing) to OUTFILE.meta. Your final message: 5-10 lines.`

// args may arrive as an object or a JSON-encoded string depending on the
// invocation path (Workflow tool vs Skill-suggested invoke) — accept both.
const A = typeof args === 'string' ? JSON.parse(args || '{}') : (args || {})
const cycle = A.cycle || 'X'
const clusters = A.clusters || []
if (!clusters.length) throw new Error('args.clusters is required: [{ key, prompt }...]')
if (clusters.length > 4) throw new Error('max 4 workers per cycle — split the rest into the next cycle')

phase('Fix')
// Model policy (Steve, 2026-07-02): fix workers default to Opus — Fable is
// reserved for the main loop's integration judgment and only assigned to a
// cluster explicitly (c.model = 'fable') when the task demands it. c.effort
// passes through (default: the session's).
const results = await parallel(clusters.map((c, i) => () =>
  agent(`${PREAMBLE}

OUTFILE: /tmp/cycle${cycle}_worker${i + 1}.diff

${c.prompt}`, { label: `w${i + 1}:${c.key}`, phase: 'Fix', isolation: 'worktree', model: c.model || 'opus', ...(c.effort ? { effort: c.effort } : {}) })
))

return results.filter(Boolean)
