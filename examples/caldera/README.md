# Caldera

Caldera is a deterministic headless simulation and game-engine laboratory
written entirely in Rue. It is a maintained application example and the
first Rue program whose application source exceeds 100,000 lines.

One transitive module graph contains the fixed-point math, world model,
physics, navigation, entity scheduling, AI, economy, scripting VM,
snapshots, rollback, replay, lockstep networking, persistence, headless
rendering, and observability core. Large generated families remain real
executable code rather than padding:

- 256 whole-world audit passes;
- 192 simulation-system evaluators;
- 128 agent behaviors;
- 96 content and simulation rules;
- 64 deterministic report adapters.

Every family member has a distinct identity and policy, is imported by one
canonical suite, executes twice, and contributes to determinism checks.

Run the comprehensive cross-oracle self-test (the default invocation):

    scripts/rue exec examples/caldera/main.rue

Run the built-in world, checked-in scenario, and invariant portfolios:

    scripts/rue exec examples/caldera/main.rue demo
    scripts/rue exec examples/caldera/main.rue run examples/caldera/demo.caldera
    scripts/rue exec examples/caldera/main.rue selftest
    scripts/rue exec examples/caldera/main.rue stress1
    scripts/rue exec examples/caldera/main.rue benchmark

Regenerate the deterministic source families and verify the size contract:

    python3 scripts/generate-caldera.py --check

The no-argument path executes all core oracles and all 736 generated family
modules twice. This gives the automatic example smoke deep behavioral and
determinism coverage while compiling the complete 100K-line graph once.
