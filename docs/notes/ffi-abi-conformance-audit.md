# FFI ABI conformance audit

Status: verified against the compiler and runtime source for RUE-738.

Rue currently has two distinct calling conventions:

1. A private native convention for calls between Rue functions. It borrows
   target register and stack rules but deliberately is not a C ABI.
2. A typed `CallingConvention::TargetC` subset for calls from generated code to
   the bundled runtime. The compiler and runtime share this contract through
   `rue-runtime-abi`.

Rue does not yet support general foreign imports or exports. In particular,
the runtime's zero-argument process entry into Rue `main` is not evidence that
an arbitrary C caller can invoke an arbitrary Rue function. This audit records
the boundary that exists today and the gaps an FFI implementation must close.

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
| Scalar result | `rax` | `x0` | `rax` or `x0`/`w0` |
| Multi-slot result | Up to six slots in `rax`, `rdx`, `rcx`, `r8`, `r9`, `r10` | Up to eight slots in `x0`-`x7` | No direct aggregate results |
| Aggregate result storage | Hidden first ordinary slot (`rdi`) | Hidden first ordinary slot (`x0`) | Explicit first out-pointer parameter where required |
| Aggregate argument order | Logical slots reversed within each value | Logical slots reversed within each value | Natural order; current parameters are scalars and pointers |
| Call-site stack alignment | 16 bytes | 16 bytes | 16 bytes |
| Red zone use | None | None | None |

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

Apple arm64 permits a 128-byte red zone, but Rue does not use it. Apple's ABI
also requires callers to extend arguments narrower than 32 bits, gives stacked
arguments their natural sizes, and places variadic arguments on the stack. The
current target-C subset has no sub-32-bit parameters, stack parameters, or
variadic calls, so these amendments are outside its present surface. Native Rue
continues to use its uniform 8-byte slot model on macOS.

## Typed target-C runtime subset

Every compiler-callable helper is identified by `RuntimeHelperId` and declares
`CallingConvention::TargetC`, ordered physical parameter types, pointer modes,
result behavior, and target availability in `rue-runtime-abi`. Shared runtime
call planning validates the logical signature before either backend assigns
registers. The runtime exports matching `extern "C"` wrappers generated from the
same manifest.

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

Backend tests additionally enforce that the typed runtime manifest remains
target-C and within each backend's implemented register-only subset. This is
not a complete bidirectional C harness because no general foreign-call syntax or
export mode exists yet. Those features should extend the matrix with separately
compiled C caller and callee probes when they introduce their respective
boundaries.

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

Before general FFI can claim conformance, its target-C path must independently
cover at least:

- C layout and classification for supported by-value aggregates;
- target-C stack arguments, including Apple's packed stack-area amendment;
- direct and indirect C aggregate returns, including AAPCS64 `x8`;
- floating-point/vector arguments and results if exposed;
- variadic calls, or an explicit diagnostic rejecting them;
- narrow integer extension at every import and export boundary;
- pointer provenance, mutability, and lifetime rules; and
- executable C-to-Rue and Rue-to-C harnesses for every supported target.

Until those paths exist, native Rue functions and native-layout values are not
an FFI surface.
