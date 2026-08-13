# Frozen Lattice compiler-performance workload

This directory freezes the Rue sources from `examples/lattice` at Rue commit
`60ca461636bdb0be8be22191c77dd05386bafc37`. It is the single Lattice fixture
used by ADR-0071's release-quality compiler target and the public performance
dashboard.

Do not update this copy when the pedagogical example changes. A deliberate
fixture change requires a new performance-suite revision and new platform
epochs so the old and new programs are never presented as one continuous
series.

Only the transitive `.rue` source closure participates in the workload identity.
The example's README and runtime input files are intentionally absent because
the compiler does not read them during a source-to-native build.
