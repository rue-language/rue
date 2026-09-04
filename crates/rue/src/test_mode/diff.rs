//! The machine-computed diff between a comparison failure's two operands
//! (ADR-0083 Phase 2.5).
//!
//! A `failure` frame carrying `left` and `right` says what the two values
//! rendered to; it does not say where they differ. Computing that here — once,
//! in the runner — is what lets the event stream and the human rendering agree,
//! and what keeps a consumer from having to re-derive it from two strings.
//!
//! Three decisions shape it.
//!
//! - **Granularity follows the values.** A value containing a newline is diffed
//!   line by line, because a character-level diff of two multi-line renderings
//!   is unreadable and enormous; anything else is diffed character by
//!   character, because that is what locates the one digit that changed.
//! - **Hunks are exact, in order, and reconstruct both sides.** Concatenating
//!   every `equal` and `delete` hunk's text yields `left` exactly;
//!   concatenating every `equal` and `insert` hunk's text yields `right`. A
//!   line unit therefore carries its own terminator, and the two invariants are
//!   what make the encoding lossless rather than a rendering.
//! - **It is dependency-free and bounded.** The renderings a compiler-synthesized
//!   printer produces are bounded to 4 KiB, so an exact quadratic
//!   longest-common-subsequence is affordable — but the channel is an open
//!   protocol (§5.1) and another producer's values are not bounded by anything
//!   the runner controls. A common prefix and suffix are removed first, and a
//!   middle whose table would exceed [`CELL_BUDGET`] is reported as one
//!   wholesale replacement instead of being refined. That keeps the work and
//!   the memory bounded for *any* input, and a diff is still produced.
//!
//! Both operands are always valid UTF-8 by the time they arrive: they are JSON
//! string fields, and a channel line that is not valid UTF-8 never becomes a
//! frame at all — `parse_channel` records it as malformed and the verdict
//! carries a `runner_note` saying the failure report could not be read. So a
//! non-UTF-8 rendering is rejected upstream rather than re-encoded here.

/// Largest longest-common-subsequence table the refinement will build, in
/// cells. A 4 KiB rendering diffed by line stays far below it; two unrelated
/// 1024-unit middles are exactly at it.
const CELL_BUDGET: usize = 1 << 20;

/// What one hunk says about the text it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffOp {
    /// Present in both values.
    Equal,
    /// Present in `left` and absent from `right`.
    Delete,
    /// Present in `right` and absent from `left`.
    Insert,
}

impl DiffOp {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Delete => "delete",
            Self::Insert => "insert",
        }
    }
}

/// One run of text with one op. Adjacent runs sharing an op are always merged,
/// so no two consecutive hunks have the same op and no hunk is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) op: DiffOp,
    pub(crate) text: String,
}

/// Whether a pair of values is diffed by line or by character.
fn line_oriented(left: &str, right: &str) -> bool {
    left.contains('\n') || right.contains('\n')
}

/// Split one value into the units it is diffed by.
///
/// A line keeps its own terminator, which is what makes the concatenation
/// invariants hold for a value whose last line is unterminated as well as one
/// whose last line is not.
fn units(text: &str, by_line: bool) -> Vec<&str> {
    if by_line {
        text.split_inclusive('\n').collect()
    } else {
        text.char_indices()
            .map(|(index, character)| &text[index..index + character.len_utf8()])
            .collect()
    }
}

/// The diff from `left` to `right`.
pub(crate) fn diff(left: &str, right: &str) -> Vec<Hunk> {
    let by_line = line_oriented(left, right);
    let left_units = units(left, by_line);
    let right_units = units(right, by_line);

    let prefix = left_units
        .iter()
        .zip(right_units.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = left_units[prefix..]
        .iter()
        .rev()
        .zip(right_units[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();

    let mut hunks = Hunks::default();
    hunks.extend(DiffOp::Equal, &left_units[..prefix]);
    let left_middle = &left_units[prefix..left_units.len() - suffix];
    let right_middle = &right_units[prefix..right_units.len() - suffix];
    if left_middle.len().saturating_mul(right_middle.len()) > CELL_BUDGET {
        // Past the budget the exact answer is not worth its cost, and a
        // wholesale replacement is still a true diff: every unit of the middle
        // really is absent from the other side's middle *as a subsequence
        // alignment*, just not as coarsely as an exact run would report.
        hunks.extend(DiffOp::Delete, left_middle);
        hunks.extend(DiffOp::Insert, right_middle);
    } else {
        refine(&mut hunks, left_middle, right_middle);
    }
    hunks.extend(DiffOp::Equal, &left_units[left_units.len() - suffix..]);
    hunks.0
}

/// Exact longest-common-subsequence alignment of the two middles.
fn refine(hunks: &mut Hunks, left: &[&str], right: &[&str]) {
    if left.is_empty() || right.is_empty() {
        hunks.extend(DiffOp::Delete, left);
        hunks.extend(DiffOp::Insert, right);
        return;
    }
    let rows = left.len() + 1;
    let columns = right.len() + 1;
    let mut table = vec![0u32; rows * columns];
    for row in (0..left.len()).rev() {
        for column in (0..right.len()).rev() {
            table[row * columns + column] = if left[row] == right[column] {
                table[(row + 1) * columns + column + 1] + 1
            } else {
                table[(row + 1) * columns + column].max(table[row * columns + column + 1])
            };
        }
    }

    // Walk forward through the table, which emits a replacement as its deletes
    // followed by its inserts — the order a reader expects, and the order the
    // `-`/`+` rendering prints.
    let (mut row, mut column) = (0, 0);
    while row < left.len() || column < right.len() {
        if row < left.len() && column < right.len() && left[row] == right[column] {
            hunks.push(DiffOp::Equal, left[row]);
            row += 1;
            column += 1;
        } else if column == right.len()
            || (row < left.len()
                && table[(row + 1) * columns + column] >= table[row * columns + column + 1])
        {
            hunks.push(DiffOp::Delete, left[row]);
            row += 1;
        } else {
            hunks.push(DiffOp::Insert, right[column]);
            column += 1;
        }
    }
}

/// Accumulator that merges adjacent units sharing an op into one hunk.
#[derive(Default)]
struct Hunks(Vec<Hunk>);

impl Hunks {
    fn push(&mut self, op: DiffOp, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(last) if last.op == op => last.text.push_str(text),
            _ => self.0.push(Hunk {
                op,
                text: text.to_owned(),
            }),
        }
    }

    fn extend(&mut self, op: DiffOp, units: &[&str]) {
        for unit in units {
            self.push(op, unit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunks(left: &str, right: &str) -> Vec<(&'static str, String)> {
        diff(left, right)
            .into_iter()
            .map(|hunk| (hunk.op.as_str(), hunk.text))
            .collect()
    }

    /// The two invariants the encoding rests on: the equal-and-delete hunks
    /// rebuild `left`, and the equal-and-insert hunks rebuild `right`.
    #[track_caller]
    fn assert_reconstructs(left: &str, right: &str) {
        let hunks = diff(left, right);
        let mut rebuilt_left = String::new();
        let mut rebuilt_right = String::new();
        for hunk in &hunks {
            match hunk.op {
                DiffOp::Equal => {
                    rebuilt_left.push_str(&hunk.text);
                    rebuilt_right.push_str(&hunk.text);
                }
                DiffOp::Delete => rebuilt_left.push_str(&hunk.text),
                DiffOp::Insert => rebuilt_right.push_str(&hunk.text),
            }
        }
        assert_eq!(rebuilt_left, left, "left side of {hunks:?}");
        assert_eq!(rebuilt_right, right, "right side of {hunks:?}");
        for pair in hunks.windows(2) {
            assert_ne!(pair[0].op, pair[1].op, "unmerged adjacent hunks: {hunks:?}");
        }
        assert!(
            hunks.iter().all(|hunk| !hunk.text.is_empty()),
            "empty hunk in {hunks:?}"
        );
    }

    #[test]
    fn identical_values_are_one_equal_hunk() {
        assert_eq!(hunks("41", "41"), [("equal", "41".to_owned())]);
        assert_reconstructs("41", "41");
    }

    #[test]
    fn two_empty_values_have_no_hunks() {
        assert_eq!(hunks("", ""), []);
    }

    #[test]
    fn an_empty_side_is_a_single_delete_or_insert() {
        assert_eq!(hunks("41", ""), [("delete", "41".to_owned())]);
        assert_eq!(hunks("", "42"), [("insert", "42".to_owned())]);
        assert_reconstructs("41", "");
        assert_reconstructs("", "42");
    }

    /// The case the character granularity exists for: one digit changed, and
    /// the diff says which one.
    #[test]
    fn a_single_changed_character_is_located_exactly() {
        assert_eq!(
            hunks("41", "42"),
            [
                ("equal", "4".to_owned()),
                ("delete", "1".to_owned()),
                ("insert", "2".to_owned()),
            ]
        );
    }

    #[test]
    fn a_pure_insertion_and_a_pure_deletion_keep_their_ops() {
        assert_eq!(
            hunks("ac", "abc"),
            [
                ("equal", "a".to_owned()),
                ("insert", "b".to_owned()),
                ("equal", "c".to_owned()),
            ]
        );
        assert_eq!(
            hunks("abc", "ac"),
            [
                ("equal", "a".to_owned()),
                ("delete", "b".to_owned()),
                ("equal", "c".to_owned()),
            ]
        );
    }

    /// The alignment prefers a deletion on a tie, so a substitution reports its
    /// deletion before its insertion — the order the `-`/`+` rendering prints.
    #[test]
    fn a_substitution_reports_the_deletion_first() {
        assert_eq!(
            hunks("{ x: 1 }", "{ x: 2 }"),
            [
                ("equal", "{ x: ".to_owned()),
                ("delete", "1".to_owned()),
                ("insert", "2".to_owned()),
                ("equal", " }".to_owned()),
            ]
        );
        assert_reconstructs("{ x: 1, y: true }", "{ x: 2, y: false }");
    }

    /// A newline on either side switches the whole diff to lines, so a
    /// multi-line rendering reports whole changed lines rather than a
    /// character soup.
    #[test]
    fn a_newline_on_either_side_selects_line_granularity() {
        assert_eq!(
            hunks("a\nb\nc\n", "a\nB\nc\n"),
            [
                ("equal", "a\n".to_owned()),
                ("delete", "b\n".to_owned()),
                ("insert", "B\n".to_owned()),
                ("equal", "c\n".to_owned()),
            ]
        );
        // Only one side has a newline: still lines, so the single-line value is
        // one unit that no line of the other side equals, rather than being
        // diffed against every character of it.
        assert_eq!(
            hunks("a", "a\nb\n"),
            [("delete", "a".to_owned()), ("insert", "a\nb\n".to_owned())]
        );
    }

    /// A line unit carries its own terminator, so an unterminated last line and
    /// a terminated one are different units and both reconstruct exactly.
    #[test]
    fn line_units_carry_their_terminators() {
        assert_reconstructs("one\ntwo", "one\ntwo\n");
        assert_eq!(
            hunks("one\ntwo", "one\ntwo\n"),
            [
                ("equal", "one\n".to_owned()),
                ("delete", "two".to_owned()),
                ("insert", "two\n".to_owned()),
            ]
        );
    }

    #[test]
    fn multibyte_scalars_are_never_split() {
        assert_reconstructs("héllo", "hallo");
        assert_eq!(
            hunks("é", "e"),
            [("delete", "é".to_owned()), ("insert", "e".to_owned())]
        );
    }

    /// Past the table budget the middle is reported as one wholesale
    /// replacement rather than refined, and the result is still a diff that
    /// reconstructs both sides.
    #[test]
    fn an_oversized_middle_falls_back_to_a_wholesale_replacement() {
        let left: String = std::iter::repeat_n("a\n", 2_000).collect();
        let right: String = std::iter::repeat_n("b\n", 2_000).collect();
        let hunks = diff(&left, &right);
        assert_eq!(
            hunks.iter().map(|hunk| hunk.op).collect::<Vec<_>>(),
            [DiffOp::Delete, DiffOp::Insert]
        );
        assert_reconstructs(&left, &right);
    }

    /// The shared prefix and suffix are removed before any table is built, so
    /// two long values that differ in one line stay exact and cheap.
    #[test]
    fn a_long_pair_differing_in_one_line_stays_exact() {
        let mut left: String = std::iter::repeat_n("same\n", 3_000).collect();
        let mut right = left.clone();
        left.push_str("one\n");
        right.push_str("two\n");
        assert_eq!(
            hunks(&left, &right)
                .iter()
                .map(|(op, _)| *op)
                .collect::<Vec<_>>(),
            ["equal", "delete", "insert"]
        );
        assert_reconstructs(&left, &right);
    }
}
