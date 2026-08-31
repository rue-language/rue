//! Query runtime tests, split by subsystem ownership.

use ahash::AHashSet;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Condvar;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{OnceLock, Weak};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use super::*;

mod concurrency_join_cancellation;
mod cross_subsystem_protocol;
mod cycles;
mod diagnostics;
mod fixtures;
mod identity_digest;
mod retention_leases_adoption;
mod revision_red_green;
mod work_task_accounting;

const MODULE_SOURCES: &[(&str, &str)] = &[
    ("identity_digest.rs", include_str!("identity_digest.rs")),
    (
        "revision_red_green.rs",
        include_str!("revision_red_green.rs"),
    ),
    ("cycles.rs", include_str!("cycles.rs")),
    (
        "concurrency_join_cancellation.rs",
        include_str!("concurrency_join_cancellation.rs"),
    ),
    (
        "retention_leases_adoption.rs",
        include_str!("retention_leases_adoption.rs"),
    ),
    ("diagnostics.rs", include_str!("diagnostics.rs")),
    (
        "work_task_accounting.rs",
        include_str!("work_task_accounting.rs"),
    ),
    (
        "cross_subsystem_protocol.rs",
        include_str!("cross_subsystem_protocol.rs"),
    ),
];

/// Machine-readable source/name inventory captured before the mechanical move.
const ORIGINAL_TEST_INVENTORY: &str = include_str!("original_test_inventory.rs");

fn declared_tests(source: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "#[test]" {
            let declaration = lines
                .next()
                .expect("test attribute must have a declaration");
            let name = declaration
                .trim()
                .strip_prefix("fn ")
                .and_then(|rest| rest.split('(').next())
                .expect("test declaration must be a function");
            names.push(name);
        }
    }
    names
}

/// Proves the relocation is an exact, one-owner move of the captured baseline.
#[test]
fn query_test_source_inventory_is_exact_and_canonical() {
    let expected: Vec<(&str, &str)> = ORIGINAL_TEST_INVENTORY
        .lines()
        .filter(|line| line.contains('\t'))
        .map(|line| {
            line.split_once('\t')
                .expect("inventory lines use name\tsource")
        })
        .collect();
    assert_eq!(
        expected.len(),
        173,
        "baseline test count changed unexpectedly"
    );

    let canonical_sources: AHashSet<&str> = MODULE_SOURCES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        canonical_sources.len(),
        8,
        "canonical source inventory has duplicate owners"
    );
    let declared_source_names: AHashSet<&str> =
        expected.iter().map(|(_, source)| *source).collect();
    assert_eq!(declared_source_names, canonical_sources);

    let mut observed = Vec::new();
    for (source_name, source) in MODULE_SOURCES {
        for test_name in declared_tests(source) {
            observed.push((test_name, *source_name));
        }
    }

    let mut expected_counts = std::collections::BTreeMap::new();
    for (name, source) in &expected {
        assert!(
            canonical_sources.contains(source),
            "unknown canonical source for {name}"
        );
        *expected_counts.entry(*name).or_insert(0usize) += 1;
    }
    assert!(
        expected_counts.values().all(|count| *count == 1),
        "baseline contains duplicate test names"
    );

    let mut observed_counts = std::collections::BTreeMap::new();
    for (name, source) in &observed {
        *observed_counts.entry(*name).or_insert(0usize) += 1;
        let declared = expected
            .iter()
            .find(|(expected_name, _)| expected_name == name);
        assert!(
            declared.is_some(),
            "unlisted relocated test {name} appears in {source}"
        );
        assert_eq!(
            declared.unwrap().1,
            *source,
            "{name} moved to the wrong owner"
        );
    }
    assert_eq!(
        observed_counts, expected_counts,
        "test names were lost or duplicated"
    );
    assert_eq!(observed.len(), expected.len());
}
