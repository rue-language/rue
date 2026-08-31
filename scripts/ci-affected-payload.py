#!/usr/bin/env python3
"""Create and verify canonical affected-target payload proofs.

GitHub job outputs are transported as independent strings. A count and SHA-256
over a canonical serialization let a consumer distinguish a deliberately empty
selection from a missing, truncated, reordered, or otherwise mutated payload.
Candidate decisions are also bounded by the planner's locally resolved
canonical narrow limit.
This is an integrity check for one workflow run, not a cryptographic trust
boundary: changes to this script themselves force the authoritative full run.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from dataclasses import dataclass


CANONICAL_COUNT = re.compile(r"(?:0|[1-9][0-9]*)\Z")
CANONICAL_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
LANE = re.compile(r"[a-z0-9][a-z0-9-]*\Z")
TARGET = re.compile(r"//[^\s:]*:[^\s:]+\Z")


class PayloadError(ValueError):
    """The payload or its claimed proof is not canonical."""


@dataclass(frozen=True)
class Payload:
    items: tuple[str, ...]
    canonical: str

    @property
    def count(self) -> int:
        return len(self.items)

    @property
    def digest(self) -> str:
        return hashlib.sha256(self.canonical.encode("utf-8")).hexdigest()


def parse_payload(kind: str, raw: str) -> Payload:
    if "\r" in raw:
        raise PayloadError("carriage returns are not canonical")

    if kind == "lanes":
        if "\n" in raw:
            raise PayloadError("lane payload must be one line")
        items = () if not raw else tuple(raw.split(" "))
        if any(not item or not LANE.fullmatch(item) for item in items):
            raise PayloadError("lane payload contains an invalid token")
        canonical = " ".join(items)
    elif kind == "targets":
        # Planner files conventionally end each target with one newline. Job
        # outputs do not preserve that terminal newline, so both spellings map
        # to the same proof; extra blank lines remain invalid.
        body = raw
        if body.endswith("\n") and body != "\n":
            body = body[:-1]
        if "\n\n" in body or body == "\n":
            raise PayloadError("target payload contains an empty line")
        items = () if not body else tuple(body.split("\n"))
        if any(not TARGET.fullmatch(item) for item in items):
            raise PayloadError("target payload contains an invalid label")
        canonical = "\n".join(items)
    else:
        raise PayloadError(f"unknown payload kind {kind!r}")

    if len(items) != len(set(items)):
        raise PayloadError("payload contains a duplicate item")
    return Payload(items, canonical)


def parse_count(value: str, name: str) -> int:
    if not CANONICAL_COUNT.fullmatch(value):
        raise PayloadError(f"{name} is not a canonical non-negative decimal")
    return int(value)


def verify(payload: Payload, expected_count: str, expected_digest: str) -> None:
    count = parse_count(expected_count, "expected count")
    if not CANONICAL_DIGEST.fullmatch(expected_digest):
        raise PayloadError("expected digest is not 64 lowercase hexadecimal digits")
    if payload.count != count:
        raise PayloadError(
            f"payload count mismatch: expected {count}, received {payload.count}"
        )
    if payload.digest != expected_digest:
        raise PayloadError("payload digest mismatch")


def write_payload(kind: str, payload: Payload) -> None:
    if payload.items:
        sys.stdout.write(payload.canonical)
        if kind == "targets":
            sys.stdout.write("\n")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    commands = result.add_subparsers(dest="command", required=True)

    proof = commands.add_parser("proof")
    proof.add_argument("kind", choices=("lanes", "targets"))

    check = commands.add_parser("verify")
    check.add_argument("kind", choices=("lanes", "targets"))
    check.add_argument("expected_count")
    check.add_argument("expected_digest")
    check.add_argument("--require-nonempty", action="store_true")

    decision = commands.add_parser("verify-decision")
    decision.add_argument("expected_lane_count")
    decision.add_argument("expected_lane_digest")
    decision.add_argument("narrowed")
    decision.add_argument("narrowing_status")
    decision.add_argument("head_target_count")
    decision.add_argument("impacted_closure_count")
    decision.add_argument("impacted_target_count")
    decision.add_argument("narrow_limit")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        kind = "lanes" if args.command == "verify-decision" else args.kind
        payload = parse_payload(kind, sys.stdin.read())
        if args.command == "proof":
            print(f"{payload.count} {payload.digest}")
            return 0
        if args.command == "verify":
            verify(payload, args.expected_count, args.expected_digest)
            if args.require_nonempty and payload.count == 0:
                raise PayloadError("payload must be nonempty")
            write_payload(args.kind, payload)
            return 0

        verify(payload, args.expected_lane_count, args.expected_lane_digest)
        head_count = parse_count(args.head_target_count, "head target count")
        closure_count = parse_count(
            args.impacted_closure_count, "impacted closure count"
        )
        live_count = parse_count(
            args.impacted_target_count, "impacted target count"
        )
        narrow_limit = parse_count(args.narrow_limit, "narrow limit")
        if head_count == 0:
            raise PayloadError("head target count must be positive")
        if narrow_limit == 0:
            raise PayloadError("narrow limit must be positive")
        if live_count > head_count:
            raise PayloadError("impacted target count exceeds the head graph")
        if live_count > closure_count:
            raise PayloadError("live impacted count exceeds the raw closure")
        state = (args.narrowing_status, args.narrowed)
        if state not in {("CANDIDATE", "true"), ("DECLINED", "false")}:
            raise PayloadError("narrowing status and flag are inconsistent")
        candidate = 0 < closure_count <= narrow_limit and live_count > 0
        expected_state = (
            ("CANDIDATE", "true") if candidate else ("DECLINED", "false")
        )
        if state != expected_state:
            raise PayloadError(
                "narrowing state disagrees with closure count, live count, or limit"
            )
        return 0
    except PayloadError as error:
        print(f"affected payload invalid: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
