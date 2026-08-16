#!/usr/bin/env python3
"""The scaling-probe drift gate (RUE-1264).

The committed sources under `performance/workloads/scale_*/` are generator
output, and the probes' single-axis property lives in the generators — a
hand edit to one size would silently turn a ratio between sizes into a
comparison of two different programs. Each generator's `--check` refuses
drift; this test is what runs it on every premerge, so the refusal happens
in CI rather than in a reviewer's memory. The epoch's
`workload_source_hashes` pin guards the same bytes for collection, but that
gate only fires when collection runs; this one fails the pull request.

Determinism is asserted separately because `--check` cannot see it: a
generator that varied its output between runs would pass `--check` exactly
once, at commit time, and start failing for whoever touches it next.
"""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

REPO = Path(__file__).resolve().parent.parent
GENERATORS = [
    REPO / "performance" / "workloads" / "scale_modules" / "generate.py",
    REPO / "performance" / "workloads" / "scale_functions" / "generate.py",
    REPO / "performance" / "workloads" / "scale_instantiations" / "generate.py",
]


def load(path: Path):
    # The generators are not siblings of this test; hand the loader their
    # path relative to scripts/.
    return load_script(str(Path("..") / path.relative_to(REPO)), __file__)


class ScaleProbeGeneratorTests(unittest.TestCase):
    def test_committed_sources_match_their_generators(self) -> None:
        for generator in GENERATORS:
            with self.subTest(generator=generator.parent.name):
                result = subprocess.run(
                    [sys.executable, str(generator), "--check"],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(
                    result.returncode,
                    0,
                    "committed sources drifted from the generator:\n%s"
                    % result.stderr,
                )

    def test_generators_are_deterministic(self) -> None:
        for generator in GENERATORS:
            module = load(generator)
            for size in module.SIZES:
                with self.subTest(generator=generator.parent.name, size=size):
                    self.assertEqual(module.render(size), module.render(size))

    def test_each_probe_declares_two_sizes_a_factor_of_four_apart(self) -> None:
        # The ratio between sizes is the probe's signal, and four is the
        # declared step: a linear phase roughly quadruples, so a drifting
        # ratio is legible against it. A generator that changed its sizes
        # changes what the manifest's `scaling` tables must say, and this
        # names that coupling.
        for generator in GENERATORS:
            module = load(generator)
            with self.subTest(generator=generator.parent.name):
                self.assertEqual(len(module.SIZES), 2)
                small, large = module.SIZES
                self.assertEqual(large, small * 4)


if __name__ == "__main__":
    unittest.main()
