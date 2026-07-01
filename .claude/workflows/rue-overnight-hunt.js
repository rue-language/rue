export const meta = {
  name: 'rue-overnight-hunt',
  description: 'Autonomous Rue compiler bug hunt for one subsystem area: fan-out finders, verify with repros, return findings to file in Linear',
  phases: [
    { title: 'Find', detail: 'finders sweep the area in parallel' },
    { title: 'Verify', detail: 'each finding re-checked with a runnable repro' },
  ],
}

// ---------------------------------------------------------------------------
// Parameters (via Workflow args): { area: <key>, angles?: <string>, finders?: <n> }
// The driver passes an area key; angles are looked up from AREAS below (or
// overridden via args.angles). Goal: MAXIMIZE capture. Duplicates are fine —
// the project would rather over-capture than miss bugs. Keep the one hard
// rule: every filed finding must have a repro that was actually RUN.
// ---------------------------------------------------------------------------

// Args may arrive as an object or as a JSON string depending on the caller;
// handle both so area targeting is reliable for unattended runs.
const A = typeof args === 'string' ? JSON.parse(args || '{}') : (args || {})
const area = A.area || 'general'
const NUM_FINDERS = A.finders || 4

// Per-area hunting angles. Each round targets ONE area so finders go deep.
// Areas marked "(swept <date>)" were covered by the 2026-06-09 hunt — the
// coverage ledger rotates through the UNSWEPT ones first.
const AREAS = {
  'opt-correctness':
    'OPTIMIZATION CORRECTNESS — the biggest untested seam (we only ever test -O0). For many small programs, compile the SAME source at -O0 and again at -O1/-O2/-O3 and DIFF the runtime output+exit code; any divergence is a miscompile. Target rue-cfg optimization passes (constant folding, dead-code elimination, copy propagation, branch simplification), x86_64/aarch64 peephole.rs, and schedule.rs. Hunt: const-fold that changes overflow-panic behavior, DCE that drops a side-effecting call or a @dbg, copy-prop across a clobber, peephole that breaks flags before a conditional jump, scheduling that reorders a div past its zero-check. Also -O on the known-broken aggregate paths.',
  'parser-lexer':
    'PARSER & LEXER — numeric literal parsing (hex 0x, binary 0b, underscores, leading zeros, literals at/over type max, i64::MIN), string/char escapes (\\n \\t \\x \\u, unterminated, embedded NUL), comment handling (nested? unterminated block), operator precedence & associativity (mixed arithmetic/bitwise/comparison/logical — does codegen match the parse tree?), error recovery (does a syntax error produce a sane message or cascade/ICE?), Unicode identifiers, deeply/unusually nested expressions (bounded — there is a known stack-overflow RUE-42, do not re-file that).',
  'pattern-matching':
    'PATTERN MATCHING — match exhaustiveness soundness (is a non-exhaustive match ever accepted? does a missing arm fall through to garbage?), unreachable/duplicate arms, wildcard placement, match on each scalar type incl bool and all int widths (note RUE-27: i64 match compares low-32 — find DISTINCT issues), nested patterns, bindings in patterns, guards if supported, match as an expression producing a value used downstream.',
  'drops-destructors':
    'DROP / DESTRUCTOR SEMANTICS — run order (struct fields, locals reverse-declaration), drop on early return / break / continue / every branch of if/match, double-drop, drop of a partially-moved value, drop of a value moved into a callee (should NOT double-drop), drop inside loops, @drop user destructors firing exactly once on all paths, @drop on @copy types. Use a String (heap) or a struct with an observable @drop to detect double/missing drops.',
  'comptime-consteval':
    'COMPTIME / CONST EVALUATION — overflow in const arithmetic (comptime { 2147483647 + 1 } — panic or wrap or wrong?), division/modulo by zero at comptime, const propagation correctness, comptime evaluation of each operator at each width, comptime if/recursion, comptime type construction edge cases, large/negative comptime values, mismatch between comptime-evaluated and runtime-evaluated results of the same expression.',
  'regalloc-spilling':
    'REGISTER ALLOCATION & SPILLING — generate high register pressure (many simultaneously-live locals, deep expression trees, many function args), verify values survive spills/reloads correctly, callee-saved register preservation across calls, correctness around div/mod (fixed rdx/rax) and shifts (fixed cl) under pressure, large functions, values live across many calls. Compare -O0 vs -O2 here too. Both x86_64 and aarch64 (cross-compile + read asm for aarch64).',
  'type-inference':
    'TYPE INFERENCE (Hindley-Milner) — integer-literal defaulting (untyped literal used in two incompatible contexts), inference through function returns, struct fields, arrays; annotation-vs-inference disagreements; whether an ill-typed program is ever accepted (unsound) or a well-typed one rejected; inference with comptime type params; literal range checks vs inferred type.',
  'control-flow':
    'CONTROL FLOW — nested loops, break/continue (labeled if supported), loops where all paths return/diverge, the never type (!), unreachable code after return/break, if/else producing values, while-let or loop-as-expression if supported, very wide if/else-if chains, empty loop bodies, infinite loop the optimizer might wrongly remove.',
  'arrays-aggregates':
    'ARRAYS & AGGREGATES — multidimensional arrays, arrays of structs, struct containing arrays, array element assignment/read at boundaries (note RUE-22/23 place-ops bugs — find DISTINCT issues), large arrays (stack layout), array copies/moves and whether all elements survive, array literals with side-effecting elements, indexing with computed/wide indices, nested field+index projections.',
  'linker-elf':
    'LINKER / ELF — symbol resolution across multi-file compilation, duplicate-symbol detection, relocation correctness (large offsets, many symbols), archive handling, large binaries / many functions, string-table and section correctness, the runtime-archive linking path. Mostly compile real multi-file programs and check the produced binary runs correctly; inspect with readelf/objdump where useful.',
}

const angles = A.angles || AREAS[area] || 'Look broadly for any Rue compiler correctness bug.'

const COMMON = `
You are hunting for REAL bugs in the Rue compiler (Rust, Buck2 build).

Build and run a Rue program — first go to the repo root, then use scripts/rue-bin for a stable binary path:
  cd "$(jj root)"                          # portable: repo root regardless of checkout location
  RUE="$(scripts/rue-bin)"                 # absolute path; builds if needed, instant if already built
  printf 'fn main() -> i32 { 0 }\\n' > /tmp/hunt_${area}_N.rue
  "$RUE" /tmp/hunt_${area}_N.rue /tmp/hunt_${area}_N_out
  timeout 10 /tmp/hunt_${area}_N_out; echo "exit: $?"     # ALWAYS timeout-wrap — programs may loop forever
Use a unique N per repro file so parallel finders don't collide.

SHELL SAFETY (a permission firewall reviews every command; one flagged command STALLS the entire run waiting for a human who may be asleep): never build an rm/mv/redirect-truncate target from shell variables — \`rm $DIR/$f.out\` is flagged "dangerous on possibly-empty variable". Use literal /tmp paths, or empty-guards (\`rm -f "\${DIR:?}/\${f:?}.out"\`) if a variable is unavoidable. Best: don't delete scratch files at all — overwrite or leave them; /tmp is disposable. Never rm inside the repo checkout.

Optimization-bug pattern (diff outputs across opt levels):
  "$RUE" -O0 s.rue o0 && "$RUE" -O2 s.rue o2
  a="$(timeout 10 ./o0; echo $?)"; b="$(timeout 10 ./o2; echo $?)"; [ "$a" = "$b" ] || echo "MISCOMPILE: O0=$a O2=$b"
Inspect IR when useful: "$RUE" --emit air|cfg|mir|asm s.rue   (add --target aarch64-macos to read ARM asm)

WHAT COUNTS as a finding: wrong compiled-program output, compiler crash/ICE/abort (SIGABRT/signal) on valid-looking input, an accept-invalid soundness hole (program that should be rejected compiles), a reject-valid (program that should compile is rejected), or memory unsafety. IGNORE style, performance, and diagnostic-wording.

HARD RULE: every finding you report MUST include a minimal .rue repro that you actually WROTE and RAN, with the exact observed behavior (stdout, exit code, any panic text). No repro, no report. Capturing volume is good — report every distinct issue you can demonstrate — but each must reproduce.

Spec for "correct" behavior lives in docs/spec/src/ (relative to repo root) — cite it when a behavior contradicts the spec.
`

const FINDINGS_SCHEMA = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          title: { type: 'string', description: 'one-line bug title' },
          severity: { type: 'string', enum: ['miscompile', 'unsound', 'ice', 'reject-valid', 'accept-invalid'] },
          file: { type: 'string', description: 'implicated compiler source file:line if known, else empty' },
          description: { type: 'string', description: 'what is wrong and the mechanism if known' },
          repro: { type: 'string', description: 'the minimal .rue source you ran' },
          observed: { type: 'string', description: 'exact observed behavior: stdout / exit code / panic text' },
          expected: { type: 'string', description: 'correct behavior (cite spec if applicable)' },
        },
        required: ['title', 'severity', 'description', 'repro', 'observed', 'expected'],
      },
    },
  },
  required: ['findings'],
}

phase('Find')
log(`hunting area "${area}" with ${NUM_FINDERS} finders`)
const found = await parallel(
  Array.from({ length: NUM_FINDERS }, (_, i) => () =>
    agent(
      `${COMMON}\n\nYOUR FOCUS AREA: ${area}\n${angles}\n\nYou are finder #${i + 1} of ${NUM_FINDERS} on this area — explore a DIFFERENT angle/sub-area than a sibling would, to maximize coverage. Report every distinct, reproduced bug you find (aim for breadth; 0 is fine if the area is clean, but dig hard first).`,
      { label: `find:${area}:${i + 1}`, phase: 'Find', schema: FINDINGS_SCHEMA }
    )
  )
)

const all = found.filter(Boolean).flatMap((r) => r.findings)
log(`${all.length} raw findings in area "${area}"`)

phase('Verify')
const VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['confirmed', 'plausible', 'refuted'] },
    evidence: { type: 'string', description: 'what you ran and what happened on current trunk' },
    title: { type: 'string', description: 'refined one-line title' },
    severity: { type: 'string', enum: ['miscompile', 'unsound', 'ice', 'reject-valid', 'accept-invalid'] },
    repro: { type: 'string', description: 'minimized repro that reproduces' },
    observed: { type: 'string' },
    expected: { type: 'string' },
    file: { type: 'string' },
  },
  required: ['verdict', 'evidence', 'title', 'severity', 'repro', 'observed', 'expected', 'file'],
}

const verified = await parallel(
  all.map((f, i) => () =>
    agent(
      `${COMMON}\n\nVerify this finding from area "${area}". Write its repro to /tmp/verify_${area}_${i}.rue, compile and run it on CURRENT TRUNK, and report whether the claimed bug actually occurs. Mark "confirmed" if you reproduce it, "plausible" if you can't fully reproduce but the mechanism looks real and worth filing, "refuted" if it's actually correct behavior (check docs/spec/src/). Minimize the repro. The project wants high capture, so prefer "plausible" over "refuted" when genuinely unsure — but never confirm something you couldn't run.\n\nFINDING:\n${JSON.stringify(f, null, 2)}`,
      { label: `verify:${area}:${i}`, phase: 'Verify', schema: VERDICT_SCHEMA }
    )
  )
)

// Keep everything not actively refuted (max capture).
const keep = verified.filter(Boolean).filter((v) => v.verdict !== 'refuted')
log(`area "${area}": ${keep.length} findings to file (${verified.filter(Boolean).filter((v) => v.verdict === 'confirmed').length} confirmed)`)

return { area, findings: keep }
