//! The directive vocabulary: the names Rue defines for `@`-directives, the
//! arguments each accepts, and the sites each is honored at (spec 2.5).
//!
//! This module is the single owner of directive spellings. The parser
//! classifies every `@name(args)` against it once, while building the AST, and
//! records the result in [`crate::ast::Directive::kind`] and
//! [`crate::ast::DirectiveArg::value`]. Post-parse validation, RIR lowering,
//! semantic analysis and the compiler session read that typed value instead of
//! re-spelling the name, so a directive or warning name appears as a string
//! exactly once in the compiler.
//!
//! Adding a name is one entry in the [`vocabulary!`] list below. The site and
//! arity tables are exhaustive matches over the enum, so the new entry does not
//! compile until it declares where it is accepted, where it is honored, and how
//! many arguments it takes.

use std::fmt;

/// Declares one closed vocabulary: the variants, their source spellings, the
/// full list and the spelling lookup are generated from a single table, so no
/// half of the mapping can drift from another.
macro_rules! vocabulary {
    (
        $(#[$enum_meta:meta])*
        $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident = $spelling:literal, )+
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $name {
            /// Every spelling the language defines, in specification order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// The source spelling of this name.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $spelling, )+
                }
            }

            /// The name a source spelling denotes, or `None` when the language
            /// defines no such name.
            pub fn from_source(text: &str) -> Option<Self> {
                match text {
                    $( $spelling => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

vocabulary! {
    /// A directive name: the builtin that modifies the item or statement it
    /// precedes (spec 2.5:8).
    DirectiveName {
        /// `@allow(<warning>, ...)` suppresses warnings (spec 2.5:11).
        Allow = "allow",
        /// `@copy` marks a struct a Copy type (spec 2.5:27).
        Copy = "copy",
        /// `@repr(<argument>)` is the representation guarantee marker
        /// (spec 2.5:33, ADR-0064 Amendment 1).
        Repr = "repr",
        /// `@non_exhaustive` opts a public enum into the source compatibility
        /// contract for matches in importing modules.
        NonExhaustive = "non_exhaustive",
    }
}

vocabulary! {
    /// A warning name `@allow` accepts (spec 2.5:13).
    WarningName {
        /// A binding is declared but never used.
        UnusedVariable = "unused_variable",
        /// A function is declared but never called.
        UnusedFunction = "unused_function",
        /// Code that cannot be reached.
        UnreachableCode = "unreachable_code",
    }
}

vocabulary! {
    /// A representation argument `@repr` accepts (spec 2.5:34).
    ReprArg {
        /// The selected target's default C data model.
        C = "c",
    }
}

/// The syntactic construct a directive is attached to.
///
/// A `test "name" { .. }` declaration (ADR-0083 §1) accepts exactly the
/// directives a function accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectiveSite {
    Function,
    Struct,
    Enum,
    Method,
    Const,
    Let,
    Test,
}

impl DirectiveSite {
    /// The plural noun a diagnostic names this site with.
    pub const fn description(self) -> &'static str {
        match self {
            DirectiveSite::Function => "functions",
            DirectiveSite::Struct => "structs",
            DirectiveSite::Enum => "enums",
            DirectiveSite::Method => "methods",
            DirectiveSite::Const => "const declarations",
            DirectiveSite::Let => "let statements",
            DirectiveSite::Test => "test declarations",
        }
    }
}

/// How many arguments a directive takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveArity {
    /// No arguments; any argument is an arity error.
    None,
    /// Exactly one argument drawn from the directive's argument vocabulary.
    ExactlyOne,
    /// Any number of arguments, each drawn from the directive's vocabulary.
    Any,
}

/// A directive argument classified against the vocabulary its directive
/// accepts.
///
/// `Unrecognized` covers a spelling outside that vocabulary, an argument on a
/// directive that takes none, and every argument of an unknown directive.
/// Post-parse validation reports each of those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveArgValue {
    Warning(WarningName),
    Repr(ReprArg),
    Unrecognized,
}

impl DirectiveName {
    /// The sites this directive may be attached to (spec 2.5). Attaching it
    /// anywhere else is a compile-time error.
    pub const fn allowed_sites(self) -> &'static [DirectiveSite] {
        match self {
            // `@allow` scopes to a body (spec 2.5:17, 2.5:19, 2.5:21) or to a
            // single binding (spec 2.5:15).
            DirectiveName::Allow => &[
                DirectiveSite::Function,
                DirectiveSite::Method,
                DirectiveSite::Test,
                DirectiveSite::Let,
            ],
            // spec 2.5:28
            DirectiveName::Copy => &[DirectiveSite::Struct],
            // spec 2.5:34
            DirectiveName::Repr => &[DirectiveSite::Struct],
            DirectiveName::NonExhaustive => &[DirectiveSite::Enum],
        }
    }

    /// The number of arguments this directive takes (spec 2.5).
    pub const fn arity(self) -> DirectiveArity {
        match self {
            // spec 2.5:12 and 2.5:23: one or more warning names.
            DirectiveName::Allow => DirectiveArity::Any,
            // spec 2.5:29
            DirectiveName::Copy => DirectiveArity::None,
            // spec 2.5:34: parameterized, exactly one representation argument.
            DirectiveName::Repr => DirectiveArity::ExactlyOne,
            DirectiveName::NonExhaustive => DirectiveArity::None,
        }
    }

    /// True when this directive may be attached to `site`.
    pub fn accepts_site(self, site: DirectiveSite) -> bool {
        self.allowed_sites().contains(&site)
    }

    /// Classify one argument identifier against this directive's argument
    /// vocabulary.
    pub fn classify_arg(self, text: &str) -> DirectiveArgValue {
        match self {
            DirectiveName::Allow => WarningName::from_source(text)
                .map_or(DirectiveArgValue::Unrecognized, DirectiveArgValue::Warning),
            DirectiveName::Repr => ReprArg::from_source(text)
                .map_or(DirectiveArgValue::Unrecognized, DirectiveArgValue::Repr),
            DirectiveName::Copy | DirectiveName::NonExhaustive => DirectiveArgValue::Unrecognized,
        }
    }
}

impl WarningName {
    /// The sites at which `@allow(<this warning>)` changes what the compiler
    /// reports (spec 2.5:15, 2.5:17, 2.5:19, 2.5:21, 2.5:40).
    ///
    /// A warning named at a site outside this set would be accepted and then
    /// ignored — the failure mode directive validation exists to prevent — so
    /// it is a compile-time error instead.
    pub const fn allowed_sites(self) -> &'static [DirectiveSite] {
        match self {
            // Honored for one binding (spec 2.5:15) and for every binding in a
            // body (spec 2.5:17).
            WarningName::UnusedVariable => &[
                DirectiveSite::Function,
                DirectiveSite::Method,
                DirectiveSite::Test,
                DirectiveSite::Let,
            ],
            // The warning is about a declaration, so only a declaration can
            // allow it (spec 2.5:19).
            WarningName::UnusedFunction => &[
                DirectiveSite::Function,
                DirectiveSite::Method,
                DirectiveSite::Test,
            ],
            // Reachability is a property of a body, not of one binding
            // (spec 2.5:21).
            WarningName::UnreachableCode => &[
                DirectiveSite::Function,
                DirectiveSite::Method,
                DirectiveSite::Test,
            ],
        }
    }

    /// True when `@allow(<this warning>)` is honored at `site`.
    pub fn honored_at(self, site: DirectiveSite) -> bool {
        self.allowed_sites().contains(&site)
    }
}

/// `@allow, @copy, @repr, and @non_exhaustive` — the directive vocabulary as a
/// diagnostic list.
pub fn directive_name_list() -> String {
    oxford_list(
        &DirectiveName::ALL
            .iter()
            .map(|name| format!("@{name}"))
            .collect::<Vec<_>>(),
    )
}

/// `unused_variable, unused_function, unreachable_code` — the warning
/// vocabulary as a diagnostic list.
pub fn warning_name_list() -> String {
    joined(WarningName::ALL.iter().map(|name| name.as_str()))
}

/// `c` — the `@repr` argument vocabulary as a diagnostic list.
pub fn repr_arg_list() -> String {
    joined(ReprArg::ALL.iter().map(|arg| arg.as_str()))
}

/// `functions, methods, and test declarations` — a site set as a diagnostic
/// list.
pub fn site_list(sites: &[DirectiveSite]) -> String {
    oxford_list(
        &sites
            .iter()
            .map(|site| site.description().to_string())
            .collect::<Vec<_>>(),
    )
}

fn joined<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.collect::<Vec<_>>().join(", ")
}

/// Join with commas and a trailing `and`, the phrasing the placement and
/// unknown-name diagnostics read with.
fn oxford_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_round_trips() {
        for name in DirectiveName::ALL {
            assert_eq!(DirectiveName::from_source(name.as_str()), Some(*name));
        }
        for name in WarningName::ALL {
            assert_eq!(WarningName::from_source(name.as_str()), Some(*name));
        }
        for arg in ReprArg::ALL {
            assert_eq!(ReprArg::from_source(arg.as_str()), Some(*arg));
        }
    }

    #[test]
    fn unknown_spellings_classify_as_none() {
        assert_eq!(DirectiveName::from_source("alllow"), None);
        assert_eq!(WarningName::from_source("unused_variabl"), None);
        assert_eq!(ReprArg::from_source("packed"), None);
    }

    #[test]
    fn diagnostic_lists_read_as_prose() {
        assert_eq!(
            directive_name_list(),
            "@allow, @copy, @repr, and @non_exhaustive"
        );
        assert_eq!(
            warning_name_list(),
            "unused_variable, unused_function, unreachable_code"
        );
        assert_eq!(repr_arg_list(), "c");
        assert_eq!(site_list(&[DirectiveSite::Struct]), "structs");
        assert_eq!(
            site_list(WarningName::UnreachableCode.allowed_sites()),
            "functions, methods, and test declarations"
        );
    }

    #[test]
    fn a_warning_is_honored_only_where_its_directive_is_accepted() {
        for warning in WarningName::ALL {
            for site in warning.allowed_sites() {
                assert!(
                    DirectiveName::Allow.accepts_site(*site),
                    "{warning} is honored at a site @allow may not be attached to"
                );
            }
        }
    }

    #[test]
    fn only_unused_variable_is_honored_on_a_let() {
        let honored: Vec<_> = WarningName::ALL
            .iter()
            .filter(|warning| warning.honored_at(DirectiveSite::Let))
            .collect();
        assert_eq!(honored, vec![&WarningName::UnusedVariable]);
    }

    #[test]
    fn argument_vocabularies_follow_the_directive() {
        assert_eq!(
            DirectiveName::Allow.classify_arg("unreachable_code"),
            DirectiveArgValue::Warning(WarningName::UnreachableCode)
        );
        assert_eq!(
            DirectiveName::Repr.classify_arg("c"),
            DirectiveArgValue::Repr(ReprArg::C)
        );
        assert_eq!(
            DirectiveName::Repr.classify_arg("unreachable_code"),
            DirectiveArgValue::Unrecognized
        );
        assert_eq!(
            DirectiveName::Copy.classify_arg("c"),
            DirectiveArgValue::Unrecognized
        );
    }
}
