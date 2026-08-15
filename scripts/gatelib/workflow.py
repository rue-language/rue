"""Splitting a GitHub workflow file into its per-job blocks.

Two CI gates carried identical copies of this; the only difference was a
local variable's name. The split is textual on purpose: these gates assert
properties of the workflow *source* (indentation-level job keys under the
top-level ``jobs:`` mapping), and a YAML load would erase the spans they
report against.
"""

from __future__ import annotations

import re
from typing import Dict

ACTION_ID = r"[A-Za-z_][A-Za-z0-9_-]*"
JOB_RE = re.compile(rf"^  ({ACTION_ID}):\s*$", re.MULTILINE)


def job_blocks(workflow: str) -> Dict[str, str]:
    """Map each job id to its source block, in workflow order."""
    marker = workflow.find("\njobs:\n")
    if marker < 0:
        raise ValueError("workflow has no top-level jobs mapping")
    body = workflow[marker + len("\njobs:\n") :]
    matches = list(JOB_RE.finditer(body))
    return {
        match.group(1): body[match.start() : matches[index + 1].start()]
        if index + 1 < len(matches)
        else body[match.start() :]
        for index, match in enumerate(matches)
    }
