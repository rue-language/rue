//! The synthesized `main` of a test image (ADR-0083 §3).
//!
//! A test image links every test in the request's closure plus one generated
//! entry point that reads a selector from `argv` and calls exactly one test
//! body. Like drop glue, the dispatcher is produced as a [`rue_air::SemanticBody`]
//! and materialized through the ordinary CFG path, so both backends, the
//! optimizer, and the memo database get it without a line of per-backend code.
//! Unlike the C export thunk, it is not hand-assembled per target.
//!
//! # The shape it generates
//!
//! ```text
//! fn main() -> i32 {
//!     if @arg_count() != 2 { __rue_test_usage_error(); return 2; }
//!     let entry = @arg_ptr(1);
//!     if @arg_len(1) != 16 { __rue_test_usage_error(); return 2; }
//!     // sixteen unrolled hex digits, accumulating the value and an OR of
//!     // every digit — see `hex digits` below
//!     if flags >= 16 { __rue_test_usage_error(); return 2; }
//!     if ordinal >= <test count> { __rue_test_usage_error(); return 2; }
//!     __rue_test_normalize_process();
//!     match ordinal { 0 => test_0(), 1 => test_1(), ..., _ => () }
//!     __rue_test_complete();
//!     return 0;
//! }
//! ```
//!
//! # Why it imports nothing
//!
//! The parse is written out of raw pointer reads and wrapping arithmetic rather
//! than `std.env` and `std.parse`. A test image's closure is whatever the tests
//! import, and a suite that imports no standard library must still link: making
//! the entry point depend on `std` would put a module in every test closure
//! that the request never asked for, changing what is analyzed and what the
//! deferred cache (§6) would key on.
//!
//! # hex digits
//!
//! The selector is exactly sixteen lowercase hex digits, so each digit maps to
//! `byte - 48` for `'0'..='9'` and `byte - 87` for `'a'..='f'`. Both use
//! wrapping subtraction: every other byte then lands far above 15 rather than
//! trapping on underflow, and the loop needs no per-digit range test. Validity
//! is the bitwise OR of all sixteen digits — a value stays below 16 exactly
//! when every digit did — which is also what rejects the uppercase digits and
//! the bytes 0x57 through 0x60, which a bare subtraction would otherwise
//! decode as 0 through 9.
//!
//! The value is accumulated with wrapping multiply and add because sixteen hex
//! digits fill a `u64` exactly: the last shift is a legitimate overflow of the
//! trapping operators, not an error.
//!
//! # Exclusion
//!
//! Dispatcher code is runner plumbing. It is excluded by construction from the
//! capability summaries and closure fingerprints the deferred ADRs compute
//! (ADR-0083 §3), which is why it carries no source span of its own and roots
//! nothing beyond the tests already in the request's root set.

use std::sync::Arc;

use rue_air::{
    AirArgMode, SemanticBody, SemanticBodyAnchor, SemanticBodyCallArg, SemanticBodyInst,
    SemanticBodyInstData, SemanticBodyMatchArm, SemanticBodyPattern,
};

type Ty = rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>;
type Body = SemanticBody<crate::StableDefinitionKey, crate::ModuleId>;
type Inst = SemanticBodyInst<crate::StableDefinitionKey, crate::ModuleId>;
type Data = SemanticBodyInstData<crate::StableDefinitionKey, crate::ModuleId>;

/// Digits in a selector, and therefore the exec contract's fixed selector
/// width (ADR-0083 §3). Sixteen lowercase hex digits are one `u64`.
const SELECTOR_DIGITS: u32 = 16;

/// Exit status for a malformed or out-of-range selector (ADR-0083 §2: `2` is
/// "compilation or runner error", and a dispatcher that cannot understand its
/// own argv is the latter).
const SELECTOR_ERROR_STATUS: u64 = 2;

/// `'9'`: the last byte that decodes through the decimal offset.
const LAST_DECIMAL_BYTE: u64 = 57;
/// `'0'`.
const DECIMAL_OFFSET: u64 = 48;
/// `'a' - 10`, the offset that maps `'a'..='f'` onto `10..=15`.
const LOWERCASE_OFFSET: u64 = 87;
/// One past the last valid digit value.
const RADIX: u64 = 16;

/// Local slot holding the selector's first byte address.
const ENTRY_SLOT: u32 = 0;
/// First of the sixteen slots holding one widened selector byte each.
const BYTE_SLOT_BASE: u32 = 1;
/// First of the sixteen slots holding one decoded digit each.
const DIGIT_SLOT_BASE: u32 = BYTE_SLOT_BASE + SELECTOR_DIGITS;
/// Local slot holding the decoded ordinal.
const ORDINAL_SLOT: u32 = DIGIT_SLOT_BASE + SELECTOR_DIGITS;
/// Total local slots the body declares.
const LOCAL_SLOTS: u32 = ORDINAL_SLOT + 1;

/// Build the canonical dispatcher body for one ordered test table.
///
/// `table` is the request's tests in inventory order (`test_inventory`), so
/// element `n` is the body selector `n` runs. This function is pure and cheap:
/// the CFG evaluator and the fact selector that prepares its inputs both call
/// it rather than passing an already-built body through a memo key.
pub(crate) fn synthesize_test_dispatcher(table: &[crate::FunctionInstanceKey]) -> Body {
    let mut builder = Builder::default();
    let mut statements = Vec::new();

    // argc: the runner always execs with exactly the image name and one
    // selector, so anything else is a hand-run image or a broken runner.
    let arg_count = builder.add(
        Data::RuntimeCall {
            runtime: rue_air::RuntimeCallKind::ArgCount,
            args: Arc::new([]),
        },
        Ty::U64,
    );
    let expected_argc = builder.constant(2, Ty::U64);
    let wrong_argc = builder.add(Data::Ne(arg_count, expected_argc), Ty::Bool);
    statements.push(builder.selector_error_guard(wrong_argc));

    // The selector's bytes. `entry` is bound once and read sixteen times.
    let selector_index = builder.constant(1, Ty::U64);
    let entry = builder.add(
        Data::RuntimeCall {
            runtime: rue_air::RuntimeCallKind::ArgPtr,
            args: Arc::new([argument(selector_index)]),
        },
        pointer_type(),
    );
    statements.push(builder.bind(ENTRY_SLOT, entry, pointer_type()));

    let length_index = builder.constant(1, Ty::U64);
    let selector_length = builder.add(
        Data::RuntimeCall {
            runtime: rue_air::RuntimeCallKind::ArgLen,
            args: Arc::new([argument(length_index)]),
        },
        Ty::U64,
    );
    let expected_length = builder.constant(u64::from(SELECTOR_DIGITS), Ty::U64);
    let wrong_length = builder.add(Data::Ne(selector_length, expected_length), Ty::Bool);
    statements.push(builder.selector_error_guard(wrong_length));

    // Sixteen unrolled digits. Unrolled rather than looped because the width is
    // fixed by the exec contract: a loop would need its own induction slot and
    // bound check to express a constant the contract already pins.
    let mut ordinal = builder.constant(0, Ty::U64);
    let mut flags = builder.constant(0, Ty::U64);
    for digit in 0..SELECTOR_DIGITS {
        let byte_slot = BYTE_SLOT_BASE + digit;
        let digit_slot = DIGIT_SLOT_BASE + digit;

        let base = builder.load(ENTRY_SLOT, pointer_type());
        let offset = builder.constant(u64::from(digit), Ty::U64);
        let address = builder.add(
            Data::Intrinsic {
                operation: rue_air::IntrinsicOperation::PtrOffset,
                name: Arc::from(rue_air::IntrinsicOperation::PtrOffset.expected_spelling()),
                args: Arc::new([argument(base), argument(offset)]),
            },
            pointer_type(),
        );
        let byte = builder.add(
            Data::Intrinsic {
                operation: rue_air::IntrinsicOperation::PtrRead,
                name: Arc::from(rue_air::IntrinsicOperation::PtrRead.expected_spelling()),
                args: Arc::new([argument(address)]),
            },
            Ty::U8,
        );
        let widened = builder.add(
            Data::IntCast {
                value: byte,
                from_ty: Ty::U8,
            },
            Ty::U64,
        );
        statements.push(builder.bind(byte_slot, widened, Ty::U64));

        let classify = builder.load(byte_slot, Ty::U64);
        let last_decimal = builder.constant(LAST_DECIMAL_BYTE, Ty::U64);
        let is_decimal = builder.add(Data::Le(classify, last_decimal), Ty::Bool);
        let decimal_source = builder.load(byte_slot, Ty::U64);
        let decimal_offset = builder.constant(DECIMAL_OFFSET, Ty::U64);
        let decimal = builder.add(Data::WrappingSub(decimal_source, decimal_offset), Ty::U64);
        let lowercase_source = builder.load(byte_slot, Ty::U64);
        let lowercase_offset = builder.constant(LOWERCASE_OFFSET, Ty::U64);
        let lowercase = builder.add(
            Data::WrappingSub(lowercase_source, lowercase_offset),
            Ty::U64,
        );
        let decoded = builder.add(
            Data::Branch {
                cond: is_decimal,
                then_value: decimal,
                else_value: Some(lowercase),
            },
            Ty::U64,
        );
        statements.push(builder.bind(digit_slot, decoded, Ty::U64));

        let value = builder.load(digit_slot, Ty::U64);
        let radix = builder.constant(RADIX, Ty::U64);
        let shifted = builder.add(Data::WrappingMul(ordinal, radix), Ty::U64);
        ordinal = builder.add(Data::WrappingAdd(shifted, value), Ty::U64);

        let validity = builder.load(digit_slot, Ty::U64);
        flags = builder.add(Data::BitOr(flags, validity), Ty::U64);
    }
    statements.push(builder.bind(ORDINAL_SLOT, ordinal, Ty::U64));

    let radix = builder.constant(RADIX, Ty::U64);
    let malformed = builder.add(Data::Ge(flags, radix), Ty::Bool);
    statements.push(builder.selector_error_guard(malformed));

    // Out of range, which an empty table makes true for every selector.
    let selected = builder.load(ORDINAL_SLOT, Ty::U64);
    let count = builder.constant(table.len() as u64, Ty::U64);
    let out_of_range = builder.add(Data::Ge(selected, count), Ty::Bool);
    statements.push(builder.selector_error_guard(out_of_range));

    // Normalization runs before the body so the test observes the pinned
    // inventory (§3) and never the selector that varies per test.
    statements.push(builder.add(
        Data::RuntimeCall {
            runtime: rue_air::RuntimeCallKind::TestNormalizeProcess,
            args: Arc::new([]),
        },
        Ty::Unit,
    ));

    let scrutinee = builder.load(ORDINAL_SLOT, Ty::U64);
    let mut arms = Vec::with_capacity(table.len() + 1);
    for (ordinal, function) in table.iter().enumerate() {
        let call = builder.add(
            Data::Call {
                function: function.clone(),
                args: Arc::new([]),
            },
            Ty::Unit,
        );
        arms.push(SemanticBodyMatchArm {
            pattern: SemanticBodyPattern::Int(ordinal as i64),
            body: call,
        });
    }
    // The range guard above already rejected every ordinal without an arm, so
    // this one exists to make the match exhaustive rather than to be taken.
    let unreached = builder.add(Data::UnitConst, Ty::Unit);
    arms.push(SemanticBodyMatchArm {
        pattern: SemanticBodyPattern::Wildcard,
        body: unreached,
    });
    statements.push(builder.add(
        Data::Match {
            scrutinee,
            arms: arms.into(),
        },
        Ty::Unit,
    ));

    // The terminal completion frame, written only here: exit 0 without it is
    // how the runner detects a body that exited before its assertions ran.
    statements.push(builder.add(
        Data::RuntimeCall {
            runtime: rue_air::RuntimeCallKind::TestComplete,
            args: Arc::new([]),
        },
        Ty::Unit,
    ));

    let success = builder.constant(0, Ty::I32);
    let ret = builder.add(Data::Ret(Some(success)), Ty::Never);
    builder.add(
        Data::Block {
            statements: statements.into(),
            value: ret,
        },
        Ty::Never,
    );

    Body {
        is_accessor: false,
        return_type: Ty::I32,
        instructions: builder.instructions.into(),
        places: Arc::new([]),
        strings: Arc::new([]),
        local_atoms: Arc::new([]),
        param_drops: Arc::new([]),
        borrow_slots: Arc::new([]),
        num_locals: LOCAL_SLOTS,
        num_param_slots: 0,
        param_by_ref: Arc::new([]),
        param_writable: Arc::new([]),
        allow_unreachable_code: false,
        warnings: Arc::new([]),
        method_references: Arc::new([]),
    }
}

/// The dispatcher's only pointer type: the raw `argv` entry it reads bytes
/// from. `std.env` builds owned copies on top of the same accessors; the
/// dispatcher reads through them directly because it must not import `std`.
fn pointer_type() -> Ty {
    Ty::PtrMut(Arc::new(Ty::U8))
}

fn argument(value: u32) -> SemanticBodyCallArg {
    SemanticBodyCallArg {
        value,
        mode: AirArgMode::Normal,
    }
}

/// Instruction accumulator for the generated body.
///
/// Every reference is to an earlier index, which the importer enforces, so the
/// builder only ever appends. Values are never shared across a branch boundary:
/// anything read more than once is bound to a local slot and reloaded, exactly
/// as an ordinary source body does.
#[derive(Default)]
struct Builder {
    instructions: Vec<Inst>,
}

impl Builder {
    fn add(&mut self, data: Data, ty: Ty) -> u32 {
        let index = u32::try_from(self.instructions.len()).expect("dispatcher body fits u32");
        self.instructions.push(SemanticBodyInst {
            data,
            ty,
            anchor: SemanticBodyAnchor { start: 0, end: 0 },
        });
        index
    }

    fn constant(&mut self, value: u64, ty: Ty) -> u32 {
        self.add(Data::Const(value), ty)
    }

    /// Introduce a local slot holding `value`, as one statement.
    fn bind(&mut self, slot: u32, value: u32, ty: Ty) -> u32 {
        let live = self.add(Data::StorageLive { slot }, ty);
        let allocation = self.add(Data::Alloc { slot, init: value }, Ty::Unit);
        self.add(
            Data::Block {
                statements: Arc::new([live]),
                value: allocation,
            },
            Ty::Unit,
        )
    }

    fn load(&mut self, slot: u32, ty: Ty) -> u32 {
        self.add(Data::Load { slot }, ty)
    }

    /// `if <condition> { __rue_test_usage_error(); return 2; }` as one
    /// statement.
    ///
    /// The diagnostic is one pinned runtime message rather than compiler-built
    /// text: the dispatcher has no string constants of its own, and the message
    /// is part of the exec contract rather than of any one image.
    fn selector_error_guard(&mut self, condition: u32) -> u32 {
        let report = self.add(
            Data::RuntimeCall {
                runtime: rue_air::RuntimeCallKind::TestUsageError,
                args: Arc::new([]),
            },
            Ty::Unit,
        );
        let status = self.constant(SELECTOR_ERROR_STATUS, Ty::I32);
        let ret = self.add(Data::Ret(Some(status)), Ty::Never);
        let taken = self.add(
            Data::Block {
                statements: Arc::new([report]),
                value: ret,
            },
            Ty::Never,
        );
        self.add(
            Data::Branch {
                cond: condition,
                then_value: taken,
                else_value: None,
            },
            Ty::Unit,
        )
    }
}
