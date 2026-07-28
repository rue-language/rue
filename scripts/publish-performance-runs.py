#!/usr/bin/env python3
"""Append validated run objects to the performance-data-v1 branch (ADR-0067).

The branch has a deliberately tiny contract:

    index.json
    runs/<content-hash>.json

A run object's filename is the SHA-256 of its bytes, so a stored record can be
verified against its own name without trusting whoever wrote it. This script
recomputes that digest rather than believing the incoming filename, and refuses
to overwrite an existing object: run objects are immutable, and a differing
object under an existing name means either a hash collision or a corrupted
pipeline, neither of which should be resolved by clobbering.

`index.json` points at run objects and nothing else. Medians, dispersion,
indexes, and chart data are rebuilt from the raw records at site build time. A
derived value stored here would eventually disagree with the records it came
from, and the stored copy is the one that would be believed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path


def canonical_bytes(payload: bytes) -> bytes:
    """Return the payload unchanged after checking it is already canonical.

    The runner writes canonical JSON — sorted keys, no insignificant
    whitespace, integers only — and the content address is the digest of those
    exact bytes. Re-serializing here would risk producing different bytes for
    the same value, so this verifies rather than normalizes.
    """
    text = payload.decode("utf-8")
    value = json.loads(text)
    expected = json.dumps(value, sort_keys=True, separators=(",", ":"))
    if expected != text:
        raise ValueError(
            "run object is not canonical JSON; refusing to publish a record "
            "whose bytes do not match their own canonical form"
        )
    return payload


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--incoming", type=Path, required=True)
    parser.add_argument("--data-root", type=Path, required=True)
    args = parser.parse_args()

    runs_dir = args.data_root / "runs"
    runs_dir.mkdir(parents=True, exist_ok=True)
    index_path = args.data_root / "index.json"

    index = json.loads(index_path.read_text()) if index_path.exists() else {}
    index.setdefault("schema_version", 1)
    entries = index.setdefault("runs", [])
    known = {entry["run"] for entry in entries}

    published = 0
    skipped = 0
    for incoming in sorted(args.incoming.glob("*.json")):
        payload = incoming.read_bytes()
        try:
            payload = canonical_bytes(payload)
        except (ValueError, UnicodeDecodeError) as error:
            print(f"error: {incoming.name}: {error}", file=sys.stderr)
            return 1

        run = json.loads(payload)
        address = hashlib.sha256(payload).hexdigest()
        identity = run["identity"]

        destination = runs_dir / f"{address}.json"
        if destination.exists():
            # Byte-identical means this exact measurement is already stored,
            # which happens when a workflow is re-run. Differing bytes under the
            # same address must never be silently resolved.
            if destination.read_bytes() != payload:
                print(
                    f"error: {address} already exists with different content",
                    file=sys.stderr,
                )
                return 1
            print(f"already stored: {address[:12]} ({identity['platform']})")
            skipped += 1
            continue

        destination.write_bytes(payload)
        if address not in known:
            # The index records only what is needed to find a run and group it
            # into a series. Everything else is read from the object itself.
            entries.append(
                {
                    "run": address,
                    "platform": identity["platform"],
                    "epoch": identity["epoch"],
                    "suite_revision": identity["suite_revision"],
                    "commit": identity["commit"],
                    "finished_at": identity["finished_at"],
                }
            )
            known.add(address)
        published += 1
        print(f"published: {address[:12]} ({identity['platform']})")

    # Sorted so the index is a function of its contents rather than of the order
    # runs happened to arrive, which keeps its diffs readable.
    entries.sort(key=lambda entry: (entry["finished_at"], entry["run"]))
    index_path.write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")

    print(f"published {published} run object(s), {skipped} already stored")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
