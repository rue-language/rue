# FFI ABI conformance audit

Status: verified against the compiler and runtime source for RUE-738.

One value type names every calling convention in the tree,
`rue_target::CallingConvention`, and its members are concrete conventions:

| Member | Convention |
| --- | --- |
| `Rue` | the private native convention for calls between Rue functions |
| `X86_64SysV` | System V AMD64 psABI |
| `Aarch64Aapcs` | AAPCS64 as on Linux |
| `Aarch64AapcsDarwin` | AAPCS64 with Apple's arm64 amendments |

`"C"` is an alias, not a member. One table, `CallingConvention::c_for_target`,
resolves it from the whole compilation target — `x86-64-linux` to `X86_64SysV`,
`aarch64-linux` to `Aarch64Aapcs`, `aarch64-macos` to `Aarch64AapcsDarwin` —
and every C boundary consults it: `extern "C"` imports and exports, the
compiler-built memory routines, and the typed runtime-helper subset the compiler
and runtime share through `rue-runtime-abi`. The alias is keyed by target rather
than by architecture because the two AArch64 targets share an architecture and
do not share a convention.

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
the macOS CI host. There is no macOS C-caller archive, so the Apple row's
stacked-argument packing is proved by unit tests over the shared signature
lowering rather than by execution.

Backend tests additionally enforce that the typed runtime manifest stays within
each backend's implemented register-only subset and that its boundary resolves
to that backend's C convention row. The C-caller/C-callee matrix is still
one-sided on macOS; a macOS C toolchain probe would close it.

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
- variadic calls, or an explicit diagnostic rejecting them;
- pointer provenance, mutability, and lifetime rules; and
- an executable Rue-to-C harness on macOS, so the C-caller/C-callee matrix is
  two-sided on every supported target.

Until those paths exist, native Rue functions and native-layout values are not
an FFI surface.
