//! Semantic review gate for the curated compiler facade.
//!
//! RUE-869 replaces the old source-line budget with an exact inventory of
//! every root export and every public `CompilerSession`/
//! `CompilerSessionUpdate` signature. The inventory records ownership,
//! stability, category, and approved consumers so intentional API changes are
//! reviewed as semantic one-line diffs.
#[cfg(test)]
mod comptime_public_contract_tests {
    use rue_air::{
        ComptimeAnonymousKind, ComptimeArgMode, ComptimeCallAdmission, ComptimeCallArgument,
        ComptimeCallKey, ComptimeCallMemoLookup, ComptimeCompletedCallMemo, ComptimeDiagnosticSite,
        ComptimeEngine, ComptimeEnv, ComptimeField, ComptimeFile, ComptimeFrame, ComptimeHost,
        ComptimeHostResult, ComptimeIdentity, ComptimeMemoizedOutcome, ComptimeName,
        ComptimeOutcome, ComptimeProgram, ComptimeProgramKey, ComptimeProgramRegistry,
        ComptimeType, ComptimeValue,
    };
    use rue_rir::{Inst, InstData, InstRef, RirEditor, RirValidationContext, ValidatedRir};
    use rue_span::Span;
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeType;
    impl ComptimeType for FakeType {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum FakeValue {
        Integer(i128),
        Boolean(bool),
        Unit,
        Type(FakeType),
    }
    impl ComptimeValue for FakeValue {
        type Type = FakeType;
        fn integer(value: i128) -> Self {
            Self::Integer(value)
        }
        fn boolean(value: bool) -> Self {
            Self::Boolean(value)
        }
        fn unit() -> Self {
            Self::Unit
        }
        fn type_value(value: <Self as ComptimeValue>::Type) -> Self {
            Self::Type(value)
        }
        fn as_integer(&self) -> Option<i128> {
            match self {
                Self::Integer(value) => Some(*value),
                _ => None,
            }
        }
        fn as_boolean(&self) -> Option<bool> {
            match self {
                Self::Boolean(value) => Some(*value),
                _ => None,
            }
        }
        fn as_type(&self) -> Option<<Self as ComptimeValue>::Type> {
            match self {
                Self::Type(value) => Some(value.clone()),
                _ => None,
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeName;
    impl Hash for FakeName {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0_u8.hash(state);
        }
    }
    impl ComptimeName for FakeName {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeFile;
    impl Hash for FakeFile {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0_u8.hash(state);
        }
    }
    impl ComptimeFile for FakeFile {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeIdentity;
    impl ComptimeIdentity for FakeIdentity {}

    #[allow(dead_code)]
    fn generic_engine_entry<H: ComptimeHost>(
        host: &mut H,
        program: H::ProgramKey,
        root: InstRef,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let _checkpoint: ComptimeHostResult<(), H::Failure> = host.check_canceled();
        let mut env =
            ComptimeEnv::<H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>::new();
        ComptimeEngine::new(host).evaluate(ComptimeFrame::expression(program, root), &mut env)
    }

    #[allow(dead_code)]
    fn generic_call_argument_contract<V>(argument: &ComptimeCallArgument<V>) {
        let _value = argument.value();
        let _direct_unit_literal = argument.is_direct_unit_literal();
    }

    #[allow(dead_code)]
    fn generic_diagnostic_site_contract<P>(site: &ComptimeDiagnosticSite<P>) {
        let _program = site.program();
        let _span = site.span();
    }

    #[test]
    fn public_domain_and_engine_contract_is_instantiable() {
        let _env: ComptimeEnv<'static, FakeValue, FakeType, FakeName, FakeFile, FakeIdentity> =
            ComptimeEnv::new();
        let _frame = ComptimeFrame {
            program: (),
            body: InstRef::from_raw(0),
            name: Some(FakeName),
            context: Some(FakeFile),
            span: Span::new(0, 0),
            function_span: Span::new(0, 0),
            type_bindings: ahash::AHashMap::<FakeName, FakeType>::new(),
            value_bindings: ahash::AHashMap::<FakeName, FakeValue>::new(),
            name_bindings: ahash::AHashMap::<FakeName, FakeName>::new(),
            call_identity: Some(FakeIdentity),
            expected_result: None,
        };
        let _field = ComptimeField {
            name: FakeName,
            ty: FakeType,
        };
        let _named_value_resolution = rue_air::ComptimeNamedValueResolution::Known(FakeValue::Unit);
        let _admission = ComptimeCallAdmission {
            name: FakeName,
            payload: (),
        };
        let _arg_mode: ComptimeArgMode = (rue_rir::RirArgMode::Normal, Span::new(0, 0));
        let _anonymous_kind = ComptimeAnonymousKind::Struct;
    }

    #[test]
    fn public_program_admission_contract_is_instantiable() {
        type Registry = ComptimeProgramRegistry<u8, u8, u8, u8>;
        type Memo = ComptimeCompletedCallMemo<u8, u8, u8, u8, u8>;
        let program_key = ComptimeProgramKey {
            declaration: 1,
            configuration: 2,
        };
        let mut registry = Registry::new();
        assert!(registry.get(&program_key).is_none());
        let mut editor = RirEditor::new();
        let body = editor.add_inst(Inst {
            data: InstData::IntConst(7),
            span: Span::new(0, 0),
        });
        let context = RirValidationContext {
            symbol_count: 0,
            source_lengths: &[(rue_span::FileId::DEFAULT, 1)],
        };
        let program = ComptimeProgram {
            rir: Arc::new(ValidatedRir::finish(editor, &context).unwrap()),
            symbols: Arc::from([]),
            imports: 0_u8,
        };
        registry.register(program_key.clone(), program).unwrap();
        assert_eq!(
            registry.get(&program_key).unwrap().rir.get(body).data,
            InstData::IntConst(7)
        );

        let call_key = ComptimeCallKey {
            declaration: 1,
            configuration: 2,
            type_arguments: std::sync::Arc::from([3]),
            value_arguments: std::sync::Arc::from([4]),
        };
        let mut memo = Memo::new();
        assert!(matches!(
            memo.lookup(&call_key),
            ComptimeCallMemoLookup::Miss
        ));
        memo.insert(call_key.clone(), ComptimeMemoizedOutcome::NotReady)
            .unwrap();
        assert!(matches!(
            memo.lookup(&call_key),
            ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::NotReady)
        ));
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }
}

// The hub owns the aggregate used by cross-phase source-shape tests. Keeping
// one authority prevents this inventory from silently drifting from the
// source consumed by the revisioned-database tests.
use crate::revisioned_query_database::{REGISTRATION_MANIFEST, REVISIONED_DATABASE_SOURCE};
use crate::session::SESSION_PRODUCTION_SOURCE;

const REVISIONED_DATABASE_PHASES: &[(&str, &str)] = &[
    (
        "shared",
        include_str!("revisioned_query_database/shared.rs"),
    ),
    (
        "backend",
        include_str!("revisioned_query_database/backend.rs"),
    ),
    (
        "parse_import",
        include_str!("revisioned_query_database/parse_import.rs"),
    ),
    (
        "parse_import_program_assembly",
        include_str!("revisioned_query_database/parse_import/program_assembly.rs"),
    ),
    (
        "semantic",
        include_str!("revisioned_query_database/semantic.rs"),
    ),
    ("body", include_str!("revisioned_query_database/body.rs")),
    (
        "body_closure_nucleus",
        include_str!("revisioned_query_database/body/closure_nucleus.rs"),
    ),
    (
        "body_durable_comptime_adapters",
        include_str!("revisioned_query_database/body/durable_comptime_adapters.rs"),
    ),
    (
        "body_provider_body",
        include_str!("revisioned_query_database/body/provider_body.rs"),
    ),
    (
        "body_revision_symbol_space",
        include_str!("revisioned_query_database/body/revision_symbol_space.rs"),
    ),
    (
        "body_transactions",
        include_str!("revisioned_query_database/body/transactions.rs"),
    ),
    (
        "registrations",
        include_str!("revisioned_query_database/registrations.rs"),
    ),
    (
        "provider",
        include_str!("revisioned_query_database/provider.rs"),
    ),
    (
        "test_support",
        include_str!("revisioned_query_database/test_support.rs"),
    ),
];

const REVISIONED_DATABASE_REGISTRATION_MODULES: &[(&str, &str)] = &[
    (
        "registrations_backend",
        include_str!("revisioned_query_database/registrations/backend.rs"),
    ),
    (
        "registrations_body",
        include_str!("revisioned_query_database/registrations/body.rs"),
    ),
    (
        "registrations_parse_import",
        include_str!("revisioned_query_database/registrations/parse_import.rs"),
    ),
    (
        "registrations_provider",
        include_str!("revisioned_query_database/registrations/provider.rs"),
    ),
    (
        "registrations_semantic",
        include_str!("revisioned_query_database/registrations/semantic.rs"),
    ),
];

fn rust_char_literal_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    assert_eq!(bytes.get(start), Some(&b'\''));
    let mut cursor = start + 1;
    while cursor < bytes.len() && !matches!(bytes[cursor], b'\n' | b'\r') {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'\'' => {
                let contents = &source[start + 1..cursor];
                let is_character = contents.starts_with('\\') || contents.chars().count() == 1;
                return is_character.then_some(cursor + 1);
            }
            _ => cursor += 1,
        }
    }
    None
}

/// Preserve code tokens while masking comments, strings, and character
/// literals. Source authority inventories use this instead of raw substring
/// counts so a test fixture, diagnostic, or ownership comment cannot
/// impersonate production runtime construction.
fn rust_code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut code = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            let start = index;
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            code[start..index].fill(b' ');
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            let start = index;
            let mut depth = 1usize;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            code[start..index].fill(b' ');
            continue;
        }
        if bytes[index] == b'\''
            && let Some(end) = rust_char_literal_end(source, index)
        {
            code[index..end].fill(b' ');
            index = end;
            continue;
        }
        if bytes[index] == b'r' {
            let mut quote = index + 1;
            while quote < bytes.len() && bytes[quote] == b'#' {
                quote += 1;
            }
            if quote < bytes.len() && bytes[quote] == b'"' {
                let hashes = quote - index - 1;
                let start = index;
                index = quote + 1;
                while index < bytes.len() {
                    if bytes[index] == b'"'
                        && bytes
                            .get(index + 1..index + 1 + hashes)
                            .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
                    {
                        index += 1 + hashes;
                        break;
                    }
                    index += 1;
                }
                code[start..index].fill(b' ');
                continue;
            }
        }
        if bytes[index] == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index = (index + 2).min(bytes.len()),
                    b'"' => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
            code[start..index].fill(b' ');
            continue;
        }
        index += 1;
    }
    String::from_utf8(code).expect("masking preserves UTF-8 byte boundaries")
}

fn code_identifier_count(source: &str, expected: &str) -> usize {
    let code = rust_code_only(source);
    code_identifiers(&code)
        .into_iter()
        .filter(|identifier| *identifier == expected)
        .count()
}

/// Count one exact function identifier as `(definitions, calls, references)`.
/// References include function-item/turbofish spellings, so converting a
/// reviewed direct call into an alias cannot disappear from the inventory.
fn function_identifier_usage(source: &str, expected: &str) -> (usize, usize, usize) {
    let code = rust_code_only(source);
    let bytes = code.as_bytes();
    let is_identifier_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut definitions = 0;
    let mut calls = 0;
    let mut references = 0;
    for (start, _) in code.match_indices(expected) {
        let identifier_start = if start >= 2 && bytes.get(start - 2..start) == Some(b"r#") {
            start - 2
        } else {
            start
        };
        let end = start + expected.len();
        if identifier_start
            .checked_sub(1)
            .and_then(|before| bytes.get(before))
            .is_some_and(|byte| is_identifier_byte(*byte))
            || bytes.get(end).is_some_and(|byte| is_identifier_byte(*byte))
        {
            continue;
        }

        let mut previous_end = identifier_start;
        while previous_end > 0 && bytes[previous_end - 1].is_ascii_whitespace() {
            previous_end -= 1;
        }
        let mut previous_start = previous_end;
        while previous_start > 0 && is_identifier_byte(bytes[previous_start - 1]) {
            previous_start -= 1;
        }
        if &code[previous_start..previous_end] == "fn" {
            definitions += 1;
            continue;
        }

        let mut next = end;
        while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
            next += 1;
        }
        if bytes.get(next) == Some(&b'(') {
            calls += 1;
        } else {
            references += 1;
        }
    }
    (definitions, calls, references)
}

/// Tokenize masked Rust code while retaining path and mutation punctuation.
/// Raw identifiers are normalized to their ordinary spelling.
fn rust_code_tokens(source: &str) -> Vec<String> {
    let code = rust_code_only(source);
    let bytes = code.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let raw_identifier = bytes[cursor..].starts_with(b"r#")
            && bytes
                .get(cursor + 2)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_');
        if raw_identifier {
            cursor += 2;
        }
        if bytes[cursor].is_ascii_alphabetic() || bytes[cursor] == b'_' {
            let start = cursor;
            cursor += 1;
            while bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                cursor += 1;
            }
            tokens.push(code[start..cursor].to_owned());
            continue;
        }
        if let Some(operator) = [
            "<<=", ">>=", "::", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "==", "!=", "=>",
            "->", "..",
        ]
        .into_iter()
        .find(|operator| code[cursor..].starts_with(operator))
        {
            tokens.push(operator.to_owned());
            cursor += operator.len();
            continue;
        }
        tokens.push(char::from(bytes[cursor]).to_string());
        cursor += 1;
    }
    tokens
}

fn rust_alias_closure(
    tokens: &[String],
    root: &str,
    include_type_aliases: bool,
) -> std::collections::BTreeSet<String> {
    let mut aliases = std::collections::BTreeSet::from([root.to_owned()]);
    loop {
        let mut changed = false;
        for (use_index, token) in tokens.iter().enumerate() {
            if token != "use" {
                continue;
            }
            let end = tokens[use_index + 1..]
                .iter()
                .position(|token| token == ";")
                .map_or(tokens.len(), |offset| use_index + 1 + offset);
            for as_index in use_index + 1..end {
                if tokens[as_index] != "as" {
                    continue;
                }
                let Some(target) = tokens.get(as_index + 1) else {
                    continue;
                };
                let source = tokens[use_index + 1..as_index]
                    .iter()
                    .rev()
                    .find(|candidate| {
                        candidate
                            .as_bytes()
                            .first()
                            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                    });
                if source.is_some_and(|source| aliases.contains(source))
                    && target != "_"
                    && aliases.insert(target.clone())
                {
                    changed = true;
                }
            }
        }
        if include_type_aliases {
            for (type_index, token) in tokens.iter().enumerate() {
                if token != "type" {
                    continue;
                }
                let Some(target) = tokens.get(type_index + 1) else {
                    continue;
                };
                let Some(equal) = (type_index + 2..tokens.len())
                    .find(|index| matches!(tokens[*index].as_str(), "=" | ";"))
                    .filter(|index| tokens[*index] == "=")
                else {
                    continue;
                };
                let end = tokens[equal + 1..]
                    .iter()
                    .position(|token| token == ";")
                    .map_or(tokens.len(), |offset| equal + 1 + offset);
                if tokens[equal + 1..end]
                    .iter()
                    .any(|source| aliases.contains(source))
                    && aliases.insert(target.clone())
                {
                    changed = true;
                }
            }
        }
        if !changed {
            return aliases;
        }
    }
}

fn revisioned_database_aliases(tokens: &[String]) -> std::collections::BTreeSet<String> {
    rust_alias_closure(tokens, "RevisionedQueryDatabase", true)
}

fn default_trait_aliases(tokens: &[String]) -> std::collections::BTreeSet<String> {
    rust_alias_closure(tokens, "Default", false)
}

fn revisioned_database_impl_ranges(
    tokens: &[String],
    aliases: &std::collections::BTreeSet<String>,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for (impl_index, token) in tokens.iter().enumerate() {
        if token != "impl" {
            continue;
        }
        let Some(open) = (impl_index + 1..tokens.len())
            .find(|index| matches!(tokens[*index].as_str(), "{" | ";"))
            .filter(|index| tokens[*index] == "{")
        else {
            continue;
        };
        let header = &tokens[impl_index + 1..open];
        let owner_start = header
            .iter()
            .rposition(|token| token == "for")
            .map_or(0, |index| index + 1);
        if !header[owner_start..]
            .iter()
            .any(|token| aliases.contains(token))
        {
            continue;
        }
        ranges.push((open, matching_token_delimiter(tokens, open, "{", "}")));
    }
    ranges
}

fn inherent_type_impl_ranges(
    tokens: &[String],
    aliases: &std::collections::BTreeSet<String>,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for (impl_index, token) in tokens.iter().enumerate() {
        if token != "impl" {
            continue;
        }
        let Some(open) = (impl_index + 1..tokens.len())
            .find(|index| matches!(tokens[*index].as_str(), "{" | ";"))
            .filter(|index| tokens[*index] == "{")
        else {
            continue;
        };
        let header = &tokens[impl_index + 1..open];
        let trait_impl = header
            .iter()
            .take_while(|token| token.as_str() != "where")
            .any(|token| token == "for");
        if trait_impl || !header.iter().any(|token| aliases.contains(token)) {
            continue;
        }
        ranges.push((open, matching_token_delimiter(tokens, open, "{", "}")));
    }
    ranges
}

/// Inventory explicit database Default references. In revisioned database
/// implementation sources, `include_short_forms` additionally treats
/// `Self::default` and `Default::default` as database construction authority.
fn revisioned_database_default_references(source: &str, include_short_forms: bool) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let aliases = revisioned_database_aliases(&tokens);
    let default_aliases = default_trait_aliases(&tokens);
    let database_impl_ranges = revisioned_database_impl_ranges(&tokens, &aliases);
    let mut references = Vec::new();
    for index in 2..tokens.len() {
        if tokens[index] != "default" || tokens[index - 1] != "::" {
            continue;
        }
        let owner = &tokens[index - 2];
        let in_database_impl = database_impl_ranges
            .iter()
            .any(|(open, close)| *open < index && index < *close);
        let qualified_owners = if owner == ">" {
            let mut depth = 0usize;
            let mut angle = None;
            for candidate in (0..index - 2).rev() {
                match tokens[candidate].as_str() {
                    ">" => depth += 1,
                    "<" if depth == 0 => {
                        angle = Some(candidate);
                        break;
                    }
                    "<" => depth -= 1,
                    _ => {}
                }
            }
            angle.map(|angle| {
                let qualification = &tokens[angle + 1..index - 2];
                let has_default_trait = qualification.iter().any(|token| token == "as")
                    && qualification
                        .iter()
                        .any(|token| default_aliases.contains(token));
                let names_revisioned_database = has_default_trait
                    && (qualification.iter().any(|token| aliases.contains(token))
                        || (in_database_impl && qualification.iter().any(|token| token == "Self")));
                let names_short_self = has_default_trait
                    && include_short_forms
                    && qualification.iter().any(|token| token == "Self");
                (names_revisioned_database, names_short_self)
            })
        } else {
            None
        };
        let normalized_owner = if aliases.contains(owner)
            || qualified_owners.is_some_and(|(revisioned, _)| revisioned)
        {
            "RevisionedQueryDatabase"
        } else if in_database_impl && default_aliases.contains(owner) {
            "RevisionedQueryDatabase"
        } else if qualified_owners.is_some_and(|(_, short_self)| short_self) {
            "Self"
        } else if include_short_forms && owner == "Self" {
            owner
        } else if include_short_forms && default_aliases.contains(owner) {
            "Default"
        } else {
            continue;
        };
        let use_kind = if tokens.get(index + 1).is_some_and(|token| token == "(") {
            "call"
        } else {
            "reference"
        };
        references.push(format!("{normalized_owner}:{use_kind}"));
    }
    references
}

fn database_construction_owner_inventory(
    sources: &[(String, &str)],
    include_short_forms: bool,
) -> Vec<(String, String, usize)> {
    let mut occurrences = sources
        .iter()
        .flat_map(|(owner, source)| {
            revisioned_database_default_references(source, include_short_forms)
                .into_iter()
                .map(move |reference| (owner.clone(), reference))
        })
        .collect::<Vec<_>>();
    occurrences.sort();
    let mut inventory: Vec<(String, String, usize)> = Vec::new();
    for (owner, reference) in occurrences {
        if let Some((last_owner, last_reference, count)) = inventory.last_mut()
            && *last_owner == owner
            && *last_reference == reference
        {
            *count += 1;
        } else {
            inventory.push((owner, reference, 1));
        }
    }
    inventory
}

/// Inventory explicit references to one type-owned method. Raw identifiers,
/// qualified paths, type/use aliases, `Self` within an impl, and function-item
/// references all retain authority.
fn type_method_references(source: &str, type_name: &str, method: &str) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let aliases = rust_alias_closure(&tokens, type_name, true);
    let type_impl_ranges = revisioned_database_impl_ranges(&tokens, &aliases);
    let mut references = Vec::new();
    for index in 2..tokens.len() {
        if tokens[index] != method || tokens[index - 1] != "::" {
            continue;
        }
        let owner = &tokens[index - 2];
        let in_type_impl = type_impl_ranges
            .iter()
            .any(|(open, close)| *open < index && index < *close);
        let qualified_type = if owner == ">" {
            let mut depth = 0usize;
            let mut angle = None;
            for candidate in (0..index - 2).rev() {
                match tokens[candidate].as_str() {
                    ">" => depth += 1,
                    "<" if depth == 0 => {
                        angle = Some(candidate);
                        break;
                    }
                    "<" => depth -= 1,
                    _ => {}
                }
            }
            angle.is_some_and(|angle| {
                let qualification = &tokens[angle + 1..index - 2];
                qualification.iter().any(|token| aliases.contains(token))
                    || (in_type_impl && qualification.iter().any(|token| token == "Self"))
            })
        } else {
            false
        };
        if !aliases.contains(owner) && !(in_type_impl && owner == "Self") && !qualified_type {
            continue;
        }
        let use_kind = if tokens.get(index + 1).is_some_and(|token| token == "(") {
            "call"
        } else {
            "reference"
        };
        references.push(format!("{type_name}:{use_kind}"));
    }
    references
}

fn revisioned_database_new_references(source: &str) -> Vec<String> {
    type_method_references(source, "RevisionedQueryDatabase", "new")
}

fn construction_token_new_references(source: &str) -> Vec<String> {
    type_method_references(source, "RevisionedQueryDatabaseConstructionToken", "new")
}

fn type_method_definitions(source: &str, type_name: &str, method: &str) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let aliases = rust_alias_closure(&tokens, type_name, true);
    let mut definitions = Vec::new();
    for (open, close) in inherent_type_impl_ranges(&tokens, &aliases) {
        let mut nested_braces = 0usize;
        for index in open + 1..close {
            match tokens[index].as_str() {
                "{" => nested_braces += 1,
                "}" => nested_braces = nested_braces.saturating_sub(1),
                "fn" if nested_braces == 0
                    && tokens.get(index + 1).is_some_and(|name| name == method) =>
                {
                    let public = tokens
                        .get(index.wrapping_sub(1))
                        .is_some_and(|token| token == "pub")
                        || (index >= 4
                            && tokens[index - 4] == "pub"
                            && tokens[index - 3] == "("
                            && tokens[index - 1] == ")");
                    definitions.push(if public { "public" } else { "private" }.to_owned());
                }
                _ => {}
            }
        }
    }
    definitions
}

fn type_method_definition_owner_inventory(
    sources: &[(String, &str)],
    type_name: &str,
    method: &str,
) -> Vec<(String, String, usize)> {
    let mut occurrences = sources
        .iter()
        .flat_map(|(owner, source)| {
            type_method_definitions(source, type_name, method)
                .into_iter()
                .map(move |visibility| (owner.clone(), visibility))
        })
        .collect::<Vec<_>>();
    occurrences.sort();
    let mut inventory: Vec<(String, String, usize)> = Vec::new();
    for (owner, visibility) in occurrences {
        if let Some((last_owner, last_visibility, count)) = inventory.last_mut()
            && *last_owner == owner
            && *last_visibility == visibility
        {
            *count += 1;
        } else {
            inventory.push((owner, visibility, 1));
        }
    }
    inventory
}

fn type_method_reference_owner_inventory(
    sources: &[(String, &str)],
    type_name: &str,
    method: &str,
) -> Vec<(String, String, usize)> {
    let mut occurrences = sources
        .iter()
        .flat_map(|(owner, source)| {
            type_method_references(source, type_name, method)
                .into_iter()
                .map(move |reference| (owner.clone(), reference))
        })
        .collect::<Vec<_>>();
    occurrences.sort();
    let mut inventory: Vec<(String, String, usize)> = Vec::new();
    for (owner, reference) in occurrences {
        if let Some((last_owner, last_reference, count)) = inventory.last_mut()
            && *last_owner == owner
            && *last_reference == reference
        {
            *count += 1;
        } else {
            inventory.push((owner, reference, 1));
        }
    }
    inventory
}

fn database_new_reference_owner_inventory(
    sources: &[(String, &str)],
) -> Vec<(String, String, usize)> {
    type_method_reference_owner_inventory(sources, "RevisionedQueryDatabase", "new")
}

fn construction_token_new_reference_owner_inventory(
    sources: &[(String, &str)],
) -> Vec<(String, String, usize)> {
    type_method_reference_owner_inventory(
        sources,
        "RevisionedQueryDatabaseConstructionToken",
        "new",
    )
}

fn revisioned_database_new_definitions(source: &str) -> usize {
    let tokens = rust_code_tokens(source);
    let aliases = revisioned_database_aliases(&tokens);
    revisioned_database_impl_ranges(&tokens, &aliases)
        .into_iter()
        .map(|(open, close)| {
            let mut nested_braces = 0usize;
            let mut definitions = 0usize;
            for index in open + 1..close {
                match tokens[index].as_str() {
                    "{" => nested_braces += 1,
                    "}" => nested_braces = nested_braces.saturating_sub(1),
                    "fn" if nested_braces == 0
                        && tokens.get(index + 1).is_some_and(|name| name == "new") =>
                    {
                        definitions += 1;
                    }
                    _ => {}
                }
            }
            definitions
        })
        .sum()
}

fn database_new_definition_owner_inventory(sources: &[(String, &str)]) -> Vec<(String, usize)> {
    sources
        .iter()
        .filter_map(|(owner, source)| {
            let definitions = revisioned_database_new_definitions(source);
            (definitions != 0).then(|| (owner.clone(), definitions))
        })
        .collect()
}

fn type_trait_impl_count(source: &str, type_name: &str, trait_name: &str) -> usize {
    let tokens = rust_code_tokens(source);
    let type_aliases = rust_alias_closure(&tokens, type_name, true);
    let trait_aliases = rust_alias_closure(&tokens, trait_name, false);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| *token == "impl")
        .filter(|(impl_index, _)| {
            let Some(open) = (*impl_index + 1..tokens.len())
                .find(|index| matches!(tokens[*index].as_str(), "{" | ";"))
                .filter(|index| tokens[*index] == "{")
            else {
                return false;
            };
            let header = &tokens[*impl_index + 1..open];
            let Some(for_index) = header.iter().rposition(|token| token == "for") else {
                return false;
            };
            header[..for_index]
                .iter()
                .any(|token| trait_aliases.contains(token))
                && header[for_index + 1..]
                    .iter()
                    .any(|token| type_aliases.contains(token))
        })
        .count()
}

fn revisioned_database_default_impl_count(source: &str) -> usize {
    type_trait_impl_count(source, "RevisionedQueryDatabase", "Default")
}

fn source_without_exact_item(source: &str, item: &str) -> String {
    let offsets = source
        .match_indices(item)
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1, "exact source item must occur once");
    let offset = offsets[0];
    format!("{}{}", &source[..offset], &source[offset + item.len()..])
}

fn matching_token_delimiter(tokens: &[String], open: usize, left: &str, right: &str) -> usize {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token == left {
            depth += 1;
        } else if token == right {
            depth = depth
                .checked_sub(1)
                .unwrap_or_else(|| panic!("token delimiter {right:?} closes before {left:?}"));
            if depth == 0 {
                return index;
            }
        }
    }
    panic!("token delimiter {left:?} has no matching {right:?}")
}

/// Return every function carrying an exact built-in `#[test]` attribute.
/// Comments, layout, additional attributes, and restricted visibility are
/// tokenized away or skipped so source inventories cannot lose a test through
/// a valid spelling change.
pub(super) fn test_function_declarations(source: &str) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let mut declarations = Vec::new();
    let mut index = 0usize;
    while index + 3 < tokens.len() {
        if tokens[index] != "#" || tokens[index + 1] != "[" {
            index += 1;
            continue;
        }
        let close = matching_token_delimiter(&tokens, index + 1, "[", "]");
        if tokens[index + 2..close] != ["test"] {
            index = close + 1;
            continue;
        }

        let mut cursor = close + 1;
        while tokens.get(cursor).is_some_and(|token| token == "#")
            && tokens.get(cursor + 1).is_some_and(|token| token == "[")
        {
            cursor = matching_token_delimiter(&tokens, cursor + 1, "[", "]") + 1;
        }
        if tokens.get(cursor).is_some_and(|token| token == "pub") {
            cursor += 1;
            if tokens.get(cursor).is_some_and(|token| token == "(") {
                cursor = matching_token_delimiter(&tokens, cursor, "(", ")") + 1;
            }
        }
        while tokens
            .get(cursor)
            .is_some_and(|token| matches!(token.as_str(), "const" | "async" | "unsafe" | "extern"))
        {
            cursor += 1;
        }
        if tokens.get(cursor).is_some_and(|token| token == "fn")
            && let Some(name) = tokens.get(cursor + 1)
        {
            declarations.push(name.clone());
        }
        index = close + 1;
    }
    declarations
}

/// Return source-level authorities that can initialize, replace, or mutably
/// expose the revisioned database runtime field. This guards safe Rust source;
/// arbitrary unsafe pointer arithmetic or proc-macro-generated code remains a
/// separate review boundary.
fn database_runtime_field_authorities(
    source: &str,
    reject_bare_runtime_macro_arguments: bool,
) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let aliases = revisioned_database_aliases(&tokens);
    let mut authorities = Vec::new();

    // A runtime place passed through a local declarative macro is mutation
    // authority even when the macro body mentions only a metavariable. This
    // also closes nested forwarding. A bare `runtime` identifier is authority
    // too because a separate field metavariable can combine it with a receiver
    // inside the macro. The live owner inventory permits only the sealed
    // composer's exact registration stream.
    for bang in 1..tokens.len().saturating_sub(1) {
        if tokens[bang] != "!" || !matches!(tokens[bang + 1].as_str(), "(" | "[" | "{") {
            continue;
        }
        let (left, right) = match tokens[bang + 1].as_str() {
            "(" => ("(", ")"),
            "[" => ("[", "]"),
            "{" => ("{", "}"),
            _ => unreachable!(),
        };
        let close = matching_token_delimiter(&tokens, bang + 1, left, right);
        let arguments = &tokens[bang + 2..close];
        if arguments
            .windows(2)
            .any(|window| window == [".", "runtime"])
        {
            authorities.push("runtime-field-macro-argument".to_owned());
        } else if reject_bare_runtime_macro_arguments
            && arguments.iter().any(|token| token == "runtime")
        {
            authorities.push("runtime-field-macro-bare-identifier".to_owned());
        }
    }

    // A database struct form containing `runtime` is either the sealed
    // initializer or a destructuring/reconstruction attempt. Owner inventory
    // below permits exactly one, inside the composer.
    for index in 0..tokens.len().saturating_sub(1) {
        if !(tokens[index] == "Self" || aliases.contains(&tokens[index]))
            || tokens[index + 1] != "{"
            || index.checked_sub(1).is_some_and(|previous| {
                matches!(
                    tokens[previous].as_str(),
                    "impl" | "for" | "struct" | "enum" | "union" | "->"
                )
            })
        {
            continue;
        }
        let close = matching_token_delimiter(&tokens, index + 1, "{", "}");
        if tokens[index + 2..close]
            .iter()
            .any(|token| token == "runtime")
        {
            authorities.push("database-runtime-struct-form".to_owned());
        }
    }

    for index in 1..tokens.len() {
        if tokens[index] != "runtime" || tokens[index - 1] != "." {
            continue;
        }
        let mut after_place = index + 1;
        while tokens.get(after_place).is_some_and(|token| token == ")") {
            after_place += 1;
        }
        let next = tokens.get(after_place).map(String::as_str);
        if next.is_some_and(|token| {
            matches!(
                token,
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
            )
        }) {
            authorities.push("runtime-field-assignment".to_owned());
        }
        if next == Some(".")
            && tokens.get(after_place + 1).is_some_and(|method| {
                matches!(
                    method.as_str(),
                    "clone_from" | "replace" | "swap" | "take" | "write" | "write_volatile"
                )
            })
        {
            authorities.push("runtime-field-mutating-method".to_owned());
        }

        let statement_start = (0..index - 1)
            .rev()
            .find(|candidate| matches!(tokens[*candidate].as_str(), ";" | "," | "{" | "}" | "=>"))
            .map_or(0, |delimiter| delimiter + 1);
        let statement_end = (index + 1..tokens.len())
            .find(|candidate| matches!(tokens[*candidate].as_str(), ";" | "}" | "=>"))
            .unwrap_or(tokens.len());
        if tokens[statement_start..index]
            .windows(2)
            .any(|window| window == ["&", "mut"])
            || tokens[statement_start..index]
                .windows(3)
                .any(|window| window == ["&", "raw", "mut"])
            || tokens[statement_start..index]
                .iter()
                .any(|token| token == "addr_of_mut")
        {
            authorities.push("runtime-field-mutable-borrow".to_owned());
        }
        let statement = &tokens[statement_start..statement_end];
        if statement.iter().any(|token| token == "ptr")
            && statement.iter().any(|token| token == "write")
        {
            authorities.push("runtime-field-pointer-write".to_owned());
        }
    }
    authorities
}

fn database_runtime_authority_inventory(
    sources: &[(String, &str)],
) -> Vec<(String, String, usize)> {
    let mut occurrences = sources
        .iter()
        .flat_map(|(owner, source)| {
            database_runtime_field_authorities(source, true)
                .into_iter()
                .map(move |authority| (owner.clone(), authority))
        })
        .collect::<Vec<_>>();
    occurrences.sort();
    let mut inventory: Vec<(String, String, usize)> = Vec::new();
    for (owner, authority) in occurrences {
        if let Some((last_owner, last_authority, count)) = inventory.last_mut()
            && *last_owner == owner
            && *last_authority == authority
        {
            *count += 1;
        } else {
            inventory.push((owner, authority, 1));
        }
    }
    inventory
}

fn identifier_owner_inventory(sources: &[(String, &str)], expected: &str) -> Vec<(String, usize)> {
    let mut inventory = sources
        .iter()
        .filter_map(|(owner, source)| {
            let count = code_identifier_count(source, expected);
            (count != 0).then(|| (owner.clone(), count))
        })
        .collect::<Vec<_>>();
    inventory.sort();
    inventory
}

fn type_alias_owner_inventory(
    sources: &[(String, &str)],
    root: &str,
) -> Vec<(String, Vec<String>)> {
    let mut inventory = sources
        .iter()
        .filter_map(|(owner, source)| {
            let mut aliases = rust_alias_closure(&rust_code_tokens(source), root, true);
            aliases.remove(root);
            (!aliases.is_empty()).then(|| (owner.clone(), aliases.into_iter().collect()))
        })
        .collect::<Vec<_>>();
    inventory.sort();
    inventory
}

/// Close aliases and local wrapper types around one root type. This is used
/// for derived-Default field review: a tuple struct, named wrapper, `Option`,
/// tuple, or alias can otherwise conceal another recursively defaulted value.
fn type_carrier_closure(source: &str, root: &str) -> std::collections::BTreeSet<String> {
    let tokens = rust_code_tokens(source);
    let mut carriers = std::collections::BTreeSet::from([root.to_owned()]);
    loop {
        let mut changed = false;
        for (use_index, token) in tokens.iter().enumerate() {
            if token != "use" {
                continue;
            }
            let end = tokens[use_index + 1..]
                .iter()
                .position(|token| token == ";")
                .map_or(tokens.len(), |offset| use_index + 1 + offset);
            for as_index in use_index + 1..end {
                if tokens[as_index] != "as" {
                    continue;
                }
                let Some(target) = tokens.get(as_index + 1) else {
                    continue;
                };
                if tokens[use_index + 1..as_index]
                    .iter()
                    .any(|source| carriers.contains(source))
                    && target != "_"
                    && carriers.insert(target.clone())
                {
                    changed = true;
                }
            }
        }
        for (type_index, token) in tokens.iter().enumerate() {
            if token != "type" {
                continue;
            }
            let Some(target) = tokens.get(type_index + 1) else {
                continue;
            };
            let Some(equal) = (type_index + 2..tokens.len())
                .find(|index| matches!(tokens[*index].as_str(), "=" | ";"))
                .filter(|index| tokens[*index] == "=")
            else {
                continue;
            };
            let end = tokens[equal + 1..]
                .iter()
                .position(|token| token == ";")
                .map_or(tokens.len(), |offset| equal + 1 + offset);
            if tokens[equal + 1..end]
                .iter()
                .any(|source| carriers.contains(source))
                && carriers.insert(target.clone())
            {
                changed = true;
            }
        }
        for (item_index, token) in tokens.iter().enumerate() {
            if !matches!(token.as_str(), "struct" | "enum") {
                continue;
            }
            let Some(target) = tokens.get(item_index + 1) else {
                continue;
            };
            let Some(open) = (item_index + 2..tokens.len())
                .find(|index| matches!(tokens[*index].as_str(), "{" | "(" | ";"))
                .filter(|index| tokens[*index] != ";")
            else {
                continue;
            };
            let close = matching_token_delimiter(
                &tokens,
                open,
                &tokens[open],
                if tokens[open] == "{" { "}" } else { ")" },
            );
            if tokens[open + 1..close]
                .iter()
                .any(|source| carriers.contains(source))
                && carriers.insert(target.clone())
            {
                changed = true;
            }
        }
        if !changed {
            return carriers;
        }
    }
}

fn struct_fields_with_type_carrier(source: &str, item: &str, root: &str) -> Vec<String> {
    let carriers = type_carrier_closure(source, root);
    let tokens = rust_code_tokens(item);
    let open = tokens
        .iter()
        .position(|token| token == "{")
        .expect("reviewed struct has a field body");
    let close = matching_token_delimiter(&tokens, open, "{", "}");
    let mut fields = Vec::new();
    let mut start = open + 1;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut angles = 0usize;
    for index in open + 1..=close {
        let token = &tokens[index];
        if (token == "," || index == close)
            && parens == 0
            && brackets == 0
            && braces == 0
            && angles == 0
        {
            let field = &tokens[start..index];
            let mut field_parens = 0usize;
            let mut field_brackets = 0usize;
            let mut field_braces = 0usize;
            let mut field_angles = 0usize;
            let colon = field.iter().enumerate().find_map(|(index, token)| {
                let at_field_level = field_parens == 0
                    && field_brackets == 0
                    && field_braces == 0
                    && field_angles == 0;
                let colon = (token == ":" && at_field_level).then_some(index);
                match token.as_str() {
                    "(" => field_parens += 1,
                    ")" => field_parens = field_parens.saturating_sub(1),
                    "[" => field_brackets += 1,
                    "]" => field_brackets = field_brackets.saturating_sub(1),
                    "{" => field_braces += 1,
                    "}" => field_braces = field_braces.saturating_sub(1),
                    "<" => field_angles += 1,
                    ">" => field_angles = field_angles.saturating_sub(1),
                    _ => {}
                }
                colon
            });
            if let Some(colon) = colon
                && field[colon + 1..]
                    .iter()
                    .any(|token| carriers.contains(token))
            {
                let name = field[..colon]
                    .iter()
                    .rev()
                    .find(|token| {
                        token
                            .as_bytes()
                            .first()
                            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                    })
                    .expect("reviewed field has a name");
                let cfg = field[..colon]
                    .windows(3)
                    .any(|tokens| tokens == ["#", "[", "cfg"]);
                fields.push(format!("{name}|cfg={cfg}|{}", field[colon + 1..].join("")));
            }
            start = index + 1;
        }
        match token.as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            "{" => braces += 1,
            "}" if index != close => braces = braces.saturating_sub(1),
            "<" => angles += 1,
            ">" => angles = angles.saturating_sub(1),
            _ => {}
        }
    }
    fields
}

fn include_literal_path(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    if bytes.get(start) == Some(&b'"') {
        let contents = start + 1;
        let mut cursor = contents;
        let mut escaped = false;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\\' => {
                    escaped = true;
                    cursor = (cursor + 2).min(bytes.len());
                }
                b'"' => {
                    let path = &source[contents..cursor];
                    return Some(if escaped {
                        format!("<escaped include path:{path}>")
                    } else {
                        path.to_owned()
                    });
                }
                _ => cursor += 1,
            }
        }
        return None;
    }
    if bytes.get(start) == Some(&b'r') {
        let mut quote = start + 1;
        while bytes.get(quote) == Some(&b'#') {
            quote += 1;
        }
        if bytes.get(quote) != Some(&b'"') {
            return None;
        }
        let hashes = quote - start - 1;
        let contents = quote + 1;
        let mut cursor = contents;
        while cursor < bytes.len() {
            if bytes[cursor] == b'"'
                && bytes
                    .get(cursor + 1..cursor + 1 + hashes)
                    .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
            {
                return Some(source[contents..cursor].to_owned());
            }
            cursor += 1;
        }
    }
    None
}

/// Inventory literal paths from code-level `include!` invocations. An include
/// whose argument is not one reviewed string literal is retained as a sentinel
/// entry so an alternate spelling cannot disappear from the closed edge set.
fn include_macro_paths(source: &str) -> Vec<String> {
    let code = rust_code_only(source);
    let bytes = code.as_bytes();
    let original = source.as_bytes();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index..].starts_with(b"include")
            || index
                .checked_sub(1)
                .and_then(|before| bytes.get(before))
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            || bytes
                .get(index + "include".len())
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            index += 1;
            continue;
        }
        let mut cursor = index + "include".len();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'!') {
            index += 1;
            continue;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if !bytes
            .get(cursor)
            .is_some_and(|delimiter| matches!(*delimiter, b'(' | b'[' | b'{'))
        {
            index += 1;
            continue;
        }
        cursor += 1;
        while original.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        paths.push(
            include_literal_path(source, cursor)
                .unwrap_or_else(|| "<non-literal include path>".to_owned()),
        );
        index = cursor.max(index + 1);
    }
    paths
}

pub(super) fn module_declarations(source: &str) -> Vec<String> {
    let code = rust_code_only(source);
    code_identifiers(&code)
        .windows(2)
        .filter_map(|identifiers| (identifiers[0] == "mod").then(|| identifiers[1].to_owned()))
        .collect()
}

fn root_item_semicolon(tokens: &[String], start: usize) -> usize {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if token == ";" && parens == 0 && brackets == 0 && braces == 0 {
            return index;
        }
        match token.as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            "{" => braces += 1,
            "}" => braces = braces.saturating_sub(1),
            _ => {}
        }
    }
    panic!("crate-root namespace item at token {start} has no terminating semicolon")
}

fn collect_use_tree_bindings(
    tokens: &[String],
    inherited_last: Option<&str>,
    bindings: &mut Vec<String>,
) {
    let mut start = 0usize;
    while tokens.get(start).is_some_and(|token| token == "::") {
        start += 1;
    }
    let Some(first) = tokens.get(start) else {
        return;
    };
    if first == "{" {
        let close = matching_token_delimiter(tokens, start, "{", "}");
        let mut item_start = start + 1;
        let mut parens = 0usize;
        let mut brackets = 0usize;
        let mut braces = 0usize;
        for index in start + 1..=close {
            let at_item_boundary = index == close
                || (tokens[index] == "," && parens == 0 && brackets == 0 && braces == 0);
            if at_item_boundary {
                collect_use_tree_bindings(&tokens[item_start..index], inherited_last, bindings);
                item_start = index + 1;
                continue;
            }
            match tokens[index].as_str() {
                "(" => parens += 1,
                ")" => parens = parens.saturating_sub(1),
                "[" => brackets += 1,
                "]" => brackets = brackets.saturating_sub(1),
                "{" => braces += 1,
                "}" => braces = braces.saturating_sub(1),
                _ => {}
            }
        }
        return;
    }
    if first == "*" {
        bindings.push(format!("*:{}", inherited_last.unwrap_or("<root>")));
        return;
    }

    let segment = first.as_str();
    let next = start + 1;
    if tokens.get(next).is_some_and(|token| token == "as") {
        if let Some(alias) = tokens.get(next + 1)
            && alias != "_"
        {
            bindings.push(alias.clone());
        }
        return;
    }
    if tokens.get(next).is_some_and(|token| token == "::") {
        let path_last = if segment == "self" {
            inherited_last
        } else {
            Some(segment)
        };
        collect_use_tree_bindings(&tokens[next + 1..], path_last, bindings);
        return;
    }
    if segment == "self" {
        if let Some(inherited_last) = inherited_last {
            bindings.push(inherited_last.to_owned());
        }
    } else {
        bindings.push(segment.to_owned());
    }
}

/// Return complete normalized root namespace declarations and every concrete
/// local name they bind. Glob imports retain an explicit source sentinel.
fn crate_root_namespace_inventory(source: &str) -> (Vec<String>, Vec<String>) {
    let tokens = rust_code_tokens(source);
    let mut declarations = Vec::new();
    let mut bindings = Vec::new();
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut index = 0usize;
    while index < tokens.len() {
        let at_root = parens == 0 && brackets == 0 && braces == 0;
        if at_root && tokens[index] == "mod" {
            if let Some(name) = tokens.get(index + 1) {
                declarations.push(format!("mod:{name}"));
                bindings.push(format!("mod:{name}"));
            }
        } else if at_root && tokens[index] == "use" {
            let end = root_item_semicolon(&tokens, index + 1);
            declarations.push(format!("use:{}", tokens[index + 1..end].join("")));
            let mut use_bindings = Vec::new();
            collect_use_tree_bindings(&tokens[index + 1..end], None, &mut use_bindings);
            bindings.extend(
                use_bindings
                    .into_iter()
                    .map(|binding| format!("use:{binding}")),
            );
            index = end + 1;
            continue;
        } else if at_root
            && tokens[index] == "extern"
            && tokens.get(index + 1).is_some_and(|token| token == "crate")
        {
            let end = root_item_semicolon(&tokens, index + 2);
            let declaration = &tokens[index + 2..end];
            declarations.push(format!("extern:crate{}", declaration.join("")));
            let alias = declaration
                .windows(2)
                .find_map(|tokens| (tokens[0] == "as").then(|| tokens[1].clone()))
                .or_else(|| declaration.first().cloned());
            if let Some(alias) = alias
                && alias != "_"
            {
                bindings.push(format!("extern:{alias}"));
            }
            index = end + 1;
            continue;
        }

        match tokens[index].as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            "{" => braces += 1,
            "}" => braces = braces.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    declarations.sort();
    bindings.sort();
    (declarations, bindings)
}

fn crate_root_macro_binding_arguments(source: &str) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let forbidden = ["core", "default", "std"];
    let mut arguments = Vec::new();
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    for index in 0..tokens.len() {
        let at_root = parens == 0 && brackets == 0 && braces == 0;
        if at_root
            && tokens[index] == "!"
            && tokens
                .get(index.wrapping_sub(1))
                .is_some_and(|token| token != "macro_rules" && token != "#")
            && let Some(open) = tokens.get(index + 1)
            && matches!(open.as_str(), "(" | "[" | "{")
        {
            let close_token = match open.as_str() {
                "(" => ")",
                "[" => "]",
                "{" => "}",
                _ => unreachable!(),
            };
            let close = matching_token_delimiter(&tokens, index + 1, open, close_token);
            for name in forbidden {
                arguments.extend(
                    tokens[index + 2..close]
                        .iter()
                        .filter(|token| token.as_str() == name)
                        .map(|_| format!("macro-argument:{name}")),
                );
            }
        }
        match tokens[index].as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            "{" => braces += 1,
            "}" => braces = braces.saturating_sub(1),
            _ => {}
        }
    }
    arguments.sort();
    arguments
}

fn forbidden_crate_root_resolution_bindings(source: &str) -> Vec<String> {
    let (_, bindings) = crate_root_namespace_inventory(source);
    let mut forbidden = bindings
        .into_iter()
        .filter(|binding| {
            binding
                .split_once(':')
                .is_some_and(|(_, name)| matches!(name, "core" | "default" | "std"))
        })
        .collect::<Vec<_>>();
    forbidden.extend(crate_root_macro_binding_arguments(source));
    forbidden.sort();
    forbidden
}

fn module_owner_inventory(sources: &[(String, &str)]) -> Vec<String> {
    let mut inventory = sources
        .iter()
        .flat_map(|(owner, source)| {
            module_declarations(source)
                .into_iter()
                .map(move |module| format!("{owner}:{module}"))
        })
        .collect::<Vec<_>>();
    inventory.sort();
    inventory
}

fn source_inventory_fingerprint(inventory: &[String]) -> u64 {
    inventory
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, entry| {
            entry
                .bytes()
                .chain(std::iter::once(b'\n'))
                .fold(hash, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                })
        })
}

/// Extract one complete braced code item beginning at `marker`. Braces inside
/// comments and literals are masked before balancing, while the returned slice
/// retains the exact original source for an executable-shape identity.
fn exact_balanced_code_item<'a>(source: &'a str, marker: &str) -> &'a str {
    let code = rust_code_only(source);
    let matches = code
        .match_indices(marker)
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one balanced code item marker {marker:?}, found {}",
        matches.len(),
    );
    let start = matches[0];
    let open = code[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("balanced code item marker {marker:?} has no opening brace"));
    let mut depth = 0usize;
    for (offset, byte) in code.as_bytes()[open..].iter().copied().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1).unwrap_or_else(|| {
                    panic!("balanced code item marker {marker:?} closes before it opens")
                });
                if depth == 0 {
                    return &source[start..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("balanced code item marker {marker:?} has no closing brace");
}

/// Return every runtime family-constructor reference. Named family-constructor
/// methods are authorities even when used as function items. The base `family`
/// method counts through UFCS, turbofish, or a nonempty method call; ordinary
/// zero-argument `node.family()` observations are deliberately excluded.
fn family_constructor_calls(source: &str) -> Vec<String> {
    let code = rust_code_only(source);
    let bytes = code.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let separator_len = if bytes[index] == b'.' {
            1
        } else if bytes[index..].starts_with(b"::") {
            2
        } else {
            index += 1;
            continue;
        };
        let mut cursor = index + separator_len;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"r#") {
            cursor += 2;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        let name = &code[name_start..cursor];
        let named_constructor =
            name.starts_with("family_with_") || name.starts_with("content_addressed_family_with_");
        let constructor = name == "family" || named_constructor;
        if !constructor {
            index += separator_len;
            continue;
        }
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let turbofish = bytes.get(cursor..cursor + 3) == Some(b"::<");
        if named_constructor || separator_len == 2 || turbofish {
            calls.push(name.to_owned());
            index = cursor.max(index + separator_len);
            continue;
        }
        if bytes.get(cursor) != Some(&b'(') {
            index += separator_len;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b')') {
            calls.push(name.to_owned());
        }
        index = cursor;
    }
    calls
}

/// Conservatively inventory the complete family-constructor vocabulary. This
/// deliberately includes zero-argument `family` observations: a bare
/// identifier used as a local macro argument must still perturb the reviewed
/// owner/count baseline. `code_identifiers` normalizes `r#name` to `name`.
fn family_authority_identifiers(source: &str) -> Vec<String> {
    let code = rust_code_only(source);
    code_identifiers(&code)
        .into_iter()
        .filter(|identifier| {
            *identifier == "family"
                || identifier.starts_with("family_with_")
                || identifier.starts_with("content_addressed_family_with_")
        })
        .map(str::to_owned)
        .collect()
}

fn family_identifier_owner_inventory(sources: &[(String, &str)]) -> Vec<(String, String, usize)> {
    let mut occurrences = sources
        .iter()
        .flat_map(|(owner, source)| {
            family_authority_identifiers(source)
                .into_iter()
                .map(move |identifier| (owner.clone(), identifier))
        })
        .collect::<Vec<_>>();
    occurrences.sort();
    let mut inventory: Vec<(String, String, usize)> = Vec::new();
    for (owner, identifier) in occurrences {
        if let Some((last_owner, last_identifier, count)) = inventory.last_mut()
            && *last_owner == owner
            && *last_identifier == identifier
        {
            *count += 1;
        } else {
            inventory.push((owner, identifier, 1));
        }
    }
    inventory
}

fn expected_registration_family_constructor(family: &str) -> &'static str {
    match family {
        "compiler.resolve-import" => "family_with_evaluator",
        "compiler.declaration-body-plan-artifacts" => {
            "family_with_equality_and_evaluator_and_retained_charge"
        }
        "compiler.parse" => "content_addressed_family_with_equality",
        "compiler.body-fact-provider-probe" => "family_with_equality",
        _ => "family_with_equality_and_evaluator",
    }
}

fn registration_include_path(owner: &str, macro_name: &str) -> String {
    if macro_name == "register_provider_probe" {
        return "provider_probe.rs".to_owned();
    }
    let leaf = macro_name
        .strip_prefix(&format!("register_{owner}_"))
        .expect("registration macro name records its owner");
    format!("{owner}/{leaf}.rs")
}

/// Exact raw-source identity for executable authority. Preserving one
/// constructor token while placing it under a loop, iterator, closure, helper,
/// or other repeated-execution wrapper must require an explicit review.
fn executable_source_shape_identity(source: &str) -> (usize, u64) {
    (
        source.len(),
        source.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }),
    )
}

fn registration_macro_invocations(source: &str) -> Vec<String> {
    let code = rust_code_only(source);
    code.match_indices("register_")
        .filter_map(|(start, _)| {
            let tail = &code[start..];
            let end = tail.find('!')?;
            let name = &tail[..end];
            name.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                .then(|| name.to_owned())
        })
        .collect()
}

fn glob_import_paths(source: &str) -> Vec<String> {
    let tokens = rust_code_tokens(source);
    let mut paths = Vec::new();
    for (use_index, token) in tokens.iter().enumerate() {
        if token != "use" {
            continue;
        }
        let end = tokens[use_index + 1..]
            .iter()
            .position(|token| token == ";")
            .map_or(tokens.len(), |offset| use_index + 1 + offset);
        let import = &tokens[use_index + 1..end];
        if import.last().is_some_and(|token| token == "*") {
            paths.push(import.join(""));
        }
    }
    paths
}

// Manifest order is already pinned to the constructor invocation stream. Each
// entry adds the exact raw source identity of that leaf, making the reviewed
// registration operation one-shot rather than merely one syntactic call.
const REGISTRATION_LEAF_ONE_SHOT_IDENTITIES: [(usize, u64); 44] = [
    (1_608, 9_349_892_297_056_354_958),
    (2_319, 162_635_033_840_982_550),
    (2_335, 11_401_802_782_486_455_398),
    (2_512, 6_814_382_478_423_834_202),
    (2_172, 10_084_900_447_980_393_497),
    (1_922, 13_163_199_842_675_560_730),
    (4_646, 13_986_428_999_966_792_987),
    (9_466, 873_764_896_819_683_713),
    (7_490, 12_196_017_486_031_950_345),
    (1_701, 16_025_502_829_918_190_611),
    (3_763, 1_124_858_669_055_323_594),
    (6_997, 6_265_749_180_761_052_443),
    (18_210, 5_072_850_203_005_355_192),
    (4_414, 16_575_646_427_208_173_073),
    (723, 13_306_205_946_719_141_945),
    (2_722, 5_872_377_445_300_688_460),
    (8_143, 86_253_652_908_143_842),
    (8_573, 4_124_823_810_140_583_654),
    (753, 3_150_885_663_910_159_936),
    (2_598, 10_270_973_964_375_394_836),
    (11_514, 1_556_117_829_400_373_530),
    (103_771, 11_567_462_448_468_948_481),
    (3_254, 11_949_940_325_034_004_149),
    (5_552, 14_658_861_127_087_730_967),
    (872, 14_092_162_116_261_787_003),
    (1_269, 5_659_334_597_769_566_021),
    (937, 12_382_607_771_958_723_582),
    (1_118, 11_609_591_179_459_560_863),
    (654, 537_683_909_027_197_867),
    (853, 11_600_476_735_713_182_533),
    (3_251, 12_606_285_774_366_104_328),
    (601, 9_799_119_223_937_523_553),
    (4_771, 3_658_938_349_782_706_550),
    (1_634, 1_948_857_066_780_783_651),
    (3_484, 10_178_552_526_305_850_966),
    (767, 4_982_863_983_734_830_002),
    (3_611, 15_282_171_189_445_768_455),
    (4_802, 13_585_732_442_571_114_436),
    (2_906, 14_218_343_244_686_701_314),
    (51_750, 5_765_149_880_362_847_366),
    (8_756, 6_802_240_310_126_667_165),
    (14_038, 8_977_560_222_769_304_814),
    (380, 6_623_933_847_739_557_204),
    (421, 8_838_427_880_540_066_171),
];

// Construction and registration execution is sealed end to end:
// `CompilerSession::new` enters its derived Default once, the sole frontend
// field creates its private capability, the token-gated database constructor
// enters the private canonical constructor and ordered composer, and one shared
// forwarding authority reaches one manifested leaf. Tests retain a separately
// cfg-gated Default adapter into the private canonical entry. Any control-flow
// or multiplicity change at a layer changes its exact executable source
// identity even when constructor, caller, and macro token counts do not.
const CONSTRUCTION_TOKEN_STRUCT_IDENTITY: (usize, u64) = (80, 5_227_448_979_315_228_973);
const CONSTRUCTION_TOKEN_IMPL_IDENTITY: (usize, u64) = (108, 2_765_439_612_714_245_239);
const COMPILER_CRATE_ROOT_IDENTITY: (usize, u64) = (10_380, 4_871_274_390_997_435_514);
const COMPILER_CRATE_ROOT_NAMESPACE_IDENTITY: (usize, u64, usize, u64) = (
    107,
    10_929_233_963_061_401_561,
    220,
    6_174_496_129_237_532_836,
);
const COMPILER_SESSION_ROOT_IDENTITY: (usize, u64) = (3_375, 11_197_616_651_825_656_461);
const COMPILER_SESSION_CONSTRUCTOR_IDENTITY: (usize, u64) = (82, 10_721_269_354_679_246_675);
const FRONTEND_DATABASE_CONSTRUCTION_IDENTITY: (usize, u64) = (268, 967_740_565_057_515_587);
const DATABASE_INHERENT_CONSTRUCTOR_IDENTITY: (usize, u64) = (148, 15_840_470_727_822_522_148);
const DATABASE_CANONICAL_CONSTRUCTOR_IDENTITY: (usize, u64) = (224, 1_314_571_707_455_964_487);
const TEST_DEFAULT_DATABASE_ADAPTER_IDENTITY: (usize, u64) = (120, 3_163_599_038_660_274_771);
const REGISTRATION_DATABASE_IMPL_IDENTITY: (usize, u64) = (34_847, 17_578_195_062_279_132_179);
const SHARED_FAMILY_FORWARDING_IDENTITY: (usize, u64) = (2_609, 9_595_658_320_490_175_466);
const ORDERED_REGISTRATION_COMPOSER_IDENTITY: (usize, u64) = (33_364, 10_468_265_158_692_030_241);
// Macro imports, definitions, re-exports, and lexical ordering participate in
// macro resolution. Seal the complete registrations authority and each wrapper
// aggregate in addition to the executable identities inside them.
const REGISTRATION_AUTHORITY_MODULE_IDENTITY: (usize, u64) = (44_597, 17_681_835_659_194_060_194);
const REGISTRATION_WRAPPER_MODULE_IDENTITIES: [(usize, u64); 5] = [
    (842, 17_134_465_730_999_135_202),
    (1_146, 9_973_452_843_887_101_603),
    (1_118, 3_506_955_039_267_909_738),
    (85, 9_154_658_544_330_042_432),
    (1_096, 1_461_060_759_810_744_828),
];

fn registered_revisioned_database_families() -> Vec<(&'static str, &'static str)> {
    crate::revisioned_query_database::REGISTRATION_MANIFEST
        .iter()
        .map(|(owner, family, _, _)| (*family, *owner))
        .collect()
}

fn registered_family_source(family: &str) -> &'static str {
    REGISTRATION_MANIFEST
        .iter()
        .find_map(|(_, name, _, source)| (*name == family).then_some(*source))
        .expect("registered family manifest entry")
}

#[test]
fn revisioned_database_hub_and_registered_family_authority_are_structural() {
    let hub = include_str!("revisioned_query_database.rs");
    let production_hub = hub
        .split("\n#[cfg(test)]\npub(crate) const REVISIONED_DATABASE_SOURCE")
        .next()
        .expect("revisioned database production hub");
    assert_eq!(
        module_declarations(production_hub),
        [
            "backend",
            "body",
            "parse_import",
            "provider",
            "registrations",
            "semantic",
            "shared",
        ]
        .map(str::to_owned)
        .to_vec(),
        "the production hub module set must exactly match the source inventory",
    );
    let mut phase_submodules = REVISIONED_DATABASE_PHASES
        .iter()
        .filter(|(owner, _)| *owner != "test_support")
        .flat_map(|(owner, source)| {
            module_declarations(source)
                .into_iter()
                .map(move |module| format!("{owner}:{module}"))
        })
        .collect::<Vec<_>>();
    phase_submodules.sort();
    assert_eq!(
        phase_submodules,
        [
            "body:closure_nucleus",
            "body:durable_comptime_adapters",
            "body:provider_body",
            "body:revision_symbol_space",
            "body:transactions",
            "parse_import:program_assembly",
            "registrations:backend",
            "registrations:body",
            "registrations:parse_import",
            "registrations:provider",
            "registrations:semantic",
        ]
        .map(str::to_owned)
        .to_vec(),
        "every production child module must have an inventoried source owner",
    );
    assert!(
        hub.lines().count() < 150,
        "revisioned query database hub regrew into a phase implementation"
    );
    let session_source = SESSION_PRODUCTION_SOURCE;
    let session_code = rust_code_only(session_source);
    let token_marker = "pub(crate) struct RevisionedQueryDatabaseConstructionToken {";
    let token_start = session_code
        .find(token_marker)
        .expect("session construction token declaration");
    assert_eq!(
        session_code[..token_start]
            .bytes()
            .rfind(|byte| !byte.is_ascii_whitespace()),
        Some(b';'),
        "the construction token cannot gain derive or cfg attributes",
    );
    let construction_token_struct = exact_balanced_code_item(session_source, token_marker);
    assert_eq!(
        executable_source_shape_identity(construction_token_struct),
        CONSTRUCTION_TOKEN_STRUCT_IDENTITY,
        "the construction token declaration changed source shape",
    );
    assert_eq!(
        rust_code_tokens(construction_token_struct),
        rust_code_tokens(
            "pub(crate) struct RevisionedQueryDatabaseConstructionToken { _private: (), }",
        ),
        "the construction token must retain one private field and no derivable public state",
    );
    let construction_token_impl = exact_balanced_code_item(
        session_source,
        "impl RevisionedQueryDatabaseConstructionToken {",
    );
    assert_eq!(
        executable_source_shape_identity(construction_token_impl),
        CONSTRUCTION_TOKEN_IMPL_IDENTITY,
        "the private construction-token authority changed source shape",
    );
    assert_eq!(
        rust_code_tokens(construction_token_impl),
        rust_code_tokens(
            "impl RevisionedQueryDatabaseConstructionToken { fn new() -> Self { Self { _private: () } } }",
        ),
        "the token constructor must remain private and one-shot",
    );
    let compiler_session_marker = "#[derive(Debug, Default)]\npub struct CompilerSession {";
    let compiler_session_start = session_code
        .find(compiler_session_marker)
        .expect("canonical CompilerSession declaration");
    assert_eq!(
        session_code[..compiler_session_start]
            .bytes()
            .rfind(|byte| !byte.is_ascii_whitespace()),
        Some(b'}'),
        "CompilerSession cannot gain an outer derive or cfg attribute",
    );
    let compiler_session_root = exact_balanced_code_item(session_source, compiler_session_marker);
    assert_eq!(
        executable_source_shape_identity(compiler_session_root),
        COMPILER_SESSION_ROOT_IDENTITY,
        "CompilerSession's complete derived-Default field shape changed",
    );
    let compiler_session_tokens = rust_code_tokens(compiler_session_root);
    let compiler_session_open = compiler_session_tokens
        .iter()
        .position(|token| token == "{")
        .expect("CompilerSession has a field body");
    assert_eq!(
        &compiler_session_tokens[..compiler_session_open],
        [
            "#",
            "[",
            "derive",
            "(",
            "Debug",
            ",",
            "Default",
            ")",
            "]",
            "pub",
            "struct",
            "CompilerSession",
        ],
        "CompilerSession must derive exactly Debug and Default in that reviewed order",
    );
    assert_eq!(
        struct_fields_with_type_carrier(
            session_source,
            compiler_session_root,
            "FrontendQueryDatabase",
        ),
        ["queries|cfg=false|FrontendQueryDatabase"],
        "derived Default must construct exactly one non-cfg frontend database field named queries",
    );
    let compiler_session_constructor_marker = "pub fn new() -> Self {";
    let compiler_session_constructor_start = session_code
        .find(compiler_session_constructor_marker)
        .expect("canonical CompilerSession constructor");
    assert_eq!(
        session_code[..compiler_session_constructor_start]
            .bytes()
            .rfind(|byte| !byte.is_ascii_whitespace()),
        Some(b'}'),
        "CompilerSession::new cannot gain an outer cfg or other attribute",
    );
    let compiler_session_constructor =
        exact_balanced_code_item(session_source, compiler_session_constructor_marker);
    assert_eq!(
        executable_source_shape_identity(compiler_session_constructor),
        COMPILER_SESSION_CONSTRUCTOR_IDENTITY,
        "the outermost CompilerSession construction entry changed executable shape",
    );
    assert_eq!(
        rust_code_tokens(compiler_session_constructor),
        rust_code_tokens("pub fn new() -> Self { <Self as ::core::default::Default>::default() }",),
        "CompilerSession::new must return exactly one absolute derived-Default UFCS call",
    );
    assert_eq!(
        type_method_definitions(session_source, "CompilerSession", "new"),
        ["public"],
        "CompilerSession must own exactly one public inherent new method",
    );
    assert!(
        type_method_definitions(session_source, "CompilerSession", "default").is_empty(),
        "CompilerSession cannot own an inherent default method that shadows the derived trait",
    );
    let compiler_session_default_references =
        revisioned_database_default_references(compiler_session_constructor, true);
    assert_eq!(
        compiler_session_default_references,
        ["Self:call"],
        "CompilerSession::new must contain one absolute derived-Default reference",
    );
    #[derive(Default)]
    struct InherentDefaultResolutionProbe {
        inherent: bool,
    }
    impl InherentDefaultResolutionProbe {
        fn default() -> Self {
            Self { inherent: true }
        }
    }
    assert!(
        InherentDefaultResolutionProbe::default().inherent,
        "the former Self::default spelling resolves to a shadowing inherent method",
    );
    assert!(
        !<InherentDefaultResolutionProbe as ::core::default::Default>::default().inherent,
        "absolute UFCS must continue to resolve the derived trait despite an inherent method",
    );
    let compiler_session_inherent_default_fixture = r#"
impl CompilerSession {
    fn default() -> Self {
        <Self as ::core::default::Default>::default()
    }
}
"#;
    assert_eq!(
        type_method_definitions(
            compiler_session_inherent_default_fixture,
            "CompilerSession",
            "default",
        ),
        ["private"],
        "the inherent-default fixture must perturb the live method-owner inventory",
    );
    let absolute_session_default = "<Self as ::core::default::Default>::default()";
    let repeated_session_default_fixtures = [
        (
            "eager map",
            r#"[(), ()]
            .map(|()| <Self as ::core::default::Default>::default())
            .into_iter()
            .next()
            .expect("the expected session is first")"#,
        ),
        (
            "loop",
            r#"{
            let mut expected = None;
            let mut iterations = 0;
            loop {
                let value = <Self as ::core::default::Default>::default();
                if expected.is_none() {
                    expected = Some(value);
                }
                iterations += 1;
                if iterations == 2 {
                    break;
                }
            }
            expected.expect("the expected session is first")
        }"#,
        ),
        (
            "local closure called twice",
            r#"{
            let construct = || <Self as ::core::default::Default>::default();
            let expected = construct();
            let _peer = construct();
            expected
        }"#,
        ),
    ];
    for (label, repeated_default) in repeated_session_default_fixtures {
        let adversarial_constructor =
            compiler_session_constructor.replacen(absolute_session_default, repeated_default, 1);
        let adversarial_constructor = exact_balanced_code_item(
            &adversarial_constructor,
            compiler_session_constructor_marker,
        );
        assert_eq!(
            revisioned_database_default_references(adversarial_constructor, true),
            compiler_session_default_references,
            "the {label} fixture must preserve the weaker Default-reference inventory",
        );
        assert_eq!(
            function_identifier_usage(adversarial_constructor, "default"),
            function_identifier_usage(compiler_session_constructor, "default"),
            "the {label} fixture must preserve the weaker Default call-expression count",
        );
        assert_ne!(
            executable_source_shape_identity(adversarial_constructor),
            COMPILER_SESSION_CONSTRUCTOR_IDENTITY,
            "the {label} fixture must fail the live outer-constructor identity",
        );
    }
    let _token_gated_constructor: fn(
        crate::session::RevisionedQueryDatabaseConstructionToken,
    )
        -> crate::revisioned_query_database::RevisionedQueryDatabase =
        crate::revisioned_query_database::RevisionedQueryDatabase::new;

    let registration_composer = include_str!("revisioned_query_database/registrations.rs");
    assert_eq!(
        executable_source_shape_identity(registration_composer),
        REGISTRATION_AUTHORITY_MODULE_IDENTITY,
        "the complete registration module changed macro-resolution or construction authority",
    );
    let actual_wrapper_identities = REVISIONED_DATABASE_REGISTRATION_MODULES
        .iter()
        .map(|(_, source)| executable_source_shape_identity(source))
        .collect::<Vec<_>>();
    assert_eq!(
        actual_wrapper_identities, REGISTRATION_WRAPPER_MODULE_IDENTITIES,
        "a registration wrapper changed its include/re-export resolution shape",
    );
    assert_eq!(
        glob_import_paths(registration_composer),
        [
            "super::body::*",
            "super::semantic::*",
            "super::*",
            "backend::*",
            "body::*",
            "parse_import::*",
            "semantic::*",
        ],
        "registration glob resolution must remain pinned to the reviewed parent imports and four production wrappers; provider is an explicit cfg(test) import",
    );
    for (wrapper_owner, source) in REVISIONED_DATABASE_REGISTRATION_MODULES {
        let phase = wrapper_owner
            .strip_prefix("registrations_")
            .expect("registration wrapper owner prefix");
        let mut actual_macros = rust_code_tokens(source)
            .into_iter()
            .filter(|identifier| identifier.starts_with("register_"))
            .collect::<Vec<_>>();
        actual_macros.sort();
        let mut expected_macros = REGISTRATION_MANIFEST
            .iter()
            .filter(|(owner, _, macro_name, _)| {
                if *macro_name == "register_provider_probe" {
                    phase == "provider"
                } else {
                    *owner == phase
                }
            })
            .map(|(_, _, macro_name, _)| (*macro_name).to_owned())
            .collect::<Vec<_>>();
        expected_macros.sort();
        assert_eq!(
            actual_macros, expected_macros,
            "wrapper {wrapper_owner} must re-export exactly its manifested leaf macros once each",
        );
    }
    let registration_code = rust_code_only(registration_composer);
    let previous_code_byte = |marker: &str| {
        let start = registration_code
            .find(marker)
            .unwrap_or_else(|| panic!("missing registration construction item {marker:?}"));
        registration_code[..start]
            .bytes()
            .rfind(|byte| !byte.is_ascii_whitespace())
    };
    let test_default_database_adapter = exact_balanced_code_item(
        registration_composer,
        "#[cfg(test)]\nimpl Default for RevisionedQueryDatabase {",
    );
    assert_eq!(
        executable_source_shape_identity(test_default_database_adapter),
        TEST_DEFAULT_DATABASE_ADAPTER_IDENTITY,
        "the cfg(test) Default adapter changed executable shape",
    );
    assert_eq!(
        revisioned_database_default_impl_count(registration_composer),
        1,
        "the database must have exactly one raw-source Default impl",
    );
    let production_registration_composer =
        source_without_exact_item(registration_composer, test_default_database_adapter);
    assert_eq!(
        revisioned_database_default_impl_count(&production_registration_composer),
        0,
        "production source must not implement Default for RevisionedQueryDatabase",
    );
    assert!(
        revisioned_database_new_references(test_default_database_adapter).is_empty(),
        "the cfg(test) Default adapter cannot forge the production capability",
    );
    assert_eq!(
        function_identifier_usage(test_default_database_adapter, "new_canonical"),
        (0, 1, 0),
        "the cfg(test) Default adapter must delegate exactly once to the private canonical constructor",
    );
    let registration_database_impl =
        exact_balanced_code_item(registration_composer, "impl RevisionedQueryDatabase {");
    assert_eq!(
        previous_code_byte("impl RevisionedQueryDatabase {"),
        Some(b'}'),
        "the registration-owned database authority cannot gain an outer cfg attribute",
    );
    assert_eq!(
        executable_source_shape_identity(registration_database_impl),
        REGISTRATION_DATABASE_IMPL_IDENTITY,
        "the complete registration-owned database authority changed executable shape",
    );
    let inherent_database_constructor =
        exact_balanced_code_item(registration_database_impl, "pub(crate) fn new(");
    assert_eq!(
        executable_source_shape_identity(inherent_database_constructor),
        DATABASE_INHERENT_CONSTRUCTOR_IDENTITY,
        "the production inherent database constructor changed executable shape",
    );
    let expected_inherent_constructor = r#"pub(crate) fn new(
        _authority: crate::session::RevisionedQueryDatabaseConstructionToken,
    ) -> Self {
        Self::new_canonical()
    }"#;
    assert_eq!(
        rust_code_tokens(inherent_database_constructor),
        rust_code_tokens(expected_inherent_constructor),
        "the production constructor must consume the exact session capability and delegate once",
    );
    let canonical_database_constructor =
        exact_balanced_code_item(registration_database_impl, "fn new_canonical() -> Self {");
    assert_eq!(
        executable_source_shape_identity(canonical_database_constructor),
        DATABASE_CANONICAL_CONSTRUCTOR_IDENTITY,
        "the private canonical database constructor changed executable shape",
    );
    let expected_canonical_constructor = r#"fn new_canonical() -> Self {
        Self::with_declaration_memo_retention_and_concurrency(
            DECLARATION_QUERY_MEMO_RETENTION,
            crate::query_concurrency(),
            u32::MAX as usize,
        )
    }"#;
    assert_eq!(
        rust_code_tokens(canonical_database_constructor),
        rust_code_tokens(expected_canonical_constructor),
        "the private canonical constructor must preserve the former Default body's exact composer arguments and order",
    );
    let composer_constructor = "with_declaration_memo_retention_and_concurrency";
    assert_eq!(
        function_identifier_usage(canonical_database_constructor, composer_constructor),
        (0, 1, 0),
        "the private canonical constructor must make one direct composer call",
    );
    assert_eq!(
        function_identifier_usage(inherent_database_constructor, "new_canonical"),
        (0, 1, 0),
        "the token-gated constructor must make one direct canonical-constructor call",
    );
    assert_eq!(
        function_identifier_usage(registration_database_impl, "new_canonical"),
        (1, 1, 0),
        "the registration impl must own one private canonical definition and the production delegation",
    );
    assert_eq!(
        function_identifier_usage(registration_database_impl, composer_constructor),
        (1, 4, 0),
        "the registration impl must own one definition, the private canonical entry, and three reviewed test-factory calls",
    );
    for factory_marker in [
        "#[cfg(test)]\n    pub(crate) fn with_declaration_memo_retention(",
        "#[cfg(test)]\n    pub(crate) fn with_query_concurrency(",
        "#[cfg(test)]\n    pub(crate) fn with_interner_limit(",
    ] {
        let test_factory = exact_balanced_code_item(registration_database_impl, factory_marker);
        assert_eq!(
            function_identifier_usage(test_factory, composer_constructor),
            (0, 1, 0),
            "each reviewed cfg(test) factory must make one direct composer call: {factory_marker:?}",
        );
    }
    let frontend_queries = include_str!("session/frontend_queries.rs");
    let frontend_database_construction =
        exact_balanced_code_item(frontend_queries, "impl Default for FrontendQueryDatabase {");
    assert_eq!(
        executable_source_shape_identity(frontend_database_construction),
        FRONTEND_DATABASE_CONSTRUCTION_IDENTITY,
        "the complete production frontend database construction entry changed executable shape",
    );
    assert_eq!(
        revisioned_database_new_references(frontend_database_construction),
        ["RevisionedQueryDatabase:call"],
        "the frontend database entry must call the inherent revisioned constructor exactly once",
    );
    assert_eq!(
        construction_token_new_references(frontend_database_construction),
        ["RevisionedQueryDatabaseConstructionToken:call"],
        "the frontend database entry must create the private construction capability exactly once",
    );
    let shared = include_str!("revisioned_query_database/shared.rs");
    let shared_family_forwarding = exact_balanced_code_item(shared, "impl CompilerQueryRuntime {");
    assert_eq!(
        executable_source_shape_identity(shared_family_forwarding),
        SHARED_FAMILY_FORWARDING_IDENTITY,
        "the complete CompilerQueryRuntime forwarding authority changed executable shape",
    );
    let ordered_registration_composer = exact_balanced_code_item(
        registration_composer,
        "fn with_declaration_memo_retention_and_concurrency(",
    );
    assert_eq!(
        rust_code_tokens(ordered_registration_composer)
            .first()
            .map(String::as_str),
        Some("fn"),
        "the ordered composer must remain private to registrations",
    );
    assert_eq!(
        executable_source_shape_identity(ordered_registration_composer),
        ORDERED_REGISTRATION_COMPOSER_IDENTITY,
        "the complete private ordered registration composer changed executable shape",
    );
    let sealed_database_initializer =
        exact_balanced_code_item(ordered_registration_composer, "Self {\n            parse,");
    assert_eq!(
        database_runtime_field_authorities(sealed_database_initializer, false),
        ["database-runtime-struct-form"],
        "the sealed composer must initialize the database runtime field exactly once",
    );
    assert!(
        registration_composer.lines().count() < 1200,
        "registration composer regrew into a family-registration monolith"
    );
    assert!(
        family_constructor_calls(registration_composer).is_empty(),
        "family references must remain owned by their phase fragments"
    );
    assert_eq!(
        hub.matches("include_str!(\"revisioned_query_database/test_support.rs\")")
            .count(),
        1,
        "the hub must be the sole aggregate authority for test-support source"
    );
    for forbidden in [
        "impl RevisionedQueryDatabase",
        "family_with_equality",
        "content_addressed_family",
    ] {
        assert!(
            !hub.contains(forbidden),
            "revisioned query database hub regained implementation authority: {forbidden}"
        );
    }

    let families = registered_revisioned_database_families();
    assert_eq!(
        families.len(),
        44,
        "the registered-family manifest is complete"
    );
    let invocations = registration_macro_invocations(registration_composer);
    let manifest_macros = REGISTRATION_MANIFEST
        .iter()
        .map(|(_, _, macro_name, _)| (*macro_name).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        invocations, manifest_macros,
        "constructor order or family ownership changed"
    );
    assert_eq!(
        registration_macro_invocations(ordered_registration_composer),
        manifest_macros,
        "the sealed composer function must own the complete ordered manifest stream",
    );

    // Eager array mapping can preserve one syntactic composer call while
    // invoking it twice and discarding the peer database. The complete
    // private-canonical-constructor identity rejects that multiplicity.
    let constructor_one_shot = r#"        Self::with_declaration_memo_retention_and_concurrency(
            DECLARATION_QUERY_MEMO_RETENTION,
            crate::query_concurrency(),
            u32::MAX as usize,
        )"#;
    let constructor_repeated = r#"        [(), ()]
            .map(|()| {
                Self::with_declaration_memo_retention_and_concurrency(
                    DECLARATION_QUERY_MEMO_RETENTION,
                    crate::query_concurrency(),
                    u32::MAX as usize,
                )
            })
            .into_iter()
            .next()
            .expect("the expected database is first")"#;
    assert_eq!(
        canonical_database_constructor
            .matches(constructor_one_shot)
            .count(),
        1,
        "the canonical-constructor fixture must replace the real composer call",
    );
    let adversarial_constructor =
        canonical_database_constructor.replacen(constructor_one_shot, constructor_repeated, 1);
    let adversarial_constructor =
        exact_balanced_code_item(&adversarial_constructor, "fn new_canonical() -> Self {");
    assert_eq!(
        code_identifier_count(adversarial_constructor, composer_constructor),
        code_identifier_count(canonical_database_constructor, composer_constructor),
        "the canonical-constructor fixture must preserve the weak composer identifier count",
    );
    assert_eq!(
        function_identifier_usage(adversarial_constructor, composer_constructor),
        function_identifier_usage(canonical_database_constructor, composer_constructor),
        "the canonical-constructor fixture must preserve the weak direct-call inventory",
    );
    assert_ne!(
        executable_source_shape_identity(adversarial_constructor),
        DATABASE_CANONICAL_CONSTRUCTOR_IDENTITY,
        "the complete canonical-constructor identity must reject repeated construction",
    );

    // Repetition at the session entry can likewise preserve one syntactic
    // `RevisionedQueryDatabase::new` call. Seal that caller independently of
    // the inherent constructor so two complete runtime graphs cannot be made.
    let frontend_one_shot = r#"revisioned: crate::revisioned_query_database::RevisionedQueryDatabase::new(
                RevisionedQueryDatabaseConstructionToken::new(),
            ),"#;
    let frontend_repeated = r#"revisioned: [(), ()]
                .map(|()| {
                    crate::revisioned_query_database::RevisionedQueryDatabase::new(
                        RevisionedQueryDatabaseConstructionToken::new(),
                    )
                })
                .into_iter()
                .next()
                .expect("the expected database is first"),"#;
    assert_eq!(
        frontend_database_construction
            .matches(frontend_one_shot)
            .count(),
        1,
        "the session multiplicity fixture must replace the real database construction",
    );
    let adversarial_frontend =
        frontend_database_construction.replacen(frontend_one_shot, frontend_repeated, 1);
    let adversarial_frontend = exact_balanced_code_item(
        &adversarial_frontend,
        "impl Default for FrontendQueryDatabase {",
    );
    assert_eq!(
        revisioned_database_new_references(adversarial_frontend),
        revisioned_database_new_references(frontend_database_construction),
        "the session fixture must preserve the weaker constructor-reference inventory",
    );
    assert_eq!(
        construction_token_new_references(adversarial_frontend),
        construction_token_new_references(frontend_database_construction),
        "the session fixture must preserve the weaker capability-reference inventory",
    );
    assert_ne!(
        executable_source_shape_identity(adversarial_frontend),
        FRONTEND_DATABASE_CONSTRUCTION_IDENTITY,
        "the complete session construction identity must reject repeated construction",
    );

    for (label, fixture, include_short_forms, expected) in [
        (
            "qualified call",
            "crate::revisioned_query_database::RevisionedQueryDatabase::default()",
            false,
            "RevisionedQueryDatabase:call",
        ),
        (
            "raw qualified function item",
            "crate::revisioned_query_database::r#RevisionedQueryDatabase::r#default",
            false,
            "RevisionedQueryDatabase:reference",
        ),
        (
            "type alias call",
            "type Db = crate::revisioned_query_database::RevisionedQueryDatabase; Db::default()",
            false,
            "RevisionedQueryDatabase:call",
        ),
        (
            "use alias function item",
            "use crate::revisioned_query_database::RevisionedQueryDatabase as Db; let make = Db::default;",
            false,
            "RevisionedQueryDatabase:reference",
        ),
        (
            "qualified Default call",
            "<RevisionedQueryDatabase as Default>::default()",
            false,
            "RevisionedQueryDatabase:call",
        ),
        (
            "Self function item",
            "let make = Self::r#default;",
            true,
            "Self:reference",
        ),
        (
            "enclosing database impl Self UFCS call",
            r#"impl RevisionedQueryDatabase {
    fn peer() -> Self { <Self as std::default::Default>::r#default() }
}"#,
            true,
            "RevisionedQueryDatabase:call",
        ),
        (
            "enclosing database impl Self UFCS function item",
            r#"impl RevisionedQueryDatabase {
    fn constructor() { let make = <Self as Default>::default; }
}"#,
            true,
            "RevisionedQueryDatabase:reference",
        ),
        (
            "Default trait alias call",
            r#"use std::default::Default as Construct;
impl RevisionedQueryDatabase {
    fn peer() -> Self { Construct::default() }
}"#,
            true,
            "RevisionedQueryDatabase:call",
        ),
        (
            "raw Default trait alias function item",
            r#"use core::default::Default as r#Construct;
impl RevisionedQueryDatabase {
    fn constructor() { let make = Construct::r#default; }
}"#,
            true,
            "RevisionedQueryDatabase:reference",
        ),
        (
            "transitive Default trait alias call",
            r#"use std::default::Default as Construct;
use self::Construct as Build;
impl RevisionedQueryDatabase {
    fn peer() -> Self { Build::default() }
}"#,
            true,
            "RevisionedQueryDatabase:call",
        ),
        (
            "composed database and Default aliases",
            r#"use crate::revisioned_query_database::RevisionedQueryDatabase as Db;
use self::Db as Store;
use core::default::Default as Construct;
use self::Construct as Build;
fn peer() -> Store { <Store as Build>::default() }"#,
            false,
            "RevisionedQueryDatabase:call",
        ),
        (
            "composed database type and transitive Default aliases",
            r#"type Store = crate::revisioned_query_database::RevisionedQueryDatabase;
use core::default::Default as Construct;
use self::Construct as Build;
fn constructor() { let make = <Store as Build>::r#default; }"#,
            false,
            "RevisionedQueryDatabase:reference",
        ),
        (
            "inferred Default call",
            "let peer: Self = Default::default();",
            true,
            "Default:call",
        ),
    ] {
        assert_eq!(
            revisioned_database_default_references(fixture, include_short_forms),
            [expected],
            "the {label} spelling must retain database-construction authority",
        );
    }
    for (label, fixture, expected) in [
        (
            "qualified inherent call",
            "crate::revisioned_query_database::RevisionedQueryDatabase::new(authority)",
            "RevisionedQueryDatabase:call",
        ),
        (
            "raw inherent function item",
            "let make = crate::revisioned_query_database::r#RevisionedQueryDatabase::r#new;",
            "RevisionedQueryDatabase:reference",
        ),
        (
            "type-aliased inherent call",
            "type Db = crate::revisioned_query_database::RevisionedQueryDatabase; Db::new(authority)",
            "RevisionedQueryDatabase:call",
        ),
        (
            "use-aliased inherent function item",
            "use crate::revisioned_query_database::RevisionedQueryDatabase as Db; let make = Db::new;",
            "RevisionedQueryDatabase:reference",
        ),
        (
            "angle-qualified inherent call",
            "<crate::revisioned_query_database::RevisionedQueryDatabase>::new(authority)",
            "RevisionedQueryDatabase:call",
        ),
        (
            "enclosing database impl Self call",
            r#"impl RevisionedQueryDatabase {
    fn peer(authority: RevisionedQueryDatabaseConstructionToken) -> Self { Self::r#new(authority) }
}"#,
            "RevisionedQueryDatabase:call",
        ),
    ] {
        assert_eq!(
            revisioned_database_new_references(fixture),
            [expected],
            "the {label} spelling must retain inherent-construction authority",
        );
    }
    for (label, fixture, expected) in [
        (
            "qualified capability call",
            "crate::session::RevisionedQueryDatabaseConstructionToken::new()",
            "RevisionedQueryDatabaseConstructionToken:call",
        ),
        (
            "raw capability function item",
            "let make = crate::session::r#RevisionedQueryDatabaseConstructionToken::r#new;",
            "RevisionedQueryDatabaseConstructionToken:reference",
        ),
        (
            "aliased capability call",
            "use crate::session::RevisionedQueryDatabaseConstructionToken as Authority; Authority::new()",
            "RevisionedQueryDatabaseConstructionToken:call",
        ),
    ] {
        assert_eq!(
            construction_token_new_references(fixture),
            [expected],
            "the {label} spelling must retain capability-construction authority",
        );
    }
    for (label, fixture) in [
        (
            "qualified Default impl",
            "impl core::default::Default for RevisionedQueryDatabase { fn default() -> Self { todo!() } }",
        ),
        (
            "raw aliased Default impl",
            r#"use core::default::Default as r#Construct;
use crate::revisioned_query_database::RevisionedQueryDatabase as r#Store;
impl Construct for Store { fn default() -> Self { todo!() } }"#,
        ),
    ] {
        assert_eq!(
            revisioned_database_default_impl_count(fixture),
            1,
            "the {label} must retain type-level construction authority",
        );
    }

    // A declarative macro can hide a generated `Default` impl and its call to
    // `Self::new` from lexical type-owner scanners. The production type
    // boundary does not depend on seeing that expansion: the generated call
    // has no session capability to pass, and the private canonical/composer
    // entries cannot be named outside their registrations owner.
    let generated_default_macro = r#"macro_rules! synthesize_default {
    ($database:ty) => {
        impl Default for $database {
            fn default() -> Self { Self::new() }
        }
    };
}
synthesize_default!(RevisionedQueryDatabase);"#;
    let indented_generated_default_macro = r#"macro_rules! synthesize_default {
    ($database:ty) => {
        impl Default for $database {
            fn default() -> Self { Self::new() }
        }
    };
}
    synthesize_default!(RevisionedQueryDatabase);"#;
    for fixture in [generated_default_macro, indented_generated_default_macro] {
        assert_eq!(
            revisioned_database_default_impl_count(fixture),
            0,
            "the lexical impl scanner deliberately cannot resolve a macro type metavariable",
        );
        assert!(
            revisioned_database_new_references(fixture).is_empty(),
            "the lexical constructor scanner deliberately cannot resolve macro-generated Self",
        );
        assert!(
            construction_token_new_references(fixture).is_empty(),
            "the adversarial macro has no capability construction authority",
        );
    }
    assert!(
        unsupported_api_layout(generated_default_macro, false).is_some(),
        "an unindented root macro invocation remains rejected lexically",
    );
    assert!(
        unsupported_api_layout(indented_generated_default_macro, false).is_none(),
        "the indented fixture records why the capability boundary, not indentation, is authoritative",
    );
    let unwrap_or_default_fixture = r#"fn peer() -> RevisionedQueryDatabase {
    Option::<RevisionedQueryDatabase>::None.unwrap_or_default()
}"#;
    assert_eq!(
        code_identifier_count(unwrap_or_default_fixture, "unwrap_or_default"),
        1,
        "the trait-derived construction fixture must remain valid Rust source",
    );
    assert!(
        revisioned_database_default_references(unwrap_or_default_fixture, true).is_empty(),
        "method syntax does not expose the inferred Default target to the lexical scanner",
    );

    let split_core_fixture = r#"
impl RevisionedQueryDatabase {
    fn replace_runtime_from_peer(&mut self) {
        let peer = Self::default();
        self.runtime = peer.runtime;
    }
}
"#;
    assert_eq!(
        revisioned_database_default_references(split_core_fixture, true),
        ["Self:call"],
        "the reviewer split-core fixture must expose its peer construction",
    );
    assert!(
        database_runtime_field_authorities(split_core_fixture, true)
            .contains(&"runtime-field-assignment".to_owned()),
        "the reviewer split-core fixture must expose runtime replacement",
    );
    let runtime_mutation_fixtures = [
        (
            "replace",
            r#"impl RevisionedQueryDatabase {
    fn replace_runtime(&mut self, peer: Self) {
        let _old = std::mem::replace(&mut self.runtime, peer.runtime);
    }
}"#,
        ),
        (
            "swap",
            r#"impl RevisionedQueryDatabase {
    fn swap_runtime(&mut self, mut peer: Self) {
        std::mem::swap(&mut self.runtime, &mut peer.runtime);
    }
}"#,
        ),
        (
            "clone_from",
            r#"impl RevisionedQueryDatabase {
    fn clone_runtime(&mut self, peer: &Self) {
        self.runtime.clone_from(&peer.runtime);
    }
}"#,
        ),
        (
            "mutable destructure",
            r#"impl RevisionedQueryDatabase {
    fn alias_runtime(&mut self, peer: &Self) {
        let Self { runtime: core, .. } = self;
        core.clone_from(&peer.runtime);
    }
}"#,
        ),
        (
            "pointer write",
            r#"impl RevisionedQueryDatabase {
    unsafe fn write_runtime(&mut self, peer: Self) {
        unsafe { std::ptr::write(&mut self.runtime, peer.runtime) };
    }
}"#,
        ),
        (
            "parenthesized assignment",
            r#"impl RevisionedQueryDatabase {
    fn replace_grouped_runtime(&mut self, peer: Self) {
        (self.runtime) = peer.runtime;
    }
}"#,
        ),
        (
            "nested raw assignment",
            r#"impl RevisionedQueryDatabase {
    fn replace_nested_runtime(&mut self, peer: Self) {
        (((self.r#runtime))) = peer.runtime;
    }
}"#,
        ),
        (
            "parenthesized mutator",
            r#"impl RevisionedQueryDatabase {
    fn clone_grouped_runtime(&mut self, peer: &Self) {
        ((self.runtime)).clone_from(&peer.runtime);
    }
}"#,
        ),
    ];
    for (label, fixture) in runtime_mutation_fixtures.iter().copied() {
        assert!(
            !database_runtime_field_authorities(fixture, true).is_empty(),
            "the {label} fixture must expose runtime mutation authority",
        );
    }
    let runtime_macro_mutation_fixtures = [
        (
            "macro assignment",
            r#"macro_rules! replace_core {
    ($place:expr, $value:expr) => {{ $place = $value; }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer = <Self as Default>::default();
        replace_core!(self.runtime, peer.runtime);
    }
}"#,
        ),
        (
            "macro replace",
            r#"macro_rules! replace_core {
    ($place:expr, $value:expr) => {{
        let _old = std::mem::replace(&mut $place, $value);
    }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer = <Self as Default>::default();
        replace_core!(self.runtime, peer.runtime);
    }
}"#,
        ),
        (
            "macro swap",
            r#"macro_rules! swap_core {
    ($left:expr, $right:expr) => {{ std::mem::swap(&mut $left, &mut $right); }};
}
impl RevisionedQueryDatabase {
    fn swap_core(&mut self) {
        let mut peer = <Self as Default>::default();
        swap_core!(self.runtime, peer.runtime);
    }
}"#,
        ),
        (
            "nested forwarding macro",
            r#"macro_rules! replace_core {
    ($place:expr, $value:expr) => {{ $place = $value; }};
}
macro_rules! forward_core {
    ($place:expr, $value:expr) => {{ replace_core!($place, $value); }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer = <Self as Default>::default();
        forward_core!(self.runtime, peer.runtime);
    }
}"#,
        ),
    ];
    for (label, fixture) in runtime_macro_mutation_fixtures.iter().copied() {
        assert!(
            database_runtime_field_authorities(fixture, true)
                .contains(&"runtime-field-macro-argument".to_owned()),
            "the {label} fixture must expose its concrete runtime macro argument",
        );
        assert_eq!(
            revisioned_database_default_references(fixture, true),
            ["RevisionedQueryDatabase:call"],
            "the {label} fixture must expose its UFCS Self peer construction",
        );
    }
    let runtime_field_ident_macro_fixtures = [
        (
            "split receiver and field assignment",
            r#"use std::default::Default as Construct;
macro_rules! replace_field {
    ($target:expr, $field:ident, $source:expr) => {{
        ($target).$field = ($source).$field;
    }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer: Self = Construct::default();
        replace_field!(self, runtime, peer);
    }
}"#,
        ),
        (
            "raw split field assignment",
            r#"use core::default::Default as r#Construct;
macro_rules! replace_field {
    ($target:expr, $field:ident, $source:expr) => {{
        ($target).$field = ($source).$field;
    }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer: Self = Construct::default();
        replace_field!(self, r#runtime, peer);
    }
}"#,
        ),
        (
            "split field replace",
            r#"use std::default::Default as Construct;
macro_rules! replace_field {
    ($target:expr, $field:ident, $source:expr) => {{
        let _old = std::mem::replace(&mut ($target).$field, ($source).$field);
    }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer: Self = Construct::default();
        replace_field!(self, runtime, peer);
    }
}"#,
        ),
        (
            "split field swap",
            r#"use std::default::Default as Construct;
macro_rules! swap_field {
    ($left:expr, $field:ident, $right:expr) => {{
        std::mem::swap(&mut ($left).$field, &mut ($right).$field);
    }};
}
impl RevisionedQueryDatabase {
    fn swap_core(&mut self) {
        let mut peer: Self = Construct::default();
        swap_field!(self, runtime, peer);
    }
}"#,
        ),
        (
            "nested split-field forwarding",
            r#"use std::default::Default as Construct;
macro_rules! replace_field {
    ($target:expr, $field:ident, $source:expr) => {{
        ($target).$field = ($source).$field;
    }};
}
macro_rules! forward_field {
    ($target:expr, $field:ident, $source:expr) => {{
        replace_field!($target, $field, $source);
    }};
}
impl RevisionedQueryDatabase {
    fn replace_core(&mut self) {
        let peer: Self = Construct::default();
        forward_field!(self, runtime, peer);
    }
}"#,
        ),
    ];
    for (label, fixture) in runtime_field_ident_macro_fixtures.iter().copied() {
        assert!(
            database_runtime_field_authorities(fixture, true)
                .contains(&"runtime-field-macro-bare-identifier".to_owned()),
            "the {label} fixture must expose its bare runtime field macro argument",
        );
        assert_eq!(
            revisioned_database_default_references(fixture, true),
            ["RevisionedQueryDatabase:call"],
            "the {label} fixture must expose its aliased Default peer construction",
        );
    }

    // A single underlying constructor expression can still execute twice when
    // wrapped in an eager map. This valid-Rust model preserves the forwarding
    // signatures and every weaker family identifier/call count, but the live
    // balanced identity of the complete impl rejects the multiplicity change.
    let shared_single_forward = r#"        self.0
            .content_addressed_family_with_equality_and_retained_charge(
                stable_name,
                retention_limit,
                value_equal,
                RetainedCharge::retained_charge,
            )"#;
    let shared_repeated_forward = r#"        [stable_name.into(), Arc::<str>::from("compiler.peer")]
            .map(|name| {
                self.0
                    .content_addressed_family_with_equality_and_retained_charge(
                        name,
                        retention_limit,
                        value_equal,
                        RetainedCharge::retained_charge,
                    )
            })
            .into_iter()
            .next()
            .expect("the expected forwarding result is first")"#;
    assert_eq!(
        shared_family_forwarding
            .matches(shared_single_forward)
            .count(),
        1,
        "the shared-helper multiplicity fixture must replace one real forwarding body",
    );
    let adversarial_shared =
        shared_family_forwarding.replacen(shared_single_forward, shared_repeated_forward, 1);
    let adversarial_shared =
        exact_balanced_code_item(&adversarial_shared, "impl CompilerQueryRuntime {");
    assert_eq!(
        family_constructor_calls(adversarial_shared),
        family_constructor_calls(shared_family_forwarding),
        "the shared-helper fixture must preserve weaker constructor-expression counts",
    );
    assert_eq!(
        family_authority_identifiers(adversarial_shared),
        family_authority_identifiers(shared_family_forwarding),
        "the shared-helper fixture must preserve weaker family identifier counts",
    );
    assert_ne!(
        executable_source_shape_identity(adversarial_shared),
        SHARED_FAMILY_FORWARDING_IDENTITY,
        "the complete forwarding identity must reject repeated execution",
    );

    // The ordered composer has the same multiplicity risk one layer higher: a
    // local closure can retain one syntactic macro invocation while executing
    // it twice. Build the model by replacing the current production statement
    // so the balanced extractor and live identity are exercised directly.
    let composer_one_shot = r#"        let parse_modules_for_batch = parse_modules.clone();
        let parse_module_batches =
            register_parse_import_parse_module_batches!(parse_modules_for_batch, runtime);"#;
    let composer_repeated = r#"        let build_parse_module_batches = || {
            let parse_modules_for_batch = parse_modules.clone();
            register_parse_import_parse_module_batches!(parse_modules_for_batch, runtime)
        };
        let parse_module_batches = build_parse_module_batches();
        let _peer_parse_module_batches = build_parse_module_batches();"#;
    assert_eq!(
        ordered_registration_composer
            .matches(composer_one_shot)
            .count(),
        1,
        "the composer multiplicity fixture must replace one real registration statement",
    );
    let adversarial_composer =
        ordered_registration_composer.replacen(composer_one_shot, composer_repeated, 1);
    let adversarial_composer = exact_balanced_code_item(
        &adversarial_composer,
        "fn with_declaration_memo_retention_and_concurrency(",
    );
    assert_eq!(
        registration_macro_invocations(adversarial_composer),
        registration_macro_invocations(ordered_registration_composer),
        "the composer fixture must preserve the complete macro invocation stream",
    );
    assert_eq!(
        family_constructor_calls(adversarial_composer),
        family_constructor_calls(ordered_registration_composer),
        "the composer fixture must preserve weaker constructor-expression counts",
    );
    assert_eq!(
        code_identifier_count(
            adversarial_composer,
            "with_declaration_memo_retention_and_concurrency",
        ),
        code_identifier_count(
            ordered_registration_composer,
            "with_declaration_memo_retention_and_concurrency",
        ),
        "the composer fixture must preserve the complete function signature owner",
    );
    assert_ne!(
        executable_source_shape_identity(adversarial_composer),
        ORDERED_REGISTRATION_COMPOSER_IDENTITY,
        "the complete composer identity must reject repeated execution",
    );

    assert_eq!(
        REGISTRATION_LEAF_ONE_SHOT_IDENTITIES.len(),
        REGISTRATION_MANIFEST.len(),
    );
    for (index, (owner, family, macro_name, source)) in REGISTRATION_MANIFEST.iter().enumerate() {
        assert_eq!(
            executable_source_shape_identity(source),
            REGISTRATION_LEAF_ONE_SHOT_IDENTITIES[index],
            "manifested family {family} in {macro_name} changed its reviewed one-shot registration shape",
        );
        assert_eq!(
            REGISTRATION_MANIFEST
                .iter()
                .filter(|entry| entry.1 == *family)
                .count(),
            1,
            "registered family has multiple manifest authorities: {family}"
        );
        assert_eq!(
            source
                .matches(&format!("macro_rules! {macro_name}"))
                .count(),
            1
        );
        assert_eq!(source.matches(&format!("\"{family}\"")).count(), 1);
        let expected_constructor = expected_registration_family_constructor(family);
        assert_eq!(
            family_constructor_calls(source),
            vec![expected_constructor.to_owned()],
            "registered family {family} must have one exact constructor in {macro_name}",
        );
        assert!(
            macro_name.starts_with(&format!("register_{owner}_"))
                || *macro_name == "register_provider_probe"
        );
    }

    let (provider_index, provider_source) = REGISTRATION_MANIFEST
        .iter()
        .enumerate()
        .find_map(|(index, (_, family, _, source))| {
            (*family == "compiler.body-fact-provider-probe").then_some((index, *source))
        })
        .expect("provider probe manifested registration leaf");
    let provider_identity = REGISTRATION_LEAF_ONE_SHOT_IDENTITIES[provider_index];
    assert_eq!(
        executable_source_shape_identity(provider_source),
        provider_identity,
    );
    let provider_fixture = |body: &str| {
        format!(
            r#"#[allow(unused_macros)]
macro_rules! register_provider_probe {{
    ($runtime:ident) => {{{{
{body}
    }}}};
}}
"#,
        )
    };
    let provider_constructor = |name: &str| {
        format!(
            r#"$runtime
    .family_with_equality(
        {name},
        BODY_QUERY_MEMO_RETENTION,
        |left: &ProviderProbeValue, right: &ProviderProbeValue| left == right,
    )
    .expect("the provider-probe family has one canonical name")"#,
        )
    };
    let literal_constructor = provider_constructor("\"compiler.body-fact-provider-probe\"");
    let variable_constructor = provider_constructor("name");
    let multiplicity_fixtures = [
        (
            "array map",
            provider_fixture(
                &r#"[
    "compiler.body-fact-provider-probe",
    "compiler.peer",
]
.map(|name| VARIABLE_REGISTER)
.into_iter()
.next()
.expect("the expected family is first")"#
                    .replace("VARIABLE_REGISTER", &variable_constructor),
            ),
        ),
        (
            "for loop",
            provider_fixture(
                &r#"let mut expected = None;
for _ in 0..2 {
    let registered = REGISTER;
    if expected.is_none() {
        expected = Some(registered);
    }
}
expected.expect("the loop registered a family")"#
                    .replace("REGISTER", &literal_constructor),
            ),
        ),
        (
            "while loop",
            provider_fixture(
                &r#"let mut attempt = 0;
let mut expected = None;
while attempt < 2 {
    let registered = REGISTER;
    if expected.is_none() {
        expected = Some(registered);
    }
    attempt += 1;
}
expected.expect("the loop registered a family")"#
                    .replace("REGISTER", &literal_constructor),
            ),
        ),
        (
            "loop expression",
            provider_fixture(
                &r#"let mut remaining = 2;
let mut expected = None;
loop {
    let registered = REGISTER;
    if expected.is_none() {
        expected = Some(registered);
    }
    remaining -= 1;
    if remaining == 0 {
        break;
    }
}
expected.expect("the loop registered a family")"#
                    .replace("REGISTER", &literal_constructor),
            ),
        ),
        (
            "iterator closure",
            provider_fixture(
                &r#"(0..2)
    .map(|_| REGISTER)
    .collect::<Vec<_>>()
    .into_iter()
    .next()
    .expect("the iterator registered a family")"#
                    .replace("REGISTER", &literal_constructor),
            ),
        ),
        (
            "local closure called twice",
            provider_fixture(
                &r#"let register = || REGISTER;
let expected = register();
let _peer = register();
expected"#
                    .replace("REGISTER", &literal_constructor),
            ),
        ),
    ];
    for (label, fixture) in multiplicity_fixtures {
        assert_eq!(
            fixture
                .matches("macro_rules! register_provider_probe")
                .count(),
            1,
        );
        assert_eq!(
            fixture
                .matches("\"compiler.body-fact-provider-probe\"")
                .count(),
            1,
            "the {label} fixture must preserve the weak literal-count invariant",
        );
        assert_eq!(
            family_constructor_calls(&fixture),
            ["family_with_equality"],
            "the {label} fixture must preserve the weak constructor-expression invariant",
        );
        assert_ne!(
            executable_source_shape_identity(&fixture),
            provider_identity,
            "the {label} fixture must fail the live one-shot leaf identity",
        );
    }

    let peer_authority_fixture = r#"
struct BodyPeer { runtime: QueryRuntime }
fn peer() {
    let runtime = QueryRuntime::with_retention_budgets(1, RetentionBudgets::default());
    let _peer = runtime.family("compiler.peer", 1);
    let _observed = node.family();
}
"#;
    assert_eq!(
        code_identifier_count(peer_authority_fixture, "QueryRuntime"),
        2
    );
    assert_eq!(
        family_constructor_calls(peer_authority_fixture),
        vec!["family".to_owned()],
        "the constructor scanner must include base family calls but not observations",
    );
    let function_item_fixture = r#"
let _family = CompilerQueryRuntime::family_with_evaluator::<K, V, _>;
let _content = CompilerQueryRuntime::content_addressed_family_with_equality::<K, V>;
let _base = QueryRuntime::family::<K, V>;
let _observed = node.family();
"#;
    assert_eq!(
        family_constructor_calls(function_item_fixture),
        [
            "family_with_evaluator",
            "content_addressed_family_with_equality",
            "family",
        ]
        .map(str::to_owned),
        "UFCS and turbofish function-item aliases must remain family authorities",
    );
    let raw_family_fixture = r#"
let _method = runtime.r#family("compiler.peer", 1);
let _base = QueryRuntime::r#family::<K, V>;
let _named = CompilerQueryRuntime::r#family_with_evaluator;
let _content = CompilerQueryRuntime::r#content_addressed_family_with_equality::<K, V>;
let _observed = node.r#family();
"#;
    assert_eq!(
        family_constructor_calls(raw_family_fixture),
        [
            "family",
            "family",
            "family_with_evaluator",
            "content_addressed_family_with_equality",
        ]
        .map(str::to_owned),
        "raw method and UFCS identifiers must retain ordinary family authority",
    );
    let macro_indirection_fixtures = [
        r#"
fn peer(runtime: &CompilerQueryRuntime) {
    macro_rules! invoke { ($r:expr, $m:ident) => { $r.$m("compiler.peer", 1) } }
    invoke!(runtime, family);
}
"#,
        r#"
fn peer(runtime: &CompilerQueryRuntime) {
    macro_rules! invoke { ($r:expr, $m:ident) => { $r.$m("compiler.peer", 1) } }
    invoke!(runtime, r#family);
}
"#,
        r#"
fn peer(runtime: &CompilerQueryRuntime) {
    macro_rules! invoke { ($r:expr, $m:ident) => { $r.$m("compiler.peer", 1) } }
    macro_rules! forward { ($r:expr, $m:ident) => { invoke!($r, $m) } }
    forward!(runtime, family);
}
"#,
    ];
    for macro_indirection_fixture in macro_indirection_fixtures.iter().copied() {
        assert!(
            family_constructor_calls(macro_indirection_fixture).is_empty(),
            "receiver/method syntax alone cannot see a macro ident argument",
        );
        assert_eq!(
            family_authority_identifiers(macro_indirection_fixture),
            ["family"],
            "ordinary, raw, and forwarded macro ident arguments must retain family authority",
        );
    }
    let literal_masking_fixture = r###"
fn peer() {
    let quote = '"';
    let escaped_quote = '\'';
    let byte_quote = b'"';
    let byte_escaped_quote = b'\'';
    let masked = br#"QueryRuntime::new(1); include!(\"masked.rs\");"#;
    let runtime = QueryRuntime::with_retention_budgets(1, RetentionBudgets::default());
}
"###;
    assert_eq!(
        code_identifier_count(literal_masking_fixture, "QueryRuntime"),
        1,
        "character, byte-character, and raw-byte-string literals must not mask later code",
    );
    assert!(include_macro_paths(literal_masking_fixture).is_empty());
    assert_eq!(
        code_identifier_count(
            "use rue_query::QueryRuntime as R; let runtime = R::with_retention_budgets(1, RetentionBudgets::default());",
            "QueryRuntime",
        ),
        1,
        "a runtime alias must retain its inventoried QueryRuntime identifier",
    );

    // Replace the revisioned database's test-bearing aggregate with every
    // production owner and registration leaf. The exact module and include
    // inventories below establish that this expansion is complete.
    let mut revisioned_production_sources =
        vec![("revisioned_database::hub".to_owned(), production_hub)];
    revisioned_production_sources.extend(
        REVISIONED_DATABASE_PHASES
            .iter()
            .copied()
            .filter(|(owner, _)| *owner != "test_support")
            .map(|(owner, source)| (format!("revisioned_database::{owner}"), source)),
    );
    revisioned_production_sources.extend(
        REVISIONED_DATABASE_REGISTRATION_MODULES
            .iter()
            .copied()
            .map(|(owner, source)| (format!("revisioned_database::{owner}"), source)),
    );
    revisioned_production_sources.extend(
        REGISTRATION_MANIFEST
            .iter()
            .map(|(_, _, macro_name, source)| {
                (format!("revisioned_database::{macro_name}"), *source)
            }),
    );
    let crate_root = include_str!("lib.rs");
    let (crate_root_namespace_declarations, crate_root_namespace_bindings) =
        crate_root_namespace_inventory(crate_root);
    assert_eq!(
        executable_source_shape_identity(crate_root),
        COMPILER_CRATE_ROOT_IDENTITY,
        "the complete crate root changed absolute-path name-resolution authority",
    );
    assert_eq!(
        (
            crate_root_namespace_declarations.len(),
            source_inventory_fingerprint(&crate_root_namespace_declarations),
            crate_root_namespace_bindings.len(),
            source_inventory_fingerprint(&crate_root_namespace_bindings),
        ),
        COMPILER_CRATE_ROOT_NAMESPACE_IDENTITY,
        "the exact crate-root module/import/extern-crate binding inventory changed",
    );
    assert!(
        forbidden_crate_root_resolution_bindings(crate_root).is_empty(),
        "the crate root cannot bind core, std, or default and redirect an absolute constructor path",
    );

    // `::core` is absolute only after crate-root resolution. Seal the raw root
    // plus every direct namespace binding, and conservatively retain
    // identifiable local macro arguments that could generate such a binding.
    // Bindings generated by external procedural macros remain an expansion-
    // review boundary rather than a claim this source scanner can prove.
    let crate_root_rebinding_fixtures: [(&str, &str, &[&str], bool); 7] = [
        (
            "reviewer core/default aliases",
            "extern crate self as core; use content_digest as default;",
            &["extern:core", "use:default"],
            true,
        ),
        (
            "raw core/default aliases",
            "extern crate self as r#core; use content_digest as r#default;",
            &["extern:core", "use:default"],
            true,
        ),
        (
            "std/default aliases",
            "extern crate self as std; use content_digest as default;",
            &["extern:std", "use:default"],
            true,
        ),
        (
            "raw module bindings",
            "mod r#core {} mod r#default {}",
            &["mod:core", "mod:default"],
            true,
        ),
        (
            "use aliases",
            "use content_digest as core; use content_digest as std; use content_digest as default;",
            &["use:core", "use:default", "use:std"],
            true,
        ),
        (
            "local macro core/default arguments",
            r#"macro_rules! bind_root_names {
    ($prelude:ident, $fallback:ident) => {
        extern crate self as $prelude;
        use content_digest as $fallback;
    };
}
bind_root_names!(core, default);"#,
            &["macro-argument:core", "macro-argument:default"],
            false,
        ),
        (
            "raw local macro std/default arguments",
            r#"macro_rules! bind_root_names {
    ($prelude:ident, $fallback:ident) => {
        extern crate self as $prelude;
        use content_digest as $fallback;
    };
}
bind_root_names!(r#std, r#default);"#,
            &["macro-argument:default", "macro-argument:std"],
            false,
        ),
    ];
    for (label, fixture, expected_forbidden, direct_namespace_change) in
        crate_root_rebinding_fixtures
    {
        let adversarial_root = format!("{crate_root}\n{fixture}\n");
        let constructor_model = format!("{adversarial_root}\n{compiler_session_constructor}");
        let preserved_constructor =
            exact_balanced_code_item(&constructor_model, compiler_session_constructor_marker);
        assert_eq!(
            executable_source_shape_identity(preserved_constructor),
            COMPILER_SESSION_CONSTRUCTOR_IDENTITY,
            "the {label} fixture must preserve the weaker constructor-byte identity",
        );
        assert_eq!(
            function_identifier_usage(preserved_constructor, "default"),
            function_identifier_usage(compiler_session_constructor, "default"),
            "the {label} fixture must preserve the weaker Default call-expression count",
        );
        assert_ne!(
            executable_source_shape_identity(&adversarial_root),
            COMPILER_CRATE_ROOT_IDENTITY,
            "the {label} fixture must fail the complete crate-root identity",
        );
        let (adversarial_declarations, adversarial_bindings) =
            crate_root_namespace_inventory(&adversarial_root);
        if direct_namespace_change {
            assert_ne!(
                (
                    adversarial_declarations.len(),
                    source_inventory_fingerprint(&adversarial_declarations),
                    adversarial_bindings.len(),
                    source_inventory_fingerprint(&adversarial_bindings),
                ),
                COMPILER_CRATE_ROOT_NAMESPACE_IDENTITY,
                "the {label} fixture must fail the direct namespace inventory",
            );
        }
        assert_eq!(
            forbidden_crate_root_resolution_bindings(&adversarial_root),
            expected_forbidden
                .iter()
                .map(|binding| (*binding).to_owned())
                .collect::<Vec<_>>(),
            "the {label} fixture must fail the live forbidden-binding gate",
        );
    }

    let mut compiler_production_sources = vec![("crate_root".to_owned(), crate_root)];
    compiler_production_sources.extend(
        PRODUCTION_MODULES
            .iter()
            .copied()
            .filter(|(owner, _)| *owner != "revisioned_query_database")
            .map(|(owner, source)| (owner.to_owned(), source)),
    );
    compiler_production_sources.extend(revisioned_production_sources.iter().cloned());

    // Every manifested macro name has exactly three resolution owners: its
    // leaf definition, its pinned wrapper re-export, and its composer call.
    // The cfg(test) provider probe additionally has the composer's explicit
    // import. Identifier inventory is deliberately broader than `name!(...)`:
    // aliases, raw spellings, shadow definitions, and macro-name forwarding
    // all retain the normalized identifier and therefore require review.
    for (owner, _, macro_name, _) in REGISTRATION_MANIFEST {
        assert_eq!(
            REGISTRATION_MANIFEST
                .iter()
                .filter(|(_, _, candidate, _)| candidate == macro_name)
                .count(),
            1,
            "registration macro names must be unique: {macro_name}",
        );
        let wrapper = if *macro_name == "register_provider_probe" {
            "provider"
        } else {
            owner
        };
        let mut expected = vec![
            (
                "revisioned_database::registrations".to_owned(),
                if *macro_name == "register_provider_probe" {
                    2
                } else {
                    1
                },
            ),
            (format!("revisioned_database::registrations_{wrapper}"), 1),
            (format!("revisioned_database::{macro_name}"), 1),
        ];
        expected.sort();
        assert_eq!(
            identifier_owner_inventory(&compiler_production_sources, macro_name),
            expected,
            "manifested registration macro {macro_name} gained an alias, shadow, forwarded name, or unreviewed resolution owner",
        );
    }

    let parse_macro = "register_parse_import_parse";
    let parse_macro_identifier_inventory =
        identifier_owner_inventory(&compiler_production_sources, parse_macro);
    let shadow_injection_marker = "impl RevisionedQueryDatabase {";
    let composer_shadow_fixtures = [
        (
            "alias and local shadow",
            r#"use parse_import::register_parse_import_parse as original_parse;
macro_rules! register_parse_import_parse {
    ($runtime:ident) => {{
        let primary = original_parse!($runtime);
        let _peer = original_parse!($runtime);
        primary
    }};
}"#,
        ),
        (
            "raw alias and shadow",
            r#"use parse_import::r#register_parse_import_parse as r#original_parse;
macro_rules! r#register_parse_import_parse {
    ($runtime:ident) => {{
        let primary = r#original_parse!($runtime);
        let _peer = r#original_parse!($runtime);
        primary
    }};
}"#,
        ),
        (
            "macro-generated shadow name",
            r#"use parse_import::register_parse_import_parse as original_parse;
macro_rules! define_registration_shadow {
    ($name:ident, $original:ident, $dollar:tt) => {
        macro_rules! $name {
            ($dollar runtime:ident) => {{
                let primary = $original!($dollar runtime);
                let _peer = $original!($dollar runtime);
                primary
            }};
        }
    };
}
define_registration_shadow!(register_parse_import_parse, original_parse, $);"#,
        ),
    ];
    for (label, shadow) in composer_shadow_fixtures {
        let adversarial_registration_composer = registration_composer.replacen(
            shadow_injection_marker,
            &format!("{shadow}\n\n{shadow_injection_marker}"),
            1,
        );
        assert_eq!(
            registration_macro_invocations(&adversarial_registration_composer),
            invocations,
            "the {label} fixture must preserve the weaker registration invocation stream",
        );
        assert_eq!(
            code_identifier_count(&adversarial_registration_composer, "QueryRuntime"),
            code_identifier_count(registration_composer, "QueryRuntime"),
            "the {label} fixture must preserve the weaker runtime identifier count",
        );
        assert_eq!(
            family_constructor_calls(&adversarial_registration_composer),
            family_constructor_calls(registration_composer),
            "the {label} fixture must preserve the weaker family-constructor count",
        );
        assert_eq!(
            executable_source_shape_identity(exact_balanced_code_item(
                &adversarial_registration_composer,
                "fn with_declaration_memo_retention_and_concurrency(",
            )),
            ORDERED_REGISTRATION_COMPOSER_IDENTITY,
            "the {label} fixture models a shadow outside the previously sealed composer body",
        );
        assert_ne!(
            executable_source_shape_identity(&adversarial_registration_composer),
            REGISTRATION_AUTHORITY_MODULE_IDENTITY,
            "the {label} fixture must fail the complete registration-module identity",
        );
        let adversarial_sources = compiler_production_sources
            .iter()
            .map(|(owner, source)| {
                if owner == "revisioned_database::registrations" {
                    (owner.clone(), adversarial_registration_composer.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(
            identifier_owner_inventory(&adversarial_sources, parse_macro),
            parse_macro_identifier_inventory,
            "the {label} fixture must fail the live registration-macro identifier inventory",
        );
    }

    let parse_wrapper_owner = "revisioned_database::registrations_parse_import";
    let parse_wrapper = revisioned_production_sources
        .iter()
        .find_map(|(owner, source)| (owner == parse_wrapper_owner).then_some(*source))
        .expect("parse/import registration wrapper source");
    let wrapper_reexport = "pub(super) use register_parse_import_parse;";
    let wrapper_shadow = r#"pub(super) use register_parse_import_parse as original_parse;
macro_rules! register_parse_import_parse {
    ($runtime:ident) => {{
        let primary = parse_import::original_parse!($runtime);
        let _peer = parse_import::original_parse!($runtime);
        primary
    }};
}
pub(super) use register_parse_import_parse;"#;
    let adversarial_parse_wrapper = parse_wrapper.replacen(wrapper_reexport, wrapper_shadow, 1);
    assert_eq!(
        include_macro_paths(&adversarial_parse_wrapper),
        include_macro_paths(parse_wrapper),
        "the wrapper shadow fixture must preserve the weaker leaf-include inventory",
    );
    assert_eq!(
        registration_macro_invocations(&adversarial_parse_wrapper),
        registration_macro_invocations(parse_wrapper),
        "the wrapper shadow fixture must preserve the weaker registration invocation count",
    );
    assert_eq!(
        code_identifier_count(&adversarial_parse_wrapper, "QueryRuntime"),
        code_identifier_count(parse_wrapper, "QueryRuntime"),
        "the wrapper shadow fixture must preserve the weaker runtime identifier count",
    );
    assert_eq!(
        family_constructor_calls(&adversarial_parse_wrapper),
        family_constructor_calls(parse_wrapper),
        "the wrapper shadow fixture must preserve the weaker family-constructor count",
    );
    assert_ne!(
        executable_source_shape_identity(&adversarial_parse_wrapper),
        REGISTRATION_WRAPPER_MODULE_IDENTITIES[2],
        "the wrapper shadow fixture must fail the pinned parse/import wrapper identity",
    );
    let adversarial_wrapper_sources = compiler_production_sources
        .iter()
        .map(|(owner, source)| {
            if owner == parse_wrapper_owner {
                (owner.clone(), adversarial_parse_wrapper.as_str())
            } else {
                (owner.clone(), *source)
            }
        })
        .collect::<Vec<_>>();
    assert_ne!(
        identifier_owner_inventory(&adversarial_wrapper_sources, parse_macro),
        parse_macro_identifier_inventory,
        "the wrapper shadow fixture must fail the live registration-macro identifier inventory",
    );

    // The raw registrations source intentionally retains a cfg(test) Default
    // adapter for the existing unit corpus. Remove that exact, identity-pinned
    // item for production construction inventories. With no production
    // `Default` impl, `Default::default`, `unwrap_or_default`, generic
    // `T: Default`, and aliases of those routes cannot produce this database;
    // Rust's trait solver rejects them before runtime authority is reachable.
    let revisioned_construction_sources = revisioned_production_sources
        .iter()
        .map(|(owner, source)| {
            if owner == "revisioned_database::registrations" {
                (owner.clone(), production_registration_composer.as_str())
            } else {
                (owner.clone(), *source)
            }
        })
        .collect::<Vec<_>>();
    let production_session_source = SESSION_PRODUCTION_SOURCE;
    let compiler_construction_sources = compiler_production_sources
        .iter()
        .map(|(owner, source)| {
            if owner == "revisioned_database::registrations" {
                (owner.clone(), production_registration_composer.as_str())
            } else if owner == "session" {
                (owner.clone(), production_session_source)
            } else {
                (owner.clone(), *source)
            }
        })
        .collect::<Vec<_>>();

    assert_eq!(
        database_new_reference_owner_inventory(&compiler_construction_sources),
        [(
            "session".to_owned(),
            "RevisionedQueryDatabase:call".to_owned(),
            1,
        )],
        "the frontend session must remain the sole production inherent-constructor caller; aliases and function items are forbidden",
    );
    assert_eq!(
        database_new_definition_owner_inventory(&compiler_construction_sources),
        [("revisioned_database::registrations".to_owned(), 1)],
        "registrations must own the sole production inherent database constructor",
    );
    assert_eq!(
        construction_token_new_reference_owner_inventory(&compiler_construction_sources),
        [(
            "session".to_owned(),
            "RevisionedQueryDatabaseConstructionToken:call".to_owned(),
            1,
        )],
        "the frontend session must remain the sole production capability constructor; aliases and function items are forbidden",
    );
    assert_eq!(
        identifier_owner_inventory(
            &compiler_construction_sources,
            "RevisionedQueryDatabaseConstructionToken",
        ),
        [
            ("revisioned_database::registrations".to_owned(), 1),
            ("session".to_owned(), 3),
        ],
        "the capability type may appear only in its session declaration/private constructor, the frontend call, and the token-gated database signature",
    );
    let expected_frontend_database_identifiers = [("session".to_owned(), 3)];
    assert_eq!(
        identifier_owner_inventory(&compiler_construction_sources, "FrontendQueryDatabase",),
        expected_frontend_database_identifiers,
        "the frontend database type must have exactly its declaration, Default owner, and canonical CompilerSession field",
    );
    assert_eq!(
        type_method_definition_owner_inventory(
            &compiler_construction_sources,
            "CompilerSession",
            "new",
        ),
        [("session".to_owned(), "public".to_owned(), 1)],
        "the compiler must have exactly one public CompilerSession::new owner",
    );
    assert!(
        type_method_definition_owner_inventory(
            &compiler_construction_sources,
            "CompilerSession",
            "default",
        )
        .is_empty(),
        "no compiler production owner may define an inherent CompilerSession::default",
    );
    let adversarial_session_with_inherent_default =
        format!("{production_session_source}\n{compiler_session_inherent_default_fixture}");
    let adversarial_inherent_default_sources = compiler_construction_sources
        .iter()
        .map(|(owner, source)| {
            if owner == "session" {
                (
                    owner.clone(),
                    adversarial_session_with_inherent_default.as_str(),
                )
            } else {
                (owner.clone(), *source)
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        type_method_definition_owner_inventory(
            &adversarial_inherent_default_sources,
            "CompilerSession",
            "default",
        ),
        [("session".to_owned(), "private".to_owned(), 1)],
        "the private inherent-default fixture must fail the live compiler-wide owner inventory",
    );
    assert!(
        type_alias_owner_inventory(&compiler_construction_sources, "FrontendQueryDatabase",)
            .is_empty(),
        "production compiler source cannot alias the frontend database type",
    );
    for trait_name in ["Clone", "Copy", "Default"] {
        assert!(
            compiler_construction_sources.iter().all(|(_, source)| {
                type_trait_impl_count(
                    source,
                    "RevisionedQueryDatabaseConstructionToken",
                    trait_name,
                ) == 0
            }),
            "the construction capability cannot implement {trait_name}",
        );
    }

    // `CompilerSession` derives `Default`, so every one of its private fields
    // is part of the runtime-construction authority. These valid-Rust models
    // preserve the sole explicit token/database calls but make the derived
    // constructor create a second frontend database. The exact root identity,
    // semantic carrier-field check, and compiler-wide identifier/alias
    // inventories independently reject the change.
    let add_compiler_session_field = |field: &str| {
        let body = compiler_session_root
            .strip_suffix('}')
            .expect("CompilerSession exact item closes with one brace");
        format!("{body}    {field}\n}}")
    };
    let compiler_session_multiplicity_fixtures = [
        (
            "direct duplicate",
            "",
            "_peer_queries: FrontendQueryDatabase,",
        ),
        (
            "type alias duplicate",
            "type PeerFrontendQueryDatabase = FrontendQueryDatabase;\n",
            "_peer_queries: PeerFrontendQueryDatabase,",
        ),
        (
            "tuple duplicate",
            "",
            "_peer_queries: (FrontendQueryDatabase,),",
        ),
        (
            "Option duplicate",
            "",
            "_peer_queries: Option<FrontendQueryDatabase>,",
        ),
        (
            "local wrapper duplicate",
            "#[derive(Default)]\nstruct PeerFrontendQueryDatabase(FrontendQueryDatabase);\n",
            "_peer_queries: PeerFrontendQueryDatabase,",
        ),
    ];
    for (label, companion, field) in compiler_session_multiplicity_fixtures {
        let adversarial_root = add_compiler_session_field(field);
        assert_ne!(
            executable_source_shape_identity(&adversarial_root),
            COMPILER_SESSION_ROOT_IDENTITY,
            "the {label} must fail the live CompilerSession root identity",
        );
        let adversarial_session = format!(
            "{}\n{companion}",
            production_session_source.replacen(compiler_session_root, &adversarial_root, 1)
        );
        assert_ne!(
            struct_fields_with_type_carrier(
                &adversarial_session,
                &adversarial_root,
                "FrontendQueryDatabase",
            ),
            ["queries|cfg=false|FrontendQueryDatabase"],
            "the {label} must fail the semantic derived-Default field inventory",
        );
        let adversarial_sources = compiler_construction_sources
            .iter()
            .map(|(owner, source)| {
                if owner == "session" {
                    (owner.clone(), adversarial_session.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            database_new_reference_owner_inventory(&adversarial_sources),
            database_new_reference_owner_inventory(&compiler_construction_sources),
            "the {label} must preserve the weaker explicit database-new inventory",
        );
        assert_eq!(
            construction_token_new_reference_owner_inventory(&adversarial_sources),
            construction_token_new_reference_owner_inventory(&compiler_construction_sources),
            "the {label} must preserve the weaker explicit capability-new inventory",
        );
        assert_ne!(
            identifier_owner_inventory(&adversarial_sources, "FrontendQueryDatabase"),
            expected_frontend_database_identifiers,
            "the {label} must fail the live compiler-wide frontend database identifier inventory",
        );
    }
    let non_frontend_session_sources = [
        include_str!("session/metrics.rs"),
        include_str!("session/rooted_artifacts.rs"),
        include_str!("session/discovery_continuation.rs"),
        include_str!("session/revision_lifecycle.rs"),
        include_str!("session/import_discovery_owner.rs"),
        include_str!("session/metrics_attempts.rs"),
        include_str!("session/program_artifacts.rs"),
        include_str!("session/rooted_projections.rs"),
        include_str!("session.rs"),
    ];
    assert!(
        non_frontend_session_sources
            .iter()
            .all(|source| revisioned_database_new_references(source).is_empty()),
        "session/frontend_queries.rs must remain the exact session construction owner",
    );
    assert!(
        non_frontend_session_sources
            .iter()
            .all(|source| construction_token_new_references(source).is_empty()),
        "session/frontend_queries.rs must remain the exact capability-construction owner",
    );
    assert!(
        database_construction_owner_inventory(&compiler_construction_sources, false).is_empty(),
        "production compiler source cannot construct RevisionedQueryDatabase through Default",
    );
    assert!(
        compiler_construction_sources
            .iter()
            .all(|(_, source)| revisioned_database_default_impl_count(source) == 0),
        "production compiler source cannot restore a Default impl for RevisionedQueryDatabase",
    );
    let compiler_default_reference_inventory =
        database_construction_owner_inventory(&compiler_construction_sources, true);
    assert_eq!(
        compiler_default_reference_inventory,
        [
            ("backend".to_owned(), "Default:call".to_owned(), 3),
            ("codegen_query".to_owned(), "Default:call".to_owned(), 3),
            ("durable_comptime".to_owned(), "Default:call".to_owned(), 2),
            (
                "local_semantic_materialization".to_owned(),
                "Self:call".to_owned(),
                1,
            ),
            ("parsed_modules".to_owned(), "Default:call".to_owned(), 1),
            ("queries".to_owned(), "Default:call".to_owned(), 2),
            ("session".to_owned(), "Self:call".to_owned(), 5),
            ("unstable".to_owned(), "Default:call".to_owned(), 10),
            ("unstable".to_owned(), "Self:call".to_owned(), 1),
        ],
        "all compiler Default::default and Self::default spellings need exact owners because source scanning cannot infer an unannotated target type",
    );
    assert!(
        database_construction_owner_inventory(&revisioned_construction_sources, true).is_empty(),
        "revisioned production children cannot construct a peer database through explicit, Self, Default, raw, qualified, alias, or function-item spellings",
    );
    assert!(
        database_new_reference_owner_inventory(&revisioned_construction_sources).is_empty(),
        "revisioned production children cannot re-enter the inherent database constructor",
    );
    let test_construction_sources = [
        (
            "revisioned_database::test_support".to_owned(),
            include_str!("revisioned_query_database/test_support.rs"),
        ),
        (
            "revisioned_database::tests::backend".to_owned(),
            include_str!("revisioned_query_database/tests/backend.rs"),
        ),
        (
            "revisioned_database::tests::body_provider::body".to_owned(),
            include_str!("revisioned_query_database/tests/body_provider/body.rs"),
        ),
        (
            "revisioned_database::tests::body_provider::provider".to_owned(),
            include_str!("revisioned_query_database/tests/body_provider/provider.rs"),
        ),
        (
            "revisioned_database::tests::parse_import".to_owned(),
            include_str!("revisioned_query_database/tests/parse_import.rs"),
        ),
        (
            "revisioned_database::tests::retention_cancellation".to_owned(),
            include_str!("revisioned_query_database/tests/retention_cancellation.rs"),
        ),
        (
            "revisioned_database::tests::semantic_declaration".to_owned(),
            include_str!("revisioned_query_database/tests/semantic_declaration.rs"),
        ),
    ];
    assert_eq!(
        database_construction_owner_inventory(&test_construction_sources, false),
        [
            (
                "revisioned_database::test_support".to_owned(),
                "RevisionedQueryDatabase:call".to_owned(),
                1,
            ),
            (
                "revisioned_database::tests::backend".to_owned(),
                "RevisionedQueryDatabase:call".to_owned(),
                15
            ),
            (
                "revisioned_database::tests::body_provider::body".to_owned(),
                "RevisionedQueryDatabase:call".to_owned(),
                54
            ),
            (
                "revisioned_database::tests::body_provider::provider".to_owned(),
                "RevisionedQueryDatabase:call".to_owned(),
                32
            ),
            (
                "revisioned_database::tests::parse_import".to_owned(),
                "RevisionedQueryDatabase:call".to_owned(),
                30
            ),
            (
                "revisioned_database::tests::semantic_declaration".to_owned(),
                "RevisionedQueryDatabase:call".to_owned(),
                49
            ),
        ],
        "independent test databases have a separate exact construction inventory",
    );

    // The complete registration impl and composer identities seal these 44
    // manifested invocations plus the existing cfg(test) provider probe. Keep
    // their bare local `runtime` arguments inventoried so adding a forwarding
    // macro anywhere in the same owner cannot hide behind an owner exemption.
    let expected_runtime_authorities = [
        (
            "revisioned_database::registrations".to_owned(),
            "database-runtime-struct-form".to_owned(),
            1,
        ),
        (
            "revisioned_database::registrations".to_owned(),
            "runtime-field-macro-bare-identifier".to_owned(),
            45,
        ),
    ];
    assert_eq!(
        database_runtime_authority_inventory(&revisioned_production_sources),
        expected_runtime_authorities,
        "the runtime field must be initialized once by the sealed composer and never replaced or mutably exposed",
    );
    let child_owner = "revisioned_database::body_closure_nucleus";
    let child_source = revisioned_production_sources
        .iter()
        .find_map(|(owner, source)| (owner == child_owner).then_some(*source))
        .expect("reviewed revisioned child source");
    let split_core_child = format!("{child_source}\n{split_core_fixture}");
    let split_core_sources = revisioned_production_sources
        .iter()
        .map(|(owner, source)| {
            if owner == child_owner {
                (owner.clone(), split_core_child.as_str())
            } else {
                (owner.clone(), *source)
            }
        })
        .collect::<Vec<_>>();
    assert!(
        !database_construction_owner_inventory(&split_core_sources, true).is_empty(),
        "the reviewer child peer construction must fail the live construction inventory",
    );
    assert_ne!(
        database_runtime_authority_inventory(&split_core_sources),
        expected_runtime_authorities,
        "the reviewer child runtime assignment must fail the live runtime inventory",
    );
    for (label, fixture) in runtime_mutation_fixtures.iter().copied() {
        let adversarial_child = format!("{child_source}\n{fixture}");
        let adversarial_sources = revisioned_production_sources
            .iter()
            .map(|(owner, source)| {
                if owner == child_owner {
                    (owner.clone(), adversarial_child.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(
            database_runtime_authority_inventory(&adversarial_sources),
            expected_runtime_authorities,
            "the {label} fixture must fail the live runtime authority inventory",
        );
    }
    for (label, fixture) in runtime_macro_mutation_fixtures.iter().copied() {
        let adversarial_child = format!("{child_source}\n{fixture}");
        let adversarial_sources = revisioned_production_sources
            .iter()
            .map(|(owner, source)| {
                if owner == child_owner {
                    (owner.clone(), adversarial_child.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(
            database_runtime_authority_inventory(&adversarial_sources),
            expected_runtime_authorities,
            "the {label} fixture must fail the live runtime authority inventory",
        );
        assert!(
            !database_construction_owner_inventory(&adversarial_sources, true).is_empty(),
            "the {label} fixture must fail the live peer-construction inventory",
        );
    }
    for (label, fixture) in runtime_field_ident_macro_fixtures.iter().copied() {
        let adversarial_child = format!("{child_source}\n{fixture}");
        let adversarial_sources = revisioned_production_sources
            .iter()
            .map(|(owner, source)| {
                if owner == child_owner {
                    (owner.clone(), adversarial_child.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(
            database_runtime_authority_inventory(&adversarial_sources),
            expected_runtime_authorities,
            "the {label} fixture must fail the live runtime authority inventory",
        );
        assert!(
            !database_construction_owner_inventory(&adversarial_sources, true).is_empty(),
            "the {label} fixture must fail the live aliased-construction inventory",
        );
    }

    let composer_constructor_owners = compiler_production_sources
        .iter()
        .filter_map(|(owner, source)| {
            let identifiers = code_identifier_count(source, composer_constructor);
            (identifiers != 0).then(|| {
                let (definitions, calls, references) =
                    function_identifier_usage(source, composer_constructor);
                (owner.clone(), definitions, calls, references, identifiers)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        composer_constructor_owners,
        [("revisioned_database::registrations".to_owned(), 1, 4, 0, 5,)],
        "the private composer must have one definition plus the canonical entry and three cfg(test) factory calls, all registrations-owned",
    );
    let canonical_constructor_owners = compiler_construction_sources
        .iter()
        .filter_map(|(owner, source)| {
            let identifiers = code_identifier_count(source, "new_canonical");
            (identifiers != 0).then(|| {
                let (definitions, calls, references) =
                    function_identifier_usage(source, "new_canonical");
                (owner.clone(), definitions, calls, references, identifiers)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        canonical_constructor_owners,
        [("revisioned_database::registrations".to_owned(), 1, 1, 0, 2)],
        "the private canonical constructor must remain registrations-owned with one token-gated production caller",
    );

    let revisioned_aggregate = PRODUCTION_MODULES
        .iter()
        .find_map(|(owner, source)| (*owner == "revisioned_query_database").then_some(*source))
        .expect("revisioned database aggregate source");
    let mut compiler_module_sources = compiler_production_sources.clone();
    compiler_module_sources.push((
        "revisioned_database::test_bearing_aggregate".to_owned(),
        revisioned_aggregate,
    ));
    let nested_module_owners = module_owner_inventory(&compiler_module_sources);
    let nested_module_fingerprint = source_inventory_fingerprint(&nested_module_owners);
    let expected_nested_module_identity = (151, 14_424_826_081_772_489_333);
    assert_eq!(
        (nested_module_owners.len(), nested_module_fingerprint),
        expected_nested_module_identity,
        "every nested compiler module edge must have an exact reviewed source owner",
    );
    for nested_module_fixture in ["mod peer { fn nested() {} }", "pub(crate) mod peer;"] {
        assert_eq!(
            module_declarations(nested_module_fixture),
            ["peer"],
            "inline and semicolon module edges must both be detected",
        );
        let adversarial_root = format!("{crate_root}\n{nested_module_fixture}");
        let adversarial_sources = compiler_module_sources
            .iter()
            .map(|(owner, source)| {
                if owner == "crate_root" {
                    (owner.clone(), adversarial_root.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        let adversarial_modules = module_owner_inventory(&adversarial_sources);
        assert_ne!(
            (
                adversarial_modules.len(),
                source_inventory_fingerprint(&adversarial_modules),
            ),
            expected_nested_module_identity,
            "a crate-root module edge must perturb the live owner inventory",
        );
    }

    let mut actual_include_sites = compiler_production_sources
        .iter()
        .flat_map(|(owner, source)| {
            include_macro_paths(source)
                .into_iter()
                .map(move |path| (owner.clone(), path))
        })
        .collect::<Vec<_>>();
    actual_include_sites.sort();
    let mut expected_include_sites = REGISTRATION_MANIFEST
        .iter()
        .map(|(owner, _, macro_name, _)| {
            let include_owner = if *macro_name == "register_provider_probe" {
                "provider"
            } else {
                owner
            };
            (
                format!("revisioned_database::registrations_{include_owner}"),
                registration_include_path(owner, macro_name),
            )
        })
        .collect::<Vec<_>>();
    expected_include_sites.sort();
    assert_eq!(
        actual_include_sites, expected_include_sites,
        "compiler production include! edges must correspond one-for-one to the registration manifest",
    );
    let mut expected_include_owner_names = expected_include_sites
        .iter()
        .map(|(owner, _)| owner.clone())
        .collect::<Vec<_>>();
    expected_include_owner_names.sort();
    let mut expected_include_identifier_owners: Vec<(String, usize)> = Vec::new();
    for owner in expected_include_owner_names {
        if let Some((last_owner, count)) = expected_include_identifier_owners.last_mut()
            && *last_owner == owner
        {
            *count += 1;
        } else {
            expected_include_identifier_owners.push((owner, 1));
        }
    }
    assert_eq!(
        expected_include_identifier_owners
            .iter()
            .map(|(_, count)| count)
            .sum::<usize>(),
        44,
        "the identifier inventory must derive from every manifested include leaf",
    );
    assert_eq!(
        identifier_owner_inventory(&compiler_production_sources, "include"),
        expected_include_identifier_owners,
        "every compiler include identifier must be one canonical direct manifest include",
    );
    let include_alias_fixture = "use std::include as inc; inc!(\"body/peer_alias.rs\");";
    assert!(
        include_macro_paths(include_alias_fixture).is_empty(),
        "the direct include parser deliberately does not mistake an alias for include!",
    );
    assert_eq!(
        code_identifier_count(include_alias_fixture, "include"),
        1,
        "the identifier inventory must retain an aliased include import",
    );
    assert_eq!(
        code_identifier_count("use std::r#include as inc;", "include"),
        1,
        "raw include identifiers must normalize to the reviewed spelling",
    );
    let mut aliased_include_sources = compiler_production_sources.clone();
    aliased_include_sources.push(("include_alias_fixture".to_owned(), include_alias_fixture));
    assert_ne!(
        identifier_owner_inventory(&aliased_include_sources, "include"),
        expected_include_identifier_owners,
        "an aliased include must perturb the live compiler-wide identifier inventory",
    );
    for (extra_include_fixture, extra_path) in [
        (
            "let quote = '\"'; include ! (\"body/peer_paren.rs\");",
            "body/peer_paren.rs",
        ),
        (
            "include ! [ \"body/peer_bracket.rs\" ];",
            "body/peer_bracket.rs",
        ),
        (
            "include ! { \"body/peer_brace.rs\" };",
            "body/peer_brace.rs",
        ),
    ] {
        assert_eq!(
            include_macro_paths(extra_include_fixture),
            [extra_path],
            "every valid include! delimiter must expose its source edge",
        );
        assert!(
            !expected_include_sites
                .iter()
                .any(|(_, path)| path == extra_path),
            "an adversarial include must remain outside the manifest: {extra_path}",
        );
    }

    let query_runtime_identifier_owners = compiler_production_sources
        .iter()
        .filter_map(|(owner, source)| {
            let count = code_identifier_count(source, "QueryRuntime");
            (count != 0).then(|| (owner.clone(), count))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        query_runtime_identifier_owners,
        [
            ("object_query".to_owned(), 1),
            ("revisioned_database::hub".to_owned(), 1),
            ("revisioned_database::shared".to_owned(), 3),
            ("revisioned_database::backend".to_owned(), 3),
            ("revisioned_database::body_transactions".to_owned(), 1),
            ("revisioned_database::registrations".to_owned(), 1),
        ],
        "every compiler QueryRuntime identifier must have an exact reviewed owner",
    );
    let object_query = PRODUCTION_MODULES
        .iter()
        .find_map(|(owner, source)| (*owner == "object_query").then_some(*source))
        .expect("object query production inventory entry");
    let (object_query_production, object_query_tests) = object_query
        .split_once("\n#[cfg(test)]\nmod tests {")
        .expect("object query inline test boundary");
    assert_eq!(
        code_identifier_count(object_query_production, "QueryRuntime"),
        0,
        "object query production code cannot own a QueryRuntime",
    );
    assert!(
        family_constructor_calls(object_query_production).is_empty(),
        "object query production code cannot construct runtime families",
    );
    assert_eq!(
        object_query_tests
            .matches("let runtime = rue_query::QueryRuntime::new(1);")
            .count(),
        1,
        "the sole non-revisioned QueryRuntime identifier is an inline cfg(test) runtime",
    );
    let registrations = REVISIONED_DATABASE_PHASES
        .iter()
        .find_map(|(owner, source)| (*owner == "registrations").then_some(*source))
        .expect("registration composer source");
    let compact_registrations = rust_code_only(registrations)
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    assert_eq!(
        std::str::from_utf8(&compact_registrations)
            .expect("whitespace removal preserves UTF-8")
            .matches("CompilerQueryRuntime(QueryRuntime::new(query_concurrency))")
            .count(),
        1,
        "the sole production runtime construction must remain registrations-owned",
    );

    let actual_family_constructor_owners = compiler_production_sources
        .iter()
        .filter_map(|(owner, source)| {
            let constructors = family_constructor_calls(source);
            (!constructors.is_empty()).then(|| (owner.clone(), constructors))
        })
        .collect::<Vec<_>>();
    let actual_family_identifier_owners =
        family_identifier_owner_inventory(&compiler_production_sources);
    let mut expected_family_identifier_owners = vec![
        ("canonical_lower".to_owned(), "family".to_owned(), 13),
        ("durable_comptime".to_owned(), "family".to_owned(), 1),
        ("object_query".to_owned(), "family".to_owned(), 2),
        (
            "object_query".to_owned(),
            "family_with_equality_and_evaluator".to_owned(),
            1,
        ),
        (
            "revisioned_database::backend".to_owned(),
            "family".to_owned(),
            2,
        ),
        (
            "revisioned_database::body_closure_nucleus".to_owned(),
            "family".to_owned(),
            8,
        ),
        (
            "revisioned_database::body_provider_body".to_owned(),
            "family".to_owned(),
            2,
        ),
        (
            "revisioned_database::parse_import".to_owned(),
            "family".to_owned(),
            3,
        ),
        (
            "revisioned_database::parse_import_program_assembly".to_owned(),
            "family".to_owned(),
            1,
        ),
        (
            "revisioned_database::register_semantic_semantic_nucleus".to_owned(),
            "family".to_owned(),
            18,
        ),
        (
            "revisioned_database::semantic".to_owned(),
            "family".to_owned(),
            13,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "content_addressed_family_with_equality".to_owned(),
            1,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "content_addressed_family_with_equality_and_retained_charge".to_owned(),
            1,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "family_with_equality".to_owned(),
            1,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "family_with_equality_and_evaluator".to_owned(),
            2,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "family_with_equality_and_evaluator_and_retained_charge".to_owned(),
            1,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "family_with_equality_and_retained_charge".to_owned(),
            1,
        ),
        (
            "revisioned_database::shared".to_owned(),
            "family_with_evaluator".to_owned(),
            1,
        ),
        ("session".to_owned(), "family".to_owned(), 21),
    ];
    expected_family_identifier_owners.extend(REGISTRATION_MANIFEST.iter().map(
        |(_, family, macro_name, _)| {
            (
                format!("revisioned_database::{macro_name}"),
                expected_registration_family_constructor(family).to_owned(),
                1,
            )
        },
    ));
    expected_family_identifier_owners.sort();
    assert_eq!(
        actual_family_identifier_owners, expected_family_identifier_owners,
        "every compiler family identifier spelling must have an exact reviewed owner and count",
    );
    let registrations_source = compiler_production_sources
        .iter()
        .find_map(|(owner, source)| {
            (owner == "revisioned_database::registrations").then_some(*source)
        })
        .expect("registration composer compiler source entry");
    for macro_indirection_fixture in macro_indirection_fixtures.iter().copied() {
        let adversarial_registrations =
            format!("{registrations_source}\n{macro_indirection_fixture}");
        let adversarial_sources = compiler_production_sources
            .iter()
            .map(|(owner, source)| {
                if owner == "revisioned_database::registrations" {
                    (owner.clone(), adversarial_registrations.as_str())
                } else {
                    (owner.clone(), *source)
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(
            family_identifier_owner_inventory(&adversarial_sources),
            expected_family_identifier_owners,
            "macro family indirection in an existing allowed owner must perturb the live inventory",
        );
    }
    let mut expected_family_constructor_owners = vec![
        (
            "object_query".to_owned(),
            vec!["family_with_equality_and_evaluator".to_owned()],
        ),
        (
            "revisioned_database::shared".to_owned(),
            [
                "family_with_equality_and_evaluator_and_retained_charge",
                "family_with_equality_and_evaluator",
                "family_with_equality_and_retained_charge",
                "content_addressed_family_with_equality_and_retained_charge",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
    ];
    expected_family_constructor_owners.extend(REGISTRATION_MANIFEST.iter().map(
        |(_, family, macro_name, _)| {
            (
                format!("revisioned_database::{macro_name}"),
                vec![expected_registration_family_constructor(family).to_owned()],
            )
        },
    ));
    assert_eq!(
        actual_family_constructor_owners, expected_family_constructor_owners,
        "every compiler family constructor must be a reviewed forwarding helper, inline test, or manifested registration",
    );
    let manifested_constructor_owners = REGISTRATION_MANIFEST
        .iter()
        .map(|(_, _, macro_name, _)| format!("revisioned_database::{macro_name}"))
        .collect::<Vec<_>>();
    assert!(
        actual_family_constructor_owners
            .iter()
            .all(|(owner, _)| owner == "object_query"
                || owner == "revisioned_database::shared"
                || manifested_constructor_owners.contains(owner)),
        "direct family construction is sealed to the shared forwarding impl and manifested leaves; object_query is the explicitly pinned cfg(test) runtime",
    );
    assert_eq!(
        REVISIONED_DATABASE_PHASES
            .iter()
            .flat_map(|(_, source)| source.match_indices("struct RevisionedQueryDatabase"))
            .count(),
        1,
        "RevisionedQueryDatabase must have exactly one canonical owner"
    );
    assert_eq!(
        REVISIONED_DATABASE_PHASES
            .iter()
            .flat_map(|(_, source)| source.match_indices("struct CompilerQueryRuntime"))
            .count(),
        1,
        "query runtime must have exactly one canonical owner"
    );
    let database_fields = REVISIONED_DATABASE_PHASES
        .iter()
        .find(|(module, _)| *module == "shared")
        .map(|(_, source)| {
            source
                .split_once("pub(crate) struct RevisionedQueryDatabase {")
                .and_then(|(_, body)| body.split_once("\n}"))
                .map(|(body, _)| body)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    assert!(
        !database_fields
            .lines()
            .any(|line| line.trim_start().starts_with("pub(crate)")),
        "RevisionedQueryDatabase fields must stay private to the module tree"
    );
    let mut declarations = Vec::new();
    for (owner, source) in REVISIONED_DATABASE_PHASES {
        // Reuse the balanced public-declaration scanner below by normalizing
        // the visibility spelling. This inventories complete multiline
        // signatures and aggregate field shapes, not merely their first line.
        let normalized = source.replace("pub(crate)", "pub");
        declarations.extend(
            public_declarations(&normalized)
                .into_iter()
                .map(|declaration| format!("{owner}|{}", canonical_signature(&declaration))),
        );
    }
    declarations.sort();
    let fingerprint = declarations
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, declaration| {
            declaration
                .bytes()
                .chain(std::iter::once(b'\n'))
                .fold(hash, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                })
        });
    assert_eq!(
        (declarations.len(), fingerprint),
        (210, 13_505_876_172_349_810_937),
        "crate-visible declaration names, signatures, fields, or phase owners changed"
    );

    // The baseline began as the identity manifest extracted from the pre-split
    // source and grows only for reviewed cross-module authorities. This catches
    // accidentally widened helpers even when a source-shape fingerprint is
    // updated alongside them. The cfg(test) registration manifest is
    // intentionally an inventory-only addition and is excluded.
    let mut actual_identities = REVISIONED_DATABASE_PHASES
        .iter()
        .flat_map(|(_, source)| crate_visible_declaration_identities(source))
        .filter(|identity| identity != "const:REGISTRATION_MANIFEST")
        .collect::<Vec<_>>();
    actual_identities.sort();
    let mut original_identities = ORIGINAL_REVISIONED_DATABASE_CRATE_SURFACE
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    original_identities.sort();
    assert_eq!(
        actual_identities, original_identities,
        "crate-visible declaration names/kinds diverged from the pre-split surface"
    );
}

#[test]
fn revisioned_body_and_program_assembly_have_exact_source_owners() {
    let source = |owner: &str| {
        REVISIONED_DATABASE_PHASES
            .iter()
            .find_map(|(candidate, source)| (*candidate == owner).then_some(*source))
            .unwrap_or_else(|| panic!("missing revisioned database source owner {owner}"))
    };

    let body_facade = source("body");
    let body_modules = body_facade
        .lines()
        .filter_map(|line| line.strip_prefix("mod "))
        .filter_map(|line| line.strip_suffix(';'))
        .collect::<Vec<_>>();
    assert_eq!(
        body_modules,
        [
            "closure_nucleus",
            "durable_comptime_adapters",
            "provider_body",
            "revision_symbol_space",
            "transactions",
        ],
        "body.rs must remain the exact ownership facade"
    );
    assert!(body_facade.lines().count() < 40);
    for forbidden in [
        "impl RevisionedQueryDatabase",
        "pub(crate) fn ",
        "pub(crate) struct ",
        "pub(crate) enum ",
        "pub(in crate::revisioned_query_database) fn ",
        "pub(in crate::revisioned_query_database) struct ",
        "pub(in crate::revisioned_query_database) enum ",
    ] {
        assert!(
            !body_facade.contains(forbidden),
            "body.rs regained catch-all implementation authority through {forbidden}"
        );
    }

    let parse_import = source("parse_import");
    assert_eq!(
        parse_import.matches("mod program_assembly;").count(),
        1,
        "parse/import must declare exactly one program-assembly owner"
    );

    for (definition, owner) in [
        (
            "pub(crate) struct BodyClosureRequest",
            "body_closure_nucleus",
        ),
        (
            "pub(in crate::revisioned_query_database) fn instance_producer_closure(",
            "body_closure_nucleus",
        ),
        (
            "pub(in crate::revisioned_query_database) fn visit_instance_anonymous_nominals",
            "body_closure_nucleus",
        ),
        (
            "pub(crate) fn collect_instance_anonymous_nominals(",
            "body_closure_nucleus",
        ),
        ("pub(crate) fn body_closure(", "body_closure_nucleus"),
        (
            "pub(crate) fn projected_declaration_semantics_for_modules(",
            "body_closure_nucleus",
        ),
        (
            "pub(crate) fn durable_type_from_instance_key(",
            "body_durable_comptime_adapters",
        ),
        (
            "pub(crate) fn durable_value_from_argument(",
            "body_durable_comptime_adapters",
        ),
        (
            "pub(in crate::revisioned_query_database) fn collect_durable_anonymous_nominal_dependencies(",
            "body_durable_comptime_adapters",
        ),
        (
            "pub(in crate::revisioned_query_database) struct DurableComptimeRootAuthority",
            "body_durable_comptime_adapters",
        ),
        ("fn body_type_instance(", "body_provider_body"),
        (
            "pub(in crate::revisioned_query_database) fn collect_published_body_references(",
            "body_provider_body",
        ),
        (
            "pub(crate) fn semantic_candidate_import_occurrences(",
            "body_provider_body",
        ),
        ("impl SemanticNucleusTypeProvider<'_>", "body_provider_body"),
        (
            "pub(in crate::revisioned_query_database) struct BodyInputResolver",
            "body_provider_body",
        ),
        (
            "pub(in crate::revisioned_query_database) struct RevisionSymbolSpace",
            "body_revision_symbol_space",
        ),
        (
            "pub(crate) enum BodyTransactionRequestFailure",
            "body_transactions",
        ),
        (
            "pub(in crate::revisioned_query_database) struct BodyTransactionEvaluator",
            "body_transactions",
        ),
        ("pub(crate) fn body_transaction(", "body_transactions"),
        ("fn parse_module_frontier(", "parse_import_program_assembly"),
        (
            "pub(crate) fn parse_program_extension(",
            "parse_import_program_assembly",
        ),
        (
            "pub(crate) fn parse_program(",
            "parse_import_program_assembly",
        ),
        ("pub(crate) fn runtime_retention_metrics(", "shared"),
        (
            "pub(crate) fn body_reachability_metrics(",
            "body_closure_nucleus",
        ),
        (
            "pub(crate) fn input_stamp_retention_metrics(",
            "parse_import_program_assembly",
        ),
    ] {
        assert!(
            source(owner).contains(definition),
            "{owner} lost exact authority {definition}"
        );
        assert_eq!(
            REVISIONED_DATABASE_PHASES
                .iter()
                .map(|(_, source)| source.matches(definition).count())
                .sum::<usize>(),
            1,
            "{definition} must have exactly one revisioned-database owner ({owner})"
        );
    }

    let durable_adapters = source("body_durable_comptime_adapters");
    let durable_code = rust_code_only(durable_adapters);
    let durable_identifiers = code_identifiers(&durable_code)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for forbidden in [
        "RevisionedQueryDatabase",
        "QueryRuntime",
        "CompilerQueryRuntime",
        "runtime",
        "family",
        "body_transaction",
        "body_transactions",
        "body_closure_root",
        "body_reachability_root",
        "lookup_root_lease",
        "PublishedLookupRootHandoff",
    ] {
        assert!(
            !durable_identifiers.contains(forbidden),
            "durable comptime adapters gained database/runtime/body authority through {forbidden}"
        );
    }
    for forbidden_prefix in ["BodyTransaction", "PublishedBody"] {
        assert!(
            durable_identifiers
                .iter()
                .all(|identifier| !identifier.starts_with(forbidden_prefix)),
            "durable comptime adapters gained body publication/control authority through {forbidden_prefix}*",
        );
    }
    assert!(
        family_constructor_calls(durable_adapters).is_empty(),
        "durable comptime adapters gained family-construction authority",
    );

    let program_assembly = source("parse_import_program_assembly");
    let program_code = rust_code_only(program_assembly);
    let program_identifiers = code_identifiers(&program_code)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for forbidden in [
        "body_transaction",
        "body_transactions",
        "body_closure_root",
        "body_reachability_root",
        "lookup_root_lease",
        "PublishedLookupRootHandoff",
        "SemanticNucleusTypeProvider",
        "ComptimeEngine",
    ] {
        assert!(
            !program_identifiers.contains(forbidden),
            "parse/import program assembly gained unrelated body authority through {forbidden}"
        );
    }
    for forbidden_prefix in [
        "BodyTransaction",
        "BodyClosure",
        "BodyReachability",
        "PublishedBody",
    ] {
        assert!(
            program_identifiers
                .iter()
                .all(|identifier| !identifier.starts_with(forbidden_prefix)),
            "parse/import program assembly gained body publication/control authority through {forbidden_prefix}*",
        );
    }

    // Every deliberately shared child entry is inventoried after translating
    // its database-tree visibility to the scanner's ordinary `pub` spelling.
    // File-local helpers are absent by construction, so widening one creates a
    // reviewed count/fingerprint change even when its name already exists in a
    // sibling owner.
    let mut shared_declarations = REVISIONED_DATABASE_PHASES
        .iter()
        .filter(|(owner, _)| {
            owner.starts_with("body_") || *owner == "parse_import_program_assembly"
        })
        .flat_map(|(owner, source)| {
            let normalized = source.replace("pub(in crate::revisioned_query_database)", "pub");
            public_declarations(&normalized)
                .into_iter()
                .map(|declaration| format!("{owner}|{}", canonical_signature(&declaration)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    shared_declarations.sort();
    let shared_fingerprint =
        shared_declarations
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, declaration| {
                declaration
                    .bytes()
                    .chain(std::iter::once(b'\n'))
                    .fold(hash, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                    })
            });
    assert_eq!(
        (shared_declarations.len(), shared_fingerprint),
        (54, 18_319_594_177_463_541_342),
        "database-tree shared body/program API changed"
    );
}

const ORIGINAL_REVISIONED_DATABASE_CRATE_SURFACE: &str = r#"
struct:TestBodyTransactionFailureGuard
struct:FrontierRendezvous
fn:new
fn:wait_for_arrivals
fn:arrivals
fn:frontier_arrivals
fn:timed_out
fn:release
struct:FrontierRendezvousGuard
struct:TestConstraintGenerationCancellationGuard
struct:CompatibilityKey
struct:RevisionedQueryDatabase
fn:new
struct:TestCodegenEvaluatorGate
fn:wait_until_entered
fn:release
struct:TestBackendBatchEvaluatorGate
fn:wait_until_all_entered_and_release
fn:peak
fn:entered
struct:ProviderObservationCounters
struct:ObservedLookupRoot
enum:LookupObservationKey
fn:body_lookup_root_identity
struct:BackendRootCandidate
struct:OptimizedCfgBatchKey
struct:RawCfgBatchKey
struct:RawCfgBatchOutput
fn:new
struct:OptimizedCfgBatchOutput
struct:CodegenUnitBatchKey
struct:CodegenUnitBatchOutput
struct:ObjectProjectionBatchKey
struct:ObjectProjectionBatchOutput
enum:BackendRootPublicationInput
struct:PublishedBackendRootMetrics
struct:InputStampRetentionMetrics
struct:ModuleIndexEntry
struct:ModuleIndex
struct:ProjectedModuleIndex
enum:StableDeclarationClassificationFailure
enum:DeclarationBodyPlanFailure
struct:WarningStaticCallHead
enum:WarningBodyReferencesValue
struct:WarningBodyReferencesBatchValue
struct:FrontierChildExecution
enum:WarningBodyReferencesFailure
enum:DeclarationShellBatchFailure
enum:SemanticNucleusBatchFailure
struct:SemanticNucleusProjection
struct:LookupNameKey
struct:LookupImportKey
enum:ImportInputTransition
fn:project_transaction_diagnostics
enum:OverlayJustification
fn:function_definition_key
fn:declaration_candidate_for_stable_key
enum:BodyTransactionRequestFailure
struct:BodyClosureRequest
fn:execution_for
fn:was_retained
fn:accrue_reachability_work
fn:accrue_candidate_body_plan_work
use:crate
use:closure_nucleus
use:durable_comptime_adapters
use:provider_body
use:transactions
fn:semantic_nucleus_failure_is_internal_error
fn:collect_instance_anonymous_nominals
fn:durable_type_from_instance_key
fn:durable_value_from_argument
fn:semantic_candidate_import_occurrences
fn:with_declaration_memo_retention
fn:with_query_concurrency
fn:with_interner_limit
fn:arm_codegen_evaluator_gate_for_test
fn:arm_codegen_batch_evaluator_gate_for_test
fn:runtime_metrics_for_test
fn:inject_body_transaction_failure_for_test
fn:cancel_constraint_generation_after_nodes_for_test
fn:cancel_frontier_constraint_generation_after_nodes_for_test
fn:arm_frontier_rendezvous_for_test
fn:constraint_generation_visits_for_test
fn:constraint_generation_attempted_siblings_for_test
fn:constraint_generation_post_cancel_attempts_for_test
fn:constraint_generation_phase_for_test
fn:provider_observation_metrics
fn:promote_published_lookup_root
fn:refresh_published_body_lookup_root
fn:lookup_pressure_metrics
const:SOURCE_INPUT
fn:current_parse_revision
fn:parse_family
fn:selected_parse_terminal
fn:last_good_parse_terminal
fn:last_good_parse_record
fn:request_parse
fn:current_semantic_revision
fn:cfg
fn:raw_cfg_batch
fn:optimized_cfg
fn:optimized_cfg_batch
fn:codegen_unit
fn:codegen_unit_batch
fn:object_projection_batch
fn:begin_backend_root
fn:retain_backend_optimized_cfg_batch
fn:retain_backend_codegen_batch
fn:retain_backend_object_projection_batch
fn:publish_backend_root
fn:backend_root_metrics_for_test
fn:raw_cfg_handoff_matches_terminal_for_test
fn:backend_cfg_key_is_retained_for_test
fn:object_projection_key_is_retained_for_test
fn:query_evictions_for_test
fn:projected_declaration_shells
fn:projected_declaration_shells_for_modules
fn:body_transaction
fn:body_closure
fn:body_source_basis_projection
fn:warning_body_reference_frontier
fn:body_produced_anonymous_projection
fn:body_input
fn:body_toolchain_demands
fn:any_body_transaction_terminal
fn:any_successful_body_transaction_for_test
fn:has_retained_body_key
fn:retained_body_identity_states_for_test
fn:retained_body_transaction_origins_for_test
fn:retained_body_transaction_for_test
fn:projected_declaration_semantics
fn:projected_declaration_semantics_for_modules
fn:begin_import_inputs
fn:import_frontier
fn:current_import_revision
fn:restore_import_revision_after_abort
fn:commit_import_request
fn:import_frontier_roots_requested
fn:exact_import_groups_dispatched
fn:import_view_full_leaves_published
fn:import_view_overlay_leaves_published
fn:import_view_ledger_entries_cloned
fn:import_view_source_entries_compared
fn:import_view_read_entries_compared
fn:identity_resolution
fn:lineage_additions
fn:clear_lineage_additions
fn:exact_import_groups
fn:publication_cone_retention_failures
fn:stage_module_parses
fn:publish_import_batch
fn:publish_trusted_successor_view
fn:import_ledger
fn:current_import_view_state
fn:source_revision
fn:module_source_input
fn:parse_program_extension
fn:parse_program
fn:compose_candidate_module_rirs
fn:projected_module_indexes
fn:select_parse
fn:select_parse_candidate
fn:parse_attempt_view
fn:parse_origin_attempt_ids
fn:runtime_retention_metrics
fn:body_reachability_metrics
fn:input_stamp_retention_metrics
fn:set_module_input_retention_for_test
fn:module_source_stamp_for_test
fn:execution
struct:ReceiverTypeIdentity
fn:new
enum:CompilerBodyProviderIncomplete
enum:CompilerBodyProviderStatus
struct:CompilerBodyProviderQueries
struct:CompilerBodyFactProvider
struct:CompilerBodyDurableSource
fn:new
fn:finish_status
fn:probe_comptime_call
enum:ProviderTypeFactsFailure
struct:ProviderTypeFacts
fn:new
struct:SignatureFacts
fn:new
struct:ProviderProbeOutcome
fn:probe_body_facts
fn:probe_ready_body_facts
fn:publish_lookup_root
fn:production_declarations
fn:durable_decl
struct:DurableDeclSource
fn:from_declarations
fn:with_anonymous_nominals
struct:EndpointNominalRender
fn:endpoint_display
fn:endpoint_is_copy
fn:endpoint_nominal_render
"#;

fn crate_visible_declaration_identities(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let declaration = line.trim_start().strip_prefix("pub(crate) ")?;
            let identifiers = code_identifiers(declaration);
            let keyword = identifiers.iter().find(|identifier| {
                matches!(
                    **identifier,
                    "fn" | "struct"
                        | "enum"
                        | "union"
                        | "trait"
                        | "type"
                        | "const"
                        | "static"
                        | "mod"
                        | "use"
                )
            })?;
            let name = identifiers
                .iter()
                .skip_while(|identifier| *identifier != keyword)
                .nth(1)?;
            Some(format!("{keyword}:{name}"))
        })
        .collect()
}

const DURABLE_COMPTIME_FACADE_SOURCE: &str = include_str!("durable_comptime.rs");
const DURABLE_COMPTIME_DIAGNOSTICS_SOURCE: &str = include_str!("durable_comptime/diagnostics.rs");
const DURABLE_COMPTIME_EFFECTS_SOURCE: &str = include_str!("durable_comptime/effects.rs");
const DURABLE_COMPTIME_HOST_SOURCE: &str = include_str!("durable_comptime/host.rs");
const DURABLE_COMPTIME_LIFECYCLE_SOURCE: &str = include_str!("durable_comptime/lifecycle.rs");
const DURABLE_COMPTIME_PROJECTION_SOURCE: &str = include_str!("durable_comptime/projection.rs");
const DURABLE_COMPTIME_SERVICES_SOURCE: &str = include_str!("durable_comptime/services.rs");
const DURABLE_COMPTIME_STRUCTURED_SOURCE: &str = include_str!("durable_comptime/structured.rs");
const DURABLE_COMPTIME_TARGET_SOURCE: &str = include_str!("durable_comptime/target.rs");
const DURABLE_COMPTIME_ENGINE_ENTRY_SOURCE: &str =
    include_str!("revisioned_query_database/body/durable_comptime_adapters.rs");
const DURABLE_COMPTIME_SOURCE: &str = concat!(
    include_str!("durable_comptime.rs"),
    include_str!("durable_comptime/diagnostics.rs"),
    include_str!("durable_comptime/effects.rs"),
    include_str!("durable_comptime/host.rs"),
    include_str!("durable_comptime/lifecycle.rs"),
    include_str!("durable_comptime/projection.rs"),
    include_str!("durable_comptime/services.rs"),
    include_str!("durable_comptime/structured.rs"),
    include_str!("durable_comptime/target.rs"),
);

const PRODUCTION_MODULES: &[(&str, &str)] = &[
    ("artifact_views", include_str!("artifact_views.rs")),
    ("backend", include_str!("backend.rs")),
    ("body_query", include_str!("body_query.rs")),
    ("bound_definitions", include_str!("bound_definitions.rs")),
    ("canonical_lower", include_str!("canonical_lower.rs")),
    ("canonical_merge", include_str!("canonical_merge.rs")),
    ("canonical_semantic", include_str!("canonical_semantic.rs")),
    ("cfg_query", include_str!("cfg_query.rs")),
    ("codegen_query", include_str!("codegen_query.rs")),
    ("content_digest", include_str!("content_digest.rs")),
    ("object_query", include_str!("object_query.rs")),
    (
        "declaration_candidate",
        include_str!("declaration_candidate.rs"),
    ),
    (
        "definition_snapshot",
        include_str!("definition_snapshot.rs"),
    ),
    (
        "dependency_envelope",
        include_str!("dependency_envelope.rs"),
    ),
    ("diagnostic", include_str!("diagnostic.rs")),
    (
        "diagnostic_attempt_store",
        include_str!("diagnostic_attempt_store.rs"),
    ),
    ("drop_glue", include_str!("drop_glue.rs")),
    ("durable_cfg", include_str!("durable_cfg.rs")),
    ("durable_comptime", DURABLE_COMPTIME_SOURCE),
    ("durable_semantics", include_str!("durable_semantics.rs")),
    ("import_discovery", include_str!("import_discovery.rs")),
    ("import_graph", include_str!("import_graph.rs")),
    ("linking", include_str!("linking.rs")),
    (
        "local_semantic_materialization",
        include_str!("local_semantic_materialization.rs"),
    ),
    ("parsed_modules", include_str!("parsed_modules.rs")),
    ("program_image_plan", include_str!("program_image_plan.rs")),
    ("queries", include_str!("queries.rs")),
    ("revisioned_query_database", REVISIONED_DATABASE_SOURCE),
    (
        "semantic_query_nucleus",
        include_str!("semantic_query_nucleus.rs"),
    ),
    ("semantic_symbols", include_str!("semantic_symbols.rs")),
    ("semantic_identity", include_str!("semantic_identity.rs")),
    ("session", SESSION_PRODUCTION_SOURCE),
    ("shared_segments", include_str!("shared_segments.rs")),
    ("source_identity", include_str!("source_identity.rs")),
    ("source_metadata", include_str!("source_metadata.rs")),
    ("source_snapshot", include_str!("source_snapshot.rs")),
    ("syntax", include_str!("syntax.rs")),
    (
        "toolchain_module_demand",
        include_str!("toolchain_module_demand.rs"),
    ),
    ("type_queries", include_str!("type_queries.rs")),
    ("typed_query_store", include_str!("typed_query_store.rs")),
    ("unstable", include_str!("unstable.rs")),
    ("warm_fresh_parity", include_str!("warm_fresh_parity.rs")),
    ("well_known_option", include_str!("well_known_option.rs")),
];

#[test]
fn local_semantic_materialization_is_an_inert_exact_fact_boundary() {
    let local = include_str!("local_semantic_materialization.rs");

    for required in [
        "canonical: &crate::body_query::CanonicalBody",
        "declarations: &[DurableDeclarationSemantic]",
        "anonymous_nominals: &[DurableAnonymousNominal]",
        "callable_facts: &[LocalCallableFact]",
        "nominal_metadata: &[LocalNominalMetadataFact]",
        "pub(crate) fn new(",
        "pub(crate) fn identity(&self) -> &StableDefinitionKey",
        "pub(crate) fn lang_item(&self) -> Option<rue_air::LangItem>",
        "rue_air::SemanticImportEpoch::new_local_in_space(",
        ".materialize_local_body_with_types(",
        "FunctionInstanceKey::AnonymousMember",
        "materialize_canonical_body_with_indexes(",
        "materialize_semantic_body_with_indexes(",
    ] {
        assert!(
            local.contains(required),
            "local semantic boundary lost exact input or identity support: {required}"
        );
    }
    for forbidden in [
        "CanonicalSemanticOutput",
        "CanonicalMergedProgram",
        "SemaOutput",
        "QueryFamily<",
        "Mutex<",
        "RwLock<",
        "cache:",
        "materialize_canonical_body(",
        "materialize_semantic_body(",
    ] {
        assert!(
            !local.contains(forbidden),
            "local semantic boundary gained a peer authority or cache: {forbidden}"
        );
    }
}

#[test]
fn the_driver_reads_accessor_declaration_rules_from_the_shared_rule_module() {
    // RUE-1232: this producer walks its own reparsed AST, but the 6.6:3-6.6:7
    // rules it applies — which forms are illegal, in which order, and how each
    // diagnostic reads — are `rue_air::declaration_validation`'s, shared with
    // the RIR producers. Spelling one here is how the two declaration-time
    // producers drifted before.
    let driver = [
        REVISIONED_DATABASE_SOURCE,
        include_str!("semantic_query_nucleus.rs"),
    ]
    .concat();
    for kind in [
        ["ErrorKind::Accessor", "RequiresBorrowSelf"].concat(),
        ["ErrorKind::Accessor", "ParamModeUnsupported"].concat(),
        ["ErrorKind::Accessor", "BodyMissingYield"].concat(),
        ["ErrorKind::Accessor", "BodyOtherExit"].concat(),
        ["ErrorKind::Accessor", "YieldNotReceiverRooted"].concat(),
    ] {
        assert!(
            !driver.contains(&kind),
            "the driver regained its own copy of an accessor declaration rule: {kind}"
        );
    }
    assert!(
        !driver.contains("\"a `-> borrow` accessor\""),
        "the driver regained its own 6.6:3 gate subject"
    );
    for required in [
        "use rue_air::declaration_validation as rules;",
        "rules::accessor_signature_for_mode(",
        "rules::accessor_body_error(",
        "accessor_method_link_error(",
    ] {
        assert!(
            driver.contains(required),
            "the driver stopped reading an accessor rule from the shared module: {required}"
        );
    }
}

#[test]
fn cfg_queries_own_local_semantic_materialization_and_terminal_domains() {
    let cfg = include_str!("cfg_query.rs");
    let domains = include_str!("durable_cfg.rs");
    let database = REVISIONED_DATABASE_SOURCE;
    for required in [
        "CfgSemanticInput::Body",
        "synthesize_canonical_drop_glue(",
        "CfgDomainProjection::from_local_body(",
        "pub(crate) type_pool: rue_air::FrozenTypeInternPool",
        "pub(crate) interner: Arc<lasso::ThreadedRodeo>",
        "pub(crate) local_atoms:",
        "&record.type_pool",
        "callable_by_symbol",
    ] {
        assert!(
            cfg.contains(required),
            "CFG ownership boundary lost: {required}"
        );
    }
    for required in [
        "crate::local_semantic_materialization::materialize_canonical_body_with_indexes_in_space(",
        "crate::local_semantic_materialization::materialize_semantic_body_with_indexes_in_space(",
    ] {
        let live_calls = cfg
            .lines()
            .filter(|line| line.trim_start().starts_with(required))
            .count();
        assert_eq!(
            live_calls, 1,
            "CFG query must have exactly one live indexed materialization call: {required}"
        );
    }
    for forbidden in ["materialize_canonical_body(", "materialize_semantic_body("] {
        assert!(
            !cfg.contains(forbidden),
            "CFG query regained a direct materialization adapter call: {forbidden}"
        );
    }
    for forbidden in [
        "struct CfgLiveInput",
        "pub(crate) live:",
        "key.live",
        "same_optimized_memo_domain",
        "optimized_memo_domain_hash",
    ] {
        assert!(
            !cfg.contains(forbidden),
            "CFG key regained request-local identity state: {forbidden}"
        );
    }
    assert!(
        !database.contains("live: Arc<crate::cfg_query::CfgLiveInput>"),
        "the registered request facade must not require caller-owned CFG live input"
    );
    assert!(domains.contains("for (_, current) in air.iter()"));
    assert!(domains.contains("stable_callable(*name)"));
    assert!(domains.contains("for (current, stable) in &materialization.materialized_types"));
    assert!(!cfg.contains(".find(|fact| fact.symbol.as_ref() == name)"));
    for forbidden in [
        "SemanticImportedBodyDomains",
        "from_body_parts(",
        "air.iter().zip(body.instructions.iter())",
        "air.places().iter().zip(body.places.iter())",
    ] {
        assert!(
            !domains.contains(forbidden),
            "CFG domains regained a second AIR/body reconstruction pass: {forbidden}"
        );
    }

    let production = cfg.rsplit_once("#[cfg(test)]").unwrap().0;
    let optimization = production
        .split_once("pub(crate) fn evaluate_optimized_cfg(")
        .unwrap()
        .1
        .split_once("fn optimize_cfg_without_accessors(")
        .unwrap()
        .0;
    assert!(
        optimization.contains("materialize_splice_interner(&state"),
        "accessor optimization must isolate the published CFG symbol universe"
    );
    let materialization = production
        .split_once("fn materialize_splice_interner(")
        .unwrap()
        .1
        .split_once("pub(crate) fn evaluate_optimized_cfg(")
        .unwrap()
        .0;
    assert_eq!(
        materialization
            .matches("copy_interner_preserving_ordinals(&state.interner")
            .count(),
        1,
        "splice interner isolation must copy the base universe exactly once at publication"
    );
    for forbidden in [
        "InternerChargeRefresh",
        "refresh_interner_retained_charge",
        "let interner = record.interner.clone()",
        "record.interner.get_or_intern",
    ] {
        assert!(
            !production.contains(forbidden),
            "published CFG interners regained a mutation path: {forbidden}"
        );
    }
}

#[test]
fn codegen_queries_consume_only_registered_optimized_cfg_domains() {
    let cfg = include_str!("cfg_query.rs");
    let codegen = include_str!("codegen_query.rs");
    let database = REVISIONED_DATABASE_SOURCE;
    for required in [
        "pub(crate) codegen: Arc<CfgCodegenDomain>",
        "record.codegen.symbol_mappings",
        "&record.cfg",
        "&record.type_pool",
        "&record.strings",
        "&record.interner",
        "key.optimized_cfg.clone()",
        "pub(crate) function: crate::FunctionInstanceKey",
    ] {
        assert!(
            cfg.contains(required) || codegen.contains(required),
            "codegen ownership boundary lost: {required}"
        );
    }
    for forbidden in [
        "struct CodegenLiveInput",
        "pub(crate) live:",
        "key.live",
        "pub(crate) function: crate::FunctionWithCfg",
    ] {
        assert!(
            !codegen.contains(forbidden),
            "codegen key regained caller-owned live state: {forbidden}"
        );
    }
    assert!(
        !database.contains("function: crate::FunctionWithCfg")
            && !database.contains("type_pool: crate::FrozenTypeInternPool")
            && !database.contains("symbol_mappings: Arc<BTreeMap<String, String>>"),
        "the registered codegen request facade must not require semantic/global live inputs"
    );
    let session = SESSION_PRODUCTION_SOURCE;
    assert!(
        session.contains("this adapter enumerates the")
            && session.contains("semantic functions only so focused tests can inspect units")
            && !session.contains("_foreign_symbols: &[String]"),
        "collection must retain only the test-only pre-object inspection adapter"
    );
}

#[test]
fn bounded_symbol_space_constructor_is_owner_only() {
    // rue-rir is a separate crate, so the revision owner cannot use a
    // crate-private constructor. The constructor is deliberately doc-hidden;
    // this inventory is the scoped-authority gate that keeps its only normal
    // build caller inside RevisionSymbolSpace, while the public test seam
    // remains CompilerSession::with_interner_limit (cfg(test) only).
    let database = REVISIONED_DATABASE_SOURCE;
    let production = database.rsplit_once("#[cfg(test)]").unwrap().0;
    assert_eq!(
        production
            .matches("next_generation_with_owner_bound")
            .count(),
        1,
        "only the revision owner may inject the test bound"
    );
}

const RUE_868_RAW_FACADE_VOCABULARY: &[&str] = &[
    "Lexer",
    "Token",
    "TokenKind",
    "Ast",
    "Rir",
    "ThreadedRodeo",
    "FrozenTypeInternPool",
    "TypeInternPool",
    "SemanticSymbol",
    "SemanticSymbolUniverse",
    "CanonicalRirOutput",
    "CanonicalSemanticOutput",
    "CanonicalMergedProgram",
    "ParsedProgram",
    "FunctionWithCfg",
    "Mir",
    "generate_emitted_asm",
    "generate_liveness_info",
    "generate_lowering_info",
    "generate_mir",
    "generate_regalloc_info",
    "generate_stack_frame_info",
    "LoweringDebugInfo",
    "RegAllocDebugInfo",
    "StackFrameInfo",
];

fn source_between_exact_boundaries<'a>(source: &'a str, start: &str, next: &str) -> &'a str {
    let start = source.find(start).expect("source boundary starts");
    let tail = &source[start..];
    let end = tail.find(next).expect("source boundary ends");
    &tail[..end]
}

#[test]
fn warning_body_projection_stays_parse_only_and_below_rir_body_analysis() {
    let parsed = include_str!("parsed_modules.rs");
    let projector = source_between_exact_boundaries(
        parsed,
        "struct ParsedBodyProjectionCollector<'a>",
        "\nfn validate_pair(",
    );
    for forbidden in ["lower_module_rir", "OwnedBodyInput", "ModuleRir"] {
        assert!(
            !projector.contains(forbidden),
            "warning syntax projection crossed into RIR/body lowering: {forbidden}"
        );
    }
    for required in [
        "fn collect_module_projections(",
        "fn visit_callable(",
        "fn visit_expr(",
        "ParsedDeclarationAstLocator::StructMethod",
    ] {
        assert!(
            projector.contains(required),
            "parser-owned warning projection lost its canonical syntax edge: {required}"
        );
    }

    let runtime = REVISIONED_DATABASE_SOURCE;
    for forbidden in [
        "compiler.warning-body-syntax",
        "WarningBodySyntaxQueryKey",
        "WarningStaticCallCollector",
        "fn warning_static_call_heads(",
    ] {
        assert!(
            !runtime.contains(forbidden),
            "warning reachability regained a peer syntax path: {forbidden}"
        );
    }
    let projection_family = registered_family_source("compiler.warning-call-head-projection");
    for required in [
        "compiler.warning-call-head-projection",
        "parse_for_warning_call_heads",
        ".declaration_warning_call_heads(candidate)",
    ] {
        assert!(
            projection_family.contains(required),
            "warning call-head terminal lost its thin parser projection: {required}"
        );
    }
    for forbidden in [".ast()", "rue_parser", "AstGen", "lower_module_rir"] {
        assert!(
            !projection_family.contains(forbidden),
            "warning call-head terminal regained syntax/lowering work: {forbidden}"
        );
    }
    let evaluator = registered_family_source("compiler.warning-body-references");
    for forbidden in [
        "raw_declaration_bodies",
        "declaration_signature_projections",
        "body_inputs",
        "body_transactions",
        "module_rirs",
        "lower_module_rir",
        ".ast()",
    ] {
        assert!(
            !evaluator.contains(forbidden),
            "warning query family crossed into a peer syntax/RIR/body path: {forbidden}"
        );
    }
    for required in [
        "call_heads_for_warning_references",
        "WarningCallHeadProjectionQueryKey",
        "DeclarationImportQueryKey",
        "CanonicalImportResolution::Resolved",
    ] {
        assert!(
            evaluator.contains(required),
            "warning query lost canonical parse/import dependency: {required}"
        );
    }
}

#[test]
fn exact_source_boundaries_ignore_braces_in_comments_and_strings() {
    let fixture = r#"
    fn selected() {
        let _ = "}";
        // }}} must not truncate the selected method
        observed();
    }

    fn next() {}
"#;
    let selected = source_between_exact_boundaries(fixture, "    fn selected()", "\n    fn next()");
    assert!(selected.contains("observed();"));
    assert!(!selected.contains("fn next"));
}

#[test]
fn semantic_signatures_preserve_parser_type_structure_without_a_text_grammar() {
    let projection_source = include_str!("semantic_query_nucleus.rs");
    let projection = source_between_exact_boundaries(
        projection_source,
        "pub(crate) fn project_semantic_signature(",
        "\n#[derive(Debug, Clone, PartialEq, Eq, Hash)]\npub(crate) struct SemanticQueryConfiguration",
    );
    for required in [
        "RirTypeSyntaxBuilder::default()",
        "push_parser_type",
        "declaration_ast(key)",
        "resolve_raw_symbol",
    ] {
        assert!(
            projection.contains(required),
            "signature projection lost its canonical parsed-structure edge: {required}"
        );
    }
    for forbidden in [
        "source_text",
        "parse_semantic_signature",
        "resolve_semantic_type_syntax",
        "render_type",
        "rue_lexer",
        "Lexer::",
        "Parser::",
        "split(",
        "trim(",
    ] {
        assert!(
            !projection.contains(forbidden),
            "signature projection regained a source-text grammar: {forbidden}"
        );
    }

    let runtime = REVISIONED_DATABASE_SOURCE;
    let provider = source_between_exact_boundaries(
        runtime,
        "impl rue_air::SemanticTypeSyntaxProvider<",
        "\nimpl ResolveSemanticSignatureError",
    );
    for forbidden in [
        "parse_type_call_syntax",
        "resolve_semantic_type_syntax(",
        "rue_lexer",
        "rue_parser",
        ".split(",
        ".split_once(",
        ".trim(",
    ] {
        assert!(
            !provider.contains(forbidden),
            "semantic nucleus provider regained a rendered-type grammar: {forbidden}"
        );
    }
    let semantic_phase = REVISIONED_DATABASE_PHASES
        .iter()
        .find_map(|(name, source)| (*name == "body_provider_body").then_some(*source))
        .expect("provider-body phase source");
    let resolver = source_between_exact_boundaries(
        semantic_phase,
        "fn resolve_parsed_semantic_signature(",
        "\n#[derive(Clone)]\npub(in crate::revisioned_query_database) struct BodyInputResolver",
    );
    assert!(resolver.contains("resolve_structured_semantic_type_syntax"));
    for forbidden in [
        "resolve_semantic_type_syntax(",
        "parse_type_call_syntax",
        "rue_lexer",
        "rue_parser",
        ".split(",
        ".split_once(",
        ".trim(",
        ".strip_prefix(",
        ".starts_with(",
    ] {
        assert!(
            !resolver.contains(forbidden),
            "semantic signature resolution regained a handwritten text grammar: {forbidden}"
        );
    }

    let projection_value = source_between_exact_boundaries(
        projection_source,
        "pub(crate) enum ParsedSemanticSignature",
        "\nimpl ParsedSemanticSignature",
    );
    for required in ["RirTypeSyntaxArena<Arc<str>>", "RirTypeSyntaxRef"] {
        assert!(
            projection_value.contains(required),
            "signature value lost dense type-syntax ownership: {required}"
        );
    }
    for forbidden in [
        "Span",
        "FileId",
        "ThreadedRodeo",
        "rue_parser",
        "source_text",
        "fragment",
    ] {
        assert!(
            !projection_value.contains(forbidden),
            "signature value retained parser/source provenance: {forbidden}"
        );
    }
}

#[test]
fn compiler_uses_air_synthetic_type_identity_policy() {
    for (name, source) in [
        (
            "local_semantic_materialization.rs",
            include_str!("local_semantic_materialization.rs"),
        ),
        ("revisioned_query_database.rs", REVISIONED_DATABASE_SOURCE),
    ] {
        for peer in [".strip_prefix(\"Str(\")", ".starts_with(\"Str(\")"] {
            assert!(
                !source.contains(peer),
                "{name} regained handwritten synthetic-type identity policy: {peer}"
            );
        }
    }
}

#[test]
fn body_transaction_has_no_complete_declaration_candidate_map() {
    let runtime = REVISIONED_DATABASE_SOURCE;
    let method = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    assert!(
        !method.contains("declaration_candidates"),
        "body_transaction must not regain the coordinator's complete candidate map"
    );
    let compact = method
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    assert!(
        !compact.contains(
            "Arc<BTreeMap<crate::StableDefinitionKey,crate::declaration_candidate::DeclarationCandidateKey>>"
        ),
        "body_transaction must derive exact shell candidates from stable keys"
    );
    for required in [
        "&self.stable_declaration_classifications",
        "StableDeclarationClassificationQueryValue::Selected",
        "&self.declaration_shells",
        "DeclarationShellQueryValue::Available",
    ] {
        assert!(
            method.contains(required),
            "body_transaction lost candidate-set shell classification: {required}"
        );
    }
    for forbidden in [
        "declaration_occurrence_indexes",
        "DeclarationOccurrenceIndex",
        "stable_syntax_candidate_set",
    ] {
        assert!(
            !method.contains(forbidden),
            "body_transaction bypassed the narrow stable classifier: {forbidden}"
        );
    }
}

#[test]
fn anonymous_body_transaction_has_one_candidate_artifact_path_and_no_frontend_reentry() {
    let runtime = REVISIONED_DATABASE_SOURCE;
    let transaction = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    for required in [
        "resolve_producer_artifact",
        "materialize_body_rir_bundle_with_declaration",
        "analyze_provider_anonymous_body",
    ] {
        assert!(
            transaction.contains(required),
            "anonymous transaction lost its canonical candidate path: {required}",
        );
    }
    for forbidden in [
        "lower_anonymous_member_body_input",
        "AnonymousMemberLowering",
        "parse_source_snapshot_module",
        "SourceSnapshot",
        "AstGen",
        "lower_parsed_declaration_body_plan_with_anonymous_anchors",
    ] {
        assert!(
            !transaction.contains(forbidden),
            "anonymous transaction regained a frontend/rematerialization path: {forbidden}",
        );
    }

    let production = runtime.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    for removed in [
        "lower_anonymous_member_body_input",
        "AnonymousMemberLowering",
        "DurableAnonymousMemberBodySyntax",
        "RawDeclarationSignatureSyntax",
        "RawAccessorSignatureSyntax",
    ] {
        assert!(
            !production.contains(removed),
            "deleted anonymous/signature carrier returned to production: {removed}",
        );
    }
}

#[test]
fn well_known_option_resolution_stays_per_body_exact_and_fail_closed() {
    let runtime = REVISIONED_DATABASE_SOURCE;
    let method = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    for required in [
        "&self.body_toolchain_demands",
        "exact_option_prerequisites(",
        "exact_option_query(",
        "WellKnownOptionResolutionFailure::Incomplete",
        "WellKnownOptionResolutionFailure::Semantic",
        "WellKnownOptionResolutionFailure::WrongProjection",
    ] {
        assert!(
            method.contains(required),
            "body_transaction lost exact atomic Option resolution: {required}"
        );
    }
    for forbidden in [
        "well_known_demands",
        "plan_well_known_option_demands",
        "WellKnownOptionDemand",
    ] {
        assert!(
            !method.contains(forbidden),
            "body_transaction regained request-global Option planning: {forbidden}"
        );
    }

    let exact_keys = include_str!("well_known_option.rs");
    for forbidden in [
        "CanonicalRirOutput",
        "CanonicalMergedProgram",
        "plan_well_known_option_demands",
        "WellKnownOptionDemand",
    ] {
        assert!(
            !exact_keys.contains(forbidden),
            "exact Option-key derivation regained whole-request input: {forbidden}"
        );
    }
}

const RUE_869_INTERNAL_ROOT_VOCABULARY: &[&str] = &[
    "BoundDefinitionId",
    "BoundDefinitionRecord",
    "BoundDefinitionSet",
    "DefinitionId",
    "DefinitionKind",
    "DefinitionNameKey",
    "DefinitionNamespace",
    "DefinitionOccurrenceId",
    "DefinitionRecord",
    "DefinitionShard",
    "DefinitionSnapshot",
    "DependencyAcceptedRead",
    "DependencyContext",
    "DependencyObservation",
    "DependencyObservationOutcome",
    "DependencyRequest",
    "DiscoverySourceAssembler",
    "IMPORT_DISCOVERY_POLICY_VERSION",
    "DiagnosticFormatter",
    "JsonDiagnostic",
    "JsonDiagnosticFormatter",
    "MultiFileFormatter",
    "MultiFileJsonFormatter",
    "CompileError",
    "CompileResult",
    "Diagnostic",
    "ErrorCode",
    "ErrorKind",
    "Suggestion",
    "WarningKind",
    "Span",
];

fn code_identifiers(source: &str) -> Vec<&str> {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .flat_map(|line| {
            line.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .filter(|token| !token.is_empty())
        })
        .collect()
}

fn public_compile_functions(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in source.lines() {
        let code = line.split_once("//").map_or(line, |(code, _)| code).trim();
        if code.starts_with("pub(") {
            continue;
        }
        let identifiers = code
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if identifiers.first() != Some(&"pub") {
            continue;
        }
        let Some(fn_index) = identifiers.iter().position(|token| *token == "fn") else {
            continue;
        };
        if let Some(name) = identifiers
            .get(fn_index + 1)
            .filter(|name| name.starts_with("compile_"))
        {
            names.push((*name).to_owned());
        }
    }
    names
}

const RUE_866_INTERNAL_VOCABULARY: &[&str] = &[
    "CompilerSessionWork",
    "FrontendQueryWork",
    "FrontendRetentionMetrics",
    "FRONTEND_DIAGNOSTIC_RETENTION_LIMIT",
    "FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT",
    "DefinitionQueryRecord",
    "SemanticQueryRecord",
    "ImportDiagnosticInputDescriptor",
    "ImportGraphInputDescriptor",
    "ImportDiscoveryRevisionArtifact",
    "ImportDiscoveryRevisionStatus",
    "FrontendDiagnosticIdentity",
    "ResolvedCodegenRevision",
    "ResolvedLinkRevision",
    "ResolvedProgramRevision",
    "CodegenInputDescriptor",
    "LinkInputDescriptor",
    "SemanticInputDescriptor",
    "ModuleResolutionInput",
    "ModuleResolutionInputs",
    "SourceStore",
    "StableLinkerInput",
    "StableOptLevel",
    "StablePreviewFeatures",
    "ParseInvalidationSummary",
    "ParsedModulesWork",
    "CanonicalMergeWork",
    "CanonicalRirWork",
    "CanonicalSemanticFailurePhase",
    "CanonicalSemanticFailureWork",
    "CanonicalSemanticWork",
    "PipelineWork",
    "SourceStats",
];

const RUE_867_DURABLE_VOCABULARY: &[&str] = &[
    "DurableBodyWork",
    "DurableConstValue",
    "DurableDeclarationPayload",
    "DurableDeclarationSemantic",
    "DurableSemanticProjectionFailure",
    "DurableSemanticProjectionWork",
    "DurableType",
];

fn public_declarations(source: &str) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut lines = source.lines();
    let mut api_attributes = Vec::new();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[") {
            if trimmed.starts_with("#[cfg(") || trimmed.starts_with("#[cfg_attr(") {
                api_attributes.push(trimmed.to_owned());
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }
        if !trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate)") {
            api_attributes.clear();
            continue;
        }

        let identifiers = code_identifiers(trimmed);
        let owns_body = identifiers
            .iter()
            .any(|identifier| matches!(*identifier, "struct" | "enum" | "union" | "trait"));
        let field_owned_body = identifiers
            .iter()
            .any(|identifier| matches!(*identifier, "struct" | "union"));
        let field = !identifiers.iter().any(|identifier| {
            matches!(
                *identifier,
                "fn" | "struct"
                    | "enum"
                    | "union"
                    | "trait"
                    | "type"
                    | "use"
                    | "const"
                    | "static"
                    | "mod"
            )
        });
        let mut declaration = api_attributes.join("\n");
        api_attributes.clear();
        if !declaration.is_empty() {
            declaration.push('\n');
        }
        let mut brace_depth = 0isize;
        let mut opened_body = false;
        let mut current = Some(line);
        loop {
            let declaration_line = current.take().expect("public declaration has a line");
            declaration.push_str(declaration_line);
            declaration.push('\n');
            for byte in declaration_line.bytes() {
                match byte {
                    b'{' => {
                        opened_body = true;
                        brace_depth += 1;
                    }
                    b'}' if opened_body => brace_depth -= 1,
                    _ => {}
                }
            }

            let complete = if owns_body {
                (opened_body && brace_depth == 0)
                    || (!opened_body && declaration_line.contains(';'))
            } else if field {
                declaration_line.contains(',')
            } else {
                declaration_line.contains(';') || declaration_line.contains('{')
            };
            if complete {
                break;
            }
            current = lines.next();
            if current.is_none() {
                break;
            }
        }
        if field_owned_body && opened_body {
            let open = declaration.find('{').expect("opened aggregate has a body");
            let close = declaration
                .rfind('}')
                .expect("balanced aggregate has a body");
            let mut public_shape = declaration[..=open].to_owned();
            public_shape.push_str(&public_declarations(&declaration[open + 1..close]).concat());
            public_shape.push('}');
            declarations.push(public_shape);
        } else {
            declarations.push(declaration);
        }
    }
    declarations
}

fn public_signatures(source: &str) -> String {
    public_declarations(source).concat()
}

fn public_declaration_name(declaration: &str) -> Option<&str> {
    let identifiers = code_identifiers(declaration);
    let keyword = identifiers.iter().position(|identifier| {
        matches!(
            *identifier,
            "fn" | "struct" | "enum" | "union" | "trait" | "type" | "const" | "static" | "mod"
        )
    })?;
    identifiers.get(keyword + 1).copied()
}

fn impl_blocks(source: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("impl ") || trimmed.starts_with("impl<") {
            let leading = line.len() - trimmed.len();
            let start = offset + leading;
            let rest = &source[start..];
            let Some(open) = rest.find('{') else {
                offset += line.len();
                continue;
            };
            let mut depth = 0usize;
            for (index, byte) in rest[open..].bytes().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            blocks.push(&rest[..open + index + 1]);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        offset += line.len();
    }
    blocks
}

fn macro_blocks(source: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("macro_rules!") || trimmed.starts_with("pub macro ") {
            let leading = line.len() - trimmed.len();
            let start = offset + leading;
            let rest = &source[start..];
            let Some(open) = rest.find('{') else {
                offset += line.len();
                continue;
            };
            let mut depth = 0usize;
            for (index, byte) in rest[open..].bytes().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            blocks.push(&rest[..open + index + 1]);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        offset += line.len();
    }
    blocks
}

fn supported_public_surface(facade: &str, modules: &[(&str, &str)]) -> String {
    let root_declarations = public_declarations(facade);
    let public_module_names = root_declarations
        .iter()
        .filter_map(|declaration| {
            declaration
                .trim_start()
                .strip_prefix("pub mod ")
                .and_then(|module| module.split(';').next())
        })
        .collect::<std::collections::BTreeSet<_>>();
    let public_uses = public_use_declarations(facade);
    let exported_identifiers = public_uses
        .iter()
        .flat_map(|declaration| code_identifiers(declaration))
        .filter(|identifier| {
            !matches!(
                *identifier,
                "pub" | "use" | "self" | "crate" | "super" | "as"
            )
        })
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let mut surface = root_declarations.concat();

    for (module, source) in modules {
        if public_module_names.contains(module) {
            surface.push_str(&public_signatures(source));
        }
        for declaration in public_declarations(source) {
            if public_declaration_name(&declaration)
                .is_some_and(|name| exported_identifiers.contains(name))
            {
                surface.push_str(&declaration);
            }
        }
        for implementation in impl_blocks(source) {
            if code_identifiers(implementation)
                .into_iter()
                .take_while(|identifier| *identifier != "pub")
                .any(|identifier| exported_identifiers.contains(identifier))
            {
                surface.push_str(&public_signatures(implementation));
            }
        }
    }
    surface
}

fn public_use_declarations(source: &str) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut declaration = String::new();
    for line in source.lines() {
        let code = line.split_once("//").map_or(line, |(code, _)| code);
        if declaration.is_empty() && !code.trim_start().starts_with("pub use ") {
            continue;
        }
        declaration.push_str(code);
        declaration.push('\n');
        if code.contains(';') {
            declarations.push(std::mem::take(&mut declaration));
        }
    }
    declarations
}

fn macro_invocation_path(line: &str) -> Option<&str> {
    let (path, _) = line.split_once('!')?;
    let path = path.trim();
    if path == "macro_rules" || path.is_empty() {
        return None;
    }
    path.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')
        .then_some(path)
}

fn unsupported_api_layout(source: &str, root: bool) -> Option<String> {
    let mut conditional_export = false;
    let mut test_only_condition = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(") || trimmed.starts_with("#[cfg_attr(") {
            if !trimmed.ends_with(']') {
                return Some(trimmed.to_owned());
            }
            test_only_condition = !conditional_export && trimmed == "#[cfg(test)]";
            conditional_export = true;
            continue;
        }
        if trimmed.starts_with("#[macro_export") {
            return Some(trimmed.to_owned());
        }
        if trimmed.starts_with("#[") {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }
        if trimmed == "pub"
            || trimmed.starts_with("pub\t")
            || trimmed.starts_with("pub/*")
            || trimmed == "impl"
            || trimmed.starts_with("impl\t")
            || trimmed.starts_with("impl/*")
        {
            return Some(trimmed.to_owned());
        }
        if trimmed.starts_with("include!(\"registrations/") {
            // Registration fragments are expression snippets composed inside
            // the canonical constructor. They are inventoried separately by
            // the family-owner guard below.
            continue;
        }
        if trimmed.starts_with("include!(") || trimmed.starts_with("pub macro ") {
            return Some(trimmed.to_owned());
        }
        if line.len() == trimmed.len()
            && let Some(path) = macro_invocation_path(trimmed)
        {
            let name = path.rsplit("::").next().unwrap_or(path);
            if root
                || !matches!(
                    name,
                    "thread_local" | "session_query_metrics_family" | "query_value_charge"
                )
            {
                return Some(trimmed.to_owned());
            }
        }
        if root && conditional_export && trimmed.starts_with("pub use ") {
            return Some(trimmed.to_owned());
        }
        let identifiers = code_identifiers(trimmed);
        if conditional_export
            && !test_only_condition
            && (trimmed.starts_with("impl ") || trimmed.starts_with("impl<"))
            && (identifiers.contains(&"CompilerSession")
                || identifiers.contains(&"CompilerSessionUpdate"))
        {
            return Some(trimmed.to_owned());
        }
        conditional_export = false;
        test_only_condition = false;
    }
    for block in macro_blocks(source) {
        let registration_macro = block
            .trim_start()
            .strip_prefix("macro_rules!")
            .and_then(|header| header.trim_start().split_whitespace().next())
            .is_some_and(|name| name.starts_with("register_"));
        if !root && registration_macro {
            continue;
        }
        if root || code_identifiers(block).contains(&"pub") {
            return Some(block.lines().next().unwrap_or("macro_rules!").to_owned());
        }
    }
    for implementation in impl_blocks(source) {
        if impl_owner(implementation).is_none() {
            continue;
        }
        for line in implementation.lines().skip(1) {
            let Some(direct) = line.strip_prefix("    ") else {
                continue;
            };
            if direct.chars().next().is_some_and(char::is_whitespace) {
                continue;
            }
            if macro_invocation_path(direct).is_some() {
                return Some(direct.to_owned());
            }
        }
    }
    None
}

fn assert_no_public_use_globs(source: &str) {
    for declaration in public_use_declarations(source) {
        assert!(
            !public_use_is_glob(&declaration),
            "public glob reexports bypass the supported facade inventory: {declaration}"
        );
    }
}

fn public_use_is_glob(declaration: &str) -> bool {
    declaration
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '*'))
        .filter(|token| !token.is_empty())
        .any(|token| token == "*")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ApiInventoryEntry {
    stability: String,
    owner: String,
    class: String,
    consumer: String,
    symbol: String,
    signature: String,
}

impl ApiInventoryEntry {
    fn new(
        stability: &str,
        owner: impl Into<String>,
        class: &str,
        consumer: &str,
        symbol: impl Into<String>,
        signature: impl Into<String>,
    ) -> Self {
        Self {
            stability: stability.to_owned(),
            owner: owner.into(),
            class: class.to_owned(),
            consumer: consumer.to_owned(),
            symbol: symbol.into(),
            signature: signature.into(),
        }
    }

    fn render(&self) -> String {
        for field in [
            &self.stability,
            &self.owner,
            &self.class,
            &self.consumer,
            &self.symbol,
            &self.signature,
        ] {
            assert!(
                !field.contains('|') && !field.contains('\n'),
                "semantic API inventory fields must remain one-line and pipe-free: {field:?}"
            );
        }
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.stability, self.owner, self.class, self.consumer, self.symbol, self.signature
        )
    }
}

fn canonical_signature(declaration: &str) -> String {
    let declaration = declaration
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<String>();
    let end = declaration
        .find('{')
        .or_else(|| declaration.find(';'))
        .unwrap_or(declaration.len());
    let source = &declaration[..end];
    let bytes = source.as_bytes();
    let mut tokens = Vec::<(String, bool)>::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push((source[start..index].to_owned(), true));
            continue;
        }
        if byte == b'\'' && index + 1 < bytes.len() && bytes[index + 1].is_ascii_alphabetic() {
            let start = index;
            index += 2;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push((source[start..index].to_owned(), true));
            continue;
        }
        let two = (index + 1 < bytes.len()).then(|| &source[index..index + 2]);
        if matches!(
            two,
            Some("->" | "::" | "=>" | ".." | "<=" | ">=" | "==" | "!=")
        ) {
            tokens.push((two.unwrap().to_owned(), false));
            index += 2;
        } else {
            tokens.push((source[index..index + 1].to_owned(), false));
            index += 1;
        }
    }

    let mut rendered = String::new();
    let mut previous_word = false;
    for (token, word) in tokens {
        if previous_word && word {
            rendered.push(' ');
        }
        rendered.push_str(&token);
        previous_word = word;
    }
    rendered
}

fn root_export_metadata(owner: &str, symbol: &str) -> (&'static str, &'static str) {
    match owner {
        "artifact_views" => ("artifact-view", "embedders+tooling"),
        "dependency_envelope" => match symbol {
            "DependencyEnvelope"
            | "DependencyEnvelopeStatus"
            | "DependencyResolutionOutcome"
            | "DependencyTopology"
            | "DependencyTopologyRecord" => ("dependency-artifact", "source-loaders+embedders"),
            _ => panic!("unclassified dependency facade export: {symbol}"),
        },
        "import_discovery" => match symbol {
            "AcceptedReadManifest"
            | "AcceptedReadManifestEntry"
            | "FileMetadataFingerprint"
            | "ImportCandidateRole"
            | "ImportDiscoveryContext"
            | "ImportOccurrenceKey"
            | "PhysicalFileIdentity" => ("dependency-artifact", "source-loaders+embedders"),
            // Requested-path → trusted-namespace classification (RUE-1736): the
            // one derivation the driver may use for error attribution, so it
            // cannot re-derive the mapping with its own namespace spelling.
            "trusted_logical_path_for_requested" => {
                ("one-shot-operation", "source-loaders+embedders")
            }
            // The host discovery-protocol records (AcceptedImportSource,
            // ImportDiscoveryPlan/Request, ImportObservation*) live under
            // `unstable` with the protocol functions that consume them. Any
            // return to the stable root is unclassified and fails here.
            _ => panic!("unclassified import-discovery facade export: {symbol}"),
        },
        "import_graph" => match symbol {
            "CanonicalImportCycle"
            | "CanonicalImportGraph"
            | "CanonicalImportGraphProblem"
            | "CanonicalImportGraphValidation"
            | "CanonicalImportRecord"
            | "CanonicalImportResolution"
            | "ImportDirective"
            | "ImportDirectives" => ("dependency-artifact", "source-loaders+embedders"),
            _ => panic!("unclassified import-graph facade export: {symbol}"),
        },
        "diagnostic_attempt_store" => ("diagnostic", "cli+embedders"),
        "rue_error" => match symbol {
            "CompileErrors" | "CompileWarning" | "MultiErrorResult" | "PreviewFeature"
            | "PreviewFeatures" | "VERSION" => ("diagnostic", "cli+embedders"),
            _ => panic!("raw diagnostic type returned to the stable facade: {symbol}"),
        },
        "queries" => match symbol {
            "CompileOptions" | "LinkerMode" | "RootSelection" => {
                ("compilation-config", "cli+embedders")
            }
            "compile_snapshot" => ("one-shot-operation", "cli+embedders"),
            "CompileOutput" | "SourceView" => ("compile-artifact", "cli+embedders"),
            _ => panic!("unclassified queries facade export: {symbol}"),
        },
        "session" => match symbol {
            "CompilerSession" | "CompilerSessionUpdate" => ("session-owner", "embedders"),
            "CanonicalImportGraphOutput" => ("dependency-artifact", "source-loaders+embedders"),
            _ => panic!("unclassified session facade export: {symbol}"),
        },
        "source_identity" | "source_metadata" | "source_snapshot" | "rue_span" => {
            ("source-input", "cli+embedders")
        }
        "toolchain_module_demand" => match symbol {
            "OPTION_MODULE_LOGICAL_PATH"
            | "ParkedToolchainModules"
            | "STRBUF_MODULE_LOGICAL_PATH"
            | "TrustedToolchainModuleDemand" => {
                ("toolchain-module-demand", "source-loaders+embedders")
            }
            _ => panic!("unclassified toolchain-module-demand facade export: {symbol}"),
        },
        "rue_cfg" | "rue_target" => ("compilation-config", "cli+embedders"),
        _ => panic!("unclassified facade export owner: {owner}::{symbol}"),
    }
}

fn root_use_exports(facade: &str) -> Vec<ApiInventoryEntry> {
    let mut entries = Vec::new();
    for declaration in public_use_declarations(facade) {
        assert!(
            !public_use_is_glob(&declaration),
            "public glob reexports bypass the semantic API inventory: {declaration}"
        );
        assert!(
            !code_identifiers(&declaration).contains(&"as"),
            "public aliases require explicit semantic inventory support: {declaration}"
        );
        let compact = declaration
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let path = compact
            .strip_prefix("pubuse")
            .and_then(|path| path.strip_suffix(';'))
            .expect("public-use scanner returns complete declarations");
        let (owner, symbols) = if let Some((owner, symbols)) = path.split_once("::{") {
            let symbols = symbols
                .strip_suffix('}')
                .expect("grouped public use closes its brace")
                .split(',')
                .filter(|symbol| !symbol.is_empty())
                .collect::<Vec<_>>();
            (owner, symbols)
        } else {
            let (owner, symbol) = path
                .rsplit_once("::")
                .expect("public reexport has an owning path");
            (owner, vec![symbol])
        };
        for symbol in symbols {
            let (class, consumer) = root_export_metadata(owner, symbol);
            entries.push(ApiInventoryEntry::new(
                "stable",
                owner,
                class,
                consumer,
                symbol,
                format!("pub use {owner}::{symbol}"),
            ));
        }
    }
    entries
}

fn impl_owner(implementation: &str) -> Option<&'static str> {
    let header = implementation.split_once('{')?.0;
    let identifiers = code_identifiers(header);
    if identifiers.contains(&"CompilerSessionUpdate") {
        Some("CompilerSessionUpdate")
    } else if identifiers.contains(&"CompilerSession") {
        Some("CompilerSession")
    } else {
        None
    }
}

fn session_method_metadata(
    owner: &str,
    module: &str,
    symbol: &str,
    _signature: &str,
) -> (&'static str, &'static str, &'static str) {
    let unstable = module == "unstable" || symbol.starts_with("unstable_");
    if unstable {
        return ("unstable", "debug-tooling", "in-tree-tooling");
    }
    let class = if owner == "CompilerSessionUpdate" {
        match symbol {
            "result" | "into_result" | "diagnostics" => "artifact-result",
            _ => panic!("unclassified stable CompilerSessionUpdate method: {symbol}"),
        }
    } else {
        match symbol {
            "new" | "update" => "session-operation",
            "published" | "committed_import_graph" | "import_diagnostics" | "rir" | "semantic" => {
                "artifact-query"
            }
            _ => panic!("unclassified stable CompilerSession method: {symbol}"),
        }
    };
    ("stable", class, "embedders")
}

fn semantic_api_inventory(facade: &str, modules: &[(&str, &str)]) -> Vec<ApiInventoryEntry> {
    assert!(
        unsupported_api_layout(facade, true).is_none(),
        "root API uses a visibility, impl, include, or macro form the exact inventory does not parse"
    );
    for (module, source) in modules {
        assert!(
            unsupported_api_layout(source, false).is_none(),
            "{module} uses a split visibility or impl form the exact inventory does not parse"
        );
    }
    let mut entries = root_use_exports(facade);
    for declaration in public_declarations(facade) {
        let signature = canonical_signature(&declaration);
        if signature.starts_with("pub use ") || signature.starts_with("pub use") {
            continue;
        }
        let symbol = public_declaration_name(&declaration)
            .expect("every direct root declaration has a name");
        if signature == "pub mod unstable" {
            entries.push(ApiInventoryEntry::new(
                "unstable",
                "unstable",
                "debug-module",
                "in-tree-tooling",
                symbol,
                signature,
            ));
        } else if symbol == "configure_thread_pool" {
            entries.push(ApiInventoryEntry::new(
                "stable",
                "lib",
                "runtime-config",
                "cli+embedders",
                symbol,
                signature,
            ));
        } else {
            panic!("unclassified direct root public declaration: {signature}");
        }
    }

    for (module, source) in modules {
        for implementation in impl_blocks(source) {
            let Some(owner) = impl_owner(implementation) else {
                continue;
            };
            for declaration in public_declarations(implementation) {
                let symbol = public_declaration_name(&declaration)
                    .expect("every public session declaration has a name");
                let signature = canonical_signature(&declaration);
                let (stability, class, consumer) =
                    session_method_metadata(owner, module, symbol, &signature);
                entries.push(ApiInventoryEntry::new(
                    stability, owner, class, consumer, symbol, signature,
                ));
            }
        }
    }

    entries.sort();
    let mut rendered = std::collections::BTreeSet::new();
    for entry in &entries {
        assert!(
            rendered.insert(entry.render()),
            "duplicate semantic API inventory entry: {}",
            entry.render()
        );
    }
    entries
}

fn render_semantic_api_inventory(facade: &str, modules: &[(&str, &str)]) -> String {
    semantic_api_inventory(facade, modules)
        .into_iter()
        .map(|entry| entry.render())
        .collect::<Vec<_>>()
        .join("\n")
}

fn inherent_impl<'a>(source: &'a str, owner: &str) -> &'a str {
    let marker = format!("impl {owner} {{");
    let start = source.find(&marker).expect("reviewed owner impl exists");
    let rest = &source[start..];
    let mut depth = 0usize;
    for (offset, byte) in rest.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..=offset];
                }
            }
            _ => {}
        }
    }
    panic!("reviewed owner impl is balanced")
}

fn inherent_impls(source: &str, owner: &str) -> String {
    let marker = format!("impl {owner} {{");
    let mut remaining = source;
    let mut implementations = String::new();
    while let Some(start) = remaining.find(&marker) {
        let candidate = &remaining[start..];
        let implementation = inherent_impl(candidate, owner);
        implementations.push_str(implementation);
        implementations.push('\n');
        remaining = &candidate[implementation.len()..];
    }
    assert!(
        !implementations.is_empty(),
        "reviewed owner impl exists: {owner}"
    );
    implementations
}

#[test]
fn compiler_session_public_inventory_scans_every_impl_block() {
    let fixture = r#"
impl CompilerSession {
    pub fn new() -> Self { todo!() }
}
impl CompilerSession {
    pub fn semantic(&self) -> SemanticView { todo!() }
}
    "#;
    assert!(
        !public_signatures(inherent_impl(fixture, "CompilerSession")).contains("semantic"),
        "the regression fixture must hide its forbidden method from the old first-impl scan"
    );
    let implementations = inherent_impls(fixture, "CompilerSession");
    assert!(
        public_signatures(&implementations).contains("semantic"),
        "the complete inventory must see a forbidden public signature in a later impl"
    );
}

#[test]
fn facade_stays_small_and_session_centered() {
    let facade = include_str!("lib.rs");
    let mut declared_modules = facade
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("mod ")
                .or_else(|| line.trim().strip_prefix("pub mod "))
        })
        .filter_map(|module| module.strip_suffix(';'))
        .collect::<Vec<_>>();
    declared_modules.sort_unstable();
    let mut inventoried_modules = PRODUCTION_MODULES
        .iter()
        .map(|(name, _)| *name)
        .chain([
            "api_inventory",
            "integration_tests",
            "pipeline_tests",
            "producer_nominal_acceptance_tests",
            "retained_charge",
            "scaling_harness",
            "supported_api_inventory",
            "test_support",
        ])
        .collect::<Vec<_>>();
    inventoried_modules.sort_unstable();
    assert_eq!(
        declared_modules, inventoried_modules,
        "every new module must be classified and added to the API inventory"
    );

    let facade_compile_names = code_identifiers(facade)
        .into_iter()
        .filter(|name| name.starts_with("compile_"))
        .collect::<Vec<_>>();
    assert_eq!(
        facade_compile_names,
        ["compile_snapshot"],
        "the facade may reexport exactly one one-shot compilation adapter"
    );

    let public_compilers = PRODUCTION_MODULES
        .iter()
        .flat_map(|(module, source)| {
            public_compile_functions(source)
                .into_iter()
                .map(move |function| (*module, function))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        public_compilers,
        [("queries", "compile_snapshot".to_owned())],
        "production modules may define exactly one public compile function"
    );
}

#[test]
fn per_body_query_boundary_is_stable_independent_and_cache_free() {
    fn item<'a>(source: &'a str, marker: &str) -> &'a str {
        let start = source
            .find(marker)
            .expect("reviewed body query item exists");
        let rest = &source[start..];
        let open = rest.find('{').expect("reviewed item has a body");
        let mut depth = 0usize;
        for (offset, byte) in rest[open..].bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &rest[..open + offset + 1];
                    }
                }
                _ => {}
            }
        }
        panic!("reviewed body query item is balanced")
    }
    let body = include_str!("body_query.rs");
    let key = item(body, "pub(crate) struct BodyQueryKey");
    assert!(key.contains("instance: crate::FunctionInstanceKey"));
    assert!(
        key.contains("configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration")
    );
    for forbidden in [
        "Revision",
        "fingerprint",
        "Span",
        "BodyOwnerToken",
        "TypeId",
    ] {
        assert!(
            !key.contains(forbidden),
            "BodyQueryKey retained unstable identity component {forbidden}"
        );
    }

    let transaction = item(body, "pub(crate) enum BodyTransaction {");
    assert!(transaction.contains("Success"));
    assert!(transaction.contains("DeterministicFailure"));
    assert!(!transaction.contains("Canceled"));
    assert!(!transaction.contains("NonTerminal"));

    let runtime = REVISIONED_DATABASE_SOURCE;
    assert!(runtime.contains("compiler.body-transaction"));
    for redundant_projection in [
        "\"compiler.body-references\"",
        "\"compiler.canonical-body\"",
    ] {
        assert!(
            !runtime.contains(redundant_projection),
            "rooted consumers must retain the already-observed BodyTransaction directly: {redundant_projection}"
        );
    }
    let session = SESSION_PRODUCTION_SOURCE;
    assert!(!session.contains("pub fn semantic("));
    assert!(!session.contains("semantic_view_from_rooted"));
    let body_transaction = source_between_exact_boundaries(
        runtime,
        "struct BodyTransactionEvaluator {",
        "\nimpl RevisionedQueryDatabase {",
    );
    assert!(body_transaction.contains("analyze_provider_ordinary_body"));
    assert!(!body_transaction.contains("analyze_body_query"));
    assert!(!body_transaction.contains("SharedDeclarationBase"));
    assert!(!body_transaction.contains("BoundBodyEpoch"));
}

#[test]
fn semantic_api_inventory_matches_every_root_export_and_session_signature() {
    let actual = render_semantic_api_inventory(include_str!("lib.rs"), PRODUCTION_MODULES);
    let approved = crate::supported_api_inventory::APPROVED.trim();
    assert_eq!(
        actual, approved,
        "the public compiler facade changed without a reviewed semantic inventory diff; \
         classify the owner, stability, category, and approved consumer in \
         supported_api_inventory.rs\n\nactual inventory:\n{actual}"
    );
}

#[test]
fn stable_facade_carries_no_compatibility_boundary_classifications() {
    // RUE-1479 removed the import-discovery compatibility facade. The
    // classifiers no longer produce these classes, so any reintroduction —
    // through a classifier arm or an approved-inventory line — fails here.
    let actual = render_semantic_api_inventory(include_str!("lib.rs"), PRODUCTION_MODULES);
    for inventory in [actual.as_str(), crate::supported_api_inventory::APPROVED] {
        for forbidden in ["compatibility-boundary", "legacy-embedders"] {
            assert!(
                !inventory.contains(forbidden),
                "a stable compatibility classification returned to the facade inventory: {forbidden}"
            );
        }
    }
}

#[test]
fn semantic_inventory_detects_root_and_signature_changes() {
    let facade = "pub use queries::CompileOptions;";
    let expanded = "pub use queries::{CompileOptions, LinkerMode};";
    assert_eq!(root_use_exports(facade).len(), 1);
    assert_eq!(root_use_exports(expanded).len(), 2);

    let original = render_semantic_api_inventory(
        "pub use session::CompilerSession;",
        &[(
            "session",
            "impl CompilerSession {\n    pub fn new() -> Self { todo!() }\n}",
        )],
    );
    let changed = render_semantic_api_inventory(
        "pub use session::CompilerSession;",
        &[(
            "session",
            "impl CompilerSession {\n    pub fn new(capacity: usize) -> Self { todo!() }\n}",
        )],
    );
    assert_ne!(
        original, changed,
        "public signature changes must alter the inventory"
    );
    assert!(original.contains("pub fn new()->Self"));
    assert!(changed.contains("pub fn new(capacity:usize)->Self"));
    assert!(render_semantic_api_inventory(facade, &[]).contains("CompileOptions"));
}

#[test]
fn semantic_inventory_rejects_aliases_and_globs() {
    let alias = public_use_declarations("pub use queries::CompileOptions as Options;");
    assert!(code_identifiers(&alias[0]).contains(&"as"));
    let glob = public_use_declarations("pub use queries::*;");
    assert!(public_use_is_glob(&glob[0]));
}

#[test]
fn semantic_inventory_rejects_unparsed_layouts_and_records_cfg_attributes() {
    for source in [
        "pub\nfn hidden() {}",
        "pub\tfn hidden() {}",
        "impl\nCompilerSession {}",
        "impl\tCompilerSession {}",
    ] {
        assert!(
            unsupported_api_layout(source, false).is_some(),
            "split API syntax bypassed the fail-closed layout guard: {source:?}"
        );
    }
    for source in [
        "include!(\"api.rs\");",
        "macro_rules! api { () => {} }",
        "generated_api!();",
        "crate::generated_api!();",
        "foo::bar!();",
    ] {
        assert!(unsupported_api_layout(source, true).is_some());
    }
    for source in [
        "include!(\"session_api.rs\");",
        "macro_rules! api { () => { pub fn escaped() {} } }",
        "generated_session_impl!();",
        "#[macro_export]\nmacro_rules! api { () => {} }",
        "#[macro_export(local_inner_macros)]\nmacro_rules! api { () => {} }",
        "impl CompilerSession {\n    generated_methods!();\n}",
        "#[cfg(any(\nunix,\nwindows\n))]\npub fn escaped() {}",
        "#[cfg(unix)]\nimpl CompilerSession {\n    pub fn new() -> Self { todo!() }\n}",
        "#[cfg(unix)]\nimpl crate::CompilerSession {\n    pub fn new() -> Self { todo!() }\n}",
    ] {
        assert!(unsupported_api_layout(source, false).is_some());
    }
    assert!(
        unsupported_api_layout("#[cfg(unix)]\npub use session::CompilerSession;", true).is_some()
    );

    let inventory = render_semantic_api_inventory(
        "pub use session::CompilerSession;",
        &[(
            "session",
            "impl CompilerSession {\n#[cfg(unix)]\npub fn new() -> Self { todo!() }\n}",
        )],
    );
    assert!(inventory.contains("#[cfg(unix)]pub fn new()->Self"));
}

#[test]
fn raw_phase_owners_and_backend_drivers_cannot_return_to_the_stable_facade() {
    let facade = include_str!("lib.rs");
    let root_exports = public_use_declarations(facade).concat();
    let session_impls = inherent_impls(SESSION_PRODUCTION_SOURCE, "CompilerSession");
    let session = public_signatures(&session_impls);
    let update = public_signatures(inherent_impl(
        include_str!("session.rs"),
        "CompilerSessionUpdate",
    ));
    let views = public_declarations(include_str!("artifact_views.rs")).concat();
    let stable_surface = [root_exports, session.clone(), update, views].concat();
    let identifiers = code_identifiers(&stable_surface);

    for forbidden in RUE_868_RAW_FACADE_VOCABULARY {
        assert!(
            !identifiers.contains(forbidden),
            "raw phase owner or backend presentation symbol returned to the stable facade: {forbidden}"
        );
    }

    let session_methods = code_identifiers(&session);
    for internal in [
        "merge",
        "stable_definitions",
        "update_for_presentation",
        "inject_stale_query_for_oracle",
    ] {
        assert!(
            !session_methods.contains(&internal),
            "internal orchestration method returned to CompilerSession's supported surface: {internal}"
        );
    }

    let root_identifiers = code_identifiers(facade);
    for required in [
        "TokenView",
        "SyntaxNodeView",
        "SyntaxView",
        "RirInstructionView",
        "RirOperandView",
        "RirView",
        "SourceIdentityView",
        "SourceLocationView",
    ] {
        assert!(
            root_identifiers.contains(&required),
            "stable owner-bound artifact view is missing from the root: {required}"
        );
    }

    let artifact_views = include_str!("artifact_views.rs");
    for raw_presentation in [
        "impl std::fmt::Display for TokenView",
        "impl fmt::Display for TokenView",
        "pub fn text(",
        "pub fn air_text(",
    ] {
        assert!(
            !artifact_views.contains(raw_presentation),
            "raw phase presentation returned through stable views: {raw_presentation}"
        );
    }

    for view in [
        "SourceLocationView",
        "SyntaxNodeView",
        "RirInstructionView",
        "RirOperandView",
    ] {
        let signatures = public_signatures(inherent_impl(artifact_views, view));
        assert!(
            !code_identifiers(&signatures).contains(&"Span"),
            "stable owner-bound view exposes a raw Span: {view}"
        );
    }
}

#[test]
fn final_internal_records_and_presenters_cannot_return_to_the_stable_root() {
    let root_exports = public_use_declarations(include_str!("lib.rs")).concat();
    let identifiers = code_identifiers(&root_exports);
    for forbidden in RUE_869_INTERNAL_ROOT_VOCABULARY {
        assert!(
            !identifiers.contains(forbidden),
            "RUE-869 internal record or presenter returned to the stable root: {forbidden}"
        );
    }
}

#[test]
fn removed_parallel_entry_points_cannot_return() {
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(_, source)| *source)
        .collect::<String>();
    let forbidden = [
        ["Compilation", "Unit"].concat(),
        ["Legacy", "Rir"].concat(),
        ["compile_", "frontend", "_from_ast"].concat(),
        ["parse_all", "_files"].concat(),
        ["merge_", "symbols"].concat(),
        ["pub fn compile_", "frontend"].concat(),
        ["pub fn compile_multi", "_file"].concat(),
        ["pub fn compile_to", "_"].concat(),
        ["pub fn from_", "sources"].concat(),
        ["pub fn source_", "file"].concat(),
        ["Source", "File"].concat(),
        "CanonicalSemanticOutput".to_owned(),
        "FunctionWithCfg".to_owned(),
        "canonical_semantic_with_cancellation".to_owned(),
        "semantic_attempt".to_owned(),
        "analyze_prepared_canonical_program_reusing_declarations".to_owned(),
        "collect_function_cfg_queries".to_owned(),
        "finish_canonical_analysis".to_owned(),
        "compose_queried_bodies".to_owned(),
        ["Rooted", "Semantic", "Output"].concat(),
        ["rooted_", "semantic"].concat(),
        ["semantic_", "projection_", "for_test"].concat(),
        ["Semantic", "View"].concat(),
        ["Function", "View"].concat(),
        ["Cfg", "View"].concat(),
        ["CfgBlock", "View"].concat(),
        ["CfgInstruction", "View"].concat(),
        ["CfgSuccessor", "View"].concat(),
        ["Type", "View"].concat(),
    ];
    for removed in forbidden {
        assert!(
            !production.contains(&removed),
            "removed compiler entry point returned: {removed}"
        );
    }

    let session = SESSION_PRODUCTION_SOURCE;
    assert!(!session.contains("pub fn semantic("));
    assert!(!session.contains("semantic_view_from_rooted"));
}

#[test]
fn compiler_parallelism_has_one_query_budget_and_no_peer_parallel_frontier() {
    let root = include_str!("lib.rs");
    let cfg = include_str!("queries.rs");
    let backend = include_str!("backend.rs");
    let database = REVISIONED_DATABASE_SOURCE;
    assert!(
        root.contains("QUERY_CONCURRENCY")
            && root.contains("configure_thread_pool")
            && database.contains("crate::query_concurrency()")
            && database.contains("QueryRuntime::new(query_concurrency)"),
        "compiler runtime configuration must feed the canonical query database budget"
    );
    for (module, source) in [("queries", cfg), ("backend", backend)] {
        for forbidden in ["rayon", ".par_iter(", ".into_par_iter("] {
            assert!(
                !source.contains(forbidden),
                "{module} reintroduced a process-global parallel frontier through {forbidden}"
            );
        }
    }
}

#[test]
fn orphaned_backend_inspection_exports_cannot_return() {
    let facade = include_str!("lib.rs");
    let backend = include_str!("backend.rs");
    let unstable = include_str!("unstable.rs");
    let removed = [
        "generate_allocated_mir",
        "generate_emitted_asm",
        "generate_liveness_info",
        "generate_lowering_info",
        "generate_mir",
        "generate_regalloc_info",
    ];

    for name in removed {
        assert!(
            !code_identifiers(facade).contains(&name),
            "test-only backend helper returned to the production facade: {name}"
        );
        assert!(
            !code_identifiers(backend).contains(&name),
            "orphaned backend inspection path returned: {name}"
        );
        assert!(
            !code_identifiers(unstable).contains(&name),
            "unstable presentation regained a parallel backend path: {name}"
        );
    }
    for (module, source) in PRODUCTION_MODULES {
        for retired in [
            "FunctionBackendProduct",
            "backend_product",
            "generate_backend_products",
            "codegen_products",
            "SectionContents",
            "presentation_fingerprint",
        ] {
            assert!(
                !code_identifiers(source).contains(&retired),
                "retired CodegenUnit ownership identifier remains in {module}: {retired}"
            );
        }
    }
    assert!(
        code_identifiers(backend).contains(&"project_backend_object"),
        "object projection must accept the canonical CodegenUnit"
    );
}

#[test]
fn query_engine_records_cannot_return_to_the_supported_root() {
    let facade = include_str!("lib.rs");
    let mut supported_exports = String::new();
    let mut in_supported_export = false;
    for line in facade.lines() {
        if line.trim_start().starts_with("pub use ") {
            in_supported_export = true;
        }
        if in_supported_export {
            supported_exports.push_str(line);
            supported_exports.push('\n');
            if line.contains(';') {
                in_supported_export = false;
            }
        }
    }
    for forbidden in RUE_866_INTERNAL_VOCABULARY
        .iter()
        .chain(RUE_867_DURABLE_VOCABULARY)
    {
        assert!(
            !supported_exports.contains(forbidden),
            "query-engine implementation record returned to the supported root: {forbidden}"
        );
    }
}

#[test]
fn unstable_views_do_not_alias_query_engine_records() {
    let unstable = include_str!("unstable.rs");
    assert!(!unstable.contains("pub type "));
    let reexports = public_use_declarations(unstable)
        .into_iter()
        .map(|declaration| {
            declaration
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reexports,
        [
            "pubusecrate::diagnostic::{ColorChoice,DiagnosticFormatter,JsonDiagnostic,JsonDiagnosticFormatter,JsonSpan,JsonSuggestion,MultiFileFormatter,MultiFileJsonFormatter,SourceInfo,};",
            "pubusecrate::import_discovery::{AcceptedImportSource,DiscoverySourceAssembler,ImportDemandFrontier,ImportDemandMode,ImportDemandRoots,ImportDiscoveryPlan,ImportDiscoveryRequest,ImportDiscoveryWave,ImportInputRevision,ImportObservation,ImportObservationLedger,ImportObservationStatus,};",
            "pubusecrate::warm_fresh_parity::ParityObservation;",
            "pubuserue_span::Span;",
            "pubusecrate::session::{ClosedDiscoveryContinuation,RootedCfgOutput,RootedCfgUnit,RootedParkOutcome,RootedPreOptimizationCfgOutput,RootedPreOptimizationCfgUnit,TrustedSuccessorDelta,};",
        ],
        "unstable may reexport only reviewed presentation, source-assembly, and host discovery-protocol records"
    );

    let facade = include_str!("lib.rs");
    assert_no_public_use_globs(facade);
    let public_modules = facade
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub mod "))
        .collect::<Vec<_>>();
    assert_eq!(public_modules, ["unstable;"]);

    let session = SESSION_PRODUCTION_SOURCE;
    let diagnostic = include_str!("diagnostic_attempt_store.rs");
    let session_impls = inherent_impls(session, "CompilerSession");
    let reviewed_public = [
        public_signatures(&session_impls),
        public_signatures(inherent_impl(session, "CompilerSessionUpdate")),
        public_signatures(inherent_impl(diagnostic, "FrontendDiagnosticSnapshot")),
        public_signatures(inherent_impl(include_str!("queries.rs"), "CompileOutput")),
        public_signatures(inherent_impl(session, "CanonicalImportGraphOutput")),
    ]
    .concat();
    for forbidden in RUE_866_INTERNAL_VOCABULARY {
        assert!(
            !reviewed_public.contains(forbidden),
            "internal vocabulary leaked through a supported public signature or field: {forbidden}"
        );
    }
    let supported_public = supported_public_surface(facade, PRODUCTION_MODULES);
    for forbidden in RUE_867_DURABLE_VOCABULARY {
        assert!(
            !supported_public.contains(forbidden),
            "durable cache schema leaked through the complete supported public surface: {forbidden}"
        );
    }
    assert!(reviewed_public.contains("pub fn stage(&self) -> DiagnosticStage"));

    for forbidden in [
        "pub fn work(&self) -> CompilerSessionWork",
        "pub fn semantic_dependency_inputs",
        "pub fn discovery_attempt(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>>",
        "pub fn last_good_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>>",
        "pub fn committed_import_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>>",
    ] {
        assert!(
            !session.contains(forbidden),
            "session implementation record leaked through a public signature: {forbidden}"
        );
    }

    let queries = include_str!("queries.rs");
    assert!(!queries.contains("pub source_stats: SourceStats"));
    assert!(!queries.contains("pub work: PipelineWork"));
}

#[test]
fn compatibility_formatters_have_one_source_authority_across_compiler_modules() {
    // Scan complete source files, including tests and string fixtures.
    // `code_identifiers` removes line comments before identifying names.
    let count_identifier = |source: &str, identifier: &str| {
        code_identifiers(source)
            .into_iter()
            .filter(|candidate| *candidate == identifier)
            .count()
    };

    for (module, source) in PRODUCTION_MODULES.iter().copied().chain([
        ("lib", include_str!("lib.rs")),
        ("retained_charge", include_str!("retained_charge.rs")),
    ]) {
        for formatter in ["DiagnosticFormatter", "JsonDiagnosticFormatter"] {
            let count = count_identifier(source, formatter);
            let expected = match module {
                "diagnostic" => 9,
                "unstable" => 1,
                _ => 0,
            };
            assert_eq!(
                count, expected,
                "{formatter} has an unexpected production reference count in {module}"
            );
        }
    }
}

#[test]
fn qualified_public_globs_are_rejected() {
    for fixture in ["pub use session::*;", "pub use unstable::{Metrics, *};"] {
        assert!(
            public_use_declarations(fixture)
                .iter()
                .any(|declaration| public_use_is_glob(declaration)),
            "qualified glob fixture escaped the public-use guard: {fixture}"
        );
    }
}

#[test]
fn supported_surface_scanner_closes_multiline_and_alias_escapes() {
    let facade = r#"
pub mod unstable;
pub use session::{Exported, ExportedRecord};
pub type Alias = durable_body::DurableType;
"#;
    let session = r#"
pub struct ExportedRecord {
    pub safe: usize,
    pub leaked:
        DurableOrdinaryBody,
}
pub struct Exported;
impl Exported {
    pub fn leaked(
        &self,
    ) -> DurableSpecializedBodyPayload {
        unreachable!()
    }
}
"#;
    let unstable = r#"
pub fn leaked(
    value: usize,
) -> DurableType {
    unreachable!()
}
"#;
    let surface = supported_public_surface(facade, &[("session", session), ("unstable", unstable)]);
    for escaped in [
        "durable_body::DurableType",
        "DurableOrdinaryBody",
        "DurableSpecializedBodyPayload",
        "DurableType",
    ] {
        assert!(
            surface.contains(escaped),
            "balanced public-surface scanner missed adversarial escape: {escaped}"
        );
    }
}

#[test]
fn durable_cache_schema_cannot_return_to_the_public_facade() {
    let facade = include_str!("lib.rs");
    // `durable_body` held only inert accounting and was removed by RUE-1541.
    // Asserting the module is absent is stronger than asserting it stayed
    // private: the schema cannot leak from a module that does not exist.
    assert!(!facade.contains("mod durable_body;"));
    assert!(facade.contains("mod durable_semantics;"));
    assert!(!facade.contains("pub mod durable_semantics;"));

    let session = SESSION_PRODUCTION_SOURCE;
    for raw_accessor in [
        "durable_specialized_body_payloads",
        "durable_ordinary_bodies",
    ] {
        assert!(
            !session.contains(raw_accessor),
            "raw durable schema accessor returned to a public signature: {raw_accessor}"
        );
    }
}

#[test]
fn query_attempts_have_one_family_owned_representation() {
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(_, source)| *source)
        .collect::<String>();
    for peer in [
        ["QueryAttempt", "Ledger"].concat(),
        ["QueryAttempt", "Work"].concat(),
        ["QueryAttempt", "Identity"].concat(),
        ["attempt_", "origin"].concat(),
        ["origins: Vec", "Deque<Arc<FrontendDiagnosticSnapshot>>"].concat(),
        ["SyntaxParse", "Producer"].concat(),
        ["CanonicalParse", "Session"].concat(),
        ["QueryPublication", "Snapshot"].concat(),
    ] {
        assert!(
            !production.contains(&peer),
            "parallel query-attempt ownership returned: {peer}"
        );
    }
    assert!(
        production.contains("attempt: Arc<dyn AttemptView>"),
        "diagnostic and metrics indexes must retain the family-owned attempt Arc"
    );
}

#[test]
fn revisioned_parse_family_is_runtime_registered_without_a_selection_wrapper() {
    let session = SESSION_PRODUCTION_SOURCE;
    let runtime = REVISIONED_DATABASE_SOURCE;
    for removed in [
        "parse: TypedQueryStore<ParseQuery>",
        "parse_inputs",
        "ParsedProgramLookup",
        "queries.parse",
        "QueryNodeFamily::Parse",
    ] {
        assert!(
            !session.contains(removed),
            "legacy parse query authority returned: {removed}"
        );
    }
    assert!(session.contains(".source_revision(&source, snapshot)"));
    assert!(session.contains("revisioned.select_parse(&attempt)"));
    assert!(!runtime.contains("RevisionedFamily"));
    assert!(!runtime.contains("selected_state_shim"));
    assert!(runtime.contains("parse: QueryFamily<"));
    assert!(runtime.contains("parse_selection: QuerySelection<"));
    assert!(runtime.contains("content_addressed_family_with_equality("));
}

#[test]
fn declaration_shell_queries_are_the_only_compiler_semantic_discovery_authority() {
    let before_tests = |source: &'static str| {
        let marker = "\n#[cfg(test)]\nmod ";
        source
            .rmatch_indices(marker)
            .find_map(|(index, _)| {
                let declaration = source[index + marker.len()..].lines().next().unwrap();
                declaration.ends_with('{').then_some(&source[..index])
            })
            .unwrap_or(source)
    };
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(name, source)| (*name, before_tests(source)))
        .collect::<Vec<_>>();
    let module = |wanted: &str| {
        production
            .iter()
            .find_map(|(name, source)| (*name == wanted).then_some(*source))
            .unwrap()
    };
    let session = module("session");
    let canonical = before_tests(include_str!("canonical_semantic.rs"));
    let parsed = module("parsed_modules");
    let runtime = module("revisioned_query_database");

    let assert_test_gated_calls = |name: &str, source: &str, adapter: &str| {
        for (offset, _) in source.match_indices(adapter) {
            let lines = source[..offset].lines().collect::<Vec<_>>();
            let function_line = lines
                .iter()
                .rposition(|line| line.contains("fn "))
                .unwrap_or_else(|| panic!("{name} called {adapter} outside a function"));
            let mut preceding = lines[..function_line]
                .iter()
                .rev()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty());
            assert_eq!(
                preceding.next(),
                Some("#[cfg(test)]"),
                "compiler module {name} called frozen declaration test adapter {adapter} from a production function"
            );
        }
    };

    for (name, source) in &production {
        for retired in [
            ".predeclare_declaration_shells()",
            ".bind_declarations()",
            ".analyze_all()",
            ".definitions().declarations()",
            ".candidate.fact()",
            "legacy_declaration_shell",
            "fallback_declaration_shell",
            "declaration_shell_cache",
            "warm_declaration_shell",
            "eager_declaration_shell",
            "legacy_raw_const_syntax",
            "fallback_raw_const_syntax",
            "raw_const_syntax_cache",
            "warm_raw_const_syntax",
            "eager_raw_const_syntax",
            "legacy_raw_declaration_signature",
            "fallback_raw_declaration_signature",
            "raw_declaration_signature_cache",
            "warm_raw_declaration_signature",
            "eager_raw_declaration_signature",
            "legacy_raw_declaration_body",
            "fallback_raw_declaration_body",
            "raw_declaration_body_cache",
            "warm_raw_declaration_body",
            "eager_raw_declaration_body",
            "legacy_declaration_import",
            "fallback_declaration_import",
            "declaration_import_cache",
            "warm_declaration_import",
            "eager_declaration_import",
            "Sema::new_synthetic(",
        ] {
            assert!(
                !source.contains(retired),
                "compiler production module {name} bypassed keyed shell authority through {retired}"
            );
        }
        for test_adapter in [
            ".predeclare_declaration_shells_for_test()",
            ".bind_declarations_for_test()",
            ".analyze_all_for_test()",
            ".resolve_declarations_for_test()",
            ".resolve_declarations_with_work_for_test()",
        ] {
            assert_test_gated_calls(name, source, test_adapter);
        }
        if !matches!(*name, "parsed_modules" | "revisioned_query_database") {
            for evaluator_only in [
                ".evaluate_declaration_shell(",
                ".evaluate_raw_declaration_signature(",
                ".declaration_import(",
                ".declaration_capabilities()",
                "project_semantic_shell(",
            ] {
                assert!(
                    !source.contains(evaluator_only),
                    "raw declaration evaluator/importer escaped into {name}: {evaluator_only}"
                );
            }
        }
        if !matches!(
            *name,
            "body_query"
                | "declaration_candidate"
                | "parsed_modules"
                | "revisioned_query_database"
                | "semantic_query_nucleus"
        ) {
            assert!(
                !code_identifiers(source).contains(&"RawDeclarationSignature"),
                "compiler production module {name} escaped the signature-projection authority allowlist"
            );
        }
        if !matches!(
            *name,
            "declaration_candidate"
                | "durable_comptime"
                | "parsed_modules"
                | "revisioned_query_database"
                | "semantic_query_nucleus"
        ) {
            assert!(
                !source.contains("DeclarationImport"),
                "compiler production module {name} escaped the declaration-import authority allowlist"
            );
        }
    }
    // The retired source-owned `Sema` plane is gone from rue-compiler entirely,
    // tests included: nothing may construct a `Sema` or call its frozen test
    // adapters in any build configuration.
    for (name, source) in PRODUCTION_MODULES {
        for retired in [
            "Sema::new_synthetic(",
            "Sema::new_for_target(",
            ".bind_declarations_for_test()",
            ".analyze_all_for_test()",
            ".analyze_all_for_test_with_stable_endpoints(",
            ".resolve_declarations_for_test()",
            ".resolve_declarations_with_work_for_test()",
            ".analyze_all_bodies_for_test()",
        ] {
            assert!(
                !source.contains(retired),
                "rue-compiler module {name} regained the retired Sema plane: {retired}"
            );
        }
    }
    assert!(
        !canonical.contains("predeclare_imported_declaration_shells"),
        "the retired shell-import recipe returned to canonical_semantic"
    );
    assert!(!canonical.contains("fn analyze_canonical_program("));
    assert_eq!(
        registered_family_source("compiler.declaration-shell")
            .matches(".evaluate_declaration_shell(")
            .count(),
        1
    );
    assert_eq!(parsed.matches("fn evaluate_declaration_shell(").count(), 1);
    for (name, source) in &production {
        for removed in [
            "RawConstSyntax",
            "raw_const_syntax",
            "compiler.raw-const-syntax",
            "RawDeclarationBody",
            "raw_declaration_body",
            "compiler.raw-declaration-body",
        ] {
            assert!(
                !source.contains(removed),
                "the deleted raw-constant authority returned in {name}: {removed}"
            );
        }
    }
    assert_eq!(
        runtime
            .matches(".evaluate_raw_declaration_signature(")
            .count(),
        0,
        "the production signature projection must not reconstruct raw syntax"
    );
    assert!(!parsed.contains("fn evaluate_raw_declaration_signature("));
    assert!(!parsed.contains("RawDeclarationSignatureLocator"));
    assert!(
        !runtime.contains("compiler.declaration-signature-projection"),
        "the semantic nucleus must not retain a peer signature-projection family"
    );
    assert_eq!(
        runtime.matches("project_semantic_signature(").count(),
        1,
        "the signature query must project the exact borrowed parsed declaration"
    );
    assert!(!runtime.contains("parse_semantic_signature("));
    assert_eq!(runtime.matches(".declaration_import(").count(), 1);
    assert_eq!(parsed.matches("fn declaration_import(").count(), 1);
    assert_eq!(
        registered_family_source("compiler.declaration-import")
            .matches("\"compiler.declaration-import\"")
            .count(),
        1
    );
    assert!(!runtime.contains("Vec::remove"));

    let toolchain_demand_evaluator = registered_family_source("compiler.body-toolchain-demands");
    assert!(toolchain_demand_evaluator.contains("DeclarationBodyPlanQueryKey"));
    assert!(toolchain_demand_evaluator.contains(".fallible_intrinsics()"));
    for forbidden in [
        "RawDeclarationBodyQueryKey",
        "scan_body_payload_kinds",
        "rue_lexer::Lexer",
    ] {
        assert!(
            !toolchain_demand_evaluator.contains(forbidden),
            "toolchain-demand evaluator regained a source-text scan: {forbidden}"
        );
    }

    let occurrence = runtime
        .split("struct DeclarationOccurrenceIndex {")
        .nth(1)
        .and_then(|tail| tail.split("enum DeclarationOccurrenceIndexValue").next())
        .unwrap();
    for forbidden in ["DeclarationShellFact", "CompileErrors", "Span", "FileId"] {
        assert!(
            !occurrence.contains(forbidden),
            "occurrence terminal regained shell/parser payload: {forbidden}"
        );
    }
    let terminal_algebra = runtime
        .split("enum DeclarationOccurrenceIndexValue")
        .nth(1)
        .and_then(|tail| tail.split("struct DeclarationShellQueryKey").next())
        .unwrap();
    assert!(!terminal_algebra.contains("CompileErrors"));
    let shell_terminal = runtime
        .split("enum DeclarationShellQueryValue")
        .nth(1)
        .and_then(|tail| tail.split("struct DeclarationBodyPlanQueryKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "InstRef",
        "Rir",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !shell_terminal.contains(forbidden),
            "shell terminal regained positioned/live semantic payload: {forbidden}"
        );
    }
    let declaration_import_terminal = runtime
        .split("enum DeclarationImportQueryValue")
        .nth(1)
        .and_then(|tail| tail.split("struct LookupNameKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "Spur",
        "Ast",
        "InstRef",
        "Rir",
        "TypeId",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !declaration_import_terminal.contains(forbidden),
            "declaration-import terminal regained positioned/live parser or semantic payload: {forbidden}"
        );
    }
    let lookup_fact = runtime
        .split("struct LookupNameFact")
        .nth(1)
        .and_then(|tail| tail.split("struct LookupNameValue").next())
        .unwrap();
    for forbidden in ["CompileErrors", "ModuleRevision", "Span", "FileId", "Spur"] {
        assert!(
            !lookup_fact.contains(forbidden),
            "LookupName retained fact regained locator/live payload: {forbidden}"
        );
    }
    let lookup_value = runtime
        .split("struct LookupNameValue")
        .nth(1)
        .and_then(|tail| {
            tail.split("/// The canonical §4 name-resolution outcome")
                .next()
        })
        .unwrap();
    for forbidden in ["CompileErrors", "ModuleRevision", "Span", "FileId", "Spur"] {
        assert!(
            !lookup_value.contains(forbidden),
            "LookupName terminal regained locator/live payload: {forbidden}"
        );
    }
    let declaration_import_payload = module("declaration_candidate")
        .split("pub(crate) struct DeclarationImportSiteKey")
        .nth(1)
        .and_then(|tail| tail.split("impl DeclarationCandidateKey").next())
        .unwrap();
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "Spur",
        "Ast",
        "Rir",
        "InstRef",
        "TypeId",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !declaration_import_payload.contains(forbidden),
            "declaration-import payload regained a positioned or live parser/semantic handle: {forbidden}"
        );
    }
    let declaration_import_locator = parsed
        .split("struct RawDeclarationImportRange")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) struct ParsedWarningCallHead").next())
        .unwrap();
    for forbidden in ["Vec<", "Arc<", "Box<"] {
        assert!(
            !declaration_import_locator.contains(forbidden),
            "declaration-import parser locator regained per-declaration heap storage: {forbidden}"
        );
    }
    for required in ["start: u32", "len: u32"] {
        assert!(
            declaration_import_locator.contains(required),
            "declaration-import parser locator lost fixed range field {required}"
        );
    }
    let declaration_import_evaluator = registered_family_source("compiler.declaration-import");
    for forbidden in [
        "module_rirs",
        "lower_module_rir",
        "CanonicalMergedProgram",
        "SemanticView",
    ] {
        assert!(
            !declaration_import_evaluator.contains(forbidden),
            "declaration-import evaluator gained a broad compiler dependency: {forbidden}"
        );
    }
    for required in [
        ".query_registered(",
        "ResolveImportKey",
        "ImportDemandMode::Rooted",
    ] {
        assert!(
            declaration_import_evaluator.contains(required),
            "declaration-import evaluator lost canonical query delegation: {required}"
        );
    }
    let resolve_import_evaluator = registered_family_source("compiler.resolve-import");
    for required in [
        "index.import_occurrence(&key.occurrence)",
        "exact_import_winner(",
        "resolve_exact_import_winner(",
        "accepted_import_provenance_input(",
        "source.metadata_identity()",
    ] {
        assert!(
            resolve_import_evaluator.contains(required),
            "resolve-import lost exact winning-provenance dependency: {required}"
        );
    }
    for forbidden in [
        "reduce_exact_import_graph(",
        "canonical ResolveImport inputs reduce deterministically",
    ] {
        assert!(
            !resolve_import_evaluator.contains(forbidden),
            "resolve-import regained broad or panicking reduction: {forbidden}"
        );
    }
    let physical_provenance_identity = runtime
        .split("fn accepted_import_provenance_input")
        .nth(1)
        .and_then(|tail| tail.split("fn import_observation_input").next())
        .unwrap();
    for required in ["identity.volume()", "identity.file()"] {
        assert!(
            physical_provenance_identity.contains(required),
            "accepted-import provenance input lost exact physical identity component: {required}"
        );
    }
    let failure_algebra = module("declaration_candidate")
        .split("pub(crate) enum DeclarationOccurrenceFailure")
        .nth(1)
        .and_then(|tail| tail.split("impl DeclarationCandidateKey").next())
        .unwrap()
        // The raw syntax terminals carry the durable, definition-relative
        // frontend anchor for each transported anonymous type literal
        // (RUE-1089). `RirStructuralAnchor` is position- and trivia-insensitive
        // by construction — the antithesis of a live IR handle — so it is
        // sanctioned here while raw `Rir`/`InstRef`/`Span` handles stay banned.
        .replace("rue_rir::RirStructuralAnchor", "<durable-anchor>");
    for forbidden in [
        "CompileErrors",
        "Span",
        "FileId",
        "InstRef",
        "Rir",
        "SemanticDefinitionToken",
        "SemanticModuleToken",
    ] {
        assert!(
            !failure_algebra.contains(forbidden),
            "declaration failure algebra regained position/live identity: {forbidden}"
        );
    }
    assert_eq!(
        canonical
            .matches("predeclare_imported_declaration_shells")
            .count(),
        0,
        "the retired shell-import recipe may not return to canonical_semantic"
    );
    for (name, source) in &production {
        assert!(
            !source.contains("predeclare_imported_declaration_shells"),
            "compiler production module {name} gained a peer shell importer"
        );
    }
    assert!(!session.contains("DeclarationShellFact"));
}

#[test]
fn canonical_semantic_body_has_no_compiler_owned_peer_algebra() {
    // This guard used to read `durable_body.rs` alone. RUE-1541 removed that
    // module, so the mirror would have to reappear in some other production
    // module — which is what this now checks.
    for (name, source) in PRODUCTION_MODULES {
        for removed in [
            "pub enum DurableAirInstData",
            "pub struct DurableAirInst",
            "pub struct DurablePlace",
            "pub enum DurableProjection",
            "pub enum DurablePattern",
            "pub struct DurableBodyAnchor",
            "pub struct DurableSpecializationIdentity",
            "pub type DurableProjection",
            "pub type DurableAirInstData",
        ] {
            assert!(
                !source.contains(removed),
                "compiler-owned canonical body mirror returned in {name}: {removed}"
            );
        }
        for removed in [
            "DurableOrdinaryBodyPayload",
            "DurableSpecializedBodyPayload",
            "convert_semantic_body_exports",
            "convert_semantic_specialized_body_exports",
        ] {
            assert!(
                !source.contains(removed),
                "removed peer body-export payload returned in {name}: {removed}"
            );
        }
    }
}

#[test]
fn rue_1027_production_body_authority_is_query_owned_and_import_only() {
    let canonical = include_str!("canonical_semantic.rs");
    let session = SESSION_PRODUCTION_SOURCE;
    let database = REVISIONED_DATABASE_SOURCE;
    let body_query = include_str!("body_query.rs");

    for removed_peer_assembler in [
        "fn finish_canonical_analysis(",
        "compose_queried_bodies(",
        "analyze_all_bodies",
        "fn recover_declaration_failure(",
    ] {
        assert!(
            !canonical.contains(removed_peer_assembler),
            "canonical semantic peer assembly returned: {removed_peer_assembler}"
        );
    }
    assert!(!session.contains("pub fn semantic("));
    assert!(!session.contains("semantic_view_from_rooted"));
    assert!(!session.contains("requires_request_local_discovery"));

    let anonymous_branch = database
        .split("Key::AnonymousNominal(query) =>")
        .nth(1)
        .and_then(|source| source.split("Key::ComptimeCall(call) =>").next())
        .expect("SemanticNucleus anonymous nominal branch");
    assert!(anonymous_branch.contains("produced_anonymous_for_semantic_nucleus"));
    assert!(!anonymous_branch.contains("Key::ComptimeCall("));
    for family in [
        "compiler.body-transaction",
        "compiler.body-produced-anonymous",
    ] {
        assert!(
            database.contains(family),
            "missing RUE-1027 family: {family}"
        );
    }
    for redundant_projection in [
        "\"compiler.body-references\"",
        "\"compiler.canonical-body\"",
    ] {
        assert!(!database.contains(redundant_projection));
    }
    assert!(body_query.contains("pub(crate) struct BodyQueryKey"));
    assert!(body_query.contains("pub(crate) struct BodyProducedAnonymousNominals"));
    assert!(!body_query.contains("Span"));
}

#[test]
fn rue_1191_anonymous_digest_collision_authority_is_body_closure_owned() {
    let database = REVISIONED_DATABASE_SOURCE;
    let production = database
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("production revisioned database source");
    let closure = registered_family_source("compiler.body-closure");

    assert!(closure.contains("anonymous_digest_owners"));
    assert!(closure.contains("compiler_anonymous_identity_digest"));
    assert!(closure.contains("BodyClosureFatal::AnonymousDigestCollision"));
    assert_eq!(
        closure
            .matches("register_body_closure_anonymous_digest(")
            .count(),
        2,
        "closure aggregation must register declaration and body-produced anonymous facts"
    );
    assert_eq!(
        production
            .matches("register_body_closure_anonymous_digest(")
            .count(),
        3,
        "the two closure call sites plus the helper definition are the complete production authority"
    );

    let body_local_projection = database
        .split("let body_produced_anonymous =")
        .nth(1)
        .and_then(|source| {
            source
                .split("let produced_anonymous_for_semantic_nucleus =")
                .next()
        })
        .expect("body-produced-anonymous evaluator");
    assert!(
        !body_local_projection.contains("register_body_closure_anonymous_digest("),
        "the per-body projection must not call the cross-body registrar"
    );
    let transaction_evaluator = production
        .split("impl BodyTransactionEvaluator {")
        .nth(1)
        .and_then(|source| source.split("\nimpl RevisionedQueryDatabase {").next())
        .expect("registered per-body transaction evaluator");
    assert!(
        !transaction_evaluator.contains("register_body_closure_anonymous_digest("),
        "the per-body evaluator must not call the cross-body registrar"
    );
}

#[test]
fn durable_const_integer_semantics_use_the_shared_kernel() {
    let source = REVISIONED_DATABASE_SOURCE;
    let durable = DURABLE_COMPTIME_SOURCE;
    assert!(
        !source.contains("SemanticConstEvaluator"),
        "production declaration-time roots must not retain a second evaluator"
    );
    assert_eq!(
        source.matches("evaluate_durable_comptime_root(").count(),
        3,
        "one shared AIR root adapter plus the two production roots"
    );
    assert_eq!(
        source
            .matches("DurableComptimeHost::new(authority)")
            .count(),
        1,
        "the AIR host must be constructed only by the shared root adapter"
    );
    for required in [
        "finish_arith",
        "DurableComptimeScalarPolicy::checked_integer_result",
        "durable_arithmetic_operation_name",
        "checked_neg_literal_report_i128",
    ] {
        assert!(
            durable.contains(required),
            "durable scalar policy missing {required}"
        );
    }
    assert!(
        source.contains("rue_air::ComptimeEngine::new(&mut host)"),
        "durable roots must run through the canonical AIR engine"
    );
}

#[test]
fn comptime_depth_consumers_use_the_air_authority() {
    let database = REVISIONED_DATABASE_SOURCE;
    assert!(
        !database.contains("SEMANTIC_COMPTIME_MAX_DEPTH"),
        "durable queries must not define a competing comptime depth constant"
    );
    assert!(
        !database.contains("MAX_SPECIALIZATION_ROUNDS"),
        "specialization must not retain a separate depth budget"
    );
    assert!(
        database.contains("comptime_specialization_depth")
            && database.contains("rue_air::comptime_depth_over_limit")
            && database.contains("rue_air::MAX_COMPTIME_CALL_DEPTH")
            && database.contains("ComptimeFrame::callable_body")
            && database.contains("!rue_air::comptime_depth_over_limit")
            && !database.contains("<= rue_air::MAX_COMPTIME_CALL_DEPTH"),
        "durable query scheduling must use the canonical AIR depth authority"
    );
}

#[test]
fn durable_named_array_length_consumers_share_one_conversion_kernel() {
    let host = DURABLE_COMPTIME_HOST_SOURCE;
    assert!(host.contains("classify_durable_named_array_length"));
    assert!(host.contains("durable_named_array_length_value"));
    assert!(host.contains("durable_named_array_length_failure"));
    assert!(host.contains("resolve_named_array_length"));
    for kernel in [
        "classify_durable_named_array_length",
        "durable_named_array_length_value",
        "durable_named_array_length_failure",
    ] {
        assert!(DURABLE_COMPTIME_PROJECTION_SOURCE.contains(kernel));
    }
}

#[test]
fn durable_specialized_producer_issuance_has_one_ordered_kernel() {
    let durable = DURABLE_COMPTIME_LIFECYCLE_SOURCE;
    let producer_kernel = durable
        .split("pub(crate) fn canonical_specialized_function_producer(")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n}\n\npub(crate) fn canonical_specialized_function_instance(")
                .next()
        })
        .expect("durable producer issuance kernel");
    let instance_kernel = durable
        .split("pub(crate) fn canonical_specialized_function_instance(")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n}\n\nimpl DurableComptimeCallContext")
                .next()
        })
        .expect("durable specialization conversion kernel");
    assert_eq!(
        durable
            .matches("pub(crate) fn canonical_specialized_function_producer(")
            .count(),
        1
    );
    for required in [
        "type_arguments",
        "value_arguments",
        "canonical_specialized_function_instance",
    ] {
        assert!(producer_kernel.contains(required));
    }
    assert!(instance_kernel.contains("function_instance_from_canonical_arguments"));
    assert!(!instance_kernel.contains("FunctionInstanceKey::Specialization"));
    for forbidden in [
        "InstData",
        "ComptimeEngine",
        "SemanticConstEvaluator",
        "query_registered",
        "self.effects",
    ] {
        assert!(!producer_kernel.contains(forbidden));
        assert!(!instance_kernel.contains(forbidden));
    }
    let source = REVISIONED_DATABASE_SOURCE;
    let identity = include_str!("semantic_identity.rs");
    let identity_production = identity
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("semantic identity production source");
    let identity_kernel = identity_production
        .split("pub(crate) fn function_instance_from_canonical_arguments(")
        .nth(1)
        .and_then(|source| source.split("\n}\n\nfn tag(").next())
        .expect("canonical specialization identity kernel");
    assert_eq!(
        identity_production
            .matches("pub(crate) fn function_instance_from_canonical_arguments(")
            .count(),
        1
    );
    assert!(identity_kernel.contains("FunctionInstanceKey::Specialization"));
    let call_root = source
        .split("Key::ComptimeCall(call) => {")
        .nth(1)
        .and_then(|source| {
            source
                .split("\n                    let kind = if matches!(value, Value::Failure(_))")
                .next()
        })
        .expect("durable comptime-call root");
    assert!(call_root.contains("evaluate_durable_comptime_root"));
    assert!(call_root.contains("canonical_specialized_function_producer"));
    assert!(!call_root.contains("FunctionInstanceKey::Specialization"));
    assert!(!call_root.contains("let canonical_arguments = crate::CanonicalArguments"));
    assert!(!call_root.contains("type_instance_from_semantic("));
    assert!(!call_root.contains("argument_value_from_semantic("));
    let body_provider = source
        .split("impl rue_air::DurableBodyLookupSource")
        .nth(1)
        .and_then(|source| source.split("impl rue_air::DurableConstSource").next())
        .expect("body-provider comptime reduction");
    assert!(body_provider.contains("canonical_specialized_function_instance"));
    assert!(!body_provider.contains("let producer = crate::FunctionInstanceKey::Specialization"));
    assert!(!body_provider.contains("type_instance_from_semantic"));
    assert!(!body_provider.contains("argument_value_from_semantic"));
    let context = durable
        .split("impl DurableComptimeCallContext {")
        .nth(1)
        .and_then(|source| source.split("impl DurableComptimeCallTicket").next())
        .expect("durable call context implementation");
    assert!(context.contains("fn canonical_function_producer("));
    assert!(!context.contains("pub(crate) fn canonical_function_producer("));
    let ticket = durable
        .split("impl DurableComptimeCallTicket {")
        .nth(1)
        .and_then(|source| source.split("\n}\n\n#[allow(dead_code)]").next())
        .expect("durable call ticket implementation");
    assert!(ticket.contains("pub(crate) fn canonical_function_producer("));
    assert!(ticket.contains("self.context.canonical_function_producer("));
}

#[test]
fn durable_comptime_services_are_named_authority_operations() {
    let facade = DURABLE_COMPTIME_SOURCE;
    let database = REVISIONED_DATABASE_SOURCE;
    let production = database
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("compiler production source");
    assert!(
        !database.contains("SemanticConstEvaluator"),
        "production roots must not retain a second declaration-time evaluator"
    );
    assert_eq!(
        database.matches("evaluate_durable_comptime_root(").count(),
        3,
        "one canonical adapter and two production query roots"
    );
    assert_eq!(
        production
            .matches("rue_air::ComptimeEngine::new(&mut host)")
            .count(),
        1,
        "the durable host must delegate to AIR exactly once"
    );
    for required in [
        "DurableComptimeSession",
        "DurableComptimeHost",
        "admit_const_root",
        "ComptimeFrame",
        "finalize_registered_imports",
        "finish_root",
        "DurableComptimeForeignQueryAuthority",
    ] {
        assert!(
            facade.contains(required) || database.contains(required),
            "missing {required}"
        );
    }
}

#[test]
fn durable_comptime_responsibilities_have_exact_module_owners() {
    let modules = [
        ("diagnostics", DURABLE_COMPTIME_DIAGNOSTICS_SOURCE),
        ("effects", DURABLE_COMPTIME_EFFECTS_SOURCE),
        ("host", DURABLE_COMPTIME_HOST_SOURCE),
        ("lifecycle", DURABLE_COMPTIME_LIFECYCLE_SOURCE),
        ("projection", DURABLE_COMPTIME_PROJECTION_SOURCE),
        ("services", DURABLE_COMPTIME_SERVICES_SOURCE),
        ("structured", DURABLE_COMPTIME_STRUCTURED_SOURCE),
        ("target", DURABLE_COMPTIME_TARGET_SOURCE),
    ];
    let declared_modules = DURABLE_COMPTIME_FACADE_SOURCE
        .lines()
        .filter_map(|line| line.trim().strip_prefix("mod "))
        .filter_map(|module| module.strip_suffix(';'))
        .collect::<Vec<_>>();
    assert_eq!(
        declared_modules,
        modules.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "the durable comptime facade must declare the exact reviewed responsibility modules"
    );
    for (name, _) in &modules {
        assert_eq!(
            DURABLE_COMPTIME_FACADE_SOURCE
                .matches(&format!("pub(crate) use {name}::*;"))
                .count(),
            1,
            "the durable comptime facade must reexport the {name} owner exactly once"
        );
    }
    assert!(DURABLE_COMPTIME_FACADE_SOURCE.lines().count() < 80);
    for forbidden in [
        "pub(crate) struct ",
        "pub(crate) enum ",
        "pub(crate) trait ",
        "pub(crate) fn ",
        "impl<",
        "impl ",
    ] {
        assert!(
            !DURABLE_COMPTIME_FACADE_SOURCE.contains(forbidden),
            "the durable comptime facade regained implementation authority through {forbidden}"
        );
    }

    for (definition, owner, source) in [
        (
            "pub(crate) enum DurableComptimeFailure",
            "diagnostics",
            DURABLE_COMPTIME_DIAGNOSTICS_SOURCE,
        ),
        (
            "pub(crate) struct DurableComptimeDiagnosticSite",
            "diagnostics",
            DURABLE_COMPTIME_DIAGNOSTICS_SOURCE,
        ),
        (
            "pub(crate) enum DurableComptimeApplicationPolicy",
            "effects",
            DURABLE_COMPTIME_EFFECTS_SOURCE,
        ),
        (
            "pub(crate) struct DurableComptimeEffects",
            "effects",
            DURABLE_COMPTIME_EFFECTS_SOURCE,
        ),
        (
            "pub(crate) struct DurableComptimeSession",
            "lifecycle",
            DURABLE_COMPTIME_LIFECYCLE_SOURCE,
        ),
        (
            "struct DurableComptimeCallToken {",
            "lifecycle",
            DURABLE_COMPTIME_LIFECYCLE_SOURCE,
        ),
        (
            "pub(crate) fn finalize_registered_imports(",
            "lifecycle",
            DURABLE_COMPTIME_LIFECYCLE_SOURCE,
        ),
        (
            "pub(crate) fn durable_type_from_instance_key(",
            "projection",
            DURABLE_COMPTIME_PROJECTION_SOURCE,
        ),
        (
            "pub(crate) enum DurableComptimeValueFitFailure",
            "projection",
            DURABLE_COMPTIME_PROJECTION_SOURCE,
        ),
        (
            "pub(crate) struct DurableComptimeScalarPolicy",
            "projection",
            DURABLE_COMPTIME_PROJECTION_SOURCE,
        ),
        (
            "pub(crate) trait DurableComptimeSemanticAuthority",
            "services",
            DURABLE_COMPTIME_SERVICES_SOURCE,
        ),
        (
            "pub(crate) trait DurableComptimeForeignCallAuthority",
            "services",
            DURABLE_COMPTIME_SERVICES_SOURCE,
        ),
        (
            "pub(crate) struct DurableComptimeServices",
            "services",
            DURABLE_COMPTIME_SERVICES_SOURCE,
        ),
        (
            "pub(crate) fn resolve_target_intrinsic_facts(",
            "target",
            DURABLE_COMPTIME_TARGET_SOURCE,
        ),
        (
            "pub(crate) struct DurableComptimeHost<'a",
            "host",
            DURABLE_COMPTIME_HOST_SOURCE,
        ),
        (
            "pub(crate) fn begin_durable_structured_type",
            "structured",
            DURABLE_COMPTIME_STRUCTURED_SOURCE,
        ),
        (
            "pub(crate) fn resume_durable_structured_type",
            "structured",
            DURABLE_COMPTIME_STRUCTURED_SOURCE,
        ),
    ] {
        assert!(source.contains(definition), "{owner} lost {definition}");
        assert_eq!(
            DURABLE_COMPTIME_SOURCE.matches(definition).count(),
            1,
            "{definition} must have exactly one durable comptime owner ({owner})"
        );
    }

    let host_fields = DURABLE_COMPTIME_HOST_SOURCE
        .split("pub(crate) struct DurableComptimeHost<'a")
        .nth(1)
        .and_then(|source| source.split("\n}").next())
        .expect("bounded durable host fields");
    assert!(host_fields.contains("services: DurableComptimeServices<'a, A>"));
    assert!(!host_fields.contains("authority:"));
    for (owner, source) in
        std::iter::once(("facade", DURABLE_COMPTIME_FACADE_SOURCE)).chain(modules.iter().copied())
    {
        for forbidden in [
            "ComptimeEngine::new",
            "SemanticConstEvaluator",
            "InstData::",
        ] {
            assert!(
                !source.contains(forbidden),
                "the {owner} durable comptime owner regained peer evaluation authority through {forbidden}"
            );
        }
    }
    assert_eq!(
        DURABLE_COMPTIME_ENGINE_ENTRY_SOURCE
            .matches("ComptimeEngine::new")
            .count(),
        1,
        "the compiler must construct AIR's evaluator exactly once, outside the durable adapter"
    );

    let lifecycle_production = DURABLE_COMPTIME_LIFECYCLE_SOURCE
        .split("\n#[cfg(test)]\npub(super) mod test_support")
        .next()
        .expect("production lifecycle source");
    assert_eq!(
        lifecycle_production
            .matches("DurableComptimeCallToken::new(")
            .count(),
        1,
        "only reservation may mint a production call token"
    );
    assert_eq!(
        lifecycle_production
            .matches("DurableComptimeAdmittedCall::new(")
            .count(),
        1,
        "only session admission may create a production admitted-call wrapper"
    );
    assert!(!lifecycle_production.contains("pub(super) struct DurableComptimeCallToken"));
    assert!(
        !lifecycle_production
            .contains("pub(super) fn new(\n        token: DurableComptimeCallToken")
    );

    for (source, definition) in [
        (
            DURABLE_COMPTIME_EFFECTS_SOURCE,
            "pub(crate) struct DurableComptimeEffects",
        ),
        (
            DURABLE_COMPTIME_LIFECYCLE_SOURCE,
            "pub(crate) struct DurableComptimeSession",
        ),
        (
            DURABLE_COMPTIME_LIFECYCLE_SOURCE,
            "pub(crate) struct DurableComptimeCallLifecycle",
        ),
    ] {
        let fields = source
            .split(definition)
            .nth(1)
            .and_then(|source| source.split("\n}").next())
            .expect("bounded durable owner fields");
        assert!(
            !fields.contains("pub(super)"),
            "durable owner state must stay private behind operations and cfg(test) inspectors"
        );
    }

    for test_name in [
        "scalar_policy_preserves_integer_precedence_and_fallbacks",
        "scalar_policy_preserves_fit_and_arithmetic_diagnostics",
        "type_intrinsic_policy_preserves_all_bounds_gates_and_mismatch",
    ] {
        assert!(DURABLE_COMPTIME_PROJECTION_SOURCE.contains(test_name));
        assert!(!DURABLE_COMPTIME_DIAGNOSTICS_SOURCE.contains(test_name));
    }
    for test_name in [
        "incremental_binding_preserves_type_then_value_order_and_substitution",
        "incremental_binding_preserves_early_type_and_range_failures",
        "incremental_binding_requires_direct_unit_for_type_arguments",
        "diagnostic_sites_are_keyed_and_reject_unknown_programs",
    ] {
        assert!(DURABLE_COMPTIME_LIFECYCLE_SOURCE.contains(test_name));
        assert!(!DURABLE_COMPTIME_PROJECTION_SOURCE.contains(test_name));
        assert!(!DURABLE_COMPTIME_DIAGNOSTICS_SOURCE.contains(test_name));
    }
    assert!(!DURABLE_COMPTIME_PROJECTION_SOURCE.contains("DurableComptimeHost"));
    assert!(!DURABLE_COMPTIME_PROJECTION_SOURCE.contains("query_registered"));
    assert!(!DURABLE_COMPTIME_SERVICES_SOURCE.contains("fn eval("));
    for invariant in [
        "reservation may create exactly one admission token",
        "admitted edge may\n//! create exactly one ticket",
        "only the matching active lifecycle may enter\n//! or finish that ticket",
        "Non-known AIR outcomes always clean up and never\n//! publish child effects",
    ] {
        assert!(
            DURABLE_COMPTIME_LIFECYCLE_SOURCE.contains(invariant),
            "lifecycle owner lost the documented one-shot invariant: {invariant}"
        );
    }
}

#[test]
fn match_patterns_have_one_air_decoder_and_one_durable_kernel() {
    let durable = DURABLE_COMPTIME_SOURCE;
    assert_eq!(
        durable
            .matches("pub(crate) fn durable_match_pattern_matches")
            .count(),
        1
    );
    assert!(durable.contains("ComptimeMatchPattern::Path"));
    for rejection in [
        "ConditionNotBoolean",
        "ArithmeticOperandNotInteger",
        "UnaryOperandNotInteger",
        "UnsupportedExpression",
    ] {
        assert!(durable.contains(rejection));
    }
}

#[test]
fn durable_air_host_is_composition_not_a_peer_interpreter() {
    let host = DURABLE_COMPTIME_HOST_SOURCE;
    let host_fields = host
        .split("pub(crate) struct DurableComptimeHost<'a")
        .nth(1)
        .and_then(|source| source.split("\n}").next())
        .expect("bounded durable host fields");
    assert!(host.contains("impl<A: DurableComptimeHostAuthority"));
    assert!(host.contains("rue_air::ComptimeHost"));
    assert!(host.contains("services: DurableComptimeServices<'a, A>"));
    assert!(host.contains("DurableComptimeServices::new(authority)"));
    assert!(host.contains("durable_session_mut()"));
    assert!(host.contains("DurableComptimeScalarPolicy"));
    assert!(host.contains("project_durable_anonymous_nominal"));
    assert!(!host_fields.contains("authority:"));
    assert!(!host.contains("InstData::"));
    assert!(!host.contains("fn eval("));
    assert!(!host.contains("SemanticConstEvaluator"));
    assert!(!host.contains("SemanticNucleusKey::ComptimeCall"));
}

#[test]
fn durable_roots_share_one_terminal_classifier() {
    let database = REVISIONED_DATABASE_SOURCE;
    assert_eq!(
        database.matches("fn durable_comptime_root_result(").count(),
        1,
        "declaration-time roots must have one terminal classifier"
    );
    assert_eq!(
        database
            .matches("durable_comptime_root_result(outcome)")
            .count(),
        2,
        "both declaration-time roots must use the shared classifier"
    );
    assert_eq!(
        database
            .matches("declaration-time comptime did not reduce to a value")
            .count(),
        1,
        "non-reduction policy must have one spelling"
    );
}

#[test]
fn durable_projection_failures_have_one_shared_semantic_mapping() {
    let durable = DURABLE_COMPTIME_DIAGNOSTICS_SOURCE;
    let database = REVISIONED_DATABASE_SOURCE;
    assert_eq!(
        durable
            .matches("pub(crate) fn durable_candidate_rir_semantic_failure(")
            .count(),
        1
    );
    assert_eq!(
        durable
            .matches("pub(crate) fn durable_materialization_semantic_failure(")
            .count(),
        1
    );
    let projection = durable
        .split("fn durable_projection_failure_error(")
        .nth(1)
        .and_then(|source| source.split("\n}\n\n").next())
        .expect("durable projection adapter");
    assert!(projection.contains("durable_candidate_rir_semantic_failure(&failure)"));
    assert!(projection.contains("durable_materialization_semantic_failure(failure)"));
    assert!(!projection.contains("CandidateRirRejected(errors)"));
    assert!(!projection.contains("Materialization::Build(error)"));
    let candidate_wrapper = database
        .split("fn candidate_rir_semantic_failure(")
        .nth(1)
        .and_then(|source| source.split("\n}\n\n").next())
        .expect("candidate failure adapter");
    assert!(
        candidate_wrapper
            .contains("crate::durable_comptime::durable_candidate_rir_semantic_failure(failure)")
    );
    let materialization_wrapper = database
        .split("fn semantic_materialization_failure(")
        .nth(1)
        .and_then(|source| source.split("\n}\n\n").next())
        .expect("materialization failure adapter");
    assert!(
        materialization_wrapper
            .contains("crate::durable_comptime::durable_materialization_semantic_failure(failure)")
    );
}

#[test]
fn durable_diagnostic_sites_use_only_registered_program_provenance() {
    let durable = DURABLE_COMPTIME_LIFECYCLE_SOURCE;
    let method = durable
        .split("pub(crate) fn diagnostic_site(")
        .nth(1)
        .and_then(|source| {
            source
                .split("    /// Atomically admit an already-prepared")
                .next()
        })
        .expect("bounded durable diagnostic-site method");
    assert!(method.contains("self.programs.get(key)"));
    assert!(method.contains("declaration_candidate_for_stable_key"));
    assert!(method.contains("DurableComptimeDiagnosticSite::new"));
    for forbidden in [
        "self.lifecycle",
        "self.next_call",
        "effects",
        "query_registered",
        "InstData",
        "ComptimeEngine",
        "SemanticConstEvaluator",
    ] {
        assert!(
            !method.contains(forbidden),
            "diagnostic-site lookup acquired unrelated authority: {forbidden}"
        );
    }
    assert_eq!(
        durable.matches("pub(crate) fn diagnostic_site(").count(),
        1,
        "the session must expose one immutable diagnostic-site lookup"
    );
}

#[test]
fn import_resolution_remains_discovery_owned() {
    let production = PRODUCTION_MODULES
        .iter()
        .map(|(_, source)| *source)
        .collect::<String>();
    assert_eq!(
        production.matches("pub struct ImportDirective {").count(),
        1,
        "the compiler must declare exactly one import-site representation"
    );
    assert_eq!(
        production
            .matches("pub enum CanonicalImportResolution {")
            .count(),
        1,
        "the compiler must declare exactly one canonical resolution outcome"
    );
    for removed in [
        ["ParsedImport", "Directive"].concat(),
        ["ParsedImport", "Site"].concat(),
        ["SemanticResolved", "Import"].concat(),
        ["SemanticModule", "Identity"].concat(),
        ["pub enum Import", "Resolution"].concat(),
        ["resolve_", "import_graph"].concat(),
        ["resolve_canonical_", "import_graph"].concat(),
        ["extract_import_", "directives"].concat(),
        ["Dir", "Resolution"].concat(),
    ] {
        assert!(
            !production.contains(&removed),
            "retired import representation or resolver returned: {removed}"
        );
    }

    let discovery = PRODUCTION_MODULES
        .iter()
        .find_map(|(name, source)| (*name == "import_discovery").then_some(*source))
        .unwrap();
    assert!(discovery.contains("pub struct ImportDiscoveryPlan"));
    assert!(discovery.contains("CanonicalImportGraph"));

    let compiler_import_policy = PRODUCTION_MODULES
        .iter()
        .filter(|(name, _)| {
            matches!(
                *name,
                "parsed_modules"
                    | "import_discovery"
                    | "import_graph"
                    | "bound_definitions"
                    | "canonical_semantic"
                    | "session"
            )
        })
        .map(|(_, source)| *source)
        .collect::<String>();
    for forbidden_probe in [
        ["std::", "fs"].concat(),
        ["fs", "::"].concat(),
        ["std::", "env"].concat(),
        ["RUE_STD", "_PATH"].concat(),
        ["canonicalize", "("].concat(),
        [".exists", "("].concat(),
    ] {
        assert!(
            !compiler_import_policy.contains(&forbidden_probe),
            "compiler import policy must not probe host state: {forbidden_probe}"
        );
    }

    let downstream_imports = PRODUCTION_MODULES
        .iter()
        .filter(|(name, _)| {
            matches!(
                *name,
                "parsed_modules"
                    | "import_graph"
                    | "bound_definitions"
                    | "canonical_semantic"
                    | "session"
            )
        })
        .map(|(_, source)| *source)
        .collect::<String>();
    for forbidden_policy in [
        ["candidate_", "groups"].concat(),
        ["resolve_explicit_", "candidates"].concat(),
    ] {
        assert!(
            !downstream_imports.contains(&forbidden_policy),
            "downstream compiler import consumer must not rediscover: {forbidden_policy}"
        );
    }
}

#[test]
fn durable_constructor_diagnostics_use_the_air_interleaver() {
    let durable = DURABLE_COMPTIME_PROJECTION_SOURCE;
    let presenter = durable
        .split_once("pub(crate) fn durable_type_diagnostic_name_with_parameters")
        .and_then(|(_, rest)| rest.split_once("pub(crate) fn inferred_durable_const_type_name"))
        .map(|(body, _)| body)
        .expect("durable constructor presenter remains an explicit production function");
    assert!(presenter.contains("rue_air::format_canonical_application("));
    assert!(!presenter.contains("let mut types ="));
    assert!(!presenter.contains("let mut values ="));
    assert!(!presenter.contains("values.next()"));
    assert!(!presenter.contains("types.next()"));
}

#[test]
fn candidate_plan_metrics_have_one_query_terminal_authority() {
    let database = REVISIONED_DATABASE_SOURCE;
    let queries = include_str!("queries.rs");
    let session = SESSION_PRODUCTION_SOURCE;
    let pipeline = include_str!("pipeline_tests.rs");
    // These consumers live outside this Buck crate, so read them from the
    // repository root at test time instead of smuggling copies into the
    // compiler target. Buck runs tests from the project root; the fallback
    // keeps direct `cargo test`-style invocations useful as well.
    let repository = std::env::current_dir().expect("source guard has a working directory");
    let read_source = |path: &str| {
        std::fs::read_to_string(repository.join(path))
            .unwrap_or_else(|error| panic!("source guard cannot read {path}: {error}"))
    };
    let driver = read_source("crates/rue/src/main.rs");
    let timing = read_source("crates/rue/src/timing.rs");
    let perf_scaling = read_source("crates/rue-perf-schema/src/scaling.rs");
    let bench_scaling = read_source("crates/rue-bench/src/scaling.rs");
    let sources = [
        database,
        queries,
        session,
        pipeline,
        driver.as_str(),
        timing.as_str(),
        perf_scaling.as_str(),
        bench_scaling.as_str(),
    ];

    // These names were the retired placeholder or a second accounting path.
    // Keep the guard source-based so a future compatibility edit cannot
    // silently reintroduce a permanent zero or timing peer.
    for forbidden in [
        "PipelineWork { lowered",
        "lowered: Default::default()",
        "LowerMetrics",
        "benchmark_semantic_body_structure",
        "BodyAnalysisBundle { structural_work:",
    ] {
        for source in sources {
            assert!(
                !source.contains(forbidden),
                "retired authority returned: {forbidden}"
            );
        }
    }

    // The historical decoder remains private, but no current renderer or
    // benchmark consumer may publish its retired lowering/index subgroup.
    assert!(!bench_scaling.contains("body_lowerings"));
    assert!(!bench_scaling.contains("rir_instructions"));
    assert!(!bench_scaling.contains("benchmark_semantic_body_structure"));
    assert!(!driver.contains("benchmark_semantic_body_structure"));
    let public_work = perf_scaling
        .split_once("pub struct CompilerWork")
        .and_then(|(_, tail)| tail.split_once("LegacySemanticBodyStructureWork"))
        .map(|(body, _)| body)
        .expect("public CompilerWork remains separate from the v2 decoder");
    assert!(!public_work.contains("body_lowerings"));
    assert!(!public_work.contains("index_builds"));

    assert!(database.contains("candidate_body_plan.construction"));
    assert!(database.contains("candidate_body_plan.materialization"));
    assert!(session.contains("accrue_candidate_body_plan_work"));
}
