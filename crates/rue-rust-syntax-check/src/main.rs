//! Runs the experimental Rust frontend over a corpus of real Rust files.
//!
//! ```text
//! rue-rust-syntax-check [--edits N] [--syn-diff N] [--rustc PATH --rustc-diff N]
//!                       [--show N] [--edition YEAR] PATH...
//! ```
//!
//! For every `.rs` file under the given paths it checks that:
//!
//! - the tree reproduces the file byte for byte;
//! - inline, deferred and parallel parsing produce identical trees;
//! - the file parses without errors (reported, not fatal: a corpus can hold
//!   intentionally invalid files);
//! - with `--edits N`, N random edits applied through `Document::edit` leave
//!   the same tree as reparsing the edited text from scratch.
//!
//! With `--syn-diff N` it also applies N independent random edits to each
//! file and reports every edited text that `syn` accepts but this parser
//! rejects: a grammar gap. Some disagreements are expected because `syn` is
//! more lenient than rustc, and are skipped: it does not reserve 2021
//! prefixes (`foo#`) or reject empty exponents (`1e`), and it accepts
//! anonymous parameters in any function. (The reverse direction is expected
//! too: this parser accepts some code that only later phases reject, such as
//! items in the wrong place.)
//!
//! With `--rustc PATH --rustc-diff N` it compares against rustc's own parser
//! (`-Zparse-crate-root-only`, enabled on a stable toolchain through
//! `RUSTC_BOOTSTRAP=1`; a local testing aid only) on each file and N random
//! edits of it. Texts rustc accepts but this parser rejects are grammar gaps
//! and fail the run; texts this parser accepts but rustc rejects are counted
//! and sampled, since leniency is sometimes deliberate.
//!
//! It then reports throughput for whole-corpus parsing with and without
//! intra-file parallelism. A file's edition comes from the nearest
//! `Cargo.toml` above it, defaulting to 2024 (Rue's own crates use Buck).

use rayon::prelude::*;
use rue_rust_syntax::{Document, Edition, Parallelism, ParseOptions, Reparse, TextRange, parse};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

struct Args {
    paths: Vec<PathBuf>,
    edits: usize,
    syn_diff: usize,
    rustc: Option<PathBuf>,
    rustc_diff: usize,
    show: usize,
    edition: Option<Edition>,
}

struct File {
    path: PathBuf,
    text: String,
    edition: Edition,
}

#[derive(Default)]
struct Outcome {
    lossless: bool,
    modes_agree: bool,
    first_error: Option<String>,
    n_errors: usize,
    edit_failures: Vec<String>,
    syn_gaps: Vec<String>,
    rustc_gaps: Vec<String>,
    rustc_lenient: Vec<String>,
    group_reparses: usize,
    full_reparses: usize,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "usage: rue-rust-syntax-check [--edits N] [--show N] [--edition YEAR] PATH..."
            );
            return ExitCode::from(2);
        }
    };

    let mut paths = Vec::new();
    for path in &args.paths {
        collect_rs_files(path, &mut paths);
    }
    paths.sort();
    let files: Vec<File> = paths
        .into_par_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let edition = args.edition.unwrap_or_else(|| edition_for(&path));
            Some(File {
                path,
                text,
                edition,
            })
        })
        .collect();
    let total_bytes: usize = files.iter().map(|f| f.text.len()).sum();
    println!(
        "{} files, {:.1} MiB",
        files.len(),
        total_bytes as f64 / (1024.0 * 1024.0)
    );

    let outcomes: Vec<Outcome> = files.par_iter().map(|f| check(f, &args)).collect();

    let mut ok = true;
    let mut with_errors = Vec::new();
    let (mut groups, mut fulls) = (0, 0);
    let mut lenient = Vec::new();
    for (file, outcome) in files.iter().zip(&outcomes) {
        if !outcome.lossless {
            ok = false;
            println!("NOT LOSSLESS: {}", file.path.display());
        }
        if !outcome.modes_agree {
            ok = false;
            println!("MODES DISAGREE: {}", file.path.display());
        }
        for failure in &outcome.edit_failures {
            ok = false;
            println!("EDIT MISMATCH: {}: {failure}", file.path.display());
        }
        if let Some(error) = &outcome.first_error {
            with_errors.push((file, outcome.n_errors, error));
        }
        for gap in &outcome.syn_gaps {
            ok = false;
            println!("SYN ACCEPTS, WE REJECT: {}: {gap}", file.path.display());
        }
        for gap in &outcome.rustc_gaps {
            ok = false;
            println!("RUSTC ACCEPTS, WE REJECT: {}: {gap}", file.path.display());
        }
        lenient.extend(outcome.rustc_lenient.iter().map(|l| (file, l)));
        groups += outcome.group_reparses;
        fulls += outcome.full_reparses;
    }
    println!(
        "{} of {} files have syntax errors",
        with_errors.len(),
        files.len()
    );
    for (file, n, error) in with_errors.iter().take(args.show) {
        println!("  {} ({n} errors): {error}", file.path.display());
    }
    if args.edits > 0 {
        println!("edits: {groups} group reparses, {fulls} full reparses");
    }
    if args.rustc_diff > 0 {
        println!(
            "{} texts accepted here but rejected by rustc",
            lenient.len()
        );
        for (file, text) in lenient.iter().take(args.show) {
            println!("  {}: {text}", file.path.display());
        }
    }

    benchmark(&files);

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn check(file: &File, args: &Args) -> Outcome {
    let (edits, syn_diff) = (args.edits, args.syn_diff);
    let options = |parallelism| ParseOptions {
        edition: file.edition,
        parallelism,
    };
    let inline = parse(&file.text, options(Parallelism::Inline));
    let deferred = parse(&file.text, options(Parallelism::Deferred));
    let parallel = parse(&file.text, options(Parallelism::Parallel));

    let mut outcome = Outcome {
        lossless: inline.green().text() == file.text,
        modes_agree: inline == deferred && inline == parallel,
        n_errors: inline.errors().len(),
        ..Outcome::default()
    };
    if let Some(error) = inline.errors().first() {
        let (line, col) = line_col(&file.text, error.range.start);
        let snippet: String = file.text[error.range.start as usize..]
            .chars()
            .take(30)
            .collect();
        outcome.first_error = Some(format!("{line}:{col}: {} at {snippet:?}", error.message));
    }

    if edits > 0 {
        let mut document = Document::new(file.text.clone(), options(Parallelism::Deferred));
        let mut rng = Rng::new(&file.text);
        for i in 0..edits {
            let (range, insert) = random_edit(&mut rng, document.text());
            match document.edit(range, &insert) {
                Reparse::Group { .. } => outcome.group_reparses += 1,
                Reparse::Full => outcome.full_reparses += 1,
            }
            let fresh = parse(document.text(), options(Parallelism::Inline));
            if &fresh != document.parse() {
                outcome.edit_failures.push(format!(
                    "edit #{i} replacing {range} with {insert:?} diverged from a fresh parse"
                ));
                break;
            }
        }
    }
    if syn_diff > 0 {
        let mut rng = Rng::new(&file.text);
        for _ in 0..syn_diff {
            let (range, insert) = random_edit(&mut rng, &file.text);
            let mut text = file.text.clone();
            text.replace_range(range.start as usize..range.end as usize, &insert);
            let ours = parse(&text, options(Parallelism::Inline));
            let expected_disagreement = ours.errors().iter().all(|error| {
                error.message.starts_with("prefix `")
                    || error.message == "expected at least one digit in exponent"
                    || error.message == "missing type for function parameter"
            });
            if !ours.errors().is_empty() && !expected_disagreement && syn::parse_file(&text).is_ok()
            {
                let error = &ours.errors()[0];
                let (line, col) = line_col(&text, error.range.start);
                let mut from = error.range.start.saturating_sub(40) as usize;
                while !text.is_char_boundary(from) {
                    from -= 1;
                }
                let context: String = text[from..].chars().take(80).collect();
                outcome.syn_gaps.push(format!(
                    "replacing {range} with {insert:?}: {line}:{col}: {} near {context:?}",
                    error.message
                ));
            }
        }
    }
    if let (Some(rustc), true) = (&args.rustc, args.rustc_diff > 0) {
        let mut rng = Rng::new(&file.text);
        for i in 0..=args.rustc_diff {
            // Check the file itself first, then its edits.
            let (range, insert) = if i == 0 {
                (TextRange::empty(0), String::new())
            } else {
                random_edit(&mut rng, &file.text)
            };
            let mut text = file.text.clone();
            text.replace_range(range.start as usize..range.end as usize, &insert);
            let ours = parse(&text, options(Parallelism::Inline));
            let Some((rustc_ok, rustc_error)) = rustc_accepts(rustc, &text, file.edition) else {
                continue;
            };
            let what = format!("replacing {range} with {insert:?}");
            match (rustc_ok, ours.errors().first()) {
                (true, Some(error)) => {
                    let (line, col) = line_col(&text, error.range.start);
                    outcome
                        .rustc_gaps
                        .push(format!("{what}: {line}:{col}: {}", error.message));
                }
                (false, None) => outcome.rustc_lenient.push(format!("{what}: {rustc_error}")),
                _ => {}
            }
        }
    }
    outcome
}

/// Whether rustc's parser accepts `text`, with its first error message, or
/// `None` if rustc could not run.
fn rustc_accepts(rustc: &Path, text: &str, edition: Edition) -> Option<(bool, String)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("rue-rust-syntax-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.rs", COUNTER.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&path, text).ok()?;
    let edition = match edition {
        Edition::Edition2015 => "2015",
        Edition::Edition2018 => "2018",
        Edition::Edition2021 => "2021",
        Edition::Edition2024 => "2024",
    };
    let output = std::process::Command::new(rustc)
        .env("RUSTC_BOOTSTRAP", "1")
        .args([
            "--edition",
            edition,
            "-Zparse-crate-root-only",
            "--error-format=short",
        ])
        .arg(&path)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_error = stderr
        .lines()
        .find(|line| line.contains("error"))
        .map(|line| {
            line.split_once(": ")
                .map_or(line, |(_, rest)| rest)
                .to_string()
        })
        .unwrap_or_default();
    Some((output.status.success(), first_error))
}

fn benchmark(files: &[File]) {
    let options = |file: &File, parallelism| ParseOptions {
        edition: file.edition,
        parallelism,
    };
    let bytes: usize = files.iter().map(|f| f.text.len()).sum();
    let mib = bytes as f64 / (1024.0 * 1024.0);
    let time = |label: &str, f: &dyn Fn()| {
        // The minimum of a few runs is the least noisy estimate on a shared
        // machine.
        let secs = (0..3)
            .map(|_| {
                let start = Instant::now();
                f();
                start.elapsed().as_secs_f64()
            })
            .fold(f64::INFINITY, f64::min);
        println!(
            "  {label:<46} {:>8.1} ms {:>8.1} MiB/s",
            secs * 1e3,
            mib / secs
        );
    };
    println!("throughput ({} threads):", rayon::current_num_threads());
    time("one thread, inline", &|| {
        for file in files {
            std::hint::black_box(parse(&file.text, options(file, Parallelism::Inline)));
        }
    });
    time("files one at a time, bodies in parallel", &|| {
        for file in files {
            std::hint::black_box(parse(&file.text, options(file, Parallelism::Parallel)));
        }
    });
    time("files in parallel, inline", &|| {
        files.par_iter().for_each(|file| {
            std::hint::black_box(parse(&file.text, options(file, Parallelism::Inline)));
        });
    });
    time("files in parallel, bodies in parallel", &|| {
        files.par_iter().for_each(|file| {
            std::hint::black_box(parse(&file.text, options(file, Parallelism::Parallel)));
        });
    });

    if let Some(largest) = files.iter().max_by_key(|f| f.text.len()) {
        println!(
            "largest file: {} ({:.0} KiB)",
            largest.path.display(),
            largest.text.len() as f64 / 1024.0
        );
        let mib = largest.text.len() as f64 / (1024.0 * 1024.0);
        let secs = (0..10)
            .map(|_| {
                let start = Instant::now();
                std::hint::black_box(rue_rust_lexer::tokenize(&largest.text));
                start.elapsed().as_secs_f64()
            })
            .fold(f64::INFINITY, f64::min);
        println!(
            "  {:<46} {:>8.2} ms {:>8.1} MiB/s",
            "lexing alone",
            secs * 1e3,
            mib / secs
        );
        for parallelism in [Parallelism::Inline, Parallelism::Parallel] {
            let options = options(largest, parallelism);
            // Time the parse alone: dropping a large tree is a separate,
            // sequential cost.
            let secs = (0..10)
                .map(|_| {
                    let start = Instant::now();
                    let parse = std::hint::black_box(parse(&largest.text, options));
                    let secs = start.elapsed().as_secs_f64();
                    drop(parse);
                    secs
                })
                .fold(f64::INFINITY, f64::min);
            println!(
                "  {:<46} {:>8.2} ms {:>8.1} MiB/s",
                format!("{parallelism:?}"),
                secs * 1e3,
                mib / secs
            );
        }
    }
}

const SNIPPETS: &[&str] = &[
    " ",
    "x",
    "1",
    ";",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "\"",
    "'",
    "/*",
    "*/",
    "//",
    "\n",
    "let y = 2;",
    "foo()",
    "a.0.1",
    "<",
    ">",
    "::",
    ",",
    "=>",
    "fn",
    "if x {}",
    "r#\"",
    "#",
    "!",
    "'a",
];

fn random_edit(rng: &mut Rng, text: &str) -> (TextRange, String) {
    let len = text.len();
    let mut start = rng.below(len + 1);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start;
    if rng.below(3) == 0 {
        end = (start + 1 + rng.below(8)).min(len);
        while !text.is_char_boundary(end) {
            end += 1;
        }
    }
    let insert = if end > start && rng.below(2) == 0 {
        String::new()
    } else {
        SNIPPETS[rng.below(SNIPPETS.len())].to_string()
    };
    (TextRange::new(start as u32, end as u32), insert)
}

/// xorshift64*, seeded from the file so runs are reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed_text: &str) -> Rng {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for byte in seed_text.bytes().take(4096) {
            seed = (seed ^ byte as u64).wrapping_mul(0x100_0000_01B3);
        }
        Rng(seed | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n.max(1) as u64) as usize
    }
}

fn line_col(text: &str, offset: u32) -> (usize, usize) {
    let before = &text[..offset as usize];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn collect_rs_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "buck-out" {
            continue;
        }
        collect_rs_files(&path, out);
    }
}

/// The edition declared by the nearest `Cargo.toml` above `path`.
fn edition_for(path: &Path) -> Edition {
    for dir in path.ancestors().skip(1) {
        let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        for line in manifest.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("edition") {
                let value = value
                    .trim_start_matches([' ', '='])
                    .trim_matches(['"', ' ']);
                return match value {
                    "2015" => Edition::Edition2015,
                    "2018" => Edition::Edition2018,
                    "2021" => Edition::Edition2021,
                    _ => Edition::Edition2024,
                };
            }
        }
        // A manifest without an edition key is 2015.
        return Edition::Edition2015;
    }
    Edition::Edition2024
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        paths: Vec::new(),
        edits: 0,
        syn_diff: 0,
        rustc: None,
        rustc_diff: 0,
        show: 20,
        edition: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| iter.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--edits" => args.edits = value("--edits")?.parse().map_err(|e| format!("{e}"))?,
            "--syn-diff" => {
                args.syn_diff = value("--syn-diff")?.parse().map_err(|e| format!("{e}"))?
            }
            "--rustc" => args.rustc = Some(PathBuf::from(value("--rustc")?)),
            "--rustc-diff" => {
                args.rustc_diff = value("--rustc-diff")?.parse().map_err(|e| format!("{e}"))?
            }
            "--show" => args.show = value("--show")?.parse().map_err(|e| format!("{e}"))?,
            "--edition" => {
                args.edition = Some(match value("--edition")?.as_str() {
                    "2015" => Edition::Edition2015,
                    "2018" => Edition::Edition2018,
                    "2021" => Edition::Edition2021,
                    "2024" => Edition::Edition2024,
                    other => return Err(format!("unknown edition {other}")),
                })
            }
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => args.paths.push(PathBuf::from(arg)),
        }
    }
    if args.rustc_diff > 0 && args.rustc.is_none() {
        return Err("--rustc-diff needs --rustc".to_string());
    }
    if args.paths.is_empty() {
        return Err("no paths given".to_string());
    }
    Ok(args)
}
