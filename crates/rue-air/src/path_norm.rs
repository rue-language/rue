//! Lexical path normalization shared across module resolution.
//!
//! Module identity is keyed by a file's *resolved* path (spec 10.2:4: an import
//! that resolves to an already-loaded file refers to that same module). But the
//! same physical file reaches the resolver under several spellings — a
//! command-line source might be listed as `std/opt.rue` while a `../`-relative
//! `@import("../std/opt.rue")` from a sibling directory resolves to
//! `a/../std/opt.rue`. These must collapse to one key, or the file is
//! double-registered and member access fails (E0707, RUE-317).
//!
//! [`normalize_module_path`] performs that collapse **lexically**: it drops `.`
//! (current-dir) components and resolves `..` against the preceding *normal*
//! component. It never touches the filesystem, so it does not follow symlinks;
//! a leading `..` with nothing to cancel (or a `..` after the root) is
//! preserved verbatim. Every module-registry / file-table key must pass through
//! this one function so all spellings of a file agree.

use std::path::{Component, Path, PathBuf};

/// Lexically normalize a path for equivalence comparison: drop `.` components
/// and collapse `..` against a preceding normal component. Purely lexical —
/// never touches the filesystem (no symlink resolution).
///
/// Examples (`/` shown for clarity; output uses the platform separator):
/// - `./foo.rue` -> `foo.rue`
/// - `sub/./foo.rue` -> `sub/foo.rue`
/// - `a/../std/opt.rue` -> `std/opt.rue`
/// - `../std/opt.rue` -> `../std/opt.rue` (leading `..` has nothing to cancel)
pub fn normalize_module_path(path: &str) -> String {
    let mut out: Vec<Component> = Vec::new();
    for comp in Path::new(path).components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Cancel a preceding *normal* component; otherwise (empty, a
                // leading `..`, or a root/prefix) keep the `..` verbatim so the
                // normalization stays purely lexical.
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    let mut buf = PathBuf::new();
    for comp in out {
        buf.push(comp.as_os_str());
    }
    buf.to_string_lossy().into_owned()
}

/// Encode an arbitrary path as one injective machine-symbol component.
///
/// Rue identifiers contain only ASCII alphanumerics and `_`; encoding every
/// other byte (including `_` itself) as `_xx` keeps the result unambiguous.
/// Paired with [`unmangle_symbol_component`], its exact inverse.
pub fn mangle_symbol_component(component: &str) -> String {
    let mut mangled = String::new();
    for byte in component.bytes() {
        match byte {
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => mangled.push(byte as char),
            _ => {
                use std::fmt::Write as _;
                write!(&mut mangled, "_{byte:02x}").expect("writing to String cannot fail");
            }
        }
    }
    mangled
}

/// Decode a machine-symbol component produced by [`mangle_symbol_component`]
/// back to its original bytes, or `None` when `component` is not a well-formed
/// mangling.
///
/// The encoding is injective: an alphanumeric byte stands for itself and every
/// other byte is `_` followed by exactly two lowercase hex digits (including
/// `_` itself, `_5f`). Decoding is therefore exact — a bare `_` not followed by
/// two hex digits, or bytes that do not form valid UTF-8, could never have been
/// emitted by the encoder, so they fail closed with `None`. Used by the
/// callable-symbol reversal to recover a nominal's defining module path from the
/// file-qualified component of its symbol without re-consulting the type pool.
pub fn unmangle_symbol_component(component: &str) -> Option<String> {
    let bytes = component.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte @ (b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z') => {
                out.push(byte);
                index += 1;
            }
            b'_' => {
                let hex = component.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok().filter(|_| {
                    // Reject upper-case or `+`/`-`-prefixed hex the encoder never
                    // emits, so the round trip is byte-exact in both directions.
                    hex.bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                })?);
                index += 3;
            }
            _ => return None,
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::{mangle_symbol_component as m, normalize_module_path as n};

    #[test]
    fn drops_cur_dir() {
        assert_eq!(n("./foo.rue"), "foo.rue");
        assert_eq!(n("sub/./foo.rue"), "sub/foo.rue");
    }

    #[test]
    fn collapses_parent_dir() {
        // The RUE-317 case: a `../`-relative import from `a/` reconciles with a
        // command-line-listed `std/opt.rue`.
        assert_eq!(n("a/../std/opt.rue"), "std/opt.rue");
        assert_eq!(n("a/b/../../std/opt.rue"), "std/opt.rue");
        assert_eq!(n("a/b/../c.rue"), "a/c.rue");
    }

    #[test]
    fn preserves_uncancelable_parent() {
        // A leading `..` has no preceding normal component to cancel.
        assert_eq!(n("../std/opt.rue"), "../std/opt.rue");
        assert_eq!(n("../../x.rue"), "../../x.rue");
    }

    #[test]
    fn both_spellings_agree() {
        // The two spellings of the same physical file must normalize equal.
        assert_eq!(n("a/../std/opt.rue"), n("std/opt.rue"));
        assert_eq!(n("./std/opt.rue"), n("std/opt.rue"));
    }

    #[test]
    fn symbol_component_mangling_is_injective_for_escaped_bytes() {
        assert_eq!(m("left/shared.rue"), "left_2fshared_2erue");
        assert_eq!(m("a_b"), "a_5fb");
        assert_ne!(m("a/b"), m("a_2fb"));
    }

    #[test]
    fn unmangle_is_the_exact_inverse_of_mangle() {
        use super::unmangle_symbol_component as um;
        for path in [
            "left/shared.rue",
            "a_b",
            "a/b",
            "m.rue",
            "pkg/nested/main.rue",
            "../std/opt.rue",
        ] {
            assert_eq!(um(&m(path)).as_deref(), Some(path), "round trip of {path}");
        }
    }

    #[test]
    fn unmangle_rejects_malformed_components() {
        use super::unmangle_symbol_component as um;
        // A bare `_` with no hex pair, a truncated pair, and an upper-case pair
        // the encoder never emits all fail closed rather than decode wrongly.
        assert_eq!(um("a_"), None);
        assert_eq!(um("a_2"), None);
        assert_eq!(um("a_2E"), None);
        assert_eq!(um("a_zz"), None);
    }
}
