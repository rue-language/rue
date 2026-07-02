---
id: 0035
title: "String model: byte strings (conventionally UTF-8) with loud pragmatism"
status: accepted
tags: [strings, text, unicode, stdlib]
feature-flag:
created: 2026-07-02
accepted: 2026-07-02
implemented:
spec-sections: ["3.7"]
superseded-by:
---

# ADR-0035: String model — byte strings with loud pragmatism

## Status

Accepted — **experimental**. Ratified by Steve on 2026-07-02 with the explicit
framing "this makes me a bit uncomfortable, but I want to see how it feels." It
is adopted to be *tried*, not vowed; if it proves wrong in practice it is
repealed via `superseded-by:`. Documenting it precisely is what makes it
possible to react to.

## Summary

Rue's `String` is a **byte string**: a growable sequence of bytes that is
*conventionally* UTF-8 but **not guaranteed valid**. Byte indexing (`s[i] -> u8`)
and slicing (`s[a..b] -> String`) are unconditional and never trap — a byte
string may legally hold any bytes. The "trap, don't corrupt" discipline that
runs through the rest of Rue applies instead at the **decode boundary** (bytes →
Unicode scalars): `s.chars()` **traps** on invalid UTF-8, and `s.chars_lossy()`
opts into `U+FFFD` substitution. `s.len()` is the byte length.

## Context

Text handling forces one genuinely unresolvable tradeoff and several coupled
decisions, so it does not shard into small independent issues — `len`, indexing,
slicing, iteration, and the "character" type are one decision wearing five hats
(you cannot pick what `len` counts without picking what `s[i]` indexes; they must
agree or you have built a trap). The design space has three axes:

| axis | options |
|---|---|
| validity invariant | guaranteed-valid (Rust `str`, Swift) vs conventionally-UTF-8 bytes (Go, `bstr`) |
| default unit | byte (Go) · scalar (Python, Rust `char`) · grapheme (Swift) |
| indexing | forbidden integer index (Rust `str`, Swift) vs integer-indexable (Go, Python) |

The prior art clusters:

- **Rust** — guaranteed-valid UTF-8; refuses integer indexing (`s[0]` won't
  compile); byte-range slicing that *panics* off a char boundary (to preserve
  the validity invariant); `char` = scalar value; `len` = bytes.
- **Go** — bytes conventionally UTF-8, can hold arbitrary bytes; `s[i]` = O(1)
  byte; `range` yields runes with byte offsets; invalid UTF-8 decodes *silently*
  to `U+FFFD`; `len` = bytes.
- **Swift** — grapheme-native (`Character` = extended grapheme cluster); opaque
  `String.Index` (not `Int`, since graphemes are variable-width so O(1) integer
  indexing is impossible); guaranteed-valid; layered `.utf8`/`.unicodeScalars`
  views.

Two facts drive the decision:

1. **`bstr`'s popularity is evidence.** Rust's guaranteed-valid-UTF-8 invariant
   is the real ergonomics pain — Unix paths (arbitrary bytes), network/file I/O,
   incremental parsing that holds a partial code point. `bstr` (and Go's model)
   drop the invariant and are widely preferred for real text/byte processing.
2. **Rue's charter is explicitly "higher-level ergonomics than Rust/Zig."**
   Copying Rust-purism for strings would adopt the exact friction Rue exists to
   remove. And Rue's `String` is *already* UTF-8 bytes with `len` = bytes, so the
   "bytes are the storage truth" corner is half-committed.

## Decision

Adopt the **byte-string / `bstr`** model with Rue's **"loud pragmatism"** twist.

- **Storage & invariant.** `String` is a growable byte sequence, conventionally
  UTF-8, **not guaranteed valid**. It may hold arbitrary bytes.
- **Byte index / slice are unconditional, no trap.**
  - `s[i] -> u8` (O(1) byte access).
  - `s[a..b] -> String` (byte-range slice). *Any* byte range is valid, because a
    byte string can hold any bytes. This is **simpler than Rust**, which traps on
    a char-boundary slice precisely to protect the validity invariant we are
    dropping.
- **"Trap, don't corrupt" lives at the decode boundary.** Interpreting bytes as
  text is where invalidity is caught, matching every other Rue surface (overflow
  traps, out-of-bounds traps, live-unreachable traps):
  - `s.chars() -> iterator of scalar` **traps** (runtime panic) on an invalid
    UTF-8 sequence. This is Rue's answer to Rust's *forbid* and Go's *silent
    U+FFFD*: you can hold garbage bytes all day, but the moment you interpret
    them as text *without asking for lossiness*, you find out loudly.
  - `s.chars_lossy()` opts into `U+FFFD` substitution — lossiness is **explicit**,
    never the default.
- **Length** is bytes: `s.len() -> u64` (already true; now stated).
- **The "character" type** is the Unicode **scalar value** (a `u32`-backed type),
  produced by `.chars()`. Grapheme clusters are a *later* addition (a
  `.graphemes()` view) and need no re-layout of `String`.
- **Design-light companions** (no unicode commitment; unblock real output):
  int→string conversion, the concatenation operator `+`, and
  `contains`/`starts_with`.

## Implementation Phases (RUE-17)

- [ ] **Phase 1 — design-light:** int→string, `+` concatenation,
  `contains`/`starts_with`. Independent of the model; can land first.
- [ ] **Phase 2 — byte access:** `s[i] -> u8`, `s[a..b] -> String` (unconditional).
- [ ] **Phase 3 — decode boundary:** the scalar type + `s.chars()` (strict, traps)
  and `s.chars_lossy()` (explicit U+FFFD). **Blocked on iterator / `for`-loop
  support** — `.chars()` yields an iterator, which Rue does not yet have. Until
  then, the byte level (Phase 2) is the usable surface; decode arrives with
  iteration. (An index-based `s.decode_at(i) -> (scalar, next)` is a possible
  stopgap but is not the intended shape.)
- [ ] **Phase 4 — later / separate:** grapheme view (`.graphemes()`), formatting /
  interpolation syntax. Not part of this ADR.

## Consequences

### Positive
- Ergonomic: strings hold arbitrary bytes (paths, raw I/O, partial data) and are
  freely byte-indexable and sliceable — the `bstr`/Go affordances.
- Simpler than Rust: no char-boundary trap on slicing (bytes slice anywhere).
- Loud, not silent: invalid UTF-8 is caught at the decode boundary with a trap,
  not silently corrupted (Go/JS) — consistent with Rue's whole personality.
- Consistent with the existing `String` (already UTF-8 bytes, `len` = bytes);
  minimal re-layout.

### Negative
- Drops the guaranteed-valid-UTF-8 invariant: a `String` can hold malformed
  bytes, so any code that *interprets* it as text must be prepared for the decode
  trap (or use `chars_lossy`). Callers carry a little more responsibility than in
  Rust.
- Differs from Rust, which will surprise Rust users (byte indexing exists;
  strings can be invalid).
- Grapheme-correctness is deferred; `.chars()` yields scalars, not human-perceived
  characters, so string length in "characters" is not directly available yet.

### Experimental posture
This is a pre-1.0 experiment. If, in dogfooding, the byte-string reality feels
worse than Rust's guarantees, this ADR is superseded and `String` gains the
validity invariant. The point of writing it down precisely is to have something
concrete to feel and argue with.

## Alternatives Considered

- **Rust (guaranteed-valid `str`, no integer indexing).** Elegant and safe, but
  it is the exact friction Rue's charter targets; `bstr`'s popularity is the
  evidence that even Rust users route around it. Rejected as the *default*, though
  a future opt-in validated-string type is not precluded.
- **Swift (grapheme-native).** The most correct-for-humans model, but it forbids
  O(1) integer indexing (opaque indices), adds real complexity, and would require
  re-laying-out `String` around grapheme segmentation. Deferred: graphemes become
  a *view*, not the default.
- **Go (bytes, but silent lossy decode).** Same storage/indexing choice we took,
  but Go decodes invalid UTF-8 *silently* to `U+FFFD`. Rejected the silence, kept
  the bytes — hence strict `.chars()` + explicit `.chars_lossy()`.
