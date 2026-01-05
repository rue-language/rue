//! Input mutation strategies for fuzzing.

/// A simple pseudo-random number generator.
///
/// Uses xorshift64 for speed and reproducibility.
pub struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    pub fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u64() as usize) % max
    }
}

/// Mutate input bytes in place.
pub fn mutate(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    if input.is_empty() {
        // Add a random byte
        input.push(rng.next_u8());
        return;
    }

    // Choose a mutation strategy
    match rng.next_usize(10) {
        0 => bit_flip(input, rng),
        1 => byte_flip(input, rng),
        2 => byte_insert(input, rng),
        3 => byte_delete(input, rng),
        4 => byte_copy(input, rng),
        5 => interesting_value(input, rng),
        6 => arithmetic(input, rng),
        7 => splice_keyword(input, rng),
        8 => shuffle_chunk(input, rng),
        _ => repeat_chunk(input, rng),
    }
}

fn bit_flip(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    let idx = rng.next_usize(input.len());
    let bit = rng.next_usize(8);
    input[idx] ^= 1 << bit;
}

fn byte_flip(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    let idx = rng.next_usize(input.len());
    input[idx] = rng.next_u8();
}

fn byte_insert(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    if input.len() >= 65536 {
        return; // Don't grow too large
    }
    let idx = rng.next_usize(input.len() + 1);
    let byte = rng.next_u8();
    input.insert(idx, byte);
}

fn byte_delete(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    if input.len() <= 1 {
        return;
    }
    let idx = rng.next_usize(input.len());
    input.remove(idx);
}

fn byte_copy(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    if input.len() >= 65536 {
        return;
    }
    let src = rng.next_usize(input.len());
    let dst = rng.next_usize(input.len() + 1);
    let byte = input[src];
    input.insert(dst, byte);
}

/// Replace a byte with an "interesting" value.
fn interesting_value(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    const INTERESTING: &[u8] = &[
        0, 1, 2, 16, 32, 64, 100, 127, 128, 255, // boundaries
        b'0', b'9', b'a', b'z', b'A', b'Z', // ASCII
        b' ', b'\t', b'\n', b'\r', // whitespace
        b'{', b'}', b'(', b')', b'[', b']', // brackets
        b'+', b'-', b'*', b'/', b'=', b'<', b'>', // operators
    ];

    let idx = rng.next_usize(input.len());
    let val = INTERESTING[rng.next_usize(INTERESTING.len())];
    input[idx] = val;
}

fn arithmetic(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    let idx = rng.next_usize(input.len());
    let delta = (rng.next_usize(35) as i8) - 16; // -16 to +16
    input[idx] = input[idx].wrapping_add(delta as u8);
}

/// Insert a Rue keyword at a random position.
fn splice_keyword(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    const KEYWORDS: &[&[u8]] = &[
        b"fn",
        b"let",
        b"mut",
        b"if",
        b"else",
        b"match",
        b"while",
        b"loop",
        b"break",
        b"continue",
        b"return",
        b"true",
        b"false",
        b"struct",
        b"enum",
        b"impl",
        b"i32",
        b"i64",
        b"u32",
        b"u64",
        b"bool",
        b"->",
        b"=>",
        b"::",
        b"==",
        b"!=",
        b"<=",
        b">=",
        b"&&",
        b"||",
        b"<<",
        b">>",
    ];

    if input.len() >= 65536 {
        return;
    }

    let keyword = KEYWORDS[rng.next_usize(KEYWORDS.len())];
    let idx = rng.next_usize(input.len() + 1);

    for (i, &b) in keyword.iter().enumerate() {
        input.insert(idx + i, b);
    }
}

fn shuffle_chunk(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    if input.len() < 4 {
        return;
    }

    let chunk_size = rng.next_usize(input.len() / 2).max(2).min(16);
    let src = rng.next_usize(input.len() - chunk_size);
    let dst = rng.next_usize(input.len() - chunk_size);

    // Swap chunks
    for i in 0..chunk_size {
        input.swap(src + i, dst + i);
    }
}

fn repeat_chunk(input: &mut Vec<u8>, rng: &mut SimpleRng) {
    if input.len() >= 65536 || input.len() < 2 {
        return;
    }

    let chunk_size = rng.next_usize(input.len() / 2).max(1).min(16);
    let src = rng.next_usize(input.len() - chunk_size);

    let chunk: Vec<u8> = input[src..src + chunk_size].to_vec();
    let dst = rng.next_usize(input.len() + 1);

    for (i, &b) in chunk.iter().enumerate() {
        input.insert(dst + i, b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rng_reproducibility() {
        let mut rng1 = SimpleRng::new(12345);
        let mut rng2 = SimpleRng::new(12345);

        for _ in 0..100 {
            assert_eq!(rng1.next_u64(), rng2.next_u64());
        }
    }

    #[test]
    fn test_mutation_changes_input() {
        let mut rng = SimpleRng::new(42);
        let original = b"fn main() -> i32 { 42 }".to_vec();

        // Run many mutations to ensure at least some change the input
        let mut changed = false;
        for _ in 0..100 {
            let mut input = original.clone();
            mutate(&mut input, &mut rng);
            if input != original {
                changed = true;
                break;
            }
        }

        assert!(changed, "mutations should change input");
    }

    #[test]
    fn test_empty_input_mutation() {
        let mut rng = SimpleRng::new(42);
        let mut input = Vec::new();

        mutate(&mut input, &mut rng);

        assert!(!input.is_empty(), "empty input should grow");
    }
}
