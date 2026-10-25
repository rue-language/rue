use crate::SyntaxKind;

/// A set of token kinds, as a bit set over the token range of [`SyntaxKind`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TokenSet([u64; 3]);

impl TokenSet {
    pub const EMPTY: TokenSet = TokenSet([0; 3]);

    pub const fn new(kinds: &[SyntaxKind]) -> TokenSet {
        let mut bits = [0u64; 3];
        let mut i = 0;
        while i < kinds.len() {
            let kind = kinds[i] as usize;
            assert!(kind < 192, "TokenSet only holds token kinds");
            bits[kind / 64] |= 1 << (kind % 64);
            i += 1;
        }
        TokenSet(bits)
    }

    pub const fn union(self, other: TokenSet) -> TokenSet {
        TokenSet([
            self.0[0] | other.0[0],
            self.0[1] | other.0[1],
            self.0[2] | other.0[2],
        ])
    }

    pub const fn contains(&self, kind: SyntaxKind) -> bool {
        let kind = kind as usize;
        kind < 192 && self.0[kind / 64] & (1 << (kind % 64)) != 0
    }
}

#[test]
fn token_kinds_fit_in_a_token_set() {
    assert!((SyntaxKind::SOURCE_FILE as usize) <= 192);
}
