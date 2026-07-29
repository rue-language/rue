# RUE-1192 Caldera peak-memory evidence

RUE-1080 removed a weak-node registry population scan from every node
insertion. The replacement remains point-addressed: insertion touches one
incarnation and `Node::drop` removes that exact weak entry. The existing
`bounded_node_registry_maintenance_is_independent_of_population` regression
test is the structural performance authority for that invariant.

## Paired baseline

The release-mode macOS arm64 measurements which opened RUE-1192 used identical
direct invocations and byte-identical output:

| Variant | Wall time | Retired instructions | Peak RSS |
| --- | ---: | ---: | ---: |
| Before RUE-1080 | 134.55 s | 483.38 B | 2.679 GB |
| Exact RUE-1080 registry patch | 9.50–9.80 s | 222.26–222.61 B | 3.381–3.387 GB |
| Exact patch, `-j1` | 11.58 s | 223.16 B | 3.374 GB |

Lattice and Meridian used less memory after the patch. Caldera was therefore
not evidence of a new globally retained registry: it was the one workload large
enough for the removed 120-second scan interval to change the process
high-water timing.

## Residency timeline

A current-source diagnostic build temporarily restored only the former
retain-on-insert population traversal. It was intentionally terminated after
204.5 seconds, before reaching the backend; it is not an end-to-end performance
sample. During the traversal, low-overhead process sampling plus `heap` and
`vmmap -summary` reported:

| Elapsed | Live malloc objects | Live requested bytes | Current physical footprint | Recorded peak |
| ---: | ---: | ---: | ---: | ---: |
| 29 s | 3,503,438 | 885.0 MB | 833.7 MB | 1.9 GB |
| 40 s | — | 908.3 MB | 856.3 MB | 1.9 GB |
| 50 s | 3,760,600 | 950.4 MB | 896.2 MB | 1.9 GB |
| 68 s | 4,085,702 | 1,033.2 MB | 983.1 MB | 1.9 GB |

At 29 seconds the malloc zone had 923.7 MB virtual, 470.3 MB resident, and
377.5 MB swapped. RSS later fell into the 0.23–0.55 GB range while live malloc
bytes continued to rise. This establishes what the old scan changed: it gave
macOS time to compress or page out cold but still-live frontend allocations
before later compiler work. It did not release those allocations or change
their logical owners.

Two attempted mitigations falsified narrower explanations:

- maximal `malloc_zone_pressure_relief(NULL, 0)` at the semantic/backend
  boundary reported zero released bytes;
- dropping the one-shot session roots while retaining the immutable RIR and
  semantic owners still reported zero released bytes and did not materially
  reduce peak RSS.

Neither experiment is part of the implementation.

## Stable ownership gauges

RSS alone cannot distinguish retained compiler state from allocator and kernel
residency. `RuntimeMetrics` therefore publishes two exact ownership pairs:

- `active_task_leases` / `peak_task_leases` count distinct terminals protected
  by live rooted-request observation sets;
- `active_retained_pins` / `peak_retained_pins` count distinct terminals
  protected by promoted `RetainedPinSet`s, including the intentional
  predecessor/successor overlap during an atomic handoff.

Acquisition, child-to-parent transfer, deduplication, and teardown update the
gauges at the ownership operation itself. Focused tests require a 64-terminal
Caldera-shaped request to read exactly 64 live/peak task leases and return to
zero at completion, and require retained-set handoff to read 1 → 2 → 1 → 0.
Family-level `retained_terminals` and configured family limits remain the
separate memo-retention bound.

These gauges are the policy boundary for future memory comparisons: a higher
RSS with equal logical ownership is a residency/high-water change; growth in
one of the ownership gauges identifies the layer whose retention policy
changed. Exact Caldera gauge values were not collected in this investigation
because the host was under sustained unrelated load; the measurements above
must not be presented as those counts.
