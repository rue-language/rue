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
//! [`normalize_module_path`] is the one lexical normalizer behind every source
//! spelling in the compiler and its driver: module-registry and file-table
//! keys, the physical paths `SourceMetadata` compares for collision, the
//! requested/canonical identity paths import discovery mints, and the driver's
//! manifest membership keys. Because one function decides, a spelling cannot be
//! declared by one layer and denied by another (RUE-1979).
//!
//! # Policy
//!
//! The reduction is purely lexical — it never touches the filesystem, so it
//! never follows symlinks. `.` components are dropped and a `..` cancels the
//! preceding *normal* component. A `..` with nothing to cancel is decided by
//! whether the path is absolute:
//!
//! - **Absolute**: the root is its own parent, so an uncancelable `..` is
//!   dropped (`/a/../..` and `/../../x` reduce to `/` and `/x`). This matches
//!   POSIX path resolution at the root and `PathBuf::pop`, which is a no-op
//!   there, so a normalized absolute path always stays under its own root and
//!   the physical spelling a host observes agrees with the identity the
//!   compiler mints for it.
//! - **Relative**: a leading `..` is preserved verbatim (`../std/opt.rue`), as
//!   there is no anchor to resolve it against. Callers that must refuse an
//!   escape (a trusted std relative path, a test candidate) check for a
//!   surviving `..` themselves; this function does not judge.
//!
//! The result is idempotent: normalizing a normalized path returns it
//! unchanged.

use std::path::{Component, Path, PathBuf};

/// Lexically normalize a path for equivalence comparison, following the module
/// policy above: drop `.`, cancel `..` against a preceding normal component,
/// then drop an uncancelable `..` on an absolute path and keep one on a
/// relative path. Purely lexical — never touches the filesystem (no symlink
/// resolution).
///
/// Examples (`/` shown for clarity; output uses the platform separator):
/// - `./foo.rue` -> `foo.rue`
/// - `sub/./foo.rue` -> `sub/foo.rue`
/// - `a/../std/opt.rue` -> `std/opt.rue`
/// - `../std/opt.rue` -> `../std/opt.rue` (relative: nothing to cancel)
/// - `/../std/opt.rue` -> `/std/opt.rue` (absolute: the root is its own parent)
pub fn normalize_module_path(path: &str) -> String {
    let path = Path::new(path);
    let absolute = path.is_absolute();
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !absolute {
                    // Relative: a leading `..` (or one behind another `..`)
                    // has no anchor to resolve against, so it stays verbatim.
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
    fn preserves_uncancelable_parent_on_relative_paths() {
        // A leading `..` has no preceding normal component to cancel and no
        // anchor to resolve against, so it survives.
        assert_eq!(n("../std/opt.rue"), "../std/opt.rue");
        assert_eq!(n("../../x.rue"), "../../x.rue");
        assert_eq!(n("a/../../b"), "../b");
    }

    #[test]
    fn drops_uncancelable_parent_on_absolute_paths() {
        // The root is its own parent, so an absolute path can never normalize
        // to a spelling above its own root. Discovery identities, physical
        // collision keys and driver manifest keys all rely on this agreeing.
        assert_eq!(n("/a/../.."), "/");
        assert_eq!(n("/.."), "/");
        assert_eq!(n("/../../x"), "/x");
        assert_eq!(n("/a/b/../../../c"), "/c");
    }

    /// The complete policy table (RUE-1979): every shape the three former
    /// normalizers disagreed on, decided once here.
    #[test]
    fn policy_table() {
        let cases = [
            // absolute: `..` cancels, and the root is its own parent
            ("/a/../..", "/"),
            ("/../../x", "/x"),
            ("/a/../b", "/b"),
            ("/a/b/../../../c", "/c"),
            ("/..", "/"),
            ("/", "/"),
            // relative: an uncancelable `..` stays verbatim
            ("../x", "../x"),
            ("a/../../b", "../b"),
            ("..", ".."),
            // `.` components and separator noise vanish either way
            ("a/./b", "a/b"),
            ("a//b", "a/b"),
            ("/a/./b/", "/a/b"),
            ("/a/b/", "/a/b"),
            ("x/", "x"),
            ("./x", "x"),
            // an empty identity stays empty; callers reject it by name
            ("", ""),
            (".", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(n(input), expected, "normalizing {input:?}");
            assert_eq!(n(expected), expected, "{input:?} normalizes idempotently");
        }
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
}
