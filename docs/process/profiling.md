# Profiling Rue executables: the symbolized build workflow

The default Rue build links with the internal linker, which deliberately emits
a minimal executable image: no section table, no symbol table, no debug or
unwind sections. That is the right default for a fast, reproducible build, but
a native profiler can only show raw addresses in it, and size-attribution
tools have nothing to attribute.

The supported workflow for a profilable executable is an explicit
system-linker build (RUE-1173):

```bash
RUE="$(scripts/rue-bin)"
"$RUE" prog.rue -O2 -o prog --linker cc
```

`--linker cc` intentionally selects the system-linker path. Rue emits one
object per function, and each object defines that function's stable semantic
symbol; a system linker carries those symbols into the executable's symbol
table (ELF `.symtab` on Linux, Mach-O `LC_SYMTAB` on macOS), where profilers,
debuggers, and size tools resolve them. `-O2` is not required for symbols but
makes the measured code representative.

This works on every supported platform with a C toolchain installed:
x86-64 Linux and AArch64 Linux (`cc` from gcc or clang), and AArch64 macOS
(`cc` from the Xcode command-line tools).

## Verifying the symbols

```bash
nm prog | grep __rue_sem_v1_    # user functions
nm prog | grep __rue_           # runtime helpers too
```

CI pins both halves of this contract: the `cli.profiling` cases
(`crates/rue-cli-tests/cases/profiling.toml`, run with
`scripts/rue cli profiling`) assert that a `--linker cc` build's symbol table
names user functions, the entry point, and runtime helpers — and that the
default internal-linker build stays unsymbolized. If either linker path
changes shape, those cases and this document must move together.

## Running a profiler

Both backends maintain frame pointers (`rbp` / `x29`), so frame-pointer call
graphs work. DWARF-based unwinding does not — there is no debug or unwind
information to drive it.

Linux:

```bash
perf record --call-graph fp ./prog
perf report
```

macOS: use Instruments' Time Profiler or `samply record ./prog`; both resolve
the same symbol table.

## Reading the symbol names

- **User functions** appear as stable semantic symbols:
  `__rue_sem_v1_..._s<len>_<hex>...`. Each `s<len>_<hex>` frame is the
  hex-encoded UTF-8 of a path or name component; `s8_686f745f6c6f6f70`
  decodes to `hot_loop` (`echo 686f745f6c6f6f70 | xxd -r -p`). A
  human-readable user-symbol mangling scheme is a separately tracked design
  (RUE-178).
- **`main`** is the Rue program's entry function.
- **Runtime helpers** keep their readable ABI names (`__rue_alloc`,
  `__rue_println`, ...). Runtime internals are Rust-mangled `_ZN...` names;
  pipe through `rustfilt` if you need them readable.

## Limitations

- **Function-level attribution only.** No DWARF is emitted, so there are no
  source lines, no variable info, and no source-annotated views in profilers
  or debuggers.
- **No unwind tables.** Rue objects carry no `.eh_frame`/compact-unwind data;
  use frame-pointer call graphs (`--call-graph fp`), not DWARF unwinding.
- **Mangled user names.** Function identity is precise but hex-framed until
  RUE-178 lands.
- **A C toolchain is required** on the build host, and the linked output
  varies with that toolchain. Keep the internal linker for reproducibility
  comparisons; use this workflow for investigation builds.

## Use in performance investigation

The compiler-performance system (ADR-0067) tracks output binary size and
compile phases. When a question turns to the emitted executable itself —
size composition, generated-code hot spots, where a workload's time goes at
run time — build the workload with this workflow and profile or run `nm`/size
tools over the symbolized output. Default benchmark builds keep the internal
linker; symbolized builds are for investigation, not for the measured series.
