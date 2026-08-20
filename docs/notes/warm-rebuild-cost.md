# Warm rebuild cost

Status: measurement and design note, 2026-08-18. This note records the
maintained-program warm-edit campaign for RUE-1578. It complements the
fresh-process measurements in ADR-0067 and the retained-session contract in
ADR-0068; it does not replace either one.

## Result

The invalidation cone is already precise. The warm cost is dominated by
revalidation after publishing a new filesystem observation, including when no
source bytes changed.

- Re-observing an unchanged Lattice tree costs 28–62% of a cold build. The
  Lattice run retired 2,313,419,205 instructions against 8,163,389,096 for the
  cold build; chain256 retired 330,301,504 against 622,366,896.
- A real edit adds little beyond that floor. On chain256, the measured spread
  from no edit to adding a struct field was 53.1% to 56.3% of a cold build;
  changing one function body added 2.2 percentage points.
- A body edit in Lattice's `model.rue` hub, imported by 123 of 131 modules,
  recomputed one CFG unit, one codegen unit, and one object projection out of
  1,280. Comment-only edits and new unreachable functions recomputed none.
- The same body edit issued 23,585 validation demands. 23,440 (99.4%) found a
  retained terminal reusable. A no-op issued 25,323 demands, all reusable.
- Warm output was byte-identical to a cold build of the same final source in
  all 71 samples, at one and four workers.

The warm cost is linear in program size for a constant edit. Moving from 256
to 1,024 modules multiplied the warm cost of the same one-line edit by 4.45x,
while the edit still caused one backend unit of recomputation.

## What the measurement isolates

Re-requesting an artifact at the revision already held by the session costs
zero refused certificates for all measured edit shapes. Inserting a
`reobserve()` that changes no bytes causes the identical request to refuse
25,323 certificates.

That makes this a publishing issue rather than an invalidation or validation
issue. ADR-0073 deliberately gives each independently observed import view a
new certificate epoch. `publish_revision` therefore mints a fresh epoch, and
`extends_for_certificate` requires epoch equality, so the retained
certificates become unusable together even though the source content and the
requested artifacts are unchanged. This behavior follows ADR-0073's current
decision table; its retained-session cost was intentionally left for later
measurement.

## Candidate directions

The measurements do not justify an implementation without an ADR amendment.
The candidates are:

1. Qualify certificates by changed input leaves instead of by the whole
   observation epoch. This preserves exact reuse when a certificate's inputs
   are outside the changed cone, but requires a precise rule for import-view
   changes and removals.
2. Introduce durability tiers over input leaves. Stable leaves could retain
   certificates across an observation while volatile or changed leaves would
   force revalidation. The tier boundary would become part of the certificate
   contract.
3. Suppress revision publication when re-observation proves that no accepted
   input changed. This is the smallest policy change for no-op rebuilds, but it
   must specify how failed, absent, and newly discovered inputs affect the
   decision.

All three need the warm/fresh correctness harness from RUE-1086 before they
can change production behavior. None is implemented by this note.

## Measurement boundary

The campaign used Lattice single-file edits of different shapes and chain
fixtures at two sizes, with the existing unstable compiler counters and
instruction attribution. The workload generator and large chain fixture are
scratch tooling rather than suite members: adding either to the committed
ADR-0067/ADR-0068 workload contract would be a separate suite-revision
decision.

The note records the observed result and the design boundary. A follow-up ADR
must choose whether the certificate invariant changes before any optimization
is attempted.
