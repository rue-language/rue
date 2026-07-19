//! Corpus loading and management.

use std::collections::BTreeSet;
use std::fmt;
use std::io::Write;
use std::path::Path;

use rue_test_runner::load_test_files;

/// Load all files from a corpus directory.
pub fn load_corpus(dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut corpus = Vec::new();

    if !dir.exists() {
        anyhow::bail!("corpus directory does not exist: {}", dir.display());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            match std::fs::read(&path) {
                Ok(data) => corpus.push(data),
                Err(e) => {
                    eprintln!("Warning: failed to read {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(corpus)
}

/// Summary of a seed-corpus build.
///
/// The counts reconcile exactly: `single_file_declarations == seeds_written +
/// duplicates`, and `declarations() == single_file_declarations +
/// multi_file_skipped`. Nothing is dropped silently — every case a spec file
/// declares is either written, counted as a duplicate, or counted as an
/// intentional multi-file skip.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SeedCorpusSummary {
    /// Single-file case sources considered, after parameter expansion.
    pub single_file_declarations: usize,
    /// Multi-file cases intentionally skipped: their imported modules
    /// (`aux_files`) cannot be represented by one seed blob.
    pub multi_file_skipped: usize,
    /// Unique seed sources written to the output directory.
    pub seeds_written: usize,
    /// Single-file sources collapsed because an identical source was already
    /// seen (content-deduplicated).
    pub duplicates: usize,
}

impl SeedCorpusSummary {
    /// Total case declarations examined across every loaded test file.
    pub fn declarations(&self) -> usize {
        self.single_file_declarations + self.multi_file_skipped
    }
}

impl fmt::Display for SeedCorpusSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} declarations -> {} seeds ({} duplicates collapsed, {} multi-file cases skipped)",
            self.declarations(),
            self.seeds_written,
            self.duplicates,
            self.multi_file_skipped
        )
    }
}

/// Deterministic 64-bit FNV-1a hash, used to derive content-addressed seed
/// identities so the same source always maps to the same seed file regardless
/// of discovery or iteration order.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Create a seed corpus from the spec test files under `source_dir`.
///
/// Sources are deserialized and parameter-expanded through the canonical
/// `rue-test-runner` [`load_test_files`] path — the exact `TestFile` schema and
/// `{key}` expansion the spec suite runs against — rather than a bespoke,
/// line-oriented TOML scan that only recognized triple-quoted `source` fields
/// and injected unexpanded placeholder text as if it were executable Rue
/// (RUE-805).
///
/// Behavior:
///
/// * Single-line, multiline, and parameter-expanded `source` values all become
///   seeds; TOML string form is irrelevant because the value is deserialized,
///   not pattern-matched.
/// * Multi-file cases (those with `aux_files`) are classified and counted, not
///   silently omitted — one seed blob cannot carry their imported modules.
/// * Discovery, read, parse, and validation failures propagate as errors, and
///   an empty corpus is itself an error, so a broken spec tree can never
///   masquerade as a successful (but empty) corpus build.
/// * Seed identities are content-addressed and deduplicated, so the output is
///   deterministic.
pub fn create_seed_corpus(
    source_dir: &Path,
    output_dir: &Path,
) -> anyhow::Result<SeedCorpusSummary> {
    // `load_test_files` discovers every `.toml`, fails on read/parse/validation
    // errors instead of skipping the file, and returns cases already expanded by
    // `expand_test_file`.
    let test_files = load_test_files(source_dir).map_err(|error| anyhow::anyhow!(error))?;

    std::fs::create_dir_all(output_dir)?;

    let mut summary = SeedCorpusSummary::default();
    // BTreeSet gives content-deduplicated, order-independent iteration.
    let mut unique_sources: BTreeSet<Vec<u8>> = BTreeSet::new();

    for (_identifier, test_file) in &test_files {
        for case in &test_file.case {
            if !case.aux_files.is_empty() {
                // Multi-file: the root `source` would compile with dangling
                // imports, so classify and count instead of emitting it.
                summary.multi_file_skipped += 1;
                continue;
            }
            summary.single_file_declarations += 1;
            if !unique_sources.insert(case.source.clone().into_bytes()) {
                summary.duplicates += 1;
            }
        }
    }

    for source in &unique_sources {
        let filename = format!("seed-{:016x}.rue", fnv1a_64(source));
        let mut file = std::fs::File::create(output_dir.join(filename))?;
        file.write_all(source)?;
        summary.seeds_written += 1;
    }

    if summary.seeds_written == 0 {
        anyhow::bail!(
            "seed corpus is empty: no single-file source cases found under {}",
            source_dir.display()
        );
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory rooted at the fuzz scratch area. Avoids a
    /// tempfile dependency while keeping cases isolated by name.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("rue-fuzz-corpus-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    fn build(name: &str, toml: &str) -> anyhow::Result<SeedCorpusSummary> {
        let cases = scratch(&format!("{name}-cases"));
        write(&cases, "fixture.toml", toml);
        let out = scratch(&format!("{name}-out"));
        create_seed_corpus(&cases, &out)
    }

    #[test]
    fn accepts_single_line_and_multiline_string_forms() {
        // Triple-quoted single-line, triple-quoted multiline, basic quoted, and
        // literal (single-quote) forms all deserialize to the same `source`
        // string, so all four become seeds.
        let summary = build(
            "string-forms",
            r#"
[section]
id = "fixtures.forms"
name = "String forms"

[[case]]
name = "triple_single_line"
source = """fn main() -> i32 { 1 }"""
exit_code = 1

[[case]]
name = "triple_multiline"
source = """
fn main() -> i32 {
    let x = 2;
    x
}
"""
exit_code = 2

[[case]]
name = "basic_quoted"
source = "fn main() -> i32 { 3 }"
exit_code = 3

[[case]]
name = "literal_quoted"
source = 'fn main() -> i32 { 4 }'
exit_code = 4
"#,
        )
        .unwrap();
        assert_eq!(summary.single_file_declarations, 4);
        assert_eq!(summary.seeds_written, 4);
        assert_eq!(summary.duplicates, 0);
        assert_eq!(summary.multi_file_skipped, 0);
    }

    #[test]
    fn expands_parameterized_cases() {
        // `{N}` is substituted per param set, so one declaration yields three
        // distinct seeds — the whole point of routing through the canonical
        // expansion path rather than emitting the literal template.
        let summary = build(
            "params",
            r#"
[section]
id = "fixtures.params"
name = "Parameters"

[[case]]
name = "returns_{N}"
source = "fn main() -> i32 { {N} }"

[[case.params]]
N = 1

[[case.params]]
N = 2

[[case.params]]
N = 3
"#,
        )
        .unwrap();
        assert_eq!(summary.single_file_declarations, 3);
        assert_eq!(summary.seeds_written, 3);
        assert_eq!(summary.duplicates, 0);
    }

    #[test]
    fn deduplicates_identical_sources_by_content() {
        let summary = build(
            "dedup",
            r#"
[section]
id = "fixtures.dedup"
name = "Dedup"

[[case]]
name = "first"
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "identical"
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#,
        )
        .unwrap();
        assert_eq!(summary.single_file_declarations, 2);
        assert_eq!(summary.seeds_written, 1);
        assert_eq!(summary.duplicates, 1);
    }

    #[test]
    fn classifies_multi_file_cases_as_skipped() {
        let summary = build(
            "multifile",
            r#"
[section]
id = "fixtures.multifile"
name = "Multi-file"

[[case]]
name = "single"
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "with_import"
source = "fn main() -> i32 { 0 }"
exit_code = 0
aux_files = { "math.rue" = "pub fn add(a: i32, b: i32) -> i32 { a + b }" }
"#,
        )
        .unwrap();
        assert_eq!(summary.single_file_declarations, 1);
        assert_eq!(summary.multi_file_skipped, 1);
        assert_eq!(summary.seeds_written, 1);
        assert_eq!(summary.declarations(), 2);
    }

    #[test]
    fn seed_identities_are_deterministic_and_content_addressed() {
        let cases = scratch("determinism-cases");
        write(
            &cases,
            "fixture.toml",
            r#"
[section]
id = "fixtures.determinism"
name = "Determinism"

[[case]]
name = "one"
source = "fn main() -> i32 { 42 }"
exit_code = 42
"#,
        );
        let out_a = scratch("determinism-out-a");
        let out_b = scratch("determinism-out-b");
        create_seed_corpus(&cases, &out_a).unwrap();
        create_seed_corpus(&cases, &out_b).unwrap();

        let names = |dir: &Path| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            names(&out_a),
            names(&out_b),
            "identities must be stable across runs"
        );
        assert_eq!(names(&out_a).len(), 1);
        assert!(names(&out_a)[0].starts_with("seed-") && names(&out_a)[0].ends_with(".rue"));
    }

    #[test]
    fn fails_on_malformed_toml() {
        let error = build("malformed", "this is not valid TOML = = = [[[")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("failed to load"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn fails_on_empty_corpus() {
        // A well-formed file with no single-file cases must not be reported as a
        // successful (but empty) build.
        let error = build(
            "empty",
            r#"
[section]
id = "fixtures.empty"
name = "Empty"

[[case]]
name = "only_multi_file"
source = "fn main() -> i32 { 0 }"
exit_code = 0
aux_files = { "math.rue" = "pub fn add(a: i32, b: i32) -> i32 { a + b }" }
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("empty"), "unexpected error: {error}");
    }

    #[test]
    fn fails_on_missing_source_directory() {
        let missing = scratch("missing").join("does-not-exist");
        let out = scratch("missing-out");
        assert!(create_seed_corpus(&missing, &out).is_err());
    }

    #[test]
    fn fails_on_unreadable_input() {
        // A file the loader cannot read must fail the build, not be silently
        // skipped. `root` bypasses permission bits, so this can only be
        // exercised as a non-root user (e.g. CI); skip it under root rather
        // than assert a guarantee the OS is not enforcing here.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let cases = scratch("unreadable-cases");
        let file = cases.join("locked.toml");
        write(
            &cases,
            "locked.toml",
            "[section]\nid = \"x\"\nname = \"x\"\n",
        );
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        std::fs::set_permissions(&file, perms).unwrap();

        let out = scratch("unreadable-out");
        let result = create_seed_corpus(&cases, &out);

        // Restore permissions so the temp dir can be cleaned up.
        let mut restore = std::fs::metadata(&file).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut restore, 0o644);
        let _ = std::fs::set_permissions(&file, restore);

        assert!(
            result.is_err(),
            "an unreadable test file must fail the build"
        );
    }
}
