#!/usr/bin/env python3
"""Parser complexity gates over generated programs (RUE-1107, spec A.2).

Appendix A.2 requires every valid program to parse in time linear in its
token count, with each decision point settled by the current token, a
bounded number of following tokens, or one forward scan to a matching
delimiter, and with no construct parsed twice. These gates hold both
frontends to that criterion from the outside: the production compiler
through `--emit ast`, and the Rue-hosted frontend `examples/ruelex` through
its `--ast-shape` dump. Neither is inspected; each is timed on generated
programs that exercise one decision point at two sizes, and the larger
input must not cost more than a fixed multiple of the smaller one. An
exponential or quadratic re-parse shows up as a ratio in the hundreds or
thousands; the multiple below is a loose ceiling that leaves scheduler
noise on a loaded runner far below it.

Each family names the decision point it stresses:

* `nested_blocks`, `nested_arrays`, `nested_array_types`, `else_if_chain`:
  depth-bounded forms (A.2:2 items 1, 4 and the `else if` continuation),
  generated near the C.6:3 nesting allowance so the whole allowance is
  parsed at the linear rate.
* `sequential_if_else`, `sequential_block_likes`: semicolon-free
  control-flow statements in one block (A.2:2 item 2), a sequence far
  longer than the nesting allowance, which also proves a statement-position
  block-like expression does not count as nesting the next one.
* `return_break_operands`: the optional operands of `return` and `break`
  (A.2:2 item 5) decided by the terminator that follows.
* `array_list_vs_repeat`: `[a, b]` against `[a; n]` in one program (A.2:2
  item 4), nested so the `;` decision repeats at every level.
* `nested_constructor_groups`, `nested_payload_patterns`: a parenthesised
  group after a path segment, decided by the token after its matching `)`
  (A.2:2 item 6): type-constructor arguments nested in a pattern head, and
  variant payload patterns nested in a payload position. The payload form
  runs on the production frontend only until ruelex parses it (RUE-2184).

Environment: `RUE_BINARY` (the compiler), `RUE_STD_PATH` (the standard
library, needed to compile ruelex), `RUE_RUELEX_ROOT` (the
`examples/ruelex/main.rue` root). `--frontend production` or
`--frontend ruelex` restricts the run; the default runs both.
"""

from __future__ import annotations

import argparse
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# The larger input is four times the smaller. A linear parser costs at most
# a few times more (process spawn dominates the small case); a quadratic
# re-parse costs sixteen times more and an exponential one is unbounded.
SIZE_RATIO = 4
MAX_TIME_RATIO = 12.0
# Every generated program at every size must also parse inside this budget,
# so a blowup that stays under the ratio by being uniformly slow still fails.
PER_RUN_BUDGET_SECONDS = 20.0
REPETITIONS = 5

# Depth families stay inside the C.6:3 allowance of 256 levels; the larger
# size is the deepest program the nesting guard admits with headroom.
DEPTH_SMALL = 48
DEPTH_LARGE = DEPTH_SMALL * SIZE_RATIO
SEQUENCE_SMALL = 300
SEQUENCE_LARGE = SEQUENCE_SMALL * SIZE_RATIO
# A nested payload pattern spends two units of the allowance per level (the
# `.` of the path and the `(` of the group), so its nest is half as deep.
PATTERN_DEPTH_SMALL = DEPTH_SMALL // 2
PATTERN_DEPTH_LARGE = PATTERN_DEPTH_SMALL * SIZE_RATIO


def nested_blocks(depth: int) -> str:
    return "fn main() -> i32 { " + "{ " * depth + "42" + " }" * depth + " }\n"


def nested_arrays(depth: int) -> str:
    return "fn main() -> i32 { let a = " + "[" * depth + "1" + "]" * depth + "; 0 }\n"


def nested_array_types(depth: int) -> str:
    ty = "[" * depth + "i32" + "; 1]" * depth
    return f"fn f(x: {ty}) -> i32 {{ 0 }}\nfn main() -> i32 {{ 0 }}\n"


def else_if_chain(depth: int) -> str:
    return (
        "fn main() -> i32 { if false { 0 } "
        + "else if false { 0 } " * depth
        + "else { 1 } }\n"
    )


def sequential_if_else(count: int) -> str:
    return "fn main() -> i32 { " + "if true { 1 } else { 2 } " * count + "0 }\n"


def sequential_block_likes(count: int) -> str:
    unit = "while false { } { 1; } loop { break; } match 0 { _ => 0 } "
    return "fn main() -> i32 { " + unit * count + "0 }\n"


def return_break_operands(count: int) -> str:
    # Every loop decides `break` with and without an operand, and every `if`
    # decides `return` with and without one, by the token after the keyword:
    # `}` ends the operand-less form, anything else begins one. One body
    # holds the whole sequence so the gate times the decision, not the
    # per-item cost of a program with thousands of functions.
    unit = "loop { if c { break } } loop { break 1; } if c { return } if c { return 0 } "
    return (
        "fn main() -> i32 { let c = false; " + unit * count + "0 }\n"
    )


def array_list_vs_repeat(depth: int) -> str:
    # Alternate list and repeat at every level so the `;`-or-`,` decision
    # repeats down the whole nest.
    inner = "1"
    for level in range(depth):
        inner = f"[{inner}; 1]" if level % 2 == 0 else f"[{inner}, 0]"
    return f"fn main() -> i32 {{ let a = {inner}; 0 }}\n"


def nested_constructor_groups(depth: int) -> str:
    # `Wrap(Wrap(...(i32)...)).Some(v)`: the outer group is followed by `.`,
    # so it is constructor arguments; every inner group is a type argument
    # parsed once by the type parser.
    ty = "i32"
    for _ in range(depth):
        ty = f"Wrap({ty})"
    return (
        "fn Wrap(comptime T: type) -> type { enum { Some(T), None } }\n"
        "fn main() -> i32 {\n"
        f"    let w = {ty}.None;\n"
        f"    match w {{ {ty}.Some(v) => 1, {ty}.None => 0 }}\n"
        "}\n"
    )


def nested_payload_patterns(depth: int) -> str:
    # `E3.Node(E2.Node(E1.Node(E0.Leaf(v))))`: every payload group is decided
    # by whether a `.` follows its matching `)`. One enum per level keeps
    # the program well-formed (an enum cannot contain itself by value).
    enums = ["enum E0 { Leaf(i32) }"] + [
        f"enum E{level} {{ Node(E{level - 1}) }}" for level in range(1, depth + 1)
    ]
    value = "E0.Leaf(1)"
    pattern = "E0.Leaf(v)"
    for level in range(1, depth + 1):
        value = f"E{level}.Node({value})"
        pattern = f"E{level}.Node({pattern})"
    return (
        "\n".join(enums)
        + "\nfn main() -> i32 {\n"
        + f"    let e = {value};\n"
        + f"    match e {{ {pattern} => v }}\n"
        + "}\n"
    )


BOTH = ("production", "ruelex")
PRODUCTION_ONLY = ("production",)

# (name, generator, small size, large size, frontends that run it)
FAMILIES = [
    ("nested_blocks", nested_blocks, DEPTH_SMALL, DEPTH_LARGE, BOTH),
    ("nested_arrays", nested_arrays, DEPTH_SMALL, DEPTH_LARGE, BOTH),
    ("nested_array_types", nested_array_types, DEPTH_SMALL, DEPTH_LARGE, BOTH),
    ("else_if_chain", else_if_chain, DEPTH_SMALL, DEPTH_LARGE, BOTH),
    ("sequential_if_else", sequential_if_else, SEQUENCE_SMALL, SEQUENCE_LARGE, BOTH),
    ("sequential_block_likes", sequential_block_likes, SEQUENCE_SMALL, SEQUENCE_LARGE, BOTH),
    ("return_break_operands", return_break_operands, SEQUENCE_SMALL, SEQUENCE_LARGE, BOTH),
    ("array_list_vs_repeat", array_list_vs_repeat, DEPTH_SMALL, DEPTH_LARGE, BOTH),
    ("nested_constructor_groups", nested_constructor_groups, PATTERN_DEPTH_SMALL, PATTERN_DEPTH_LARGE, BOTH),
    # ruelex does not parse a nested payload pattern yet (RUE-2184).
    ("nested_payload_patterns", nested_payload_patterns, PATTERN_DEPTH_SMALL, PATTERN_DEPTH_LARGE, PRODUCTION_ONLY),
]


class Frontend:
    def __init__(self, name: str, command: list[str]):
        self.name = name
        self.command = command

    def parse(self, path: Path, env: dict[str, str]) -> tuple[float, subprocess.CompletedProcess]:
        started = time.perf_counter()
        completed = subprocess.run(
            [*self.command, str(path)],
            capture_output=True,
            env=env,
            timeout=PER_RUN_BUDGET_SECONDS * 2,
        )
        return time.perf_counter() - started, completed


def median_parse(frontend: Frontend, path: Path, env: dict[str, str]) -> float:
    samples = []
    for _ in range(REPETITIONS):
        elapsed, completed = frontend.parse(path, env)
        if completed.returncode != 0:
            sys.stderr.write(
                f"{frontend.name} rejected {path.name} (exit {completed.returncode}):\n"
                f"{completed.stdout.decode(errors='replace')[:2000]}\n"
                f"{completed.stderr.decode(errors='replace')[:2000]}\n"
            )
            sys.exit(1)
        if elapsed > PER_RUN_BUDGET_SECONDS:
            sys.stderr.write(
                f"{frontend.name} took {elapsed:.1f}s on {path.name}; "
                f"the budget is {PER_RUN_BUDGET_SECONDS:.0f}s\n"
            )
            sys.exit(1)
        samples.append(elapsed)
    return statistics.median(samples)


def compile_ruelex(compiler: str, root: Path, env: dict[str, str], out: Path) -> None:
    completed = subprocess.run(
        [compiler, str(root), "-o", str(out)], capture_output=True, env=env
    )
    if completed.returncode != 0:
        sys.stderr.write(
            "compiling ruelex failed:\n"
            f"{completed.stdout.decode(errors='replace')}\n"
            f"{completed.stderr.decode(errors='replace')}\n"
        )
        sys.exit(1)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument(
        "--frontend",
        choices=["both", "production", "ruelex"],
        default="both",
    )
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    compiler = os.environ.get("RUE_BINARY")
    if not compiler:
        sys.stderr.write("RUE_BINARY is not set\n")
        return 2
    env = dict(os.environ)

    with tempfile.TemporaryDirectory(prefix="rue-parser-complexity-") as scratch_name:
        scratch = Path(scratch_name)
        frontends: list[Frontend] = []
        if args.frontend in ("both", "production"):
            frontends.append(Frontend("production", [compiler, "--emit", "ast"]))
        if args.frontend in ("both", "ruelex"):
            ruelex_root = os.environ.get("RUE_RUELEX_ROOT")
            if not ruelex_root:
                sys.stderr.write("RUE_RUELEX_ROOT is not set\n")
                return 2
            ruelex = scratch / "ruelex"
            compile_ruelex(compiler, Path(ruelex_root), env, ruelex)
            frontends.append(Frontend("ruelex", [str(ruelex), "--ast-shape"]))

        failures = 0
        for name, generate, small, large, runs_on in FAMILIES:
            small_path = scratch / f"{name}_{small}.rue"
            large_path = scratch / f"{name}_{large}.rue"
            small_path.write_text(generate(small))
            large_path.write_text(generate(large))
            for frontend in frontends:
                if frontend.name not in runs_on:
                    continue
                small_time = median_parse(frontend, small_path, env)
                large_time = median_parse(frontend, large_path, env)
                ratio = large_time / small_time if small_time > 0 else float("inf")
                verdict = "ok" if ratio <= MAX_TIME_RATIO else "FAIL"
                if verdict == "FAIL":
                    failures += 1
                if not args.quiet or verdict == "FAIL":
                    print(
                        f"{verdict:4} {frontend.name:10} {name:24} "
                        f"{small:>5} -> {large:>5}: "
                        f"{small_time * 1000:7.1f} ms -> {large_time * 1000:7.1f} ms "
                        f"(x{ratio:.1f}, limit x{MAX_TIME_RATIO:.0f})"
                    )
        if failures:
            print(f"{failures} parser complexity gate(s) failed", file=sys.stderr)
            return 1
        if not args.quiet:
            print(f"parser complexity gates passed for {', '.join(f.name for f in frontends)}")
        return 0


if __name__ == "__main__":
    sys.exit(main())
