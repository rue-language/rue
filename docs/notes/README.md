# Notes index

One row per note in this directory. `current` means the note still describes
present reality (a live audit, policy, or reference measurement); `historical`
means it records a completed event, a point-in-time measurement, or a retired
architecture and is kept as the record of that history. Where a later document
carries the living version of the content, the last column names it.
`scripts/validate-doc-links.py` keeps this table complete and its file names
real.

| File | Records | Status | Superseded by |
| --- | --- | --- | --- |
| [adr-0076-symbol-handle-ordered-use-audit.md](adr-0076-symbol-handle-ordered-use-audit.md) | ADR-0076 Phase 1: the complete inventory of ordered and value-bearing symbol-handle uses, their conversions, and what a revision-shared interner still owes. | current | — |
| [adr-0071-phase-1-reference-baseline.md](adr-0071-phase-1-reference-baseline.md) | The first complete `fresh_source_to_native_v1` reference measurements closing ADR-0071's measurement phase. | current | — |
| [adr-0071-horizontal-vertical-ownership-reaudit.md](adr-0071-horizontal-vertical-ownership-reaudit.md) | Current-source whole-pipeline ownership re-audit after the accepted ADR-0071 and RUE-1510 work, with the next measured verticals. | current | — |
| [adr-0071-phase-2-semantic-cfg-ownership-audit.md](adr-0071-phase-2-semantic-cfg-ownership-audit.md) | Point-in-time Phase 2 audit (2026-08-13) of semantic-to-CFG ownership before the accepted payload-sharing and packed-artifact work. | historical | [adr-0071-horizontal-vertical-ownership-reaudit.md](adr-0071-horizontal-vertical-ownership-reaudit.md) |
| [adr-0071-phase-3-linux-compiler-allocator.md](adr-0071-phase-3-linux-compiler-allocator.md) | The Linux compiler-host mimalloc selection, hermetic Zig native-build policy, runtime-settings rationale, and the update/rollback procedure. | current | — |
| [body-analysis-cfg-incrementality-audit.md](body-analysis-cfg-incrementality-audit.md) | RUE-720 completion audit of the retired durable body/CFG import architecture that preceded ADR-0063. | historical | [post-adr-0063-cold-compiler-architecture-audit.md](post-adr-0063-cold-compiler-architecture-audit.md) |
| [canonical-query-completion-audit.md](canonical-query-completion-audit.md) | The canonical compiler boundary after RUE-720 and the RUE-627 completion decision, pre-ADR-0063. | historical | [post-adr-0063-cold-compiler-architecture-audit.md](post-adr-0063-cold-compiler-architecture-audit.md) |
| [compiler-worker-scaling.md](compiler-worker-scaling.md) | Measured worker-scaling curve for the compiler on four shapes, the phase-level efficiency behind it, and why additional workers currently cost rather than buy. | current | — |
| [compact-layout-default.md](compact-layout-default.md) | The 2026-07-19 cutover (RUE-987) making ADR-0052's compact physical layout Rue's only memory representation, and what observably changed. | historical | ADR-0052 and the specification hold the living layout rules |
| [ffi-abi-conformance-audit.md](ffi-abi-conformance-audit.md) | RUE-738 audit of Rue's two calling conventions and the compiler/runtime `TargetC` ABI contract. | current | — |
| [per-body-identity-closure-materialization.md](per-body-identity-closure-materialization.md) | Instruction-level measurement of per-body identity closure materialization on Lattice, Mosaic, and a generated chain shape, with the sharing designs it does and does not support. | current | — |
| [post-adr-0063-cold-compiler-architecture-audit.md](post-adr-0063-cold-compiler-architecture-audit.md) | Point-in-time implementation audit (2026-08-10) of the compiler architecture after ADR-0063 and the cold-performance work through RUE-1348. | historical | [adr-0071-horizontal-vertical-ownership-reaudit.md](adr-0071-horizontal-vertical-ownership-reaudit.md) |
| [rue-1033-acceptance-ledger.md](rue-1033-acceptance-ledger.md) | The final structural and latency witnesses for ADR-0063's Phase 12 fresh-link boundary, with their measurement context. | historical | — |
| [rue-1089-acceptance-ledger.md](rue-1089-acceptance-ledger.md) | Archived pre-cutover acceptance record for the producer-nominal anonymous-type identity cut (ADR-0066 / RUE-1089). | historical | — |
| [rue-1250-ci-architecture.md](rue-1250-ci-architecture.md) | The design conclusion of the RUE-1250 CI investigation: one uniform mechanism instead of three hand-picked subsets. | current | — |
| [rue-1250-premerge-critical-path.md](rue-1250-premerge-critical-path.md) | Measurement (RUE-1262 evidence) showing one test function is 81% of the premerge lane. | historical | conclusions carried by [rue-1250-ci-architecture.md](rue-1250-ci-architecture.md) |
| [rue-1250-shard-topology-analysis.md](rue-1250-shard-topology-analysis.md) | Measured reassessment of the four-way CLI shard topology; keeps four shards and fixes the mechanism. | historical | conclusions carried by [rue-1250-ci-architecture.md](rue-1250-ci-architecture.md) |
| [rue-1505-remote-execution-evaluation.md](rue-1505-remote-execution-evaluation.md) | The measurements behind ADR-0069 Amendment 1: how each number was obtained, its population, and its caveats. | historical | ADR-0069 Amendment 1 records the decision |
| [rue-1548-request-scoped-universe-evaluation.md](rue-1548-request-scoped-universe-evaluation.md) | Three-shape instruction-level comparison of body-local semantic epochs against shared-base and request-scoped-universe alternatives, as evidence for the RUE-1548 decision. | current | — |
| [structured-wait-label-ownership.md](structured-wait-label-ownership.md) | Current implementation audit (RUE-1349) of why wait-graph labels are owned where they are. | current | — |
