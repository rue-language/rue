---
id: 0060
title: "Network IO v1: blocking IPv4 TCP in pure Rue"
status: accepted
tags: [stdlib, io, networking, tcp, syscalls, error-handling, ownership]
feature-flag: null
created: 2026-07-17
accepted: 2026-07-17
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-713", "RUE-970", "RUE-982", "RUE-983", "RUE-984", "RUE-985", "RUE-986", "RUE-1701", "ADR-0034", "ADR-0057", "ADR-0059"]
---

# ADR-0060: Network IO v1 — blocking IPv4 TCP in pure Rue

## Status

Accepted. Ratified by Steve on 2026-07-17, resolving the RUE-713 design gate.
The rulings below are settled; this ADR records their boundaries and the
implementation sequence.

RUE-713 builds on decisions already made by ADR-0057: network IO is pure Rue
standard-library code over `@syscall`, failed syscalls expose a normalized
`-errno` contract, and owned file descriptors use a sentinel plus drop-close.
Those choices are inherited, not reopened here. ADR-0059 likewise places byte
order conversion in reusable Rue library code rather than compiler intrinsics.

## Summary

`std.net` v1 provides blocking TCP clients and servers on Linux x86-64 and
aarch64. It supports IPv4 addresses through an extensible public address enum,
marshals kernel socket addresses explicitly through a `SockAddr` abstraction,
and exposes separate owned `TcpListener` and `TcpStream` descriptor types.
Errors are normalized from errno into a target-independent network error enum.
UDP, IPv6, DNS, TLS, macOS, and nonblocking/readiness APIs are outside v1.

## Context

Rue's standard library can perform file IO, but cannot yet connect to a service
or accept a TCP connection. RUE-713 is the design gate for that facility. The
main decisions are the initial protocol and platform scope, a public address
shape that does not fossilize IPv4, and a sound boundary between Rue values and
kernel ABI structures.

Current source and tests constrain this ADR. ADR-0057 and RUE-712 established
`std.fs` as pure Rue over `@syscall`, including errno normalization and the
owned-fd sentinel/drop-close pattern. RUE-945 has since made `@syscall` report
Darwin failures as `-errno`, matching Linux. Some comments in `std.fs` or its
design history may still describe the old carry-flag gap; they are historical,
not evidence that the generic Darwin normalization bug remains. Current source,
tests, and tracker state are authoritative.

That generic fix does not make macOS networking ready. The network syscall ABI,
constants, `sockaddr` layout (including Darwin's `sin_len`), errno mapping, and
native loopback coverage still require target-specific work. RUE-986 owns that
work.

## Decision

### 1. v1 is pure Rue, blocking TCP, on Linux

`std.net` is Rue source over `@syscall`, following ADR-0057 and ADR-0034. v1
targets Linux x86-64 and aarch64 only and provides the syscall operations needed
for a blocking TCP client and server:

- `socket` and `connect` for clients;
- `socket`, `bind`, `listen`, and `accept` for servers;
- `read`, `write`, and `write_all` on connected streams;
- `shutdown` and `close`.

The public API packages those primitives into constructors and methods rather
than exposing raw descriptors. A likely surface is `TcpStream.connect(...)`,
`TcpListener.bind(...)`, and `TcpListener.accept()`. `accept` returns a
`TcpStream`. The exact private syscall-helper names are implementation detail.

Operations block the calling thread. v1 has no UDP, DNS, TLS, nonblocking mode,
polling, or readiness API. UDP is deferred without a current issue;
nonblocking/readiness also needs no issue now. These boundaries keep v1 focused
on the smallest end-to-end client/server facility.

### 2. Public addresses are IPv4 today without being IPv4-shaped forever

The public address API follows Rust's extensible shape: an enum has one IPv4
variant initially, rather than putting four octets directly into every network
operation. Conceptually:

```rue
struct Ipv4Addr { a: u8, b: u8, c: u8, d: u8 }

enum IpAddr {
    V4(Ipv4Addr),
}

struct SockAddr {
    ip: IpAddr,
    port: u16,
}
```

Names may be adjusted to match standard-library conventions, but the contract
is an extensible address enum plus a socket address carrying an address and
port. The top-level connect/bind surface must not be shaped as four IPv4 octets.
RUE-982 owns the IPv4 types and `SockAddr`; RUE-985 tracks adding IPv6.

### 3. `SockAddr` owns explicit kernel marshalling

`SockAddr` is a real abstraction, not an alias for a Rue struct assumed to have
C layout. It centralizes conversion between the public address value and the
byte buffer passed to the kernel. Rue struct layout is not part of the C ABI and
must not accidentally become a socket ABI promise.

On the two v1 targets, Linux `sockaddr_in` is exactly 16 bytes:

| Bytes | Encoding |
| --- | --- |
| 0..2 | native `AF_INET` family field; `02 00` on the supported little-endian Linux targets |
| 2..4 | port as a big-endian `u16` |
| 4..8 | the four IPv4 octets in network order |
| 8..16 | eight zero bytes |

Checked Rue code allocates or prepares a byte buffer, writes each field at its
specified offset, and passes that buffer and its length to `@syscall`. Any
operation that exposes a kernel-filled address performs the inverse decode only
after validating the returned family and length; v1's stream-only `accept` may
pass null address arguments when it does not expose the peer address. The code
does not cast a `SockAddr` or `Ipv4Addr` value to a raw pointer and hope its
physical representation matches the kernel.

This boundary is deliberate isolation. IPv6 has a different structure and
size; macOS IPv4 adds `sin_len` and has different ABI details. Adding either
extends the `SockAddr` marshaller without exposing those kernel layouts through
the public API.

### 4. Byte order is explicit, reusable Rue library code

Network marshalling uses reusable pure Rue helpers whose names state the byte
order at the call site. The port conversion is therefore visibly big-endian;
the family field is visibly native/little-endian for the supported Linux
targets. The helpers are not compiler intrinsics, and they do not use
host-dependent C names such as `htons` that obscure which order is requested.

This follows ADR-0059's rule that endianness policy remains source-defined over
byte primitives. RUE-970, now Todo, owns the reusable helper surface. RUE-982 is
blocked on it so `SockAddr` consumes the canonical helpers rather than growing a
private competing conversion path.

### 5. Listener and stream are distinct owned descriptor types

`TcpListener` and `TcpStream` are separate structs even though each owns an fd.
A listener accepts connections; a stream transfers bytes. `accept` constructs
and returns a `TcpStream`, so operations that require a connected socket are not
available on a listener.

Both types follow `File`'s ownership contract from ADR-0057:

- scope exit closes a live descriptor in a `drop fn`;
- `close(self)` consumes the value and returns a checkable result;
- consuming close first replaces the fd with the `-1` sentinel, preventing the
  consumed value's drop glue from closing it twice.

Where a file and connected stream perform the same kind of IO, method names stay
parallel: `read`, `write`, `write_all`, and `close`. Rue has no traits, so v1
does not invent a common IO trait or a substitute trait mechanism. The two
types implement their small parallel surfaces directly. `TcpStream` additionally
provides `shutdown` with an explicit read/write/both mode.

### 6. Errors are normalized into a network sibling enum

Fallible network operations return `Result(T, NetworkError)` (the final type
name may follow local naming conventions). Like `FileError`, it normalizes
per-target errno values into logical categories and retains the raw value for
the long tail:

```rue
enum NetworkError {
    Unsupported,
    PermissionDenied,
    AddressInUse,
    AddressNotAvailable,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    TimedOut,
    Interrupted,
    WouldBlock,
    InvalidInput,
    Other(i64),
}
```

This list is an ADR-level contract, not a demand for speculative errno coverage:
implementation should map useful, stable logical cases and route all remaining
errors to `Other(i64)`. Raw errno values do not become the public matching API.
For Linux socket sends, `sendto(2)` uses `MSG_NOSIGNAL` so a broken peer is
reported to Rue instead of terminating the process with `SIGPIPE`; both
`ECONNRESET` and `EPIPE` map to `ConnectionReset`.
`WouldBlock` remains available for unusual kernel/configuration behavior and
future evolution even though v1 itself creates blocking sockets.

Linux-only is a fail-closed platform boundary, not permission to issue guessed
Darwin syscalls. On macOS, v1 network constructors return `Unsupported` without
invoking a socket syscall. RUE-986 replaces that branch with the real Darwin
ABI and execution coverage.

## Implementation Phases

The implementation dependency chain is exact:

- [x] **Reusable explicit-endian helpers** — RUE-970
- [x] **IPv4 and `SockAddr` types plus Linux marshalling** — RUE-982, blocked by RUE-970
- [x] **Blocking `TcpListener`/`TcpStream` operations** — RUE-983, blocked by RUE-982
- [ ] **Deterministic real-binary Linux loopback coverage** — RUE-984, blocked by RUE-983

RUE-984 may narrowly extend the CLI harness to coordinate two processes. Tests
must use loopback, request an ephemeral port instead of assuming a fixed free
port, impose bounded timeouts, and clean up both processes and descriptors on
success or failure. This is real compiled-binary coverage, not only unit tests
of marshalling helpers.

RUE-985 (IPv6) and RUE-986 (macOS network ABI and coverage) are deferred bugs
related to RUE-713; neither is in the v1 dependency chain.

## Consequences

### Positive

- Rue programs gain a minimal, useful TCP client and server facility without
  expanding the Rust runtime or its cross-target archive surface.
- Explicit marshalling prevents accidental dependence on Rue struct layout and
  gives IPv6 and macOS one well-defined extension point.
- Address and error APIs are portable shapes rather than Linux integer/layout
  details leaking into user code.
- Separate affine listener and stream types make descriptor ownership and valid
  operations clear, including checked consuming close.

### Negative

- v1 is Linux-only and IPv4-only.
- Syscall numbers, socket constants, errno mappings, and ABI marshalling remain
  target-maintained library data.
- Without traits, `File` and `TcpStream` have deliberately duplicated method
  declarations for their parallel IO operations.
- A blocking-only API cannot serve event loops or high-concurrency servers.

### Neutral

- The design adds no language feature, preview gate, compiler intrinsic, or
  runtime Rust helper.
- DNS, TLS, UDP, IPv6, macOS, and readiness remain separable library work.

## Alternatives Considered

- **Support every current OS first.** Rejected: generic Darwin `@syscall` error
  normalization is fixed by RUE-945, but network-specific constants, layouts,
  errno details, and loopback validation remain substantial. RUE-986 owns that
  work without blocking a coherent Linux v1.
- **Pass Rue structs as C `sockaddr` structures.** Rejected: Rue does not promise
  that source structs have C layout. Explicit byte-buffer marshalling is checked,
  reviewable, and isolates target ABI differences.
- **Make the public API IPv4-shaped.** Rejected: adding IPv6 would then require a
  parallel top-level API or a breaking redesign. An enum permits extension from
  the first release.
- **Put socket helpers in the Rust runtime.** Rejected by ADR-0034 for the same
  reason as file IO: pure Rue avoids multiplying host-only runtime code and
  archive validation. It also follows ADR-0057's canonical path.
- **Abstract `File` and `TcpStream` behind a trait.** Unavailable: Rue has no
  traits. Parallel method names preserve familiarity without inventing a new
  language mechanism in a networking ADR.
- **Include UDP, nonblocking/readiness, DNS, or TLS in v1.** Deferred. Each adds
  a distinct protocol, execution model, resolver, or security surface; none is
  necessary to validate blocking TCP end to end.

## Future Work

- RUE-985: IPv6 address variants and kernel marshalling.
- RUE-986: macOS socket ABI, constants, errno/layout details, `sin_len`, and
  deterministic loopback coverage.
- UDP, nonblocking/readiness, DNS, and TLS after their requirements are settled.

## References

- RUE-713 — network IO design gate
- ADR-0034 — per-target runtime archives and the pure-Rue library boundary
- ADR-0057 / RUE-712 — file IO, normalized errno, and owned-fd precedent
- ADR-0059 — endianness remains explicit source-defined library policy
- RUE-945 — implemented Darwin `@syscall` error normalization
- RUE-970, RUE-982, RUE-983, RUE-984 — v1 implementation chain
- RUE-985, RUE-986 — deferred IPv6 and macOS bugs
