"""Shared plumbing for the scripts/validate-* gates (RUE-1522).

Each gate keeps its own predicate — the curated tables and per-gate logic are
the product — but the mechanics every gate reimplemented live here exactly
once: the Rust comment/literal masker, the repository walker with its prune
policy, the workflow job splitter, the ``--source CRATE=DIR`` parser, the
gate main skeleton, and the dash-named-script loader the tool tests use.

Runs on Python 3.9 (the repository tooling floor in
docs/process/tooling-baseline.md).
"""

from __future__ import annotations

from .discovery import SKIP_DIRECTORIES, prune_names, walk_files
from .gate import parse_crate_sources, run_gate
from .loader import load_script
from .rust import mask_rust_non_code
from .workflow import job_blocks

__all__ = [
    "SKIP_DIRECTORIES",
    "job_blocks",
    "load_script",
    "mask_rust_non_code",
    "parse_crate_sources",
    "prune_names",
    "run_gate",
    "walk_files",
]
