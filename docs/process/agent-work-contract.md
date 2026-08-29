# Agent Work Contract (proposal)

> **Status: proposal for review by Steve and Dorian.** This is a deliberately
> small, manual experiment for Codex and Claude. It does not add hooks, scripts,
> skills, or tracker automation.

## Purpose

AI agents can make a bounded change quickly, but coordination fails when intent,
decisions, implementation, and evidence are copied into competing notes. This
proposal defines a shared contract for an agent taking an issue implementation
or risk-appropriate adversarial-review handoff and returning it to a
coordinator. It favors one authoritative location for each fact and thin
adapters around those facts; it is not a rule for every routine delegation.

The contract is tool-neutral: Codex, Claude, or a future assistant may satisfy
it through its own interface. `AGENTS.md` controls when sources or instructions
conflict. Existing process documents and assistant commands are useful context
but may be historical adapters; inspect them against current source, tests, and
the applicable `AGENTS.md` rather than treating every statement as current.

This proposal borrows the AI-native SDLC playbook's modular adoption, explicit
gates, and one-source-of-truth principles without adopting its complete artifact
or automation model. The starting point is the [AI-Native SDLC Playbook](https://claude.com/blog/the-ai-native-sdlc-playbook);
Rue's existing authorities and failure history determine the local shape.

## Authoritative source map

Each source owns a different kind of fact. An agent may summarize a fact in a
handoff, but the summary must point back to the owner and must not become a
second authority.

| Fact | Authority | What an agent should do |
| --- | --- | --- |
| Repository-wide agent contract and guardrails | `AGENTS.md` | Read the applicable file before acting; it controls when instructions conflict, including safety, architecture, testing, and publication rules. |
| Intent, acceptance criteria, ownership, dependencies, and status | Linear issue | Read the issue and comments; keep scope and state there. |
| Current implementation behavior and proof | Current source and tests | Inspect call sites and relevant tests; use them to prove what the repository does now, and report gaps rather than relying on historical prose. |
| Architectural or language decisions | Accepted ADRs and the specification | Treat these as design and language authorities; they do not by themselves prove the current implementation. Escalate conflicts instead of choosing silently. |
| Implementation change and review record | GitHub pull request | Keep the diff, review findings, and requested revisions here; the PR is a change/review record, not a replacement for issue intent or source/test proof. |
| Build, test, benchmark, and generated outputs | CI checks and their artifacts | Report the command/check and result; preserve the artifact link or path where available. |

Tool adapters (MCP tools, commands, slash commands, or UI integrations) stay
thin: they locate or update the authoritative source and do not create a
parallel plan, tracker, decision record, or evidence database.

## Cross-agent task contract

For an issue implementation or risk-appropriate adversarial review, use this
small, copyable exchange. Prefer the task conversation or a Linear comment for
the full contract. The issue and PR remain the durable record; a PR description
should stay tool-neutral and concise, with the full exchange linked or recorded
in the task conversation or Linear comment. Use a Linear comment when the next
tool cannot access the originating task conversation.

```text
Objective: one sentence describing the user-visible or repository outcome.
Scope: outcome, subsystem, anticipated files, and explicit non-goals; name anything intentionally left alone.
Authority: Linear issue; applicable AGENTS.md; ADR/spec sections; related PR or artifact.
Current source/tests proof: paths, call sites, and tests establishing the current behavior or gap.
Constraints: design decisions, compatibility, safety, platform, and publication limits.
Deliverable: files or review result expected from this task.
Verification: focused checks, broader checks when warranted, and expected evidence.
Expected terminal condition: what must be true at handoff and, if applicable, after publication.
Handoff: changed files, rationale, tests and results, known risks, blockers, and open questions.
```

Scope is defined primarily by outcome, subsystem, and non-goals; the files list
is an anticipated route, not a promise that no other in-scope file can change.
The implementation agent may make ordinary in-scope fixes and reasonable
assumptions, but must note them in the return. Material scope expansion,
unresolved design choices, or a change in the expected outcome requires the
coordinator's decision before proceeding.

One coordinator sends the brief to one implementation agent for an issue
implementation. The coordinator owns integration, publication, and cleanup.
Add one independent adversarial reviewer only when the risk warrants it (for
example, architectural, cross-phase, unsafe, security-sensitive, or unusually
risky work); the reviewer does not become a second implementation owner. The
receiving coordinator checks the return against the contract and authoritative
sources before acceptance. A handoff is not acceptance: review and required CI
gates still apply.

## Lifecycle and gates

1. **Intake.** Confirm the Linear issue, owner, acceptance criteria, and
   applicable repository instructions. If intent or a design decision is
   missing, stop at the issue/ADR discussion rather than inventing one.
2. **Brief.** Send the task contract above, including the smallest useful scope
   and the evidence expected at handoff.
3. **Work.** Inspect current source and preserve unrelated changes. Update only
   what the scoped outcome requires, including ordinary in-scope fixes. Note
   reasonable assumptions and unanticipated files in the return; stop for a
   material expansion. Use existing skills, commands, and process documents as
   aids, not as new authorities.
4. **Verify.** Run the focused check first, then proportionate formatting,
   documentation, tests, or artifact checks. Record failures and their
   interpretation, including when a failure is a harness or environment issue.
5. **Review.** Return the handoff, then have the receiving agent or maintainer
   review the diff against the issue, ADR/spec, and evidence. Resolve blockers;
   do not hide them in a summary.
6. **Publish and close.** The coordinator owns the PR, CI, merge, Linear
   transition, and cleanup, unless the user explicitly assigns that ownership
   elsewhere. Unless the user narrows the request, Rue's default terminal
   condition is: the PR is merged, Linear is closed, the source branch is
   deleted, upstream has been fetched with the checkout's native VCS, and the
   working copy is clean and based on the updated upstream `trunk`. Never treat
   an open PR or green local run as proof of completion.

The gates are intentionally human-checkable:

- **Scope gate:** objective and non-goals are clear in the issue/brief.
- **Authority gate:** every design or semantic choice has an identified source,
  or an explicit review question.
- **Evidence gate:** the handoff names checks and their actual results.
- **Review gate:** a second reading confirms the diff and acceptance criteria.
- **Publication gate:** the owner verifies CI, merge state, tracker state, and
  cleanup required by the issue.

## Skills, checks, and workflows

Keep three layers separate:

- **Skills** carry judgment and institutional knowledge: how to interpret an
  ADR, choose proportionate tests, recognize a harness failure, or escalate a
  design conflict. They should explain decisions and point to source material.
- **Scripts and checks** perform deterministic mechanics: validate a file shape,
  enumerate expected jobs, format files, or compare an output against a known
  contract. They should be narrow, reproducible, and fail with actionable
  evidence.
- **Workflows and hooks** sequence stable practices across events. They should
  be considered only after the manual pilot demonstrates a repeated, agreed
  behavior and a clear owner for false positives and exceptions.

Do not encode unsettled judgment in a check, and do not use a workflow to paper
over an unclear authority boundary.

## Ten-issue manual pilot

Run this contract manually on ten ordinary issues spanning documentation,
compiler, CLI, test, and cross-platform work. Steve and Dorian select the ten
issue IDs in advance. Use one ordinary Linear issue as the authoritative pilot
record: preregister the ten IDs, definitions, baseline/comparison method, and
the issue-3 and issue-10 decision points there. Record each issue's observations
in that issue's existing conversation or PR as appropriate, then summarize the
aggregate in the pilot record. Create no separate pilot tracker, database, or
per-issue contract files. For each issue, save the full contract in the task
conversation or a Linear comment; keep any PR description concise and
tool-neutral.

Choose a mix that includes at least two issues with an ADR/spec boundary, two
with platform or CI evidence, one documentation-only change, one likely
harness-failure investigation, and one coordinator-to-one implementation-agent
handoff. Add an independent adversarial review only if that issue's risk calls
for it. The composition categories may overlap. The remaining issues should be
representative day-to-day work rather than carefully staged demonstrations. Do
not add automation during the pilot.

### Pilot measurement record

Before issue 1, record the ten IDs and these definitions in the single Linear
pilot record. The baseline cohort is the ten most recent completed Rue
issue-level Codex or Claude handoffs before the pilot for which the task, Linear,
or PR record is inspectable. Apply that rule mechanically rather than selecting
examples for similarity or outcome. If fewer than ten records satisfy it, the
pilot cannot reach an adoption decision at issue 10; extend or revise the study
until ten baseline records exist.

The primary outcome is the number of issues with at least one coordination
rework event. A coordination rework event is a clarification round or reverted
edit caused by a missing, ambiguous, contradictory, or misrouted contract fact.
Compare that single count for the ten pilot issues with the count for the ten
baseline issues. The other measurements are secondary safety and usability
evidence rather than inputs to a composite score.

- **Contract completeness:** required brief and return fields present at intake
  and handoff.
- **First-pass contract acceptance:** the coordinator finds no missing,
  contradictory, or out-of-scope contract element on the first review. This
  excludes technical review findings about the implementation itself.
- **Coordination rework:** whether the issue had at least one event under the
  primary-outcome definition above; also record the number of events as
  descriptive detail.
- **Verification quality:** focused check run, actual result recorded, and
  artifact or output findable.
- **Handoff time:** elapsed time from the brief being sent to the coordinator
  marking the return review-ready; omit timing rather than infer it when either
  timestamp is unavailable.
- **Incidents:** unintended tracker/publication changes, missed paired work, or
  misclassified test/harness failures.

### Evaluation categories

Record observations against categories grounded in Rue coordination failures:

| Category | Failure signal to look for | Historical reminder |
| --- | --- | --- |
| Authority and scope | An agent follows a stale note, invents a design, or edits outside the requested boundary. | Canonical-session and root-module rules in the repository contract; RUE-767. |
| Environment and baseline | A check passes under a newer local tool but fails on the supported baseline. | Python 3.11 versus the 3.9 floor (RUE-1509); Bash 4+ constructs on macOS (RUE-1506). |
| Syntax and validation interpretation | A parser or validator reports the wrong class of problem, or a real syntax error is waived. | Unbalanced shell quoting (RUE-1511) and the extglob parse exception (RUE-1512). |
| Harness versus product diagnosis | An oracle/test harness gap is reported as a compiler defect, wasting a CI round. | The registered oracle model gap guidance (RUE-1711). |
| Cross-target and shared-policy coverage | One backend or consumer is updated while its paired implementation or canonical path is missed. | ADR-0048’s shared codegen boundary and its paired-backend review checklist. |
| Tracker and publication safety | A link changes an issue unexpectedly, or a task is called complete before merge/cleanup is verified. | Accidental reopening through `refs` in PR #2318; merge-queue verification rules. |
| Evidence and handoff quality | The next agent cannot reproduce the result, distinguish a blocker from a warning, or find the artifact. | CI/artifact and diagnostics contracts in `docs/process/`. |

These are observation categories, not a scoring system for agents, and a single
observation may belong to more than one category. Capture a short example, the
source consulted, and the correction or time lost.

## Metrics and decision gates

Collect the definitions above manually for each pilot issue, using the issue or
PR as the local record and the single Linear pilot issue for the aggregate.

Use the observations to make an explicit decision:

- **After issue 3 — continue, revise, or stop.** Record the first three issues'
  primary and secondary observations in the pilot issue. Do not draw a baseline
  comparison from the incomplete pilot cohort. Continue only if the contract is
  understandable and no safety or tracker incident has appeared. Revise the
  wording if completion repeatedly requires coaching; stop if it causes an
  unsafe or materially confusing workflow.
- **After issue 10 — adopt, revise/extend, or reject.** Adopt the manual contract
  only if fewer pilot issues than baseline issues required coordination rework,
  at least 8 of 10 handoffs are complete on first contract review, every issue
  has reproducible evidence, and there are zero unintended tracker/publication
  incidents. Record each count, the secondary evidence, and the decision in the
  pilot issue. Revise or extend if the baseline is incomplete, the primary count
  improves but a secondary threshold or usability is missed, or the result is
  otherwise inconclusive. Reject if the complete cohorts show no reduction in
  issues requiring coordination rework, or if the contract introduces repeated
  false authority/evidence claims.

Only after adoption should the maintainers decide whether any stable mechanic
belongs in a script/check, skill, workflow, or hook. That follow-up is a new
decision and is outside this proposal.

## Non-goals

- No per-issue intent, specification, or plan files.
- No duplicate tracker, pilot database, or new status system.
- No immediate hooks, workflows, or automation.
- No vendor-specific source of truth; Codex and Claude are adapters to the same
  repository sources.
- No replacement of Linear, ADRs, the specification, GitHub review, or CI.
- No implementation of skills, workflows, or deterministic checks in this
  proposal.

## Open review questions

- Are the authority boundaries and publication gate clear enough for both Codex
  and Claude without adding tool-specific instructions?
- Are the pilot composition and adoption thresholds small enough to run during
  normal work, while still exposing the historical failure categories?
- Which, if any, manual behavior should be considered for a later skill or
  deterministic check after the pilot?
