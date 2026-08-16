"""Loading a dash-named sibling script as a module, for the tool tests.

The gates are executables with dashes in their names, so 25 test files each
carried the same five-line ``importlib`` incantation — and 25 copies of the
``assert`` that is the only thing standing between a typo'd filename and a
confusing ``None`` crash.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType


def load_script(name: str, relative_to: str) -> ModuleType:
    """Load ``name`` (e.g. ``"validate-payload-ownership.py"``) as a module.

    ``relative_to`` is the caller's ``__file__``; a bare ``name`` is resolved
    as its sibling, which is where every gate and its test live. ``name`` may
    also be a relative path for the few scripts a test loads from elsewhere
    in the tree; it resolves against the caller's directory, and the module
    name is then qualified with the script's directory so same-named scripts
    (three generators are all ``generate.py``) do not collide in
    ``sys.modules``.
    """
    script = (Path(relative_to).resolve().parent / name).resolve()
    if not script.is_file():
        raise FileNotFoundError(f"no script {name!r} relative to {relative_to}")
    module_name = script.stem.replace("-", "_")
    if Path(name).name != name:
        module_name = f"{script.parent.name}_{script.stem}".replace("-", "_")
    spec = importlib.util.spec_from_file_location(module_name, script)
    if spec is None or spec.loader is None:
        raise ImportError(f"could not build an import spec for {script}")
    module = importlib.util.module_from_spec(spec)
    # Registered before execution: `@dataclass` resolves a class's module
    # through `sys.modules`, and fails outright when it is missing.
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(spec.name, None)
        raise
    return module
