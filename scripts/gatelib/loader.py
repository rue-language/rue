"""Loading a dash-named sibling script as a module, for the tool tests.

The gates are executables with dashes in their names, so 25 test files each
carried the same five-line ``importlib`` incantation — and 25 copies of the
``assert`` that is the only thing standing between a typo'd filename and a
confusing ``None`` crash.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
from types import ModuleType


def load_script(name: str, relative_to: str) -> ModuleType:
    """Load ``name`` (e.g. ``"validate-payload-ownership.py"``) as a module.

    ``relative_to`` is the caller's ``__file__``; the script is resolved as
    its sibling, which is where every gate and its test live.
    """
    script = Path(relative_to).resolve().with_name(name)
    if not script.is_file():
        raise FileNotFoundError(f"no sibling script {name!r} next to {relative_to}")
    spec = importlib.util.spec_from_file_location(
        script.stem.replace("-", "_"), script
    )
    if spec is None or spec.loader is None:
        raise ImportError(f"could not build an import spec for {script}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
