//! The one owner of live symbol spelling.
//!
//! A *live* symbol is the presentation-and-join name a callable or nominal
//! carries through AIR and the CFG: what `--emit air` and `--emit cfg` print,
//! what `durable_cfg` keys its symbol mappings on, and what a synthesized drop
//! glue or member callable is looked up by. It is a separate surface from the
//! machine symbol, which `rue-compiler`'s stable encoder owns.
//!
//! Two vocabularies name the same entities: the live type pool during semantic
//! analysis and code generation, and the durable semantic identities the rooted
//! projection spells callables from. Both spell through this module, so a
//! nominal cannot be qualified in one phase and bare in the other, and a member
//! callable cannot pick up a second separator policy.
//!
//! [`crate::drop_glue_names`] owns the `__rue_drop_*` family the same way and
//! builds its nominal fragments from the spellings here.

/// The mangled module component every qualified nominal symbol carries.
///
/// Module paths are normalized before mangling so an aliased or relatively
/// spelled import cannot produce a second component for one module.
pub fn module_symbol_component(logical_module_path: &str) -> String {
    crate::path_norm::mangle_symbol_component(&crate::path_norm::normalize_module_path(
        logical_module_path,
    ))
}

/// The live symbol of a named nominal type.
///
/// Every named user nominal is unconditionally qualified with the module it is
/// declared in (`P$left_2fmodel_2erue`, ADR-0066, RUE-1089): producer-nominal
/// identity makes two same-named types in different files distinct, so their
/// symbols must never depend on whether a collision happened to be observed.
/// `$` cannot appear in a source identifier, so a qualified name can never
/// collide with a real type.
///
/// `keeps_bare_symbol` marks the nominals that pair with a definition named
/// outside this rule and therefore keep their unqualified source name — see
/// [`enum_keeps_bare_symbol`] and the type pool's registry membership tests.
/// An exempt nominal never asks for its module component, which is why that
/// component arrives as a thunk: deriving it costs a path normalization and a
/// mangle, and nominal symbols are spelled once per member call site.
pub fn named_nominal_symbol(
    name: &str,
    keeps_bare_symbol: bool,
    module_component: impl FnOnce() -> String,
) -> String {
    if keeps_bare_symbol {
        return name.to_owned();
    }
    let module_component = module_component();
    let mut symbol = String::with_capacity(name.len() + 1 + module_component.len());
    symbol.push_str(name);
    symbol.push('$');
    symbol.push_str(&module_component);
    symbol
}

/// Whether an enum keeps its bare source name as its live symbol.
///
/// The reserved built-in enums (`Arch`, `Os`, `DataModel`) are injected by the
/// compiler, so their symbols pair with definitions this rule does not spell.
/// The builtin universe owns that membership test and this is its one exported
/// face, so a newly reserved enum cannot be exempt in one phase and qualified
/// in another.
pub fn enum_keeps_bare_symbol(name: &str) -> bool {
    crate::builtin_universe::BuiltinUniverse::builtin_enum_name(name)
}

/// The live symbol of a callable that belongs to a nominal — a method
/// (`P.get`), an associated function (`P::make`), or a destructor
/// (`P.__drop`).
///
/// The separator carries the receiver: `.` when the callable takes `self`,
/// `::` when it does not. Both components are already-rendered symbols, which
/// is what lets a caller that spells several members of one owner render that
/// owner once; the exact final capacity is reserved up front, so joining never
/// reallocates.
pub fn member_callable_name(owner: &str, member: &str, has_self: bool) -> String {
    let separator = if has_self { "." } else { "::" };
    let mut name = String::with_capacity(owner.len() + separator.len() + member.len());
    name.push_str(owner);
    name.push_str(separator);
    name.push_str(member);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_nominals_are_qualified_unless_exempt() {
        assert_eq!(
            named_nominal_symbol("Record", false, || "pkg_2fmain_2erue".to_owned()),
            "Record$pkg_2fmain_2erue"
        );
        assert_eq!(
            named_nominal_symbol("StrBuf", true, || unreachable!(
                "an exempt nominal never derives a module component"
            )),
            "StrBuf"
        );
    }

    #[test]
    fn module_components_normalize_before_mangling() {
        assert_eq!(module_symbol_component("pkg/main.rue"), "pkg_2fmain_2erue");
        assert_eq!(
            module_symbol_component("./pkg/main.rue"),
            module_symbol_component("pkg/main.rue")
        );
    }

    #[test]
    fn only_the_builtin_universe_exempts_an_enum() {
        for reserved in ["Arch", "Os", "DataModel"] {
            assert!(enum_keeps_bare_symbol(reserved));
        }
        assert!(!enum_keeps_bare_symbol("Choice"));
    }

    #[test]
    fn member_separators_carry_the_receiver() {
        assert_eq!(member_callable_name("Owner", "get", true), "Owner.get");
        assert_eq!(member_callable_name("Owner", "make", false), "Owner::make");
        assert_eq!(
            member_callable_name("Owner", "__drop", true),
            "Owner.__drop"
        );
    }

    /// The anonymous owner spelling the member installation loops hoist, with
    /// the three member shapes they install: method, associated function, and
    /// destructor. One rendered owner spells all of them.
    #[test]
    fn member_names_extend_the_rendered_owner_spelling() {
        let owner = "__anon_struct_0123456789abcdef0123456789abcdef";
        assert_eq!(
            member_callable_name(owner, "len", true),
            "__anon_struct_0123456789abcdef0123456789abcdef.len"
        );
        assert_eq!(
            member_callable_name(owner, "make", false),
            "__anon_struct_0123456789abcdef0123456789abcdef::make"
        );
        assert_eq!(
            member_callable_name(owner, "__drop", true),
            "__anon_struct_0123456789abcdef0123456789abcdef.__drop"
        );
    }
}
