"""Deterministic source generators for compiler scaling diagnostics.

The generated programs are deliberately synthetic. They isolate monotonically
growing compiler work; they do not model real applications or runtime speed.
"""

from __future__ import annotations

from pathlib import Path


GENERATOR_VERSION = 1


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def function_declarations(root: Path, size: int) -> Path:
    lines = ["// deterministic frontend declaration scaling probe"]
    lines.extend(f"fn f{i:04}(x: i32) -> i32 {{ x + {i} }}" for i in range(size))
    lines.append(f"fn main() -> i32 {{ f{size - 1:04}(1) }}")
    path = root / "main.rue"
    _write(path, "\n".join(lines) + "\n")
    return path


def module_fanout(root: Path, size: int) -> Path:
    lines = ["// deterministic module-discovery fanout scaling probe"]
    for i in range(size):
        lines.append(f'const m{i:04} = @import("modules/m{i:04}.rue");')
        _write(root / f"modules/m{i:04}.rue", f"pub fn value() -> i32 {{ {i} }}\n")
    lines.append(f"fn main() -> i32 {{ m{size - 1:04}.value() }}")
    path = root / "main.rue"
    _write(path, "\n".join(lines) + "\n")
    return path


def struct_declarations(root: Path, size: int) -> Path:
    lines = ["// deterministic semantic/type scaling probe"]
    lines.extend(
        f"struct S{i:04} {{ a: i32, b: i32, c: i32, d: i32 }}"
        for i in range(size)
    )
    lines.append(
        f"fn main() -> i32 {{ let value = S{size - 1:04} "
        "{ a: 1, b: 2, c: 3, d: 4 }; value.d }"
    )
    path = root / "main.rue"
    _write(path, "\n".join(lines) + "\n")
    return path


def cfg_functions(root: Path, size: int) -> Path:
    lines = ["// deterministic CFG scaling probe"]
    lines.extend(
        f"fn branch{i:04}(x: i32) -> i32 {{ if x < 0 {{ 0 }} "
        f"else if x == {i} {{ {i} }} else {{ x + 1 }} }}"
        for i in range(size)
    )
    lines.append(f"fn main() -> i32 {{ branch{size - 1:04}(1) }}")
    path = root / "main.rue"
    _write(path, "\n".join(lines) + "\n")
    return path


def backend_live_values(root: Path, size: int) -> Path:
    lines = ["// deterministic backend/register-allocation scaling probe"]
    body = " ".join(f"let v{i} = x + {i};" for i in range(24))
    total = " + ".join(f"v{i}" for i in range(24))
    lines.extend(f"fn pressure{i:04}(x: i32) -> i32 {{ {body} {total} }}" for i in range(size))
    lines.append(f"fn main() -> i32 {{ pressure{size - 1:04}(1) }}")
    path = root / "main.rue"
    _write(path, "\n".join(lines) + "\n")
    return path


GENERATORS = {
    "function_declarations": function_declarations,
    "module_fanout": module_fanout,
    "struct_declarations": struct_declarations,
    "cfg_functions": cfg_functions,
    "backend_live_values": backend_live_values,
}


def generate(name: str, root: Path, size: int) -> Path:
    try:
        generator = GENERATORS[name]
    except KeyError as error:
        raise ValueError(f"unknown scaling generator: {name}") from error
    return generator(root, size)
