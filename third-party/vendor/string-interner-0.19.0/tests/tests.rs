mod allocator;

use allocator::TracingAllocator;
use string_interner::{backend, DefaultHashBuilder, DefaultSymbol, Symbol};

#[global_allocator]
static ALLOCATOR: TracingAllocator = TracingAllocator::new();

/// Creates the symbol `S` from the given `usize`.
///
/// # Panics
///
/// Panics if the conversion is invalid.
#[inline]
pub(crate) fn expect_valid_symbol<S>(index: usize) -> S
where
    S: Symbol,
{
    S::try_from_usize(index).expect("encountered invalid symbol")
}

/// Stats for the backend.
pub trait BackendStats {
    /// The expected minimum memory overhead for this string interner backend.
    const MIN_OVERHEAD: f64;
    /// The expected maximum memory overhead for this string interner backend.
    const MAX_OVERHEAD: f64;
    /// The amount of allocations per 1M words.
    const MAX_ALLOCATIONS: usize;
    /// The amount of deallocations per 1M words.
    const MAX_DEALLOCATIONS: usize;
    /// The name of the backend for debug display purpose.
    const NAME: &'static str;
}

impl BackendStats for backend::BucketBackend<DefaultSymbol> {
    const MIN_OVERHEAD: f64 = 2.2;
    const MAX_OVERHEAD: f64 = 3.1;
    const MAX_ALLOCATIONS: usize = 65;
    const MAX_DEALLOCATIONS: usize = 42;
    const NAME: &'static str = "BucketBackend";
}

impl BackendStats for backend::StringBackend<DefaultSymbol> {
    const MIN_OVERHEAD: f64 = 1.7;
    const MAX_OVERHEAD: f64 = 1.93;
    const MAX_ALLOCATIONS: usize = 62;
    const MAX_DEALLOCATIONS: usize = 59;
    const NAME: &'static str = "StringBackend";
}

impl BackendStats for backend::BufferBackend<DefaultSymbol> {
    const MIN_OVERHEAD: f64 = 1.35;
    const MAX_OVERHEAD: f64 = 1.58;
    const MAX_ALLOCATIONS: usize = 43;
    const MAX_DEALLOCATIONS: usize = 41;
    const NAME: &'static str = "BufferBackend";
}

/// Memory profiling stats.
pub struct ProfilingStats {
    /// The minimum memory usage overhead as factor.
    pub overhead: f64,
    /// The total amount of allocations of the profiling test.
    pub allocations: usize,
    /// The total amount of deallocations of the profiling test.
    pub deallocations: usize,
}

macro_rules! gen_tests_for_backend {
    ( $backend:ty ) => {
        type StringInterner =
            string_interner::StringInterner<$backend, DefaultHashBuilder>;

        fn profile_memory_usage(words: &[String]) -> ProfilingStats {
            ALLOCATOR.reset();
            ALLOCATOR.start_profiling();
            let mut interner = StringInterner::new();
            ALLOCATOR.end_profiling();

            for word in words {
                ALLOCATOR.start_profiling();
                interner.get_or_intern(word);
            }
            interner.shrink_to_fit();
            ALLOCATOR.end_profiling();

            let stats = ALLOCATOR.stats();
            let len_allocations = stats.len_allocations();
            let len_deallocations = stats.len_deallocations();
            let current_allocated_bytes = stats.current_allocated_bytes();
            let total_allocated_bytes = stats.total_allocated_bytes();

            assert_eq!(interner.len(), words.len());

            println!(
                "\
                \n\t- # words         = {}\
                \n\t- # allocations   = {}\
                \n\t- # deallocations = {}\
                \n\t- allocated bytes = {}\
                \n\t- requested bytes = {}\
                ",
                words.len(),
                len_allocations, len_deallocations, current_allocated_bytes, total_allocated_bytes,
            );

            let ideal_memory_usage = words.len() * words[0].len();
            let memory_usage_overhead =
                (current_allocated_bytes as f64) / (ideal_memory_usage as f64);
            println!("\t- ideal allocated bytes  = {}", ideal_memory_usage);
            println!("\t- actual allocated bytes = {}", current_allocated_bytes);
            println!("\t- % actual overhead      = {:.02}%", memory_usage_overhead * 100.0);

            ProfilingStats {
                overhead: memory_usage_overhead,
                allocations: len_allocations,
                deallocations: len_deallocations,
            }
        }

        #[test]
        #[cfg_attr(any(miri, not(feature = "test-allocations")), ignore)]
        fn test_memory_consumption() {
            let len_words = 1_000_000;
            let words = (0..).take(len_words).map(|i| {
                format!("{:20}", i)
            }).collect::<Vec<_>>();

            println!();
            println!("Benchmark Memory Usage for {}", <$backend as BackendStats>::NAME);
            let mut min_overhead = None;
            let mut max_overhead = None;
            let mut max_allocations = None;
            let mut max_deallocations = None;
            for i in 0..10 {
                let len_words = 100_000 * (i+1);
                let words = &words[0..len_words];
                let stats = profile_memory_usage(words);
                if min_overhead.map(|min| stats.overhead < min).unwrap_or(true) {
                    min_overhead = Some(stats.overhead);
                }
                if max_overhead.map(|max| stats.overhead > max).unwrap_or(true) {
                    max_overhead = Some(stats.overhead);
                }
                if max_allocations.map(|max| stats.allocations > max).unwrap_or(true) {
                    max_allocations = Some(stats.allocations);
                }
                if max_deallocations.map(|max| stats.deallocations > max).unwrap_or(true) {
                    max_deallocations = Some(stats.deallocations);
                }
            }
            let actual_min_overhead = min_overhead.unwrap();
            let actual_max_overhead = max_overhead.unwrap();
            let expect_min_overhead = <$backend as BackendStats>::MIN_OVERHEAD;
            let expect_max_overhead = <$backend as BackendStats>::MAX_OVERHEAD;
            let actual_max_allocations = max_allocations.unwrap();
            let actual_max_deallocations = max_deallocations.unwrap();
            let expect_max_allocations = <$backend as BackendStats>::MAX_ALLOCATIONS;
            let expect_max_deallocations = <$backend as BackendStats>::MAX_DEALLOCATIONS;

            println!();
            println!("- % min overhead      = {:.02}%", actual_min_overhead * 100.0);
            println!("- % max overhead      = {:.02}%", actual_max_overhead * 100.0);
            println!("- % max allocations   = {}", actual_max_allocations);
            println!("- % max deallocations = {}", actual_max_deallocations);

            assert!(
                actual_min_overhead < expect_min_overhead,
                "{} string interner backend minimum memory overhead is greater than expected. expected = {:?}, actual = {:?}",
                <$backend as BackendStats>::NAME,
                expect_min_overhead,
                actual_min_overhead,
            );
            assert!(
                actual_max_overhead < expect_max_overhead,
                "{} string interner backend maximum memory overhead is greater than expected. expected = {:?}, actual = {:?}",
                <$backend as BackendStats>::NAME,
                expect_max_overhead,
                actual_max_overhead,
            );
            assert_eq!(
                actual_max_allocations, expect_max_allocations,
                "{} string interner backend maximum amount of allocations is greater than expected. expected = {:?}, actual = {:?}",
                <$backend as BackendStats>::NAME,
                expect_max_allocations,
                actual_max_allocations,
            );
            assert_eq!(
                actual_max_deallocations, expect_max_deallocations,
                "{} string interner backend maximum amount of deallocations is greater than expected. expected = {:?}, actual = {:?}",
                <$backend as BackendStats>::NAME,
                expect_max_deallocations,
                actual_max_deallocations,
            );
        }

        #[test]
        fn new_works() {
            let interner = StringInterner::new();
            assert_eq!(interner.len(), 0);
            assert!(interner.is_empty());
            let other = StringInterner::new();
            assert_eq!(interner, other);
        }

        #[test]
        fn is_empty_works() {
            let mut interner = StringInterner::new();
            assert!(interner.is_empty());
            interner.get_or_intern("aa");
            assert!(!interner.is_empty());
        }

        #[test]
        fn clone_works() {
            let mut interner = StringInterner::new();
            assert_eq!(interner.get_or_intern("aa").to_usize(), 0);

            let mut cloned = interner.clone();
            assert_eq!(interner, cloned);
            // And the clone should have the same interned values
            assert_eq!(cloned.get_or_intern("aa").to_usize(), 0);
        }

        #[test]
        fn get_or_intern_works() {
            let mut interner = StringInterner::new();
            // Insert 3 unique strings:
            let aa = interner.get_or_intern("aa").to_usize();
            let bb = interner.get_or_intern("bb").to_usize();
            let cc = interner.get_or_intern("cc").to_usize();
            // All symbols must be different from each other.
            assert_ne!(aa, bb);
            assert_ne!(bb, cc);
            assert_ne!(cc, aa);
            // The length of the string interner must be 3 at this point.
            assert_eq!(interner.len(), 3);
            // Insert the same 3 unique strings, yield the same symbols:
            assert_eq!(interner.resolve(
                <DefaultSymbol>::try_from_usize(aa).unwrap()), Some("aa"));
            assert_eq!(
                interner.get_or_intern("aa").to_usize(),
                aa,
                "'aa' did not produce the same symbol",
            );
            assert_eq!(
                interner.get_or_intern("bb").to_usize(),
                bb,
                "'bb' did not produce the same symbol",
            );
            assert_eq!(
                interner.get_or_intern("cc").to_usize(),
                cc,
                "'cc' did not produce the same symbol",
            );
            assert_eq!(interner.len(), 3);
        }

        #[test]
        fn get_or_intern_static_works() {
            let mut interner = StringInterner::new();
            // Insert 3 unique strings:
            let a = interner.get_or_intern_static("aa").to_usize();
            let b = interner.get_or_intern_static("bb").to_usize();
            let c = interner.get_or_intern_static("cc").to_usize();
            // All symbols must be different from each other.
            assert_ne!(a, b);
            assert_ne!(b, c);
            assert_ne!(c, a);
            // The length of the string interner must be 3 at this point.
            assert_eq!(interner.len(), 3);
            // Insert the same 3 unique strings, yield the same symbols:
            assert_eq!(interner.get_or_intern_static("aa").to_usize(), a);
            assert_eq!(interner.get_or_intern_static("bb").to_usize(), b);
            assert_eq!(interner.get_or_intern_static("cc").to_usize(), c);
            assert_eq!(interner.len(), 3);
        }

        #[test]
        fn resolve_works() {
            let mut interner = StringInterner::new();
            // Insert 3 unique strings:
            let aa = interner.get_or_intern("aa");
            let bb = interner.get_or_intern("bb");
            let cc = interner.get_or_intern("cc");
            assert_eq!(interner.len(), 3);
            // Resolve valid symbols:
            assert_eq!(interner.resolve(aa), Some("aa"));
            assert_eq!(interner.resolve(bb), Some("bb"));
            assert_eq!(interner.resolve(cc), Some("cc"));
            assert_eq!(interner.len(), 3);
            // Resolve invalid symbols:
            let dd = expect_valid_symbol(1000);
            assert_ne!(aa, dd);
            assert_ne!(bb, dd);
            assert_ne!(cc, dd);
            assert_eq!(interner.resolve(dd), None);
        }

        #[test]
        fn resolve_unchecked_works() {
            let mut interner = StringInterner::new();
            // Insert 3 unique strings:
            let aa = interner.get_or_intern("aa");
            let bb = interner.get_or_intern("bb");
            let cc = interner.get_or_intern("cc");
            assert_eq!(interner.len(), 3);
            // Resolve valid symbols:
            assert_eq!(unsafe { interner.resolve_unchecked(aa) }, "aa");
            assert_eq!(unsafe { interner.resolve_unchecked(bb) }, "bb");
            assert_eq!(unsafe { interner.resolve_unchecked(cc) }, "cc");
            assert_eq!(interner.len(), 3);
            // Resolve invalid symbols:
            let dd = expect_valid_symbol(1000);
            assert_ne!(aa, dd);
            assert_ne!(bb, dd);
            assert_ne!(cc, dd);
        }

        #[test]
        fn get_works() {
            let mut interner = StringInterner::new();
            // Insert 3 unique strings:
            let aa = interner.get_or_intern("aa");
            let bb = interner.get_or_intern("bb");
            let cc = interner.get_or_intern("cc");
            assert_eq!(interner.len(), 3);
            // Get the symbols of the same 3 strings:
            assert_eq!(interner.get("aa"), Some(aa));
            assert_eq!(interner.get("bb"), Some(bb));
            assert_eq!(interner.get("cc"), Some(cc));
            assert_eq!(interner.len(), 3);
            // Get the symbols of some unknown strings:
            assert_eq!(interner.get("dd"), None);
            assert_eq!(interner.get("ee"), None);
            assert_eq!(interner.get("ff"), None);
            assert_eq!(interner.len(), 3);
        }

        #[test]
        fn from_iter_works() {
            let strings = ["aa", "bb", "cc", "dd", "ee", "ff"];
            let expected = {
                let mut interner = StringInterner::new();
                for &string in &strings {
                    interner.get_or_intern(string);
                }
                interner
            };
            let actual = strings.into_iter().collect::<StringInterner>();
            assert_eq!(actual.len(), strings.len());
            assert_eq!(actual, expected);
        }

        #[test]
        fn extend_works() {
            let strings = ["aa", "bb", "cc", "dd", "ee", "ff"];
            let expected = {
                let mut interner = StringInterner::new();
                for &string in &strings {
                    interner.get_or_intern(string);
                }
                interner
            };
            let actual = {
                let mut interner = StringInterner::new();
                interner.extend(strings.iter().copied());
                interner
            };
            assert_eq!(actual.len(), strings.len());
            assert_eq!(actual, expected);
        }

        #[test]
        fn iter_works() {
            let mut interner = StringInterner::new();
            let strings = ["aa", "bb", "cc", "dd", "ee", "ff"];
            let symbols = strings.iter().map(|s| interner.get_or_intern(s)).collect::<Vec<_>>();
            let expected_iter = symbols.into_iter().zip(strings);
            assert!(Iterator::eq(expected_iter, &interner));
        }

        #[test]
        fn shrink_to_fit_works() {
            let mut interner = StringInterner::new();
            // Insert 3 unique strings:
            let aa = interner.get_or_intern("aa").to_usize();
            let bb = interner.get_or_intern("bb").to_usize();
            let cc = interner.get_or_intern("cc").to_usize();

            interner.shrink_to_fit();

            assert_eq!(
                interner.get_or_intern("aa").to_usize(),
                aa,
                "'aa' did not produce the same symbol",
            );
            assert_eq!(
                interner.get_or_intern("bb").to_usize(),
                bb,
                "'bb' did not produce the same symbol",
            );
            assert_eq!(
                interner.get_or_intern("cc").to_usize(),
                cc,
                "'cc' did not produce the same symbol",
            );
            assert_eq!(interner.len(), 3);
        }
    };
}

mod bucket_backend {
    use super::*;

    gen_tests_for_backend!(backend::BucketBackend<DefaultSymbol>);
}

mod string_backend {
    use super::*;

    gen_tests_for_backend!(backend::StringBackend<DefaultSymbol>);
}

mod buffer_backend {
    use super::*;

    gen_tests_for_backend!(backend::BufferBackend<DefaultSymbol>);
}
