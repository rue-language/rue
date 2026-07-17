#!/usr/bin/env python3
"""Generate the deterministic AIR payload-schema performance fixtures."""

import argparse
from pathlib import Path


def match_source(functions: int) -> str:
    lines = ["enum Shape { Circle(i32), Rect(i32, i32), Empty }"]
    for index in range(functions):
        tail = (
            f" + match_{index + 1:04}(Shape.Empty)"
            if index + 1 < functions
            else ""
        )
        lines.extend(
            [
                f"fn match_{index:04}(s: Shape) -> i32 {{",
                "    (match s {",
                f"        Shape.Circle(r) => r + {index},",
                "        Shape.Rect(w, h) => w + h,",
                "        Shape.Empty => 0,",
                f"    }}){tail}",
                "}",
            ]
        )
    lines.append("fn main() -> i32 { match_0000(Shape.Rect(1, 2)) }")
    return "\n".join(lines) + "\n"


def generic_source(instances: int) -> str:
    lines = [
        "fn scale(comptime factor: i32, value: i32) -> i32 { factor * value }",
        "fn identity(comptime T: type, value: T) -> T { value }",
    ]
    for index in range(instances):
        tail = f" + generic_{index + 1:04}(x)" if index + 1 < instances else ""
        lines.append(
            f"fn generic_{index:04}(x: i32) -> i32 {{ "
            f"identity(i32, scale({index + 1}, x)){tail} }}"
        )
    lines.append("fn main() -> i32 { generic_0000(1) }")
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--scale", type=int, default=512)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "match-heavy.rue").write_text(match_source(args.scale))
    (args.output / "generic-specialization.rue").write_text(generic_source(args.scale))


if __name__ == "__main__":
    main()
