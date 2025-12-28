use string_interner::{
    backend::{Backend, BucketBackend, BufferBackend, StringBackend},
    DefaultSymbol,
    StringInterner,
};

/// Alphabet containing all characters that may be put into a benchmark string.
const ALPHABET: [u8; 64] = [
    b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h', b'i', b'j', b'k', b'l', b'm', b'n', b'o', b'p',
    b'q', b'r', b's', b't', b'u', b'v', b'w', b'x', b'y', b'z', b'A', b'B', b'C', b'D', b'E', b'F',
    b'G', b'H', b'I', b'J', b'K', b'L', b'M', b'N', b'O', b'P', b'Q', b'R', b'S', b'T', b'U', b'V',
    b'W', b'X', b'Y', b'Z', b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'_', b'-',
];

/// A word builder for benchmark purposes.
///
/// Creates unique words of same sizes.
struct WordBuilder {
    indices: Vec<u8>,
}

impl WordBuilder {
    /// Creates a new word builder for words with given length.
    pub fn new(word_len: usize) -> Self {
        Self {
            indices: vec![0x00; word_len],
        }
    }

    /// Fills the internal buffer with the next unique word indices.
    fn next_indices(&mut self) -> Option<&[u8]> {
        'l: for index in &mut self.indices {
            if *index == (64 - 1) {
                *index = 0;
                continue 'l;
            }
            *index += 1;
            return Some(&self.indices[..]);
        }
        None
    }

    /// Returns the next unique word of the same size.
    fn next_word(&mut self) -> Option<String> {
        self.next_indices()
            .map(|indices| {
                indices
                    .iter()
                    .map(|&index| {
                        assert!(index < 64);
                        ALPHABET[index as usize]
                    })
                    .collect::<Vec<_>>()
            })
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }
}

impl Iterator for WordBuilder {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_word()
    }
}

/// Generates a vector of `len` unique words of the same given length.
pub fn generate_test_strings(len: usize, word_len: usize) -> Vec<String> {
    let words = WordBuilder::new(word_len).take(len).collect::<Vec<_>>();
    assert_eq!(words.len(), len);
    assert_eq!(words[0].len(), word_len);
    words
}

/// The number of strings that are going to be interned in the benchmarks.
pub const BENCH_LEN_STRINGS: usize = 100_000;

/// The length of a single interned string.
pub const BENCH_STRING_LEN: usize = 5;

type FxBuildHasher = fxhash::FxBuildHasher;
type StringInternerWith<B> = StringInterner<B, FxBuildHasher>;

pub trait BackendBenchmark {
    const NAME: &'static str;
    type Backend: Backend;

    fn setup() -> StringInternerWith<Self::Backend> {
        <StringInternerWith<Self::Backend>>::new()
    }

    fn setup_with_capacity(cap: usize) -> StringInternerWith<Self::Backend> {
        <StringInternerWith<Self::Backend>>::with_capacity(cap)
    }

    fn setup_filled(words: &[String]) -> StringInternerWith<Self::Backend> {
        words.iter().collect::<StringInternerWith<Self::Backend>>()
    }

    fn setup_filled_with_ids(
        words: &[String],
    ) -> (
        StringInternerWith<Self::Backend>,
        Vec<<Self::Backend as Backend>::Symbol>,
    ) {
        let mut interner = <StringInternerWith<Self::Backend>>::new();
        let word_ids = words
            .iter()
            .map(|word| interner.get_or_intern(word))
            .collect::<Vec<_>>();
        (interner, word_ids)
    }
}

pub struct BenchBucket;
impl BackendBenchmark for BenchBucket {
    const NAME: &'static str = "BucketBackend";
    type Backend = BucketBackend<DefaultSymbol>;
}

pub struct BenchString;
impl BackendBenchmark for BenchString {
    const NAME: &'static str = "StringBackend";
    type Backend = StringBackend<DefaultSymbol>;
}

pub struct BenchBuffer;
impl BackendBenchmark for BenchBuffer {
    const NAME: &'static str = "BufferBackend";
    type Backend = BufferBackend<DefaultSymbol>;
}
