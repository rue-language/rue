#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lasso::ThreadedRodeo;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::{AstGen, InstData, Rir};
    use rue_span::FileId;

    fn lower_files(files: &[(&str, FileId)]) -> (Rir, ThreadedRodeo) {
        let mut interner = ThreadedRodeo::default();
        let mut items = Vec::new();
        for &(source, file_id) in files {
            let (tokens, next) = Lexer::with_interner_and_file_id(source, interner, file_id)
                .tokenize()
                .unwrap();
            let (ast, next) = Parser::new(tokens, next).parse().unwrap();
            items.extend(ast.items);
            interner = next;
        }
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&items);
        let rir = astgen.finish();
        (rir, interner)
    }

    #[test]
    fn inline_constructor_precompute_candidates_are_scoped_to_the_requested_body() {
        let source = r#"
            fn Pair(comptime T: type) -> type { struct { value: T } }
            fn take(value: i32) -> i32 { value }
            fn first(flag: bool, selector: i32) -> i32 {
                let direct = Pair(i32) { value: 1 };
                let nested = { let value = Pair(i32) { value: 2 }; value.value };
                let branched = if flag {
                    let value = Pair(i32) { value: 3 };
                    value.value
                } else {
                    let value = Pair(i32) { value: 4 };
                    value.value
                };
                let matched = match selector {
                    0 => { let value = Pair(i32) { value: 5 }; value.value },
                    _ => { let value = Pair(i32) { value: 6 }; value.value },
                };
                let called = take(Pair(i32) { value: 7 }.value);
                let Hidden = struct {
                    value: i32,
                    fn hidden() -> i32 {
                        let value = Pair(i32) { value: 8 };
                        value.value
                    }
                };
                direct.value + nested + branched + matched + called
            }
            fn second() -> i64 {
                let value = Pair(i64) { value: 2 };
                value.value
            }
        "#;
        let (rir, interner) = lower_files(&[(source, FileId::DEFAULT)]);
        let body = |source_name: &str| {
            rir.iter()
                .find_map(|(_, inst)| match inst.data {
                    InstData::FnDecl { name, body, .. }
                        if interner.resolve(&name) == source_name =>
                    {
                        Some(body)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing {source_name} body"))
        };

        let first = crate::sema::comptime_eval::inline_ctor_head_candidates(&rir, body("first"));
        let second = crate::sema::comptime_eval::inline_ctor_head_candidates(&rir, body("second"));
        let hidden = crate::sema::comptime_eval::inline_ctor_head_candidates(&rir, body("hidden"));

        assert_eq!(
            first.len(),
            7,
            "nested blocks, branches, match arms, and call arguments are scanned"
        );
        assert_eq!(second.len(), 1);
        assert_eq!(hidden.len(), 1);
        assert!(first.is_sorted_by_key(|candidate| candidate.as_u32()));
        assert!(first.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(
            first
                .iter()
                .all(|candidate| matches!(rir.get(*candidate).data, InstData::Call { .. }))
        );
        assert!(
            first.iter().all(|candidate| !hidden.contains(candidate)),
            "the enclosing body scan stops at the nested anonymous declaration"
        );
        assert!(first.iter().all(|candidate| !second.contains(candidate)));
        assert!(matches!(rir.get(second[0]).data, InstData::Call { .. }));
    }

    #[test]
    fn comptime_type_alias_filter_keeps_every_type_producing_shape() {
        let source = r#"
            fn Make() -> type { struct { value: i32 } }
            fn main(flag: bool, value: i32) -> i32 {
                let Direct = i32;
                let Name = Direct;
                let Call = Make();
                let QualifiedCall = module.Make();
                let QualifiedPath = module.Item;
                let Struct = struct { value: i32 };
                let Enum = enum { Some(i32), None };
                let Wrapped = comptime { i32 };
                let Block = { let Inner = i32; Inner };
                let Branch = if flag { i32 } else { i64 };
                let Match = match value { 0 => i32, _ => i64 };
                let Array = [i32; 2];

                let Integer = 1;
                let Boolean = true;
                let Unit = ();
                let Arithmetic = value + 1;
                let Aggregate = [value, 2];
                0
            }
        "#;
        let (rir, interner) = lower_files(&[(source, FileId::DEFAULT)]);
        let mut candidates = HashMap::new();
        for (_, inst) in rir.iter() {
            if let InstData::Alloc {
                name: Some(name),
                init,
                ..
            } = inst.data
            {
                candidates.insert(
                    interner.resolve(&name).to_owned(),
                    crate::sema::comptime_eval::initializer_may_evaluate_to_type(&rir, init),
                );
            }
        }

        for name in [
            "Direct",
            "Name",
            "Call",
            "QualifiedCall",
            "QualifiedPath",
            "Struct",
            "Enum",
            "Wrapped",
            "Block",
            "Branch",
            "Match",
            "Array",
        ] {
            assert_eq!(candidates.get(name), Some(&true), "must retain {name}");
        }
        for name in ["Integer", "Boolean", "Unit", "Arithmetic", "Aggregate"] {
            assert_eq!(candidates.get(name), Some(&false), "must skip {name}");
        }
    }
}
