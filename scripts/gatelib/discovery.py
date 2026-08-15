"""Repository walking with the shared prune policy.

Two baseline gates carried character-identical ``_prune`` functions and a
third reimplemented the same policy inline. The policy: skip the build and
vendor directories below, and skip nested checkouts by their own VCS marker
rather than by name — the working agreement puts sibling worktrees under
``.claude/worktrees/``, and reporting a path inside one as though it were a
path in THIS tree is worse than not scanning it; that copy has its own gate.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Callable, Iterator, List, Optional

SKIP_DIRECTORIES = {
    ".buckd",
    ".git",
    ".jj",
    "buck-out",
    "buck2-bin",
    "node_modules",
    "target",
    "third-party",
}


def prune_names(directory: Path, names: List[str]) -> List[str]:
    """Directory names under ``directory`` to descend into, sorted."""
    kept = []
    for name in sorted(names):
        if name in SKIP_DIRECTORIES:
            continue
        path = directory / name
        if (path / ".git").exists() or (path / ".jj").exists():
            continue
        kept.append(name)
    return kept


def walk_files(
    root: Path,
    keep: Optional[Callable[[Path], bool]] = None,
) -> Iterator[Path]:
    """Every file under ``root`` surviving the prune policy, in sorted order.

    ``keep`` filters files (not directories); omit it to yield everything.
    """
    for current, directories, files in os.walk(root):
        directory = Path(current)
        directories[:] = prune_names(directory, directories)
        for name in sorted(files):
            path = directory / name
            if keep is None or keep(path):
                yield path
