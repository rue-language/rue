"""The gate main skeleton and the ``--source CRATE=DIR`` parser.

Every gate is the same program with a different predicate: parse arguments,
run ``validate``, print each error to stderr with a ``1`` exit, or print a
one-line summary with a ``0``. ``run_gate`` is that program once. Gates
with richer interfaces (extra flags, regeneration modes) keep their own
mains and use the pieces they need.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Callable, Iterable, List, Optional, Sequence, Tuple


def run_gate(
    validate: Callable[[], Sequence[str]],
    summary: str,
    failure_heading: Optional[str] = None,
) -> int:
    """Run one gate predicate and report in the shared shape.

    ``validate`` returns the error list (empty for a pass). ``summary`` is
    the single pass line. ``failure_heading``, when given, prints once
    before the itemized errors.
    """
    errors = list(validate())
    if errors:
        if failure_heading:
            print(failure_heading, file=sys.stderr)
            for error in errors:
                print(f"  - {error}", file=sys.stderr)
        else:
            for error in errors:
                print(f"error: {error}", file=sys.stderr)
        return 1
    print(summary)
    return 0


def parse_crate_sources(
    items: Iterable[str],
    allowed: Optional[Iterable[str]] = None,
) -> List[Tuple[str, Path]]:
    """Parse repeated ``--source CRATE=DIR`` values.

    Raises ``ValueError`` naming the offending item; gates turn that into
    their usage exit. ``allowed`` restricts the crate names when the gate
    owns a fixed inventory.
    """
    allowed_set = set(allowed) if allowed is not None else None
    sources: List[Tuple[str, Path]] = []
    for item in items:
        crate, separator, directory = item.partition("=")
        if not separator or (allowed_set is not None and crate not in allowed_set):
            raise ValueError(f"invalid --source {item!r}")
        sources.append((crate, Path(directory)))
    return sources
