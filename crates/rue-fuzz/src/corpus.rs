//! Corpus loading and management.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use rue_test_runner::load_test_files;

/// Corpus files come from an Actions cache and are therefore inputs, not
/// trusted repository files. Keep one malformed cache entry from consuming a
/// runner's memory or disk budget before the target gets a chance to reject it.
pub const MAX_CORPUS_FILES: usize = 8_192;
pub const MAX_CORPUS_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_CORPUS_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_EVOLVED_FILES: usize = 4_096;
pub const MAX_EVOLVED_BYTES: u64 = 64 * 1024 * 1024;

/// Load all files from a corpus directory.
pub fn load_corpus(dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut corpus = Vec::new();
    let mut total_bytes: u64 = 0;

    if !dir.exists() {
        anyhow::bail!("corpus directory does not exist: {}", dir.display());
    }

    let (mut entries, truncated) = read_entries_limited(dir, MAX_CORPUS_FILES)?;
    if truncated {
        eprintln!(
            "Warning: corpus directory has more than {} entries; ignoring the rest",
            MAX_CORPUS_FILES
        );
    }
    // Stable ordering makes a fixed --seed replay independent of directory
    // enumeration order. The bytes are still selected by the seeded RNG.
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                eprintln!("Warning: failed to inspect corpus entry: {error}");
                continue;
            }
        };
        // Do not follow cache-provided symlinks. A cache must never turn the
        // fuzzer into a reader for arbitrary files on the runner.
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let size = match entry.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                eprintln!("Warning: failed to inspect corpus file: {error}");
                continue;
            }
        };
        if size > MAX_CORPUS_FILE_BYTES {
            eprintln!(
                "Warning: ignoring oversized corpus file ({} bytes; limit {})",
                size, MAX_CORPUS_FILE_BYTES
            );
            continue;
        }
        if total_bytes.saturating_add(size) > MAX_CORPUS_BYTES {
            eprintln!(
                "Warning: ignoring corpus files after the {}-byte limit",
                MAX_CORPUS_BYTES
            );
            break;
        }
        match std::fs::read(&path) {
            Ok(data) => {
                total_bytes += data.len() as u64;
                corpus.push(data);
            }
            Err(e) => {
                eprintln!("Warning: failed to read corpus file: {e}");
            }
        }
    }

    Ok(corpus)
}

/// Bounded, content-addressed output for successful mutated inputs.
///
/// The writer is deliberately independent from the input directory: a nightly
/// run can restore old evolved inputs, merge fresh spec seeds, and write only
/// successful mutations to the target's private cache directory. Crash inputs
/// are handled by [`crate::harness::CrashReporter`] and never reach this type.
pub struct EvolvedCorpus {
    dir: PathBuf,
    files: BTreeMap<String, u64>,
    bytes: u64,
    max_files: usize,
    max_bytes: u64,
}

impl EvolvedCorpus {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        Self::open_with_limits(dir, MAX_EVOLVED_FILES, MAX_EVOLVED_BYTES)
    }

    fn open_with_limits(dir: &Path, max_files: usize, max_bytes: u64) -> anyhow::Result<Self> {
        match std::fs::symlink_metadata(dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                anyhow::bail!("corpus path is not a regular directory: {}", dir.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir)?;
            }
            Err(error) => return Err(error.into()),
        }
        let mut files = BTreeMap::new();
        let mut bytes: u64 = 0;
        let mut loaded_bytes: u64 = 0;
        let (mut entries, _) = read_entries_limited(dir, max_files.saturating_add(1))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_symlink() || !file_type.is_file() {
                continue;
            }
            let size = match entry.metadata() {
                Ok(metadata) => metadata.len(),
                Err(_) => continue,
            };
            if size > MAX_CORPUS_FILE_BYTES {
                continue;
            }
            if loaded_bytes.saturating_add(size) > MAX_CORPUS_BYTES {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((expected_hash, expected_len)) = parse_evolved_name(&name) else {
                continue;
            };
            if expected_len != size {
                continue;
            }
            // A cache entry is not trusted merely because its filename looks
            // right. Verify the content address before allowing it to consume
            // the bounded retained set.
            let content = match std::fs::read(entry.path()) {
                Ok(content) => content,
                Err(_) => continue,
            };
            loaded_bytes = loaded_bytes.saturating_add(size);
            if fnv1a_64(&content) != expected_hash {
                continue;
            }
            files.insert(name, size);
            bytes += size;
        }
        let mut corpus = Self {
            dir: dir.to_path_buf(),
            files,
            bytes,
            max_files,
            max_bytes,
        };
        corpus.trim_to_limits()?;
        Ok(corpus)
    }

    /// Save an input if it is new and the bounded cache still has room.
    /// Returns whether a new file was written.
    pub fn record(&mut self, input: &[u8]) -> std::io::Result<bool> {
        if input.len() as u64 > MAX_CORPUS_FILE_BYTES {
            return Ok(false);
        }
        let name = evolved_name(input);
        if self.files.contains_key(&name) {
            return Ok(false);
        }
        if !self.would_retain(&name, input.len() as u64) {
            return Ok(false);
        }
        let path = self.dir.join(&name);
        // `create_new` preserves content-addressed idempotence even if two
        // target processes happen to share a cache directory.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(input) {
                    let _ = std::fs::remove_file(&path);
                    self.files.remove(&name);
                    return Err(error);
                }
                self.bytes += input.len() as u64;
                self.files.insert(name.clone(), input.len() as u64);
                self.trim_to_limits()?;
                Ok(self.files.contains_key(&name))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn would_retain(&self, name: &str, size: u64) -> bool {
        if self.max_files == 0 || size > self.max_bytes {
            return false;
        }
        if self.files.len() < self.max_files && self.bytes.saturating_add(size) <= self.max_bytes {
            return true;
        }
        let mut candidates = self.files.clone();
        candidates.insert(name.to_owned(), size);
        let mut bytes = self.bytes.saturating_add(size);
        while candidates.len() > self.max_files || bytes > self.max_bytes {
            let Some(evicted) = candidates.keys().next_back().cloned() else {
                break;
            };
            let size = candidates.remove(&evicted).unwrap_or(0);
            bytes = bytes.saturating_sub(size);
        }
        candidates.contains_key(name)
    }

    fn trim_to_limits(&mut self) -> std::io::Result<()> {
        while self.files.len() > self.max_files || self.bytes > self.max_bytes {
            let Some(name) = self.files.keys().next_back().cloned() else {
                break;
            };
            let path = self.dir.join(&name);
            let size = self.files.get(&name).copied().unwrap_or(0);
            let _ = std::fs::remove_file(path);
            self.files.remove(&name);
            self.bytes = self.bytes.saturating_sub(size);
        }
        Ok(())
    }

    #[cfg(test)]
    fn file_count(&self) -> usize {
        self.files.len()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SanitizedCorpusSummary {
    pub fresh_seeds: usize,
    pub restored_inputs: usize,
    pub retained_inputs: usize,
    pub ignored_inputs: usize,
    pub bytes: u64,
}

/// Build a clean target corpus from an untrusted restored cache and fresh
/// specification seeds. The restored tree is never used as the fuzzer input
/// or as the cache-save path; only validated, bounded regular files are copied
/// into `output_dir`. Seed and evolved namespaces cannot overwrite each other.
pub fn sanitize_corpus(
    restored_dir: &Path,
    fresh_seed_dir: &Path,
    input_dir: &Path,
    output_dir: &Path,
) -> anyhow::Result<SanitizedCorpusSummary> {
    validate_non_overlapping_paths(&[
        ("restored corpus", restored_dir),
        ("fresh corpus", fresh_seed_dir),
        ("input corpus", input_dir),
        ("output corpus", output_dir),
    ])?;
    ensure_directory_or_absent(restored_dir)?;
    ensure_directory(fresh_seed_dir)?;
    ensure_absent(input_dir)?;
    ensure_absent(output_dir)?;
    std::fs::create_dir_all(input_dir)?;
    std::fs::create_dir_all(output_dir)?;

    let fresh = load_corpus(fresh_seed_dir)?;
    let mut summary = SanitizedCorpusSummary::default();
    let mut total_bytes = 0u64;
    let mut total_files = 0usize;
    for seed in fresh {
        if total_files >= MAX_CORPUS_FILES
            || total_bytes.saturating_add(seed.len() as u64) > MAX_CORPUS_BYTES
        {
            summary.ignored_inputs += 1;
            continue;
        }
        let name = format!("seed-{:016x}-{:08x}.rue", fnv1a_64(&seed), seed.len());
        let path = input_dir.join(name);
        if path.exists() {
            // A same-name/different-content collision is never overwritten.
            if std::fs::read(&path)? != seed {
                summary.ignored_inputs += 1;
                continue;
            }
        } else {
            std::fs::write(path, &seed)?;
            total_files += 1;
            total_bytes += seed.len() as u64;
        }
        summary.fresh_seeds += 1;
    }

    let restored = if restored_dir.exists() {
        EvolvedCorpus::open_with_limits(restored_dir, MAX_EVOLVED_FILES, MAX_EVOLVED_BYTES)?
    } else {
        EvolvedCorpus::open_with_limits(restored_dir, 0, 0)?
    };
    let mut clean =
        EvolvedCorpus::open_with_limits(output_dir, MAX_EVOLVED_FILES, MAX_EVOLVED_BYTES)?;
    for name in restored.files.keys() {
        let input = match std::fs::read(restored_dir.join(name)) {
            Ok(input) => input,
            Err(_) => {
                summary.ignored_inputs += 1;
                continue;
            }
        };
        summary.restored_inputs += 1;
        if clean.record(&input)? {
            summary.retained_inputs += 1;
        } else {
            summary.ignored_inputs += 1;
        }
    }
    for name in clean.files.keys() {
        let input = std::fs::read(output_dir.join(name))?;
        if total_files >= MAX_CORPUS_FILES
            || total_bytes.saturating_add(input.len() as u64) > MAX_CORPUS_BYTES
        {
            break;
        }
        std::fs::write(input_dir.join(name), &input)?;
        total_files += 1;
        total_bytes += input.len() as u64;
    }
    summary.bytes = total_bytes;
    Ok(summary)
}

/// Publish a clean fuzz input tree into the cache path. GitHub Actions cache
/// restores and saves a path as the same workspace-relative archive root, so
/// this explicit, Rust-owned copy keeps the untrusted restore staging tree
/// separate during fuzzing while ensuring the next cache generation contains
/// only the sanitized, bounded tree.
pub fn publish_corpus(source_dir: &Path, cache_dir: &Path) -> anyhow::Result<usize> {
    validate_non_overlapping_paths(&[("clean corpus", source_dir), ("cache corpus", cache_dir)])?;
    ensure_directory(source_dir)?;
    ensure_directory_or_absent(cache_dir)?;
    let source = read_strict_evolved_entries(source_dir, MAX_EVOLVED_BYTES)?;
    let cache = if cache_dir.exists() {
        read_strict_evolved_entries(cache_dir, MAX_EVOLVED_BYTES)?
    } else {
        Vec::new()
    };

    // Both trees have been fully preflighted before any mutation. Replace only
    // immediate regular files; a nested directory or symlink is rejected above
    // rather than recursively deleted.
    for (name, _) in cache {
        std::fs::remove_file(cache_dir.join(name))?;
    }
    if !cache_dir.exists() {
        std::fs::create_dir_all(cache_dir)?;
    }
    for (name, content) in &source {
        std::fs::write(cache_dir.join(name), content)?;
    }
    Ok(source.len())
}

fn read_strict_evolved_entries(
    dir: &Path,
    byte_limit: u64,
) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let (mut entries, truncated) = read_entries_limited(dir, MAX_EVOLVED_FILES)?;
    if truncated {
        anyhow::bail!("corpus exceeds {} files", MAX_EVOLVED_FILES);
    }
    entries.sort_by_key(|entry| entry.file_name());
    let mut total_bytes = 0u64;
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            anyhow::bail!("corpus contains a non-regular immediate entry");
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((hash, length)) = parse_evolved_name(&name) else {
            anyhow::bail!("corpus contains a nonconforming evolved identity");
        };
        let content = std::fs::read(entry.path())?;
        let size = content.len() as u64;
        if size != length || fnv1a_64(&content) != hash {
            anyhow::bail!("corpus contains a spoofed evolved identity");
        }
        if size > MAX_CORPUS_FILE_BYTES || total_bytes.saturating_add(size) > byte_limit {
            anyhow::bail!("corpus exceeds its byte bound");
        }
        total_bytes += size;
        result.push((name, content));
    }
    Ok(result)
}

fn ensure_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("corpus path is not a regular directory: {}", path.display());
    }
    Ok(())
}

fn ensure_directory_or_absent(path: &Path) -> anyhow::Result<()> {
    if path.exists() || std::fs::symlink_metadata(path).is_ok() {
        ensure_directory(path)?;
    }
    Ok(())
}

fn ensure_absent(path: &Path) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        anyhow::bail!("corpus output must not already exist: {}", path.display());
    }
    Ok(())
}

fn validate_non_overlapping_paths(paths: &[(&str, &Path)]) -> anyhow::Result<()> {
    let current = lexical_absolute(Path::new("."))?;
    let mut absolute = Vec::with_capacity(paths.len());
    for (label, path) in paths {
        let normalized = lexical_absolute(path)?;
        if normalized == current || normalized.parent().is_none() {
            anyhow::bail!("{label} path is too broad: {}", path.display());
        }
        absolute.push((*label, normalized));
    }
    for (index, (left_label, left)) in absolute.iter().enumerate() {
        for (right_label, right) in absolute.iter().skip(index + 1) {
            if left == right || left.starts_with(right) || right.starts_with(left) {
                anyhow::bail!(
                    "{left_label} and {right_label} paths overlap: {} and {}",
                    left.display(),
                    right.display()
                );
            }
        }
    }
    Ok(())
}

fn lexical_absolute(path: &Path) -> anyhow::Result<std::path::PathBuf> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("corpus path is empty");
    }
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn read_entries_limited(
    dir: &Path,
    limit: usize,
) -> std::io::Result<(Vec<std::fs::DirEntry>, bool)> {
    let mut entries = Vec::new();
    let mut iter = std::fs::read_dir(dir)?;
    while entries.len() < limit {
        let Some(entry) = iter.next() else {
            return Ok((entries, false));
        };
        entries.push(entry?);
    }
    Ok((entries, iter.next().is_some()))
}

fn evolved_name(input: &[u8]) -> String {
    format!("input-{:016x}-{:08x}.bin", fnv1a_64(input), input.len())
}

fn parse_evolved_name(name: &str) -> Option<(u64, u64)> {
    let body = name.strip_prefix("input-")?.strip_suffix(".bin")?;
    let (hash, len) = body.split_once('-')?;
    if hash.len() != 16 || len.len() != 8 {
        return None;
    }
    Some((
        u64::from_str_radix(hash, 16).ok()?,
        u64::from_str_radix(len, 16).ok()?,
    ))
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

    #[test]
    fn load_corpus_ignores_symlinks_and_oversized_files() {
        let dir = scratch("untrusted-inputs");
        write(&dir, "valid", "not executable, just bytes");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", dir.join("outside")).unwrap();
        write(
            &dir,
            "oversized",
            &"x".repeat((MAX_CORPUS_FILE_BYTES + 1) as usize),
        );

        let corpus = load_corpus(&dir).unwrap();
        assert_eq!(corpus, vec![b"not executable, just bytes".to_vec()]);
    }

    #[test]
    fn evolved_corpus_is_content_addressed_bounded_and_idempotent() {
        let dir = scratch("evolved");
        let mut evolved = EvolvedCorpus::open(&dir).unwrap();
        assert!(evolved.record(b"one").unwrap());
        assert!(!evolved.record(b"one").unwrap());
        assert!(!evolved.record(b"one").unwrap());
        assert_eq!(evolved.file_count(), 1);

        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("input-")
        );
    }

    #[test]
    fn evolved_corpus_does_not_save_oversized_inputs() {
        let dir = scratch("evolved-limit");
        let mut evolved = EvolvedCorpus::open(&dir).unwrap();
        assert!(
            !evolved
                .record(&vec![0u8; (MAX_CORPUS_FILE_BYTES + 1) as usize])
                .unwrap()
        );
        assert_eq!(evolved.file_count(), 0);
    }

    #[test]
    fn evolved_corpus_rejects_spoofed_content_addresses() {
        let dir = scratch("evolved-spoof");
        std::fs::write(dir.join("input-0000000000000000-00000003.bin"), b"bad").unwrap();
        std::fs::write(dir.join("input-not-a-hash-00000003.bin"), b"bad").unwrap();
        let evolved = EvolvedCorpus::open(&dir).unwrap();
        assert_eq!(evolved.file_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn evolved_corpus_rejects_root_symlink() {
        let parent = scratch("evolved-root-symlink");
        let actual = parent.join("actual");
        let link = parent.join("link");
        std::fs::create_dir_all(&actual).unwrap();
        std::os::unix::fs::symlink(&actual, &link).unwrap();
        assert!(EvolvedCorpus::open(&link).is_err());
    }

    #[test]
    fn evolved_corpus_replaces_largest_identity_at_file_cap() {
        let dir = scratch("evolved-rotation");
        let inputs: [&[u8]; 4] = [b"first", b"second", b"third", b"fourth"];
        let mut names: Vec<_> = inputs.iter().map(|input| evolved_name(input)).collect();
        names.sort();

        let mut evolved = EvolvedCorpus::open_with_limits(&dir, 2, MAX_EVOLVED_BYTES).unwrap();
        for input in inputs {
            evolved.record(input).unwrap();
        }

        let retained: Vec<_> = evolved.files.keys().cloned().collect();
        assert_eq!(retained, names.into_iter().take(2).collect::<Vec<_>>());
        assert_eq!(evolved.file_count(), 2);
    }

    #[test]
    fn evolved_corpus_enforces_byte_cap_during_replacement() {
        let dir = scratch("evolved-byte-rotation");
        let mut evolved = EvolvedCorpus::open_with_limits(&dir, 8, 5).unwrap();
        evolved.record(b"four").unwrap();
        evolved.record(b"five!").unwrap();
        evolved.record(b"sixsix").unwrap();

        assert!(evolved.bytes <= 5);
        assert!(evolved.files.values().copied().sum::<u64>() <= 5);
    }

    #[test]
    fn sanitize_corpus_keeps_fresh_seed_namespace_and_rejects_untrusted_entries() {
        let parent = scratch("sanitize");
        let restored = parent.join("restored");
        let fresh = parent.join("fresh");
        let input = parent.join("input");
        let output = parent.join("output");
        std::fs::create_dir_all(&restored).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        let evolved = b"restored input";
        std::fs::write(restored.join(evolved_name(evolved)), evolved).unwrap();
        write(&restored, "spoof.bin", "not content addressed");
        write(&fresh, "spec-seed", "fresh source");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", restored.join("outside")).unwrap();

        let summary = sanitize_corpus(&restored, &fresh, &input, &output).unwrap();
        assert_eq!(summary.fresh_seeds, 1);
        assert_eq!(summary.restored_inputs, 1);
        assert_eq!(summary.retained_inputs, 1);
        assert!(input.join(evolved_name(evolved)).is_file());
        assert!(output.join(evolved_name(evolved)).is_file());
        assert_eq!(
            std::fs::read_dir(&input).unwrap().count(),
            2,
            "fresh and evolved inputs must be available to the target"
        );
        assert_eq!(
            std::fs::read_dir(&output).unwrap().count(),
            1,
            "evolved output must exclude fresh seeds"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sanitize_corpus_rejects_root_symlink() {
        let parent = scratch("sanitize-symlink");
        let actual = parent.join("actual");
        let restored = parent.join("restored");
        let fresh = parent.join("fresh");
        let output = parent.join("output");
        let input = parent.join("input");
        std::fs::create_dir_all(&actual).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::os::unix::fs::symlink(&actual, &restored).unwrap();

        assert!(sanitize_corpus(&restored, &fresh, &input, &output).is_err());
    }

    #[test]
    fn sanitize_corpus_rejects_current_and_overlapping_paths_before_mutation() {
        let root = scratch("sanitize-paths");
        let fresh = root.join("fresh");
        let input = root.join("input");
        let output = root.join("output");
        std::fs::create_dir_all(&fresh).unwrap();
        assert!(sanitize_corpus(Path::new("."), &fresh, &input, &output).is_err());
        assert!(sanitize_corpus(Path::new("/"), &fresh, &input, &output).is_err());
        assert!(!input.exists());
        assert!(!output.exists());

        let parent = scratch("sanitize-paths-overlap");
        let restored = parent.join("restored");
        let fresh = parent.join("fresh");
        let input = parent.join("input");
        let output = input.join("nested-output");
        std::fs::create_dir_all(&restored).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        assert!(sanitize_corpus(&restored, &fresh, &input, &output).is_err());
        assert!(!input.exists());
    }

    #[test]
    fn publish_corpus_replaces_staging_with_only_clean_content() {
        let clean = scratch("publish-clean");
        let cache = scratch("publish-cache");
        let input = b"clean evolved input";
        std::fs::write(clean.join(evolved_name(input)), input).unwrap();
        let old = b"old evolved input";
        std::fs::write(cache.join(evolved_name(old)), old).unwrap();

        assert_eq!(publish_corpus(&clean, &cache).unwrap(), 1);
        assert!(!cache.join(evolved_name(old)).exists());
        assert!(cache.join(evolved_name(input)).is_file());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
    }

    #[test]
    fn publish_corpus_rejects_nested_cache_without_partial_deletion() {
        let clean = scratch("publish-nested-clean");
        let cache = scratch("publish-nested-cache");
        let input = b"clean evolved input";
        let old = b"old evolved input";
        std::fs::write(clean.join(evolved_name(input)), input).unwrap();
        std::fs::write(cache.join(evolved_name(old)), old).unwrap();
        std::fs::create_dir(cache.join("nested")).unwrap();

        assert!(publish_corpus(&clean, &cache).is_err());
        assert!(cache.join(evolved_name(old)).is_file());
        assert!(cache.join("nested").is_dir());
    }
}
