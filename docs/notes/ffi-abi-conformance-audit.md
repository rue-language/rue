# FFI ABI conformance audit

Status: verified against the compiler and runtime source for RUE-738.

One value type names every calling convention in the tree,
`rue_target::CallingConvention`, and its members are concrete conventions:

| Member | Name (`name()`) | Convention |
| --- | --- | --- |
| `Rue` | `rue` | the private native convention for calls between Rue functions |
| `X86_64SysV` | `x86-64-sysv` | System V AMD64 psABI |
| `Aarch64Aapcs` | `aarch64-aapcs` | AAPCS64 as on Linux |
| `Aarch64AapcsDarwin` | `aarch64-aapcs-darwin` | AAPCS64 with Apple's arm64 amendments |

`"C"` is an alias, not a member. One table, `CallingConvention::c_for_target`,
resolves it from the whole compilation target — `x86-64-linux` to `X86_64SysV`,
`aarch64-linux` to `Aarch64Aapcs`, `aarch64-macos` to `Aarch64AapcsDarwin` —
and every C boundary consults it: `extern` imports and exports, the
compiler-built memory routines, and the typed runtime-helper subset the compiler
and runtime share through `rue-runtime-abi`. The alias is keyed by target rather
than by architecture because the two AArch64 targets share an architecture and
do not share a convention.

An `extern` declaration may also name a C convention outright, writing that row's
name from the middle column above instead of `"C"` (spec 9.3:1b, ADR-0064
Amendment 2). `rue` is not among them: the native convention is not a foreign
boundary, so naming it in an `extern` position describes no crossing.
`rue_target::ForeignAbi` is that surface, and `parse_abi_string` reads the same
name table `--emit abi` prints from, so the two cannot drift. The declaration's
convention is resolved once — when its signature is checked, against the
compilation target — and carried from there: the stable plane's `CallAbiFacts`,
the foreign-symbol map code generation reads, the export thunk's
`ExportSignature`, and the ABI report all take the resolved row rather than
asking the target for its C row again. Because each C row is the convention of
exactly one supported target, an accepted name places values exactly as `"C"`
would on that target; naming a row the target does not implement is E1110
(spec 9.3:1c).

The native `Rue` convention borrows target register and stack rules but
deliberately is not a C ABI.

Every psABI rule the compiler needs from a C row is *data*: `CConventionSpec`,
beside `CallingConvention` in `rue-target`, carries the argument and result
roster sizes for both register banks, where the hidden indirect-result pointer
travels and whether the callee echoes it, the call-boundary stack alignment and
the callee shadow space, how the outgoing argument area is packed, who extends a
narrow integer, and which aggregate rule applies. `docs/runtime-abi.md` tabulates
the three rows. Only the register *names* live in the two code-generation
backends, which map a roster index to a register.

One function reads that data against a type's facts:
`rue_air::lower_c_signature` answers where every argument and the result of a
`"C"` signature lives, and its `LoweredSignature` is consumed by every C
crossing site — the `extern "C"` import planner, the `pub extern "C" fn` export
thunk, and the stable query plane's `compiler.call-abi`. That single answer is
ADR-0064's ratified acceptance criterion (the by-value classifier agreeing across
calls, returns, exports, and callbacks) discharged by construction rather than by
review; `the_c_by_value_classifier_agrees_across_sites_shapes_and_planes` pins it
for every scalar kind, both pointer flavors, and `@repr(c)` struct shapes on the
eightbyte boundaries, on both planes and every convention row. Callbacks do not
exist yet and will be the fourth consumer of the same function.

This audit records the boundary that exists today and the gaps an FFI
implementation must close.

## Normative references

The target-C comparisons use the upstream specifications:

- [System V AMD64 ABI](https://gitlab.com/x86-psABIs/x86-64-ABI/-/jobs/artifacts/master/raw/x86-64-ABI/abi.pdf?job=build)
- [Procedure Call Standard for the Arm 64-bit Architecture (AAPCS64)](https://github.com/ARM-software/abi-aa/blob/main/aapcs64/aapcs64.rst)
- [Writing ARM64 code for Apple platforms](https://developer.apple.com/documentation/xcode/writing-arm64-code-for-apple-platforms)
- [Addressing architectural differences in your macOS code](https://developer.apple.com/documentation/apple-silicon/addressing-architectural-differences-in-your-macos-code)

This is an implementation audit, not a declaration that native Rue values have
C layout. C interoperability must use a target-C signature and an explicitly
supported representation.

## Current convention matrix

| Property | Native Rue: x86-64 Linux | Native Rue: AArch64 Linux/macOS | Current target-C runtime subset |
| --- | --- | --- | --- |
| Integer/pointer argument registers | `rdi`, `rsi`, `rdx`, `rcx`, `r8`, `r9` | `x0`-`x7` | Same target registers |
| Stack arguments | 8-byte slots, first overflow slot nearest return address | 8-byte slots beginning at incoming `sp` | Not implemented; every current helper fits registers |
| Stacked target-C arguments (`extern "C"`) | 8-byte slots | 8-byte slots on Linux; scalars at natural size and alignment, packed, on macOS | Not reached |
| Scalar result | `rax` | `x0` | `rax` or `x0`/`w0` |
| Multi-slot result | Up to six slots in `rax`, `rdx`, `rcx`, `r8`, `r9`, `r10` | Up to eight slots in `x0`-`x7` | No direct aggregate results |
| Aggregate result storage | Hidden first ordinary slot (`rdi`) | Hidden first ordinary slot (`x0`) | Explicit first out-pointer parameter where required |
| Aggregate argument order | Logical slots reversed within each value | Logical slots reversed within each value | Natural order; current parameters are scalars and pointers |
| Call-site stack alignment | 16 bytes | 16 bytes | 16 bytes |
| Red zone use | None | None | None |

The `extern "C"` import and export paths are not this runtime subset: they reach
every placement the C rows describe — stacked scalars and composites, SysV
byval-on-stack, AAPCS64 by-reference copies, register-packed aggregates, and both
indirect-result conventions — through the one lowered signature.

## Native Rue convention

The convention this section and the matrix above describe is the one
[ADR-0084](../designs/0084-native-calling-convention.md) replaces: the native
convention becomes the compilation target's C convention plus a return bank
wider than C's, so the slot flattening, the reversal, and the
hidden-first-ordinary-argument sret below all retire. The description here stays
accurate until that migration's return phase (RUE-2038) lands, and is rewritten
with it.

The canonical native call planner flattens a source value into logical 8-byte
slots. Scalars occupy one slot, each aggregate leaf occupies one slot, zero-size
value parameters occupy none, and `borrow`/`inout` parameters occupy one pointer
slot. This is a compiler representation, not the target C layout algorithm.

This logical 8-byte slot flattening is the *call-ABI value decomposition*, which
ADR-0052 deliberately separates from the *physical memory layout* of a type. The
call convention is preserved unchanged while memory layout migrates to the
compact representation (previewed by the `aggregate_layout` feature; the
canonical native classifier is RUE-976), so the slot counts above stay valid
even after `@size_of`, field offsets, and stack frames adopt natural byte
widths. Physical layout alone never certifies that a value can be passed by
value through this convention.

For a multi-slot value, the call planner reverses that value's slots before
assigning argument locations. The callee reconstructs logical field order. This
means that even an aggregate small enough to remain in registers is not passed
like the corresponding C struct. C ABIs instead classify and place aggregates
according to target layout, padding, and register classes.

On x86-64, the first six slots use `rdi`, `rsi`, `rdx`, `rcx`, `r8`, and `r9`.
Remaining slots are pushed in reverse call order so the first overflow slot is
at `[rbp+16]` in a conventional callee frame, followed by successive 8-byte
slots. On AArch64, the first eight slots use `x0`-`x7`; overflow slots begin at
the incoming `sp` and increase by 8 bytes. Both backends maintain 16-byte stack
alignment at calls.

Native direct aggregate results use logical order, not the argument reversal.
x86-64 extends the result register set to six slots (`rax`, `rdx`, `rcx`, `r8`,
`r9`, `r10`), although System V AMD64 supplies at most two integer eightbytes.
AArch64 extends direct results through `x0`-`x7`, although AAPCS64 integer-like
composite results are limited to `x0` and `x1` before indirect return is needed.

`StrBuf` and sufficiently large structs and arrays use native indirect return.
The caller allocates a 16-byte-aligned buffer and supplies its address as a
hidden first native slot, shifting user arguments. This differs on both targets:

- System V AMD64 uses `rdi` for the hidden address and requires the callee to
  return that address in `rax`; native Rue does not promise the `rax` echo.
- AAPCS64 uses dedicated register `x8` for the indirect-result address; native
  Rue uses `x0`.

Consequently, neither direct aggregate returns nor native indirect returns may
be exposed as C entry points without a target-C lowering path.

### Preserved machine state

The x86-64 allocator uses `rbx` and `r12`-`r15` for values live across calls;
generated prologues save and restore every used callee-saved register along with
`rbp`. It also allocates the caller-saved `r11` to values that survive no call,
which conforms: a caller-saved register carries no obligation across a call
boundary, and a value in one never crosses one. Every other caller-saved
register is reserved for a fixed instruction operand, an ABI position, or
rewrite scratch. The allocator does not use the 128-byte System V red zone. Rue emits no direction-
flag-setting, x87, or MMX operations; a conforming clear direction flag remains
clear on return.

The AArch64 allocator uses `x19`-`x28` for values live across calls and preserves
them along with frame pointer and link register. It also allocates the
caller-saved `x13` and `x14` to values that survive no call; the remaining
caller-saved temporaries `x9`-`x12` and `x15` are rewrite and address scratch.
It avoids the indirect-result register `x8`, the platform register `x18`, and
veneer scratch registers `x16`/`x17`. Vector registers are currently
unused. The stack pointer remains 16-byte aligned, and generated code does not
access memory below `sp`. Condition flags are caller-clobbered.

Apple arm64 permits a 128-byte red zone, but Rue does not use it. Apple's other
amendments are answered by the `Aarch64AapcsDarwin` convention row rather than
by an operating-system test inside a backend:

- **Stacked scalars take their natural size and alignment.** The signature
  lowering packs the outgoing argument area per the convention's
  `stacked_argument_packing`, so on macOS a stacked `i8`,
  `i16`, `i32`, `i64` tail lies at offsets 0, 2, 4, 8 where AAPCS64 and SysV put
  it at 0, 8, 16, 24, and the AArch64 lowerer commits each store at the C width
  (`strb`/`strh`/`str w`/`str`). A stacked composite keeps whole eightbytes at
  8-byte alignment under every row: it crosses through its eightbyte image, and a
  byte-exact copy needs marshaling this path does not have. Apple's byte-exact
  placement of a stacked composite whose size or alignment is not a multiple of
  eight is therefore still open, and is listed below.
- **The caller extends arguments narrower than 32 bits.** Rue's canonical
  64-bit-extension invariant already produces a value satisfying it, so the
  import side needs nothing Darwin-specific; a unit assertion pins that every C
  row asks for the same extension. The export thunk loads every incoming narrow
  value through its canonical extension on every row, because the native body
  needs the canonical 64-bit form, which is stronger than Apple's 32-bit
  guarantee.
- **Variadic arguments go on the stack.** Out of scope: variadic `extern "C"`
  declarations are rejected.

The typed runtime-helper subset still has no sub-32-bit or stack parameters, so
these amendments do not change it today; they govern the `extern "C"` import and
export paths, which do reach stacked arguments and narrow scalars. Native Rue
continues to use its uniform 8-byte slot model on macOS.

## Rue-to-C export thunks

A `pub extern "C" fn` is compiled as an ordinary native body under a mangled
symbol, plus one extra object whose single global symbol is the unmangled C name.
That object's body reads the export's `LoweredSignature` in the callee direction
— the C caller has already put every argument where the lowering says it goes —
and adapts each value to what the native body expects.

The adaptation is small because the compact memory image of a `@repr(c)`
aggregate *is* its C object layout, and the native convention's indirect
transports already move that exact image through memory:

- A by-value aggregate the native classifier rules indirect is handed to the body
  as a pointer to the C caller's own bytes, wherever they are: the saved incoming
  argument registers, the caller's outgoing argument area, or the caller-owned
  copy an AAPCS64 by-reference argument points at. Nothing is repacked.
- A direct native crossing is marshaled leaf by leaf through the compact image
  map — each leaf loaded at its physical width through its canonical extension —
  and a multi-slot value's slots are reversed, which is the native convention's
  rule.
- When both directions are indirect, the C caller's indirect-result storage *is*
  the native body's sret storage, so the result is never copied; SysV's `rax`
  echo is then a reload of the saved pointer. When the native body returns in
  registers, the thunk writes each returned slot into the C image at its own byte
  position, zeroing padding first so the image is deterministic.

The signature classes semantic analysis still rejects for an export are about
identity rather than marshaling: a generic (`comptime`) function has no single C
symbol, a `borrow`/`inout` parameter has no C spelling, and the name `main` is
the program's own entry point (E1106, spec 9.3:6). Aggregate parameters and
returns and parameter lists past the argument-register budget all cross.

## Typed target-C runtime subset

Every compiler-callable helper is identified by `RuntimeHelperId` and declares
ordered physical parameter types, pointer modes, result behavior, and target
availability in `rue-runtime-abi`. A manifest row carries no convention field:
`rue-runtime-abi` is the `no_std`, dependency-free manifest crate (ADR-0055) and
cannot name a `Target`, so it records only that a helper crosses the platform C
boundary and the caller resolves the concrete row through the one `"C"` alias
table. Shared runtime call planning validates the logical signature before
either backend assigns registers. The runtime exports matching `extern "C"`
wrappers generated from the same manifest.

The current physical surface is intentionally smaller than either complete C
ABI:

- direct parameters are `i32`, `i64`, `u32`, `u64`, the 64-bit boolean word,
  and pointers;
- direct results are void, those scalar types, or a pointer;
- no helper accepts or directly returns a by-value aggregate or floating-point
  value;
- aggregate source results use an explicit ordinary first out pointer to a
  canonical `repr(C)` storage shape; and
- no helper is variadic or requires a stack argument. The largest current
  signature has five physical parameters.

The x86-64 lowerer maps those parameters to the System V integer registers and
returns scalars in `rax`. The AArch64 lowerer maps them to `x0`-`x7` and returns
scalars in `x0`/`w0`. Caller-owned aggregate-result storage is rounded to a
16-byte call-frame allocation. When source `i8`/`u8` values feed debugging
helpers, the call adapter sign- or zero-extends them to the manifest's 64-bit
parameter before the C call.

This subset conforms for all signatures currently present. It must not grow by
assuming that arbitrary C signatures share the native Rue slot rules. Backend
unit guards fail if a manifest helper exceeds the current register-only budget;
adding such a helper requires implementing and testing target-C stack placement.

## Reading a placement out of the compiler

`rue --emit abi <root>` prints, for every function the root module reaches, the
convention its signature follows and where each parameter and the result
actually travels — a register by name, a byte offset in the outgoing argument
area, a pointer to a caller-owned copy, or nothing at all for a zero-sized
value. It is the answer to "why is this argument on the stack" and "which
registers carry this return" without reading assembly.

The stage honors `--target`, so a Darwin placement is readable on any host:

```console
rue --emit abi --preview c_ffi --target aarch64-macos main.rue
```

It is evidence rather than commentary because it consumes what code generation
consumes and nothing else: a C boundary's placements come from
`rue_air::lower_c_signature` through the same `ForeignCallInputs` /
`ExportSignature` projections the import lowering and the export thunk build,
and the native side's come from `NativeCallAbi` plus the shared
`assign_abi_slots` / `ReturnPlan` slot plan. Register *names* are asked of the
backend that owns the roster. A `pub extern "C" fn` export prints both halves of
its crossing, so the work its entry thunk performs — the native convention's
reversed aggregate slots against C's ascending eightbytes — is visible side by
side.

One thing the stage cannot show is an FFI-predicate failure beside the function
that caused it: the predicates reject an `extern` or export *signature* while
that signature is resolved, before any body is analyzed, so the compile fails
with the ordinary `E1104` diagnostic — which carries the failing field path and
the reject reason — and no report is printed. `emit.abi` (UI) pins the report
text on all three rows; `cli.emit_pipeline` pins the failure path.

## Executable evidence

`cli.abi_conformance` compiles and independently links one probe for each
supported target: x86-64 Linux, AArch64 Linux, and AArch64 macOS. The matching
native CI host executes it and checks exact output. Foreign targets receive the
CLI harness's structural executable validation.

The probe covers:

- native argument-register exhaustion and native stack arguments;
- a nine-slot native aggregate indirect return;
- values kept live across a target-C runtime call, exercising preserved state;
- signed `i8` and unsigned `u8` extension into 64-bit debug helpers;
- scalar target-C results and pointer/length inputs;
- a five-parameter parse helper with explicit aggregate-result storage; and
- allocation and mutation through the `StrBuf` runtime boundary.

`cli.c_ffi` covers the `extern "C"` import and export paths. Its executing
cases pair a Rue program with a C archive on x86-64 Linux and AArch64 Linux.
`x86_64_linux_aggregate_exports_called_from_c` and its AArch64 pair cover the
export direction where the two conventions disagree most: a C caller passes an
8-byte `@repr(c)` struct by value and receives one back, passes a 24-byte struct
(byval on the stack under SysV, by reference under AAPCS64) and receives one
through caller storage, and calls a nine-scalar export whose tail is stacked
under both rows. The 24-byte case also checks SysV's `rax` echo by reading the
returned struct back *through the returned pointer*, in a hand-written assembly
archive member; AAPCS64 has no echo to check, and its dedicated `x8` path is
exercised by the compiler-generated caller.
`aarch64_macos_narrow_exports_build_under_the_apple_row` compiles and links a
narrow-parameter export for AArch64 macOS, so the Apple row's export thunks are
generated, encoded, and placed in a Mach-O image on every host and executed on
the macOS CI host. That case has no C caller of its own; the generated matrix
below supplies one, and the Apple row's stacked-argument packing is additionally
pinned by unit tests over the shared signature lowering.

### The generated conformance matrix

The cases above are hand-written, and each one's C side is hand-assembled
machine code. That does not scale to a matrix, so
`//crates/rue-c-abi-matrix:c-abi-matrix-test` generates one. At test time it
emits paired C and Rue sources, compiles the C side with the host `cc`, compiles
the Rue side with the real driver, links the two with `--linker cc
--link-archive`, runs the executable, and compares its stdout with checksums the
generator computed from the same table it emitted both sources from.

The grid is shape x position x direction x ABI spelling:

- **Shapes** (20): `i8`, `u8`, `i16`, `u16`, `i32`, `u32`, `i64`, `u64`, `bool`,
  `ptr const u8`, and ten `@repr(c)` structs chosen for the classification
  boundaries — `{u8}`, `{u8,u8}`, `{i32,i32}`, `{i64,u8}`, `{i64,i64}`,
  `{i64,i64,u8}`, `{i64,i64,i64}`, `{u8,i64}`, a nested `{{i32,i32},i64}`, and
  `{[u8;4],i32}`. Floats are still rejected at the boundary; the table is shaped
  so adding them is a table edit.
- **Positions** (5): argument 0, the convention's last argument register, the
  first stack slot, a deep stack slot, and the result type. The indices come
  from `CConventionSpec::gp_argument_registers`, so a convention with a
  different register budget moves them without a source edit. Every other
  argument is an `i64` filler, and every cell also stacks arguments.
- **Directions** (2): import (Rue calls generated C) and export (a generated C
  driver calls a `pub extern` Rue function).
- **ABI spellings** (2): `"C"` and the host row's own name, so the alias and the
  explicit spelling are proven equal by execution rather than by inspection.

Every cell reduces its whole argument list to one `u64` checksum: each filler,
and each *leaf* of the shape separately, contributes its 64-bit pattern times
its own odd multiplier, accumulated with wrapping arithmetic on both sides. A
swapped field, a truncated half, a missing sign extension, or a slot read from
the wrong stack offset changes the sum, and the failure report names the
direction, shape, position, ABI spelling, and function. Return-position cells
also round-trip a seed: the callee answers a deliberately different value when
the seed did not arrive intact, so a broken argument crossing cannot hide behind
a correct result.

That is 400 cells in four generated programs — one per direction and spelling —
which compile, link, and run in about five seconds. The generated C is
freestanding on every row: no headers, no libc, fixed-width typedefs spelled
from the target's data model with `_Static_assert`s holding them, and no
platform conditionals. Linking goes through `cc` because the objects `cc`
produces carry relocation and section kinds the internal linker's static subset
does not promise to handle.

The target is host-only by construction and carries the `rue_platform_native`
label, so the native lanes run it: SysV AMD64 on linux-x64, AAPCS64 on
linux-arm64, and the Apple arm64 row on macos-arm64. A host with no `cc` and
`ar` on `PATH` reports every trial as ignored rather than failing. Run it with
`./buck2 test //crates/rue-c-abi-matrix:c-abi-matrix-test`; `scripts/rue
premerge` includes it, and `scripts/rue quick` deliberately does not.

One gap the grid does *not* close is the open Apple amendment above. Every
filler is an `i64`, so a stacked composite is followed by an 8-byte-aligned
argument that re-aligns the outgoing area, and the difference between Apple's
natural-size footprint and the whole-eightbyte one Rue emits is absorbed rather
than observed. Distinguishing them needs a filler narrower than a slot next to a
stacked composite, which is worth adding with the byte-granular marshaling that
would make it pass.

Backend tests additionally enforce that the typed runtime manifest stays within
each backend's implemented register-only subset and that its boundary resolves
to that backend's C convention row. The generated matrix makes the
C-caller/C-callee matrix two-sided on every supported host, macOS included,
because the macOS lane compiles the same generated C with the host toolchain.

## Finding

The target-C runtime subset had no conformance defect within its current
surface. The audit did find one native return-classification defect:

- [RUE-946](https://linear.app/rue/issue/RUE-946/large-payload-enum-returns-exceed-the-register-budget-and-ice-instead)
  tracked large payload enums. Enum returns were excluded from indirect
  return classification, so a payload wider than six x86-64 slots or eight
  AArch64 slots indexed beyond the backend's return-register table and
  panicked. The fix routes oversized enum returns through the same sret policy
  as structs and arrays.

RUE-946 owns the compiler repair and regression cases. Keeping it separate
prevents an investigative ABI audit from silently becoming a cross-backend
return-representation change.

## Requirements for future FFI work

C layout and classification for supported by-value aggregates, direct and
indirect C aggregate returns including AAPCS64 `x8`, and narrow-integer
extension at every import and export boundary are covered by the one lowered
signature and its executing cases. What remains open:

- Apple's byte-exact placement of a stacked composite argument, which needs
  byte-granular marshaling rather than the whole-eightbyte image stores the
  current path emits;
- floating-point/vector arguments and results, which the boundary still rejects;
- variadic calls, or an explicit diagnostic rejecting them; and
- pointer provenance, mutability, and lifetime rules.

The executable Rue-to-C harness on macOS that this list used to ask for is the
generated conformance matrix above: it runs both directions on every native
host, so the C-caller/C-callee matrix is no longer one-sided there.

Until those paths exist, native Rue functions and native-layout values are not
an FFI surface.
