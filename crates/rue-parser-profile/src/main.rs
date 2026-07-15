//! Allocation-counting driver for the isolated Rue parser kernel.

use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::fs;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use lasso::ThreadedRodeo;
use rue_lexer::Lexer;
use rue_parser::Parser;
use rue_span::FileId;
use serde_json::json;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        new_pointer
    }
}

fn usage() -> ! {
    eprintln!("usage: rue-parser-profile FILE [FILE ...]");
    process::exit(2);
}

fn main() {
    let paths: Vec<String> = env::args().skip(1).collect();
    if paths.is_empty() {
        usage();
    }

    let sources: Vec<String> = paths
        .iter()
        .map(|path| {
            fs::read_to_string(path).unwrap_or_else(|error| {
                eprintln!("cannot read {path}: {error}");
                process::exit(2);
            })
        })
        .collect();
    match profile_sources(&sources) {
        Ok(profile) => println!("{profile}"),
        Err(index) => {
            eprintln!("{}: fixture must lex successfully", paths[index]);
            process::exit(2);
        }
    }
}

fn profile_sources(sources: &[String]) -> Result<serde_json::Value, usize> {
    let mut interner = ThreadedRodeo::new();
    let mut parser_ns = 0_u128;
    let mut allocations = 0_u64;
    let mut allocated_bytes = 0_u64;
    let mut source_bytes = 0_usize;
    let mut tokens = 0_usize;
    let mut outcome = "success";

    for (source_index, source) in sources.iter().enumerate() {
        source_bytes += source.len();
        let lexer = Lexer::with_interner_and_file_id(source.as_str(), interner, FileId::default());
        let (file_tokens, next_interner) = lexer
            .tokenize_preserving_interner()
            .map_err(|(_errors, _interner)| source_index)?;
        tokens += file_tokens.len();

        ALLOCATIONS.store(0, Ordering::SeqCst);
        ALLOCATED_BYTES.store(0, Ordering::SeqCst);
        let started = Instant::now();
        let parsed = Parser::new(file_tokens, next_interner).parse_preserving_interner();
        parser_ns += started.elapsed().as_nanos();
        allocations += ALLOCATIONS.load(Ordering::SeqCst);
        allocated_bytes += ALLOCATED_BYTES.load(Ordering::SeqCst);

        match parsed {
            Ok((_ast, next_interner)) => interner = next_interner,
            Err((_errors, next_interner)) => {
                interner = next_interner;
                outcome = "parse_error";
            }
        }
    }

    Ok(json!({
        "schema_version": 1,
        "files": sources.len(),
        "source_bytes": source_bytes,
        "tokens": tokens,
        "outcome": outcome,
        "parser_ns": parser_ns,
        "allocations": allocations,
        "allocated_bytes": allocated_bytes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_smoke_covers_success_error_and_measured_allocation_boundary() {
        let success = profile_sources(&["fn main() -> i32 { 42 }".to_string()]).unwrap();
        assert_eq!(success["schema_version"], 1);
        assert_eq!(success["files"], 1);
        assert_eq!(success["outcome"], "success");
        assert!(success["tokens"].as_u64().unwrap() > 0);
        assert!(success["parser_ns"].as_u64().unwrap() > 0);
        assert!(success["allocations"].as_u64().unwrap() > 0);
        assert!(success["allocated_bytes"].as_u64().unwrap() > 0);

        let malformed =
            profile_sources(&["fn broken() -> i32 { let value: = 0; value }".to_string()]).unwrap();
        assert_eq!(malformed["outcome"], "parse_error");
        assert!(malformed["allocations"].as_u64().unwrap() > 0);
        assert!(malformed["allocated_bytes"].as_u64().unwrap() > 0);
    }
}
