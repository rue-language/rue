# Per-function string table benchmark

This benchmark isolates the cost of retaining the program-wide string table in
every function object. It was added for RUE-784 and is intentionally generated
outside the normal benchmark manifest because its intermediate-object metric
comes from the compiler's structured `codegen complete` event.

## Workload

The workload has 400 functions. Each function passes a distinct 1,024-byte
literal to `println`, and `main` calls every function. The generated source is
428,023 bytes:

```python
from pathlib import Path

functions = 400
payload = 1024
parts = []
for index in range(functions):
    marker = f"RUE784_{index:04}_"
    literal = marker + chr(ord("a") + index % 26) * (payload - len(marker))
    parts.append(f'fn f{index:04}() -> i32 {{ println("{literal}"); 0 }}\n')

calls = " ".join(f"f{index:04}();" for index in range(functions))
parts.append(f"fn main() -> i32 {{ {calls} 0 }}\n")
Path("/tmp/rue-784-string-heavy.rue").write_text("".join(parts))
```

Build the compiler with `scripts/rue build`, resolve its path with
`scripts/rue-bin`, and warm it with one compilation. Intermediate object bytes
come from this command (the pre-RUE-784 field was named `code_bytes`):

```bash
RUST_LOG=info "$RUE" /tmp/rue-784-string-heavy.rue -o /tmp/rue-784-out
```

Measure wall time and peak RSS with three alternating warmed runs, using the
median:

```bash
/usr/bin/time -l "$RUE" /tmp/rue-784-string-heavy.rue -o /tmp/rue-784-out
stat -f '%z' /tmp/rue-784-out
```

## RUE-784 result

Measured on arm64 macOS 26.5.1. The baseline was commit
`24646460b38f2266897f75ecab04929a9d82f332`; both compilers were built with the
repository's Buck2 release target. Timing and RSS are medians of three warmed,
alternating runs. Byte counts are deterministic single-run results.

| Metric | Baseline | Per-function tables | Reduction |
| --- | ---: | ---: | ---: |
| Compile wall time | 0.43 s | 0.13 s | 69.77% |
| Peak RSS | 914,046,976 B | 30,523,392 B | 96.66% |
| Intermediate object bytes | 169,574,765 B | 657,995 B | 99.61% |
| Final binary size | 164,637,936 B | 477,936 B | 99.71% |

The final binary size is the post-write file size reported by `stat`; it can be
slightly larger than the linker's `output_bytes` event on macOS because the CLI
performs the final platform-specific file preparation after linking.
