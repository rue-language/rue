//! Thread-safe module registry.
//!
//! This module contains [`ModuleRegistry`], the shared registry of imported
//! modules used during semantic analysis. A semantic epoch prepopulates the
//! registry in canonical durable-ID order before analysis begins.
//!
use std::borrow::Cow;
use std::sync::{PoisonError, RwLock};

use crate::types::{ModuleDef, ModuleId};

/// The logical-path namespace every trusted standard-library module carries.
///
/// `rue_compiler::source_identity::TRUSTED_STANDARD_LIBRARY_NAMESPACE` is the
/// authority that mints these identities; AIR sits below that crate, so the
/// spelling is repeated here (as it already is in [`crate::types`]) rather
/// than depended upon. The leading NUL is what keeps the namespace disjoint
/// from every expressible filesystem path — and is exactly why it must never
/// reach a reader.
const TRUSTED_STANDARD_LIBRARY_NAMESPACE: &str = "\0rue-std/";

/// The file stem of the standard library's root module, which a program names
/// as plain `std`.
const STANDARD_LIBRARY_ROOT_STEM: &str = "_std";

/// How a module is named in a diagnostic that talks about it.
///
/// One rendering for every emitter, and it is the name the *source* uses, not
/// the identity the compiler keys on:
///
/// * A user module renders as the import path the source wrote, so
///   ``module `sub/lib.rue` has no member `x` `` says which file was
///   consulted. Rendering only the file stem loses exactly the part that
///   distinguishes same-named files in different directories, which is the
///   case the reader most needs to see.
/// * A trusted standard-library module renders as the dotted path a program
///   writes after `const std = @import("std")` — `std` for the root
///   (`_std.rue`) and `std.arraybuf` for a submodule — which is how the
///   specification and the standard library's own documentation spell them
///   (spec 4.14, `std.arraybuf.ArrayBuf`). Its logical path is
///   `\0rue-std/<name>.rue`, whose leading NUL is a provenance marker rather
///   than anything a reader could type; leaking it produced RUE-2164's
///   ``module `<NUL>rue-std/_std.rue` has no member `io` ``.
///
/// The argument is the import path rather than a [`ModuleDef`] because the
/// emitters hold a module in several shapes — a registry definition, an
/// aggregate module fact, and a durable module identity — and the import path
/// is what they share. Pass [`ModuleDef::import_path`] (or a durable
/// [`ModuleId`]'s logical path); do not re-render it.
pub fn module_display_name(import_path: &str) -> Cow<'_, str> {
    let Some(relative) = import_path.strip_prefix(TRUSTED_STANDARD_LIBRARY_NAMESPACE) else {
        return Cow::Borrowed(import_path);
    };
    let stem = relative.strip_suffix(".rue").unwrap_or(relative);
    if stem == STANDARD_LIBRARY_ROOT_STEM {
        return Cow::Borrowed("std");
    }
    // A nested trusted module (`math/float.rue`) is reached through the same
    // dotted bindings as a flat one, so the separator renders as a dot too.
    Cow::Owned(format!("std.{}", stem.replace('/', ".")))
}

/// The prelude's text free functions (spec 3.7): callable by bare name in any
/// program, with no import and no module path.
const PRELUDE_TEXT_FUNCTIONS: &str = "`print`, `println`, `eprint`, and `eprintln`";

/// The `help:` line E0707 carries when a missing `std` member is really one of
/// the prelude's free functions written as though it lived in a module.
///
/// `std.io.println("x")` is the shape this exists for: there is no `std.io`,
/// and the reader's next question is not "which members does `std` have" but
/// "where is `println`". The answer — nowhere, it needs no path — is the same
/// for the four functions themselves (`std.println`) and for the module a
/// reader reaches for to find them (`std.io`).
pub fn unknown_module_member_help(module_display: &str, member_name: &str) -> Option<String> {
    if module_display != "std" || !reaches_for_prelude_text(member_name) {
        return None;
    }
    Some(format!(
        "{PRELUDE_TEXT_FUNCTIONS} are free functions in the prelude: \
         call `println(x)` directly, with no module path"
    ))
}

/// Is `member` reaching for one of the prelude's text free functions?
///
/// Exactly the four names, the module names a reader expects to find them
/// under, and any near miss that still spells `print` (`printf`,
/// `print_line`, `printLn`).
fn reaches_for_prelude_text(member: &str) -> bool {
    let folded: String = member
        .chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    matches!(folded.as_str(), "io" | "stdio" | "console") || folded.contains("print")
}

/// Build E0707 for `member_name` missing from `module_display`, with the
/// prelude `help:` line attached when it applies.
///
/// The one place the diagnostic is assembled, so no emitter can attach the
/// advice in some positions and not others. `module_display` must already be
/// the [`module_display_name`] rendering; callers that hold a raw import or
/// logical path pass it through that function first.
pub fn unknown_module_member(
    module_display: &str,
    member_name: &str,
    span: rue_span::Span,
) -> rue_error::CompileError {
    let error = rue_error::CompileError::new(
        unknown_module_member_kind(module_display, member_name),
        span,
    );
    match unknown_module_member_help(module_display, member_name) {
        Some(help) => error.with_help(help),
        None => error,
    }
}

/// E0707's payload, for the failure carriers that hold an [`ErrorKind`]
/// without a span of their own and attach the help separately.
///
/// [`ErrorKind`]: rue_error::ErrorKind
pub fn unknown_module_member_kind(module_display: &str, member_name: &str) -> rue_error::ErrorKind {
    rue_error::ErrorKind::UnknownModuleMember {
        module_name: module_display.to_owned(),
        member_name: member_name.to_owned(),
    }
}

/// Thread-safe registry for modules.
///
/// The registry allows concurrent lookups after canonical construction.
#[derive(Debug)]
pub struct ModuleRegistry {
    /// Module definitions indexed by compact, epoch-local ModuleId. Canonical
    /// semantic construction prepopulates this vector in durable-ID order.
    defs: RwLock<Vec<ModuleDef>>,
}

impl ModuleRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            defs: RwLock::new(Vec::new()),
        }
    }

    /// Append a module while constructing a fresh canonical semantic epoch.
    pub(crate) fn push_canonical(&self, def: ModuleDef) -> ModuleId {
        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        let id = ModuleId::new(defs.len() as u32);
        defs.push(def);
        id
    }

    /// Get a module definition by ID.
    pub fn get_def(&self, id: ModuleId) -> ModuleDef {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id.index() as usize)
            .cloned()
            .expect("Invalid ModuleId")
    }

    /// Get the number of modules in the registry.
    pub fn len(&self) -> usize {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{module_display_name as display, unknown_module_member_help as help};

    #[test]
    fn a_user_module_renders_as_the_import_path_it_was_written_with() {
        for path in ["lib.rue", "sub/lib.rue", "../shared/lib.rue", "std.rue"] {
            assert_eq!(display(path), path);
        }
    }

    #[test]
    fn a_trusted_module_renders_as_the_dotted_name_a_program_writes() {
        // The root module's file stem is an implementation detail; a program
        // reaches it as plain `std`.
        assert_eq!(display("\0rue-std/_std.rue"), "std");
        assert_eq!(display("\0rue-std/arraybuf.rue"), "std.arraybuf");
        assert_eq!(display("\0rue-std/binary_heap.rue"), "std.binary_heap");
        // A nested trusted module is reached through the same dotted bindings.
        assert_eq!(display("\0rue-std/math/float.rue"), "std.math.float");
    }

    /// The whole point (RUE-2164): the provenance marker is an identity, and
    /// no rendering may carry it to a reader.
    #[test]
    fn no_trusted_rendering_carries_the_provenance_marker() {
        for path in [
            "\0rue-std/_std.rue",
            "\0rue-std/arraybuf.rue",
            "\0rue-std/math/float.rue",
        ] {
            let rendered = display(path);
            assert!(
                !rendered.contains('\0') && !rendered.contains("rue-std"),
                "{path:?} rendered as {rendered:?}"
            );
        }
    }

    #[test]
    fn the_prelude_help_belongs_to_std_and_to_the_text_functions() {
        for member in ["io", "print", "println", "eprint", "eprintln"] {
            let advice = help("std", member).expect("a prelude miss is advised");
            assert!(advice.contains("`println`") && advice.contains("no module path"));
        }
        // Near misses reach for the same four functions.
        for member in ["printf", "print_line", "printLn", "stdio", "console"] {
            assert!(help("std", member).is_some(), "{member} is a near miss");
        }
        // An ordinary std miss is not about printing.
        for member in ["arraybuf", "nope", "sqrt"] {
            assert!(help("std", member).is_none(), "{member} is not a near miss");
        }
        // And the advice is keyed on the trusted module, not on the spelling
        // of the binding: a user module named `std.rue` is not the prelude.
        assert!(help("std.rue", "println").is_none());
        assert!(help("io.rue", "println").is_none());
        assert!(help("std.arraybuf", "println").is_none());
    }
}
