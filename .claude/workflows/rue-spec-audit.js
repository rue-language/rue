export const meta = {
  name: 'rue-spec-audit',
  description: 'Recurring spec-quality audit: drift (spec vs compiler), soundness, consistency, and prose<->formal-core correspondence',
  whenToUse: 'Run once per sprint (or before a release) to catch spec-code drift early. Returns structured findings per lens; the caller synthesizes + files Linear issues under RUE-201. Args: { lenses?: ["drift","soundness","consistency","formal"] } (default all).',
  phases: [{ title: 'Audit', detail: 'one agent per lens -> structured findings' }],
}

const A = typeof args === 'string' ? JSON.parse(args || '{}') : (args || {})
const LENSES = A.lenses || ['drift', 'soundness', 'consistency', 'formal']

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    lens: { type: 'string' },
    summary: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          location: { type: 'string' },
          kind: { type: 'string' },
          title: { type: 'string' },
          detail: { type: 'string' },
          evidence: { type: 'string', description: 'quote the rule + the program you RAN and what the compiler actually did' },
          compiler_behavior: { type: 'string', description: 'ALLOWS/REJECTS + exit/error, or N/A' },
          severity: { type: 'string', enum: ['critical', 'blocker-for-alt-compiler', 'high', 'medium', 'low'] },
        },
        required: ['location', 'kind', 'title', 'detail', 'severity'],
      },
    },
  },
  required: ['lens', 'summary', 'findings'],
}

const COMMON = `The Rue spec is docs/spec/src/ (prose, ~640 normative rules with cat= tags) and docs/formal/ (formal core). The compiler is in crates/. Build: bash scripts/rue build; RUE=$(scripts/rue-bin); write tiny programs, RUN them, use --emit ast/air/cfg. Rue = memory-safe-without-GC systems language; spine: access model (borrow/inout/sink, ADR-0037), multiplicity lattice (Copy<=Affine<=Linear, ADR-0039), second-class borrows, comptime monomorphization, unchecked escape hatch. Do NOT edit anything or file issues — return structured findings only. Cite exact rule ids; wherever you claim behavior, RUN the compiler and report what it did. Prioritize the highest-severity findings; don't pad.`

const PROMPTS = {
  drift: COMMON + `\n\nLENS: DRIFT (spec vs compiler — the recurring problem). For features that shipped recently (check git log / the recent ADRs), does the spec still match the compiler? Find: (1) normative rules that are now FALSE against the compiler (behavior changed, rule didn't); (2) compiler behavior/intrinsics/flags with NO spec coverage (grep known_symbols.rs + the intrinsic registry vs the spec's intrinsics inventory 4.13); (3) stale examples that no longer compile (RUN every code example in changed chapters). Focus where the compiler moved fastest. Return the schema.`,
  soundness: COMMON + `\n\nLENS: ADVERSARIAL SOUNDNESS. Assume the compiler faithfully implements the spec's rules AS WRITTEN. Construct a program those rules PERMIT that violates memory safety (use-after-free, double-free, read-uninitialized, out-of-bounds without trap, mutable aliasing, type confusion). Attack the move/linear rules (3.8), destructors + drop order (3.9), the access model, enum-payload move-out, partial moves, second-class-borrow escape, and the checked<->unchecked boundary. For each: name the permitting rules, give the minimal program, and RUN it — report whether the COMPILER allows the hole (critical: compiler AND spec wrong) or rejects it (high: spec weaker than the compiler). The compiler_behavior field (ALLOWS vs REJECTS) is the point. Return the schema.`,
  consistency: COMMON + `\n\nLENS: INTERNAL CONSISTENCY. Find where rules disagree with each other, their own examples, or the diagnostics: (1) rule-vs-rule contradictions; (2) example-vs-rule mismatches (RUN the examples); (3) error-code descriptions vs actual emission; (4) a term used with different meanings across chapters; (5) broken cross-references. Cite both sides; run the compiler for examples/errors. Return the schema.`,
  formal: COMMON + `\n\nLENS: PROSE <-> FORMAL-CORE CORRESPONDENCE. Read docs/formal/ fully, then the prose. Bidirectional diff: (1) judgments in the core vs their prose counterparts — flag divergences; (2) things in the core absent from prose or vice versa; (3) is the authority boundary stated (which governs when they differ)?; (4) undefined notation/metavariables in the core. Cite core-rule <-> prose-rule pairs. Return the schema.`,
}

phase('Audit')
const results = await parallel(
  LENSES.filter((l) => PROMPTS[l]).map((l) => () => agent(PROMPTS[l], { label: 'lens:' + l, schema: SCHEMA }))
)
return results.filter(Boolean)
