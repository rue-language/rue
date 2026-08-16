#!/usr/bin/env python3
"""Focused tests for the ADR registry validator's status coherence check."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

adrs = load_script("validate-adrs.py", __file__)


def body(status_section: str) -> list[str]:
    """A minimal ADR body around the given ``## Status`` section text."""
    return f"""\
# ADR-0999: Fixture

## Status

{status_section}

## Summary

Prose.
""".splitlines()


class StatusBodyCoherenceTests(unittest.TestCase):
    def test_accepted_with_proposal_opener_fails(self) -> None:
        errors = adrs.status_body_errors(body("Proposal\n\nDraft prose."), "accepted")
        self.assertEqual(len(errors), 1)
        self.assertIn("'Proposal'", errors[0])
        self.assertIn("'accepted'", errors[0])

    def test_accepted_with_draft_disclaimer_fails(self) -> None:
        errors = adrs.status_body_errors(
            body("This is a draft. Nothing here is accepted."), "accepted"
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("nothing is accepted", errors[0])

    def test_disclaimer_match_is_case_insensitive(self) -> None:
        # ADR-0049's real spelling was mid-sentence lowercase: "it makes no
        # commitment and nothing here is accepted".
        errors = adrs.status_body_errors(
            body("It makes no commitment and nothing here is accepted."),
            "implemented",
        )
        self.assertEqual(len(errors), 1)

    def test_both_markers_report_both_errors(self) -> None:
        errors = adrs.status_body_errors(
            body("Proposal\n\nNothing here is accepted."), "accepted"
        )
        self.assertEqual(len(errors), 2)

    def test_proposal_status_keeps_draft_body(self) -> None:
        # A genuine draft is coherent: the check gates only ratified statuses.
        self.assertEqual(
            adrs.status_body_errors(
                body("Proposal\n\nNothing here is accepted."), "proposal"
            ),
            [],
        )

    def test_ratified_history_body_passes(self) -> None:
        self.assertEqual(
            adrs.status_body_errors(
                body(
                    "Accepted on 2026-07-16; Phases 1 and 2 shipped in July "
                    "2026 (RUE-926, RUE-927)."
                ),
                "accepted",
            ),
            [],
        )

    def test_missing_status_section_is_not_flagged(self) -> None:
        lines = ["# ADR-0999: Fixture", "", "## Summary", "", "Prose."]
        self.assertEqual(adrs.status_body_errors(lines, "accepted"), [])

    def test_proposal_prose_mention_outside_status_section_passes(self) -> None:
        # The check is scoped to the Status section: a later section quoting
        # the word "Proposal" or discussing acceptance is not a draft marker.
        lines = body("Accepted on 2026-07-16.") + [
            "",
            "## History",
            "",
            "Proposal",
            "An earlier draft said nothing here is accepted.",
        ]
        self.assertEqual(adrs.status_body_errors(lines, "accepted"), [])

    def test_superseded_and_stable_statuses_are_out_of_scope(self) -> None:
        for status in ("stable", "superseded", "rejected", ""):
            self.assertEqual(
                adrs.status_body_errors(body("Proposal"), status), [], status
            )


if __name__ == "__main__":
    unittest.main()
