//! RUE-1089 producer-nominal anonymous-type identity — programmatic acceptance
//! tests.
//!
//! This module is the home for the acceptance criteria that cannot be expressed
//! as spec/CLI TOML cases, because they need programmatic assertions
//! (warm/fresh/cold parity, symbol-set comparison, and execution of the linked
//! ELF).
//!
//! Companion behavioral cases live in
//! `crates/rue-spec/cases/expressions/producer_nominal_acceptance.toml` and
//! `crates/rue-cli-tests/cases/producer_nominal_targets.toml`. The full
//! criterion → test map is in `docs/notes/rue-1089-acceptance-ledger.md`.
//!
//! Anonymous-type anchors are now consumed directly from the canonical candidate
//! artifact, so the Wrap payload shape compiles without a synthetic-source
//! transport authority.

use crate::*;
use std::collections::BTreeSet;

use std::sync::Arc;

use ahash::{AHashMap, AHashSet};
use rue_target::Target;

// ===========================================================================
// RUE-1112 — per-body well-known `Option(payload)` demand, projection, and AIR
// try-operand consumption.
//
// Every fallible intrinsic (`@read_line`, `@parse_i32/i64/u32/u64`) returns the
// exact trusted standard-library `Option(payload)`. Each registered body derives
// its exact payload set from its canonical body-toolchain-demand node and maps
// every payload directly to one trusted `Option(payload)` comptime query. The
// complete registry is installed atomically before AIR analysis; a failed or
// wrong-result specialization fails closed, and no same-shape local lookalike
// can supply the missing identity.
// ===========================================================================

/// The trusted standard-library `Option` module source, verbatim from
/// `std/option.rue`. Provided at the trusted logical path so each exact per-body
/// query and projection resolves the real std producer identity.
const TRUSTED_OPTION_SOURCE: &str = r#"
pub fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}
"#;

/// The `FileId` the helpers assign to the root module.
const ROOT_FILE: FileId = FileId::new(1);
/// The `FileId` the helpers assign to the trusted `\0rue-std/option.rue` module.
/// Because the test controls this assignment, an enum whose `EnumDef.file_id`
/// equals it was produced by the trusted std `Option`, not a local lookalike —
/// the provenance signal the acceptance criteria demand.
const TRUSTED_OPTION_FILE: FileId = FileId::new(2);

/// Build a snapshot whose root module is `root_source`, with the trusted std
/// `Option` module provided at `\0rue-std/option.rue`. The root reaches it with
/// `@import("std/option.rue")` (physical-path suffix match). The trusted flag is
/// what makes this module the canonical toolchain-owned `Option` declaration.
fn trusted_option_snapshot(root_source: &str) -> SourceSnapshot {
    trusted_option_snapshot_with_source(root_source, TRUSTED_OPTION_SOURCE)
}

fn trusted_option_snapshot_with_source(root_source: &str, option_source: &str) -> SourceSnapshot {
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        ROOT_FILE,
        AHashMap::from([
            (ROOT_FILE, "/project/main.rue".to_owned()),
            (TRUSTED_OPTION_FILE, "/project/std/option.rue".to_owned()),
        ]),
        AHashMap::from([
            (ROOT_FILE, "main.rue".to_owned()),
            (TRUSTED_OPTION_FILE, "\0rue-std/option.rue".to_owned()),
        ]),
        AHashSet::from([TRUSTED_OPTION_FILE]),
    )
    .expect("trusted-std metadata is valid");
    SourceSnapshot::new(
        metadata,
        vec![
            (ROOT_FILE, Arc::new(root_source.to_owned())),
            (TRUSTED_OPTION_FILE, Arc::new(option_source.to_owned())),
        ],
    )
    .expect("trusted-option snapshot is valid")
}

/// Publish `root_source` alongside the trusted std `Option` module and return
/// the rooted CFG output. Import resolution is the test-fixture graph, exactly as
/// the other multi-module frontend tests use.
fn rooted_cfg_with_trusted_option(
    root_source: &str,
    options: &CompileOptions,
) -> Result<RootedCfgOutput, CompileErrors> {
    let snapshot = trusted_option_snapshot(root_source);
    let (_, semantic, _) = crate::test_frontend_snapshot(&snapshot, options)?;
    Ok(semantic)
}

/// Compile and link `root_source` with the trusted std `Option` module present,
/// producing a runnable ELF.
fn compile_with_trusted_option(
    root_source: &str,
    options: &CompileOptions,
) -> Result<CompileOutput, CompileErrors> {
    let snapshot = trusted_option_snapshot(root_source);
    crate::test_compile_snapshot(&snapshot, options)
}

/// The result Option enum types bound to each fallible parse/read intrinsic in
/// a rooted CFG output — the exact type `resolve_option_result_type` chose. Each
/// entry is the intrinsic's runtime kind paired with the `EnumId` of its
/// `Option` result. Reading the AIR instruction's `ty` directly gives the
/// binding the `?` site consumed, not merely what exists in the pool.
fn fallible_intrinsic_option_enums(
    semantic: &RootedCfgOutput,
) -> Vec<(rue_air::RuntimeCallKind, String)> {
    let mut found = Vec::new();
    for function in semantic.functions() {
        for (_, inst) in function.record.air.iter() {
            if let rue_air::AirInstData::Intrinsic { operation, .. } = &inst.data
                && matches!(
                    operation,
                    rue_air::IntrinsicOperation::ParseI32
                        | rue_air::IntrinsicOperation::ParseI64
                        | rue_air::IntrinsicOperation::ParseU32
                        | rue_air::IntrinsicOperation::ParseU64
                        | rue_air::IntrinsicOperation::ReadLine
                )
                && let rue_air::TypeKind::Enum(enum_id) = inst.ty.kind()
            {
                let def = function.record.type_pool.enum_def(enum_id);
                assert_eq!(
                    def.variants.iter().map(Arc::as_ref).collect::<Vec<_>>(),
                    ["Some", "None"],
                    "the intrinsic result must be Option-shaped",
                );
                found.push((operation.runtime_call().unwrap(), def.name.to_string()));
            }
        }
    }
    found
}

/// The stable producer digest (`__anon_enum_<hash>`) of the single `Option`
/// enum bound to a `@parse_i64(...)?` site. Panics unless there is exactly one.
/// Because the digest hashes the producer's LOGICAL module identity plus a
/// definition-relative anchor (RUE-1089, allocation-order-independent), the same
/// producer yields the same digest across programs — so a digest computed from a
/// std-only reference program identifies the trusted std `Option(i64)` anywhere.
fn bound_parse_i64_digest(semantic: &RootedCfgOutput) -> String {
    let parse = fallible_intrinsic_option_enums(semantic)
        .into_iter()
        .filter(|(runtime, _)| *runtime == rue_air::RuntimeCallKind::ParseI64)
        .collect::<Vec<_>>();
    assert_eq!(
        parse.len(),
        1,
        "expected exactly one @parse_i64 binding, got {} ({parse:?})",
        parse.len(),
    );
    parse[0].1.clone()
}

/// The producer digests of every `Some(i64)/None`-shaped anonymous enum in a
/// pool. Distinct producers of the same shape (a std `Option` and a local
/// lookalike) appear as distinct digests, so the size of this set counts how
/// many independent `Option(i64)` identities the program materialized.
fn option_i64_pool_digests(semantic: &RootedCfgOutput) -> BTreeSet<String> {
    semantic
        .type_pools()
        .flat_map(|pool| {
            pool.all_enum_ids().filter_map(move |id| {
                let def = pool.enum_def(id);
                (def.variants.iter().map(Arc::as_ref).eq(["Some", "None"])
                    && def.variant_payload(0) == [rue_air::Type::I64])
                .then(|| def.name.to_string())
            })
        })
        .collect()
}

/// The stable digest that identifies the trusted std `Option(i64)`: computed
/// from a std-only reference program whose sole `Option(i64)` is unambiguously
/// the trusted producer.
fn std_option_i64_reference_digest(options: &CompileOptions) -> String {
    let reference = r#"
const opt = @import("std/option.rue");
fn probe(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match probe("1") { O.Some(v) => @intCast(v), O.None => 0 }
}
"#;
    let semantic =
        rooted_cfg_with_trusted_option(reference, options).expect("std reference program compiles");
    bound_parse_i64_digest(&semantic)
}

/// t-win. With the trusted std `Option` module present, a bare `@parse_i64(s)?`
/// binds the STD `Option(i64)`. Asserted by provenance: the intrinsic's result
/// enum carries the trusted producer's stable digest (distinct from the digest a
/// local `Option` producer yields), and the program links and executes to 42.
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_well_known_parse_binds_trusted_std_option() {
    let options = CompileOptions::default();
    let source = r#"
const opt = @import("std/option.rue");
fn read_num(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match read_num("42") {
        O.Some(v) => @intCast(v),
        O.None => 0,
    }
}
"#;
    let semantic = rooted_cfg_with_trusted_option(source, &options)
        .expect("trusted-option parse program compiles");
    let bound = bound_parse_i64_digest(&semantic);
    assert_eq!(
        bound,
        std_option_i64_reference_digest(&options),
        "the ?-bound Option(i64) must carry the trusted std producer's digest",
    );

    // The digest is provenance-sensitive rather than shape-only: RUE-1112's
    // `well_known_registry_wins_over_local_lookalike` proves the std producer is
    // bound even when a same-shape local `Option(i64)` lookalike is materialized
    // in the very same body. A same-shape local lookalike is not a valid `?`
    // operand — only the trusted std producer is — so that control lives in the
    // registry-wins test above.

    // End to end: the trusted-option program links and runs to the payload 42.
    #[cfg(unix)]
    {
        let output = compile_with_trusted_option(source, &options).expect("t-win links");
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            let execution = execute_wrap(&output, "twin");
            assert_eq!(
                execution.status.code(),
                Some(42),
                "t-win must execute to 42: {execution:?}",
            );
        } else {
            let _ = output;
        }
    }
}

/// t-win (registry wins over a materialized local lookalike). The requesting
/// body materializes a LOCAL same-shape `Option(i64)` in its own universe — the
/// exact enum a shape-based lookup could pick — yet `@parse_i64(s)?` still binds
/// the TRUSTED std `Option(i64)`. The narrow well-known registry is authoritative,
/// so provenance is the trusted producer even though a compatible local enum is
/// in scope.
#[test]
fn well_known_registry_wins_over_local_lookalike() {
    let options = CompileOptions::default();
    let source = r#"
const opt = @import("std/option.rue");
fn Local(comptime T: type) -> type { enum { Some(T), None } }
fn read_num(s: str) -> opt.Option(i64) {
    // A local Option(i64) lookalike, materialized in THIS body's universe. It
    // remains independently observable but cannot supply the intrinsic result.
    let L = Local(i64);
    let _decoy: L = L.None;
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match read_num("42") {
        O.Some(v) => @intCast(v),
        O.None => 0,
    }
}
"#;
    let semantic =
        rooted_cfg_with_trusted_option(source, &options).expect("lookalike program compiles");

    // Both a std and a local Option(i64) identity were materialized.
    let pool_digests = option_i64_pool_digests(&semantic);
    let std_digest = std_option_i64_reference_digest(&options);
    assert!(
        pool_digests.contains(&std_digest),
        "the std Option(i64) must be present in the pool: {pool_digests:?}",
    );
    assert!(
        pool_digests.iter().any(|d| *d != std_digest),
        "a distinct local Option(i64) lookalike must also be present: {pool_digests:?}",
    );

    // The `?` site bound the trusted std producer, NOT the local lookalike.
    assert_eq!(
        bound_parse_i64_digest(&semantic),
        std_digest,
        "the registry must win: `?` binds the trusted std Option, not the local lookalike",
    );
}

#[test]
fn malformed_trusted_option_surfaces_diagnostics_instead_of_using_a_lookalike() {
    let source = r#"
const opt = @import("std/option.rue");
fn LocalOption(comptime T: type) -> type { enum { Some(T), None } }
fn main() -> i32 {
    let L = LocalOption(i32);
    let _lookalike: L = L.None;
    let _result = @parse_i32("42");
    0
}
"#;
    let snapshot = trusted_option_snapshot_with_source(
        source,
        "pub fn Option(comptime T: type) -> type { missing }",
    );
    let errors = crate::test_frontend_snapshot(&snapshot, &CompileOptions::default())
        .expect_err("a committed malformed trusted specialization must fail the semantic request");
    let rendered = errors.to_string();
    assert!(
        rendered.contains("missing"),
        "the trusted Option semantic failure must surface its diagnostics: {rendered}"
    );
}

/// Freestanding `?` legality by producer identity (RUE-1112). There is no
/// structural-shape fallback: `?` legality is exact trusted-producer identity in
/// every context — even freestanding.
///
/// 1. A freestanding program whose `?` operand is a LOCAL (non-std) `Option`
///    lookalike is rejected (E0504, `QuestionOnNonOption`): the local producer
///    is not the trusted std `Option`, and no fallback resolves it anymore.
/// 2. A program with the trusted std `Option` acquired resolves `?` on a bare
///    `@parse_i64` against that std producer and compiles — the std-acquired
///    path is the only one that works now.
#[test]
fn freestanding_local_option_try_is_now_rejected_without_std() {
    let options = CompileOptions::default();

    // (1) Freestanding local-Option `?` — no trusted std, no fallible intrinsic.
    // The operand of `?` is the local `Option(i64)` lookalike; its producer is
    // the user's own `Option` function, not the trusted std one, so `?` is
    // rejected: no structural-shape fallback resolves a lookalike.
    let local_lookalike = r#"
fn Option(comptime T: type) -> type { enum { Some(T), None } }
fn make() -> Option(i64) {
    let O = Option(i64);
    O.Some(1)
}
fn use_it() -> Option(i64) {
    let v = make()?;
    let O = Option(i64);
    O.Some(v)
}
fn main() -> i32 {
    let O = Option(i64);
    match use_it() {
        O.Some(v) => @intCast(v),
        O.None => 0,
    }
}
"#;
    let errors = fresh_rooted_cfg(local_lookalike, &options)
        .expect_err("a local-Option `?` operand is no longer accepted (fallback deleted)");
    let rendered = errors.to_string();
    assert!(
        rendered.contains("the `?` operator can only be applied to an `Option`"),
        "expected E0504 QuestionOnNonOption for a lookalike `?` operand: {rendered}",
    );

    // (2) With the trusted std `Option` acquired, `?` on a bare `@parse_i64`
    // binds the std producer and the program compiles. This is the freestanding
    // fallible-intrinsic case's real, std-acquired resolution path.
    let std_acquired = r#"
const opt = @import("std/option.rue");
fn read_num(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match read_num("42") {
        O.Some(v) => @intCast(v),
        O.None => 0,
    }
}
"#;
    let semantic = rooted_cfg_with_trusted_option(std_acquired, &options)
        .expect("std-acquired `@parse_i64(s)?` compiles");
    // The `?` bound the trusted std Option(i64).
    assert_eq!(
        bound_parse_i64_digest(&semantic),
        std_option_i64_reference_digest(&options),
        "std-acquired `?` binds the trusted std Option",
    );
}

/// StrBuf-payload encoding (RUE-1112). `@read_line`'s `Option(StrBuf)` carries a
/// NOMINAL payload, unlike the scalar `@parse_*` intrinsics. The body's lexical
/// scan names that payload independent of module presence, and the exact-key
/// helper spells the trusted `StrBuf` stable key directly. Missing modules are
/// parked before the body transaction rather than filtering this demand.
#[test]
fn read_line_has_an_exact_strbuf_nominal_option_key() {
    let body = r#"{
    let _ = @read_line()?;
    let _ = @parse_i64(s)?;
}
"#;
    let kinds = crate::well_known_option::scan_body_payload_kinds(body);
    assert_eq!(
        kinds,
        BTreeSet::from([
            crate::well_known_option::FalliblePayload::I64,
            crate::well_known_option::FalliblePayload::StrBuf,
        ])
    );
    let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
        target: Target::X86_64Linux,
        preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
    };
    let (strbuf_payload, strbuf_call) = crate::well_known_option::exact_option_query(
        crate::well_known_option::FalliblePayload::StrBuf,
        &configuration,
    );
    let strbuf_nominal = crate::durable_semantics::DurableType::Nominal(
        crate::StableDefinitionKey::from_stable_parts(
            crate::ModuleId::from_trusted_standard_library_path(crate::STRBUF_MODULE_LOGICAL_PATH)
                .unwrap(),
            crate::StableDefinitionNamespace::Type,
            crate::StableDefinitionKind::Struct,
            "StrBuf",
            None,
        ),
    );
    assert_eq!(strbuf_payload, strbuf_nominal);
    assert_eq!(
        strbuf_call.type_arguments.as_ref(),
        &[(Arc::<str>::from("T"), strbuf_nominal)]
    );
}

/// t1. The canonical body scan is exact and presence-independent. A quiet body
/// has no payload; a fallible body always names its payload so the session can
/// park missing trusted modules before entering the transaction.
#[test]
fn no_fallible_intrinsic_has_zero_exact_body_demands() {
    assert!(crate::well_known_option::scan_body_payload_kinds("{ let x = 1; x }").is_empty());
    assert_eq!(
        crate::well_known_option::scan_body_payload_kinds("{ let value = @parse_i64(s)?; value }"),
        BTreeSet::from([crate::well_known_option::FalliblePayload::I64]),
        "module presence cannot filter a reached body's exact payload demand",
    );
}

/// t2. Two distinct bodies each demanding the SAME payload (`i64`) share ONE
/// materialized `Option(i64)` specialization: the nucleus memoizes the
/// `ComptimeCall` rooted under each body's lease, so both `?` sites bind the
/// identical `EnumId` and the pool holds exactly one std `Option(i64)` identity.
/// (The nucleus's Computed-then-Reused execution is internal to the body-request
/// loop; the shared identity is its observable, request-stable consequence.)
#[test]
fn two_bodies_same_payload_share_one_specialization() {
    let options = CompileOptions::default();
    let source = r#"
const opt = @import("std/option.rue");
fn first(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn second(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match first("1") { O.Some(_a) => match second("2") { O.Some(_b) => 0, O.None => 1 }, O.None => 2 }
}
"#;
    let semantic =
        rooted_cfg_with_trusted_option(source, &options).expect("two-body program compiles");
    let parse_enums = fallible_intrinsic_option_enums(&semantic)
        .into_iter()
        .filter(|(runtime, _)| *runtime == rue_air::RuntimeCallKind::ParseI64)
        .map(|(_, id)| id)
        .collect::<Vec<_>>();
    assert_eq!(
        parse_enums.len(),
        2,
        "both bodies must contribute a @parse_i64 binding, got {parse_enums:?}",
    );
    assert_eq!(
        parse_enums[0], parse_enums[1],
        "both bodies must bind the one shared std Option(i64) specialization",
    );
    let std_digest = std_option_i64_reference_digest(&options);
    let pool = option_i64_pool_digests(&semantic);
    assert_eq!(
        pool,
        BTreeSet::from([std_digest]),
        "exactly one std Option(i64) identity must exist across both bodies: {pool:?}",
    );
}

/// Publish `root_source` (trusted std `Option` present) into `session` through
/// the discovery protocol, and return the rooted CFG output.
fn publish_trusted_rooted_cfg(
    session: &mut CompilerSession,
    root_source: &str,
    options: &CompileOptions,
) -> Result<RootedCfgOutput, CompileErrors> {
    let snapshot = trusted_option_snapshot(root_source);
    crate::test_support::publish_test_snapshot(session, &snapshot)?;
    session.rooted_cfg(options)
}

/// Warm/fresh parity on t-win. A WARM incremental compile (reached after the
/// session already compiled an unrelated prior revision that also carried the
/// trusted std module) produces rooted CFG output identical to a FRESH compile:
/// the per-body demand, projection, and narrow install are deterministic and
/// session-issuer-independent, so the well-known `Option(i64)` binding and every
/// symbol agree exactly.
#[test]
fn well_known_parse_warm_and_fresh_agree() {
    let options = CompileOptions::default();
    let target = r#"
const opt = @import("std/option.rue");
fn read_num(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match read_num("42") { O.Some(v) => @intCast(v), O.None => 0 }
}
"#;

    let mut warm_session = CompilerSession::new();
    // An unrelated prior revision (still trusted-std-bearing) warms the session.
    publish_trusted_rooted_cfg(
        &mut warm_session,
        r#"
const opt = @import("std/option.rue");
fn main() -> i32 { 0 }
"#,
        &options,
    )
    .ok();
    let warm = publish_trusted_rooted_cfg(&mut warm_session, target, &options)
        .expect("warm compile of t-win");

    let fresh = rooted_cfg_with_trusted_option(target, &options).expect("fresh compile of t-win");

    crate::test_support::assert_rooted_cfg_value_parity("warm/fresh t-win", &warm, &fresh);
    assert_eq!(
        bound_parse_i64_digest(&warm),
        bound_parse_i64_digest(&fresh),
        "warm/fresh disagreed on the well-known Option(i64) binding",
    );
}

/// The per-site `Option(i64)` `EnumId` bound at every `@parse_i64` instruction in
/// a rooted CFG output, in AIR order. Reading `inst.ty` gives the identity each
/// site actually consumed, so mixed `?`-operand and plain uses can be compared
/// directly for shared identity.
fn parse_i64_bound_enums(semantic: &RootedCfgOutput) -> Vec<String> {
    fallible_intrinsic_option_enums(semantic)
        .into_iter()
        .filter(|(runtime, _)| *runtime == rue_air::RuntimeCallKind::ParseI64)
        .map(|(_, id)| id)
        .collect()
}

/// B1(a). A plain, unannotated `let _ = @parse_i64("1")` — no `?`, no type
/// annotation, no surrounding `match` — compiles, and the intrinsic result type
/// IS the trusted std `Option(i64)`. The fallible intrinsic OWNS its exact
/// trusted `Option` identity in every context: context does not select the
/// nominal, the per-body well-known registry does. Provenance is asserted by the
/// bound producer's stable digest matching the std-only reference program's.
#[test]
fn plain_unannotated_parse_owns_trusted_std_option() {
    let options = CompileOptions::default();
    let source = r#"
const opt = @import("std/option.rue");
fn main() -> i32 {
    let _ = @parse_i64("1");
    0
}
"#;
    let semantic = rooted_cfg_with_trusted_option(source, &options)
        .expect("a plain unannotated @parse_i64 compiles");
    assert_eq!(
        bound_parse_i64_digest(&semantic),
        std_option_i64_reference_digest(&options),
        "a bare `let _ = @parse_i64(..)` result carries the trusted std producer's digest",
    );
}

/// B1(b), one body. A single body mixing a plain `@parse_i64` use and a
/// `@parse_i64(..)?` operand of the SAME payload compiles with no fail-closed
/// E9000, both sites bind the identical `EnumId`, and the whole-program pool
/// holds exactly ONE `Option(i64)` identity (the trusted std producer). The
/// per-body registry memoizes the one specialization; a plain use and a `?`
/// operand cannot fork it into two identities.
#[test]
fn mixed_plain_and_try_in_one_body_share_one_identity() {
    let options = CompileOptions::default();
    let source = r#"
const opt = @import("std/option.rue");
fn read_num(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    let _plain = @parse_i64(s);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match read_num("42") { O.Some(v) => @intCast(v), O.None => 0 }
}
"#;
    let semantic = rooted_cfg_with_trusted_option(source, &options)
        .expect("mixed plain + `?` in one body compiles with no E9000");
    let bound = parse_i64_bound_enums(&semantic);
    assert_eq!(
        bound.len(),
        2,
        "both the plain and the `?` @parse_i64 site must bind, got {bound:?}",
    );
    assert!(
        bound.iter().all(|id| *id == bound[0]),
        "the plain and `?` sites must bind ONE shared Option(i64) EnumId: {bound:?}",
    );
    assert_eq!(
        option_i64_pool_digests(&semantic),
        BTreeSet::from([std_option_i64_reference_digest(&options)]),
        "exactly one std Option(i64) identity must exist for the mixed body",
    );
}

/// B1(b), across two bodies. A plain `@parse_i64` in one body and a
/// `@parse_i64(..)?` operand in another (same payload) compile with no E9000,
/// every site binds the identical `EnumId`, and the pool holds exactly ONE
/// `Option(i64)` identity shared across both bodies.
#[test]
fn mixed_plain_and_try_across_two_bodies_share_one_identity() {
    let options = CompileOptions::default();
    let source = r#"
const opt = @import("std/option.rue");
fn plainly(s: str) -> i32 {
    let _ = @parse_i64(s);
    0
}
fn fallibly(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match fallibly("42") { O.Some(v) => @intCast(v) + plainly("1"), O.None => 0 }
}
"#;
    let semantic = rooted_cfg_with_trusted_option(source, &options)
        .expect("mixed plain + `?` across two bodies compiles with no E9000");
    let bound = parse_i64_bound_enums(&semantic);
    assert_eq!(
        bound.len(),
        2,
        "the plain body and the `?` body must each bind a @parse_i64 site: {bound:?}",
    );
    assert!(
        bound.iter().all(|id| *id == bound[0]),
        "sites in distinct bodies must bind ONE shared Option(i64) EnumId: {bound:?}",
    );
    assert_eq!(
        option_i64_pool_digests(&semantic),
        BTreeSet::from([std_option_i64_reference_digest(&options)]),
        "exactly one std Option(i64) identity must exist across the two bodies",
    );
}

/// B1(c). Warm/fresh parity on the mixed (plain + `?`) program: a warm
/// incremental compile and a fresh compile agree on the full rooted CFG parity
/// snapshot and on the shared well-known `Option(i64)` binding. The per-body
/// demand, projection, and narrow install are deterministic and
/// session-issuer-independent even when a body mixes plain and `?` uses.
#[test]
fn mixed_plain_and_try_warm_and_fresh_agree() {
    let options = CompileOptions::default();
    let target = r#"
const opt = @import("std/option.rue");
fn read_num(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    let _plain = @parse_i64(s);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let O = opt.Option(i64);
    match read_num("42") { O.Some(v) => @intCast(v), O.None => 0 }
}
"#;

    let mut warm_session = CompilerSession::new();
    publish_trusted_rooted_cfg(
        &mut warm_session,
        r#"
const opt = @import("std/option.rue");
fn main() -> i32 { 0 }
"#,
        &options,
    )
    .ok();
    let warm = publish_trusted_rooted_cfg(&mut warm_session, target, &options)
        .expect("warm compile of the mixed program");
    let fresh = rooted_cfg_with_trusted_option(target, &options)
        .expect("fresh compile of the mixed program");

    crate::test_support::assert_rooted_cfg_value_parity("warm/fresh mixed program", &warm, &fresh);
    let warm_bound = parse_i64_bound_enums(&warm);
    let fresh_bound = parse_i64_bound_enums(&fresh);
    assert!(
        warm_bound.iter().all(|id| *id == warm_bound[0])
            && fresh_bound.iter().all(|id| *id == fresh_bound[0]),
        "each of warm and fresh must bind one shared identity: {warm_bound:?} / {fresh_bound:?}",
    );
    assert_eq!(
        option_i64_pool_digests(&warm),
        option_i64_pool_digests(&fresh),
        "warm/fresh disagreed on the Option(i64) identity set for the mixed program",
    );
}

/// The canonical RUE-1089 Wrap repro: a GENERIC struct producer whose method
/// reaches an anonymous-enum MEMBER (`self.inner`, of type `Option(T)`) under
/// the contextual (generic) anchor. This was the sole shape that hit the
/// fail-closed E9000 frontier before the anchor-transport fix. It now compiles
/// and exits 42, with the receiver field type, the match enum key, the payload
/// operation, and the enum layout all referring to ONE nominal identity.
const WRAP_REPRO: &str = r#"
fn Option(comptime T: type) -> type { enum { Some(T), None } }
fn Wrap(comptime T: type) -> type {
    struct {
        inner: Option(T),
        fn get_or(self, d: T) -> T {
            let O = Option(T);
            match self.inner { O.Some(v) => v, O.None => d }
        }
    }
}
fn main() -> i32 {
    let W = Wrap(i32);
    let O = Option(i32);
    let w: W = W { inner: O.Some(42) };
    w.get_or(0)
}
"#;

/// A methodful producer that mints several distinct anonymous types in
/// different positions. Used for the determinism / warm-fresh / identity
/// stability criteria (4 and 3).
const MULTI_ANON_PRODUCER: &str = r#"
fn Holder() -> type {
    struct {
        v: i32,
        fn first(self) -> i32 {
            let A = struct { x: i32 };
            let a: A = A { x: self.v };
            a.x
        }
        fn second(self) -> i32 {
            let B = struct { y: i32 };
            let b: B = B { y: self.v };
            b.y * 2
        }
    }
}
fn main() -> i32 {
    let H = Holder();
    let h: H = H { v: 14 };
    h.first() + h.second()
}
"#;

/// The same program as [`MULTI_ANON_PRODUCER`] with the sibling methods
/// REORDERED and an unrelated top-level declaration added. Producer-nominal
/// identity must be unchanged by these edits.
const MULTI_ANON_PRODUCER_REORDERED: &str = r#"
fn unrelated() -> i32 { 99 }
fn Holder() -> type {
    struct {
        v: i32,
        fn second(self) -> i32 {
            let B = struct { y: i32 };
            let b: B = B { y: self.v };
            b.y * 2
        }
        fn first(self) -> i32 {
            let A = struct { x: i32 };
            let a: A = A { x: self.v };
            a.x
        }
    }
}
fn main() -> i32 {
    let H = Holder();
    let h: H = H { v: 14 };
    h.first() + h.second()
}
"#;

/// Compile a single (import-free) source through a FRESH session and return its
/// canonical rooted CFG output (or the collected errors).
fn fresh_rooted_cfg(
    source: &str,
    options: &CompileOptions,
) -> Result<RootedCfgOutput, CompileErrors> {
    let snapshot = SourceSnapshot::single("<acceptance>", source).map_err(CompileErrors::from)?;
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result()?;
    session.rooted_cfg(options)
}

/// Compile a single source WARM: publish an unrelated prior revision, compile
/// it, then publish and compile the target source in the same session. This
/// exercises the incremental path against the same session state a fresh
/// compile never sees.
fn warm_rooted_cfg(
    prior: &str,
    source: &str,
    options: &CompileOptions,
) -> Result<RootedCfgOutput, CompileErrors> {
    let mut session = CompilerSession::new();
    let prior_snapshot =
        SourceSnapshot::single("<acceptance>", prior).map_err(CompileErrors::from)?;
    session.update(&prior_snapshot).into_result()?;
    session.rooted_cfg(options).ok(); // prior revision is not oracled.
    let snapshot = SourceSnapshot::single("<acceptance>", source).map_err(CompileErrors::from)?;
    session.update(&snapshot).into_result()?;
    session.rooted_cfg(options)
}

#[test]
fn friendly_anonymous_method_diagnostic_is_warm_fresh_parity_safe() {
    let source = r#"
linear struct Token { v: i32 }
fn Wrap(comptime T: type) -> type { enum { Some(T), None } }
fn use(value: Wrap(Token)) -> i32 { value.missing(); 0 }
fn main() -> i32 {
    use(Wrap(Token).Some(Token { v: 0 }));
    0
}
"#;
    let prior = "fn main() -> i32 { 0 }";
    let options = CompileOptions::default();
    let fresh = fresh_rooted_cfg(source, &options)
        .expect_err("the focused method diagnostic must fail in a fresh session")
        .to_string();
    let warm = warm_rooted_cfg(prior, source, &options)
        .expect_err("the focused method diagnostic must fail in a warm session")
        .to_string();
    assert_eq!(fresh, warm, "warm and fresh diagnostics diverged");
    assert!(
        fresh.contains("no method named 'missing'"),
        "expected method diagnostic: {fresh}"
    );
    assert!(
        fresh.contains("Wrap(Token)"),
        "lost constructor display: {fresh}"
    );
    assert!(
        !fresh.contains("__anon_"),
        "raw anonymous name leaked: {fresh}"
    );
}

/// The emitted symbol names of a rooted CFG output: every struct/enum symbol and
/// every function machine name. Two independent cold compiles of one program
/// must produce identical sets.
fn symbol_names(semantic: &RootedCfgOutput) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for pool in semantic.type_pools() {
        for id in pool.all_struct_ids() {
            names.insert(format!("struct:{}", pool.struct_symbol_name(id)));
        }
        for id in pool.all_enum_ids() {
            names.insert(format!("enum:{}", pool.enum_symbol_name(id)));
        }
    }
    for function in semantic.functions() {
        names.insert(format!("fn:{}", function.record.codegen.defined_symbol));
    }
    let plan = crate::semantic_identity::AnonymousSymbolPlan::for_reached_set(
        semantic
            .anonymous_nominals()
            .iter()
            .map(|nominal| &nominal.identity),
    );
    for nominal in semantic.anonymous_nominals() {
        let symbol =
            crate::semantic_identity::anonymous_nominal_source_symbol_in(&plan, &nominal.identity);
        let kind = match &nominal.shape {
            crate::durable_semantics::DurableAnonymousNominalShape::Struct { .. } => "struct",
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { .. } => "enum",
        };
        names.insert(format!("{kind}:{symbol}"));
    }
    names
}

/// The named (non-anonymous) type/function symbols of a rooted CFG output. These
/// are invariant under reordering and unrelated edits. Anonymous synthetic
/// symbols are asserted separately via [`anonymous_symbols`], which since the
/// Stage-A stable-naming cut (ADR-0066, RUE-1089) are also invariant under those
/// edits — their disambiguating suffix is a digest of the producer identity, not
/// an allocation-order counter.
fn named_symbols(semantic: &RootedCfgOutput) -> BTreeSet<String> {
    symbol_names(semantic)
        .into_iter()
        .filter(|name| !name.contains("__anon_"))
        .filter(|name| !name.contains("unrelated")) // the intentionally-added extra decl
        .collect()
}

/// The anonymous synthetic type symbols of a rooted CFG output — the struct/enum
/// symbols whose spelling carries the `__anon_struct_`/`__anon_enum_` prefix.
/// Since the Stage-A cut (ADR-0066, RUE-1089) each spelling is a STABLE digest
/// of the producer identity, so this set is identical across independent cold
/// compiles and across warm/fresh, and unchanged by unrelated edits.
fn anonymous_symbols(semantic: &RootedCfgOutput) -> BTreeSet<String> {
    symbol_names(semantic)
        .into_iter()
        .filter(|name| name.contains("__anon_struct_") || name.contains("__anon_enum_"))
        .collect()
}

/// Count the anonymous struct/enum types minted into the type pool.
fn anonymous_type_count(semantic: &RootedCfgOutput) -> usize {
    anonymous_symbols(semantic).len()
}

// ---------------------------------------------------------------------------
// Criterion 3 — identity stability under unrelated edits
// ---------------------------------------------------------------------------

/// Reordering sibling methods and adding an unrelated declaration does not
/// change which anonymous identities exist, nor the named symbol surface.
#[test]
fn producer_nominal_identity_is_stable_under_unrelated_edits() {
    let options = CompileOptions::default();
    let baseline = fresh_rooted_cfg(MULTI_ANON_PRODUCER, &options)
        .expect("baseline multi-anon producer compiles");
    let reordered = fresh_rooted_cfg(MULTI_ANON_PRODUCER_REORDERED, &options)
        .expect("reordered multi-anon producer compiles");

    // The same set of anonymous identities exists in both orderings.
    assert_eq!(
        anonymous_type_count(&baseline),
        anonymous_type_count(&reordered),
        "reordering methods / adding an unrelated decl changed the anonymous identity count",
    );
    assert!(
        anonymous_type_count(&baseline) >= 2,
        "the Holder producer should mint at least the two written anonymous struct identities \
         (found {})",
        anonymous_type_count(&baseline),
    );

    // The named symbol surface (Holder's methods, main, drop glue) is unchanged.
    assert_eq!(
        named_symbols(&baseline),
        named_symbols(&reordered),
        "reordering methods / adding an unrelated decl changed the named symbol surface",
    );

    // Stage A (RUE-1089): the ANONYMOUS symbol surface is likewise unchanged.
    // Each anonymous symbol's suffix is a digest of its producer identity
    // (method name + definition-relative anchor), both preserved when sibling
    // methods are reordered and an unrelated top-level decl is added, so the
    // allocation-order-independent spellings match exactly.
    let baseline_anon = anonymous_symbols(&baseline);
    assert!(
        !baseline_anon.is_empty(),
        "the Holder producer must emit at least one anonymous symbol to assert stability over",
    );
    assert_eq!(
        baseline_anon,
        anonymous_symbols(&reordered),
        "reordering methods / adding an unrelated decl changed the anonymous symbol spellings \
         (Stage-A stable naming must make them allocation-order-independent)",
    );
}

// ---------------------------------------------------------------------------
// Criterion 4 — warm / fresh / cold parity: identical semantic bodies,
// layouts, and symbols
// ---------------------------------------------------------------------------

/// Two independent COLD compiles of the same program produce byte-identical
/// rooted CFG output (bodies, layouts, type pool, dependencies) and an identical
/// emitted symbol set. This is the determinism half of the parity oracle,
/// asserted through the same canonical rooted CFG comparisons the scaling
/// harness uses.
#[test]
fn producer_nominal_rooted_cfg_is_deterministic_across_cold_compiles() {
    let options = CompileOptions::default();
    let first = fresh_rooted_cfg(MULTI_ANON_PRODUCER, &options).expect("first cold compile");
    let second = fresh_rooted_cfg(MULTI_ANON_PRODUCER, &options).expect("second cold compile");

    crate::test_support::assert_rooted_cfg_value_parity(
        "two cold compiles of the same program",
        &first,
        &second,
    );
    assert_eq!(
        symbol_names(&first),
        symbol_names(&second),
        "two cold compiles of the same program emitted different symbol names",
    );

    // Stage A (RUE-1089): assert the ANONYMOUS symbols specifically, not merely
    // that the full set matches. Their spellings are stable digests of the
    // producer identity, so two independent cold compiles must agree on every
    // `__anon_struct_`/`__anon_enum_` symbol exactly — the property that made
    // the prior allocation-order counter unsound for incremental linking and
    // parallel compilation.
    let first_anon = anonymous_symbols(&first);
    assert!(
        !first_anon.is_empty(),
        "the acceptance producer must emit anonymous symbols to assert determinism over",
    );
    assert_eq!(
        first_anon,
        anonymous_symbols(&second),
        "two cold compiles emitted different anonymous symbol spellings",
    );
}

/// Distinct producers minting same-shape anonymous types receive DISTINCT stable
/// anonymous symbols; the same producer key receives the same symbol. This is
/// the identity half of the Stage-A naming property (a digest that both
/// disambiguates producers and is allocation-order-independent).
#[test]
fn distinct_producers_receive_distinct_stable_anonymous_symbols() {
    // Two separate producers `L` and `R` each mint an anonymous struct of the
    // SAME shape (`{ x: i32 }`). Producer-nominal identity makes them distinct
    // types, so their stable symbols must differ.
    let source = r#"
fn L() -> type { struct { x: i32 } }
fn R() -> type { struct { x: i32 } }
fn main() -> i32 {
    let TL = L();
    let TR = R();
    let a: TL = TL { x: 40 };
    let b: TR = TR { x: 2 };
    a.x + b.x
}
"#;
    let options = CompileOptions::default();
    let first = fresh_rooted_cfg(source, &options).expect("distinct-producer program compiles");
    let anon = anonymous_symbols(&first);
    assert_eq!(
        anon.len(),
        2,
        "two distinct same-shape producers must yield two distinct anonymous symbols, got {anon:?}",
    );

    // The same program compiled again yields the SAME two symbols (stability),
    // and they are the same set (determinism) — never a re-numbered pair.
    let second = fresh_rooted_cfg(source, &options).expect("second compile");
    assert_eq!(
        anon,
        anonymous_symbols(&second),
        "distinct-producer anonymous symbols were not stable across cold compiles",
    );
}

/// Compile one single-file program whose only file is assigned an explicit,
/// caller-chosen request-local `FileId` while keeping the same LOGICAL module
/// identity (same path). This lets a test present the same logical program under
/// different `FileId` assignments.
fn rooted_cfg_at_root_file_id(
    source: &str,
    file_id: FileId,
    logical_path: &str,
    options: &CompileOptions,
) -> RootedCfgOutput {
    let physical: AHashMap<FileId, String> = [(file_id, logical_path.to_owned())].into();
    let logical = physical.clone();
    let metadata =
        SourceMetadata::new(file_id, physical, logical).expect("single-file metadata is valid");
    let snapshot = SourceSnapshot::new(metadata, vec![(file_id, Arc::new(source.to_owned()))])
        .expect("single-file snapshot is valid");
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().expect("publish");
    session
        .rooted_cfg(options)
        .expect("permuted-FileId program compiles")
}

/// Theme 4 (RUE-1089, ADR-0066). The stable anonymous-symbol digest hashes the
/// LOGICAL module identity (its canonical path) rather than the request-local
/// numeric `FileId`. Presenting the same logical program under different `FileId`
/// assignments (as differing input orders or added/removed unrelated files do
/// across sessions) must therefore emit byte-identical anonymous symbols.
///
/// Before the fix the digest relocated tokens through the numeric `FileId`
/// endpoint, so the two presentations below (file at `FileId(0)` vs `FileId(7)`,
/// same path) would have derived different `__anon_*` spellings — the exact
/// cross-session instability the reviewer flagged.
#[test]
fn anonymous_symbols_are_stable_across_permuted_file_ids() {
    let source = r#"
fn Producer() -> type { struct { x: i32 } }
fn Wrapper() -> type { enum { Some(i32), None } }
fn main() -> i32 {
    let T = Producer();
    let t: T = T { x: 42 };
    t.x
}
"#;
    let options = CompileOptions::default();
    let at_zero = rooted_cfg_at_root_file_id(source, FileId::new(0), "prog.rue", &options);
    let at_seven = rooted_cfg_at_root_file_id(source, FileId::new(7), "prog.rue", &options);

    let anon = anonymous_symbols(&at_zero);
    assert!(
        !anon.is_empty(),
        "the permuted-FileId program must mint anonymous symbols to compare",
    );
    assert_eq!(
        anon,
        anonymous_symbols(&at_seven),
        "anonymous symbols must be identical across permuted FileId assignments — the digest \
         hashes the logical module identity, not the numeric FileId",
    );
}

/// A WARM (incremental) compile of the acceptance program — reached after the
/// session already compiled an unrelated prior revision — produces semantic
/// output identical to a FRESH compile of the same program. This reuses the
/// scaling harness's canonical rooted CFG comparisons test-side.
#[test]
fn producer_nominal_warm_and_fresh_rooted_cfg_output_agree() {
    let options = CompileOptions::default();
    let warm = warm_rooted_cfg("fn main() -> i32 { 0 }", MULTI_ANON_PRODUCER, &options)
        .expect("warm compile of multi-anon producer");
    let fresh = fresh_rooted_cfg(MULTI_ANON_PRODUCER, &options).expect("fresh compile");

    crate::test_support::assert_rooted_cfg_value_parity(
        "warm/fresh acceptance producer",
        &warm,
        &fresh,
    );
    assert_eq!(
        symbol_names(&warm),
        symbol_names(&fresh),
        "warm/fresh emitted symbol names diverged for the acceptance producer",
    );

    // Stage A (RUE-1089): the warm incremental session and the fresh session
    // assign different session-local token issuers, so an allocation-order name
    // could diverge here. The stable digest resolves each token to its
    // request-independent endpoint content first, so every anonymous symbol
    // agrees exactly.
    let warm_anon = anonymous_symbols(&warm);
    assert!(
        !warm_anon.is_empty(),
        "the acceptance producer must emit anonymous symbols to compare warm vs fresh",
    );
    assert_eq!(
        warm_anon,
        anonymous_symbols(&fresh),
        "warm/fresh emitted different anonymous symbol spellings",
    );
}

// ---------------------------------------------------------------------------
// Criterion 5 — the Wrap repro's single-nominal-identity check
// (currently fail-closed E9000)
// ---------------------------------------------------------------------------

/// Execute a linked Rue program and return its process output. Mirrors
/// `pipeline_tests::execute_compiled_output`.
#[cfg(unix)]
fn execute_wrap(output: &CompileOutput, label: &str) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rue-producer-nominal-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::write(&path, &output.elf).expect("write linked Rue executable");
    let mut permissions = std::fs::metadata(&path)
        .expect("read executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("make executable runnable");
    let result = std::process::Command::new(&path).output();
    std::fs::remove_file(&path).expect("remove executable after execution");
    result.expect("execute linked Rue program")
}

/// Count the anonymous ENUM identities minted into the pool.
fn anonymous_enum_count(semantic: &RootedCfgOutput) -> usize {
    semantic
        .anonymous_nominals()
        .iter()
        .filter(|nominal| {
            matches!(
                &nominal.shape,
                crate::durable_semantics::DurableAnonymousNominalShape::Enum { .. }
            )
        })
        .count()
}

/// FLIPPED-POST-ANCHOR-FIX (RUE-1089). The generic `Wrap` whose `get_or` method
/// matches its anonymous-enum field `Option(T)` now compiles and executes to the
/// payload value 42. astgen and the durable fragment evaluator agree on the
/// anonymous-type anchor: the receiver field type, the `Option(T)` inside the
/// match, the match enum key, the payload operation, and the enum layout all
/// resolve to ONE `Option$…` nominal identity — observable as exactly one
/// anonymous enum in the type pool.
#[cfg(unix)]
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_wrap_single_nominal_identity_executes_to_the_payload() {
    let options = CompileOptions::default();
    let semantic = fresh_rooted_cfg(WRAP_REPRO, &options).expect("Wrap repro compiles");

    // A single anonymous Option identity backs every reach of `self.inner`.
    assert_eq!(
        anonymous_enum_count(&semantic),
        1,
        "the Wrap repro must resolve to exactly one anonymous Option enum identity",
    );

    let snapshot = SourceSnapshot::single("<wrap>", WRAP_REPRO).expect("snapshot");
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().expect("publish");
    let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
        .expect("Wrap repro links");
    // The default target is `x86-64-linux`; only run the linked ELF when the
    // host triple matches it (mirrors
    // `platform_native_wrap_payload_executes_on_both_backend_targets`).
    // The semantic/compile/link assertions above stay unconditional.
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let execution = execute_wrap(&output, "single");
        assert_eq!(
            execution.status.code(),
            Some(42),
            "Wrap repro must execute to the payload value 42: {execution:?}",
        );
    } else {
        let _ = output;
    }
}

// ---------------------------------------------------------------------------
// Criterion 6 — both backends execute the Wrap payload regression
// (currently fail-closed on both backend targets)
// ---------------------------------------------------------------------------

/// FLIPPED-POST-ANCHOR-FIX (RUE-1089). The Wrap payload regression now compiles
/// on BOTH backend targets: the unified anchor is a frontend fact reached before
/// backend selection, so both `x86-64-linux` and `aarch64-linux` link. The
/// target matching the host triple executes to exit 42; off-host targets stay
/// structural cross-compile checks (mirroring `cli.abi_conformance`), so the
/// suite is green on non-Linux hosts too.
#[cfg(unix)]
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_wrap_payload_executes_on_both_backend_targets() {
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        let options = CompileOptions {
            target,
            ..CompileOptions::default()
        };
        fresh_rooted_cfg(WRAP_REPRO, &options).unwrap_or_else(|errors| {
            panic!("target {target:?}: Wrap repro must compile: {errors}")
        });

        let snapshot = SourceSnapshot::single("<wrap>", WRAP_REPRO).expect("snapshot");
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().expect("publish");
        let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
            .unwrap_or_else(|errors| panic!("target {target:?}: Wrap repro must link: {errors}"));

        let (host_can_execute, suffix) = match target {
            Target::X86_64Linux => (
                cfg!(all(target_os = "linux", target_arch = "x86_64")),
                "x86",
            ),
            Target::Aarch64Linux => (
                cfg!(all(target_os = "linux", target_arch = "aarch64")),
                "aarch64",
            ),
            _ => (false, "other"),
        };
        if host_can_execute {
            let execution = execute_wrap(&output, suffix);
            assert_eq!(
                execution.status.code(),
                Some(42),
                "target {target:?}: Wrap payload must execute to 42: {execution:?}",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Criterion 7 — retired source-transport markers have no semantic authority
// ---------------------------------------------------------------------------

/// The Wrap shape (a generic struct producer whose method reaches its
/// anonymous-enum field), carrying a comment marker inside the producer whose
/// durable identity a reached member consumes. The marker used to select the
/// removed synthetic-source transport fault seam; it is now ordinary trivia.
fn fault_probe_program(marker: &str) -> String {
    format!(
        r#"
fn Option(comptime T: type) -> type {{ enum {{ Some(T), None }} }}
fn Wrap(comptime T: type) -> type {{
    // {marker}
    struct {{
        inner: Option(T),
        fn get_or(self, d: T) -> T {{
            let O = Option(T);
            match self.inner {{ O.Some(v) => v, O.None => d }}
        }}
    }}
}}
fn main() -> i32 {{
    let W = Wrap(i32);
    let O = Option(i32);
    let w: W = W {{ inner: O.Some(42) }};
    w.get_or(0)
}}
"#
    )
}

/// The canonical candidate artifact owns anonymous anchors. A comment carrying
/// the retired reparse transport marker cannot alter that identity.
#[test]
fn retired_divergent_transport_marker_is_inert() {
    let options = CompileOptions::default();
    let program = fault_probe_program("__RUE1089_FAULT_DIVERGE__");
    fresh_rooted_cfg(&program, &options)
        .expect("a source comment cannot corrupt candidate-artifact anchor identity");
}

/// None of the retired synthetic-source transport markers has authority over
/// the packed candidate's indexed anchors.
#[test]
fn retired_resolve_transport_markers_are_inert() {
    let options = CompileOptions::default();
    for marker in [
        "__RUE1089_FAULT_MISSING__",
        "__RUE1089_FAULT_DUPLICATE__",
        "__RUE1089_FAULT_WRONG_KIND__",
    ] {
        let program = fault_probe_program(marker);
        fresh_rooted_cfg(&program, &options)
            .unwrap_or_else(|errors| panic!("marker {marker} changed semantics: {errors}"));
    }
}

/// Without a fault marker the same probe compiles and runs — proving the hook is
/// inert by default and the fault behavior above is caused by the injection.
#[cfg(unix)]
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_fault_probe_compiles_and_runs_cleanly_without_a_marker() {
    let options = CompileOptions::default();
    let program = fault_probe_program("no fault here");
    fresh_rooted_cfg(&program, &options).expect("the unmarked probe must compile");
    let snapshot = SourceSnapshot::single("<fault>", &program).expect("snapshot");
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().expect("publish");
    let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
        .expect("unmarked probe links");
    // Default target is `x86-64-linux`; execute only on a matching host triple.
    // The compile/link assertions above stay unconditional.
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let execution = execute_wrap(&output, "clean");
        assert_eq!(
            execution.status.code(),
            Some(42),
            "unmarked probe: {execution:?}"
        );
    } else {
        let _ = output;
    }
}

// ---------------------------------------------------------------------------
// Evaluator correspondence — two same-kind sites in one producer, reversed
// order, must not swap identities.
// ---------------------------------------------------------------------------

/// A single comptime producer binds two same-kind anonymous structs with
/// DIFFERENT fields, then selects one by a comptime flag. Each site must map to
/// its own frontend anchor: a span→anchor mix-up would give the selected local
/// the other site's anchor, which the runtime reference (under AstGen's real
/// anchor) could not resolve. Both selections, in both source orders, must
/// compile and run correctly — a set-equality check alone would miss a swap.
#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_evaluator_correspondence_two_same_kind_sites_do_not_swap() {
    let options = CompileOptions::default();
    // `A` bound first, `B` second; the field names differ so a swap changes the
    // constructed field and fails to compile or returns the wrong value.
    let forward = r#"
fn Choose(comptime pick_a: bool) -> type {
    let A = struct { a: i32 };
    let B = struct { b: i32 };
    if pick_a { A } else { B }
}
fn main() -> i32 {
    let TA = Choose(true);
    let TB = Choose(false);
    let a: TA = TA { a: 40 };
    let b: TB = TB { b: 2 };
    a.a + b.b
}
"#;
    // The two bindings in the opposite source order (byte offsets shift, anchors
    // must not).
    let reversed = r#"
fn Choose(comptime pick_a: bool) -> type {
    let B = struct { b: i32 };
    let A = struct { a: i32 };
    if pick_a { A } else { B }
}
fn main() -> i32 {
    let TA = Choose(true);
    let TB = Choose(false);
    let a: TA = TA { a: 40 };
    let b: TB = TB { b: 2 };
    a.a + b.b
}
"#;
    for (label, source) in [("forward", forward), ("reversed", reversed)] {
        let snapshot = SourceSnapshot::single("<choose>", source).expect("snapshot");
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().expect("publish");
        let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
            .unwrap_or_else(|errors| panic!("{label}: Choose must compile: {errors}"));
        // Default target is `x86-64-linux`; execute the linked ELF only when the
        // host triple matches it. The compile assertion above stays unconditional.
        #[cfg(unix)]
        {
            if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                let execution = execute_wrap(&output, label);
                assert_eq!(
                    execution.status.code(),
                    Some(42),
                    "{label}: two same-kind sites must keep their own identities: {execution:?}",
                );
            } else {
                let _ = output;
            }
        }
        #[cfg(not(unix))]
        let _ = output;
    }
}

/// Only one of two syntactic anonymous sites is ever evaluated (a comptime `if`
/// picks a branch), so runtime consumption is a strict SUBSET of the transported
/// table. This must compile — the fail-closed rule requires every CONSUMED
/// locator to resolve, never every transported entry to be observed.
#[test]
fn selected_branch_consumes_a_subset_of_the_transported_table() {
    let options = CompileOptions::default();
    let source = r#"
fn Pick(comptime take_first: bool) -> type {
    if take_first { struct { first: i32 } } else { struct { second: i32 } }
}
fn main() -> i32 {
    let T = Pick(true);
    let t: T = T { first: 42 };
    t.first
}
"#;
    fresh_rooted_cfg(source, &options).expect("only the selected branch's site is consumed");
}

/// Trivia and unrelated declarations before the producer shift every module and
/// fragment byte offset, but the transported anchor is definition-relative, so
/// behavior is unchanged.
#[test]
fn anchor_transport_survives_trivia_and_unrelated_shifts() {
    let options = CompileOptions::default();
    let baseline = r#"
fn Box(comptime T: type) -> type {
    struct {
        v: T,
        fn get(self) -> T { self.v }
    }
}
fn main() -> i32 {
    let B = Box(i32);
    let b: B = B { v: 42 };
    b.get()
}
"#;
    let shifted = r#"
// an unrelated leading comment that shifts every byte offset below
fn unrelated_helper() -> i32 { 7 }

fn Box(comptime T: type) -> type {
    // trivia inside the producer body
    struct {
        v: T,
        fn get(self) -> T { self.v }
    }
}
fn main() -> i32 {
    let B = Box(i32);
    let b: B = B { v: 42 };
    b.get()
}
"#;
    let base = fresh_rooted_cfg(baseline, &options).expect("baseline compiles");
    let shift = fresh_rooted_cfg(shifted, &options).expect("shifted compiles");
    assert_eq!(
        anonymous_type_count(&base),
        anonymous_type_count(&shift),
        "trivia/unrelated shifts changed the anonymous identity count",
    );
}
