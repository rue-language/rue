//! "Did you mean?" suggestion engine using edit distance

use std::collections::HashMap;

/// "Did you mean?" suggestion engine
#[derive(Debug, Default)]
pub struct SuggestionEngine {
    /// Known identifiers organized by scope
    identifiers: HashMap<String, Vec<String>>,
}

impl SuggestionEngine {
    /// Create a new suggestion engine
    pub fn new() -> Self {
        Self::default()
    }

    /// Add known identifiers for a scope
    pub fn add_scope(&mut self, scope_name: impl Into<String>, identifiers: Vec<String>) {
        self.identifiers.insert(scope_name.into(), identifiers);
    }

    /// Add a single identifier to a scope
    pub fn add_identifier(&mut self, scope_name: impl Into<String>, identifier: impl Into<String>) {
        self.identifiers
            .entry(scope_name.into())
            .or_default()
            .push(identifier.into());
    }

    /// Clear all identifiers for a scope
    pub fn clear_scope(&mut self, scope_name: &str) {
        self.identifiers.remove(scope_name);
    }

    /// Clear all identifiers
    pub fn clear(&mut self) {
        self.identifiers.clear();
    }

    /// Find similar identifiers using edit distance
    pub fn suggest_similar(&self, target: &str, scope: Option<&str>) -> Vec<String> {
        let mut candidates = Vec::new();

        // Determine which scopes to search
        let scopes: Vec<&str> = match scope {
            Some(scope) => vec![scope],
            None => self.identifiers.keys().map(|s| s.as_str()).collect(),
        };

        // Search for similar identifiers
        for scope_name in scopes {
            if let Some(identifiers) = self.identifiers.get(scope_name) {
                for identifier in identifiers {
                    let distance = edit_distance(target, identifier);
                    // Accept suggestions with small edit distance relative to length
                    if should_suggest(target, identifier, distance) {
                        candidates.push((distance, identifier.clone()));
                    }
                }
            }
        }

        // Sort by distance and take the best matches
        candidates.sort_by_key(|(distance, _)| *distance);
        candidates
            .into_iter()
            .take(3)
            .map(|(_, name)| name)
            .collect()
    }

    /// Get the best suggestion for a target
    pub fn best_suggestion(&self, target: &str, scope: Option<&str>) -> Option<String> {
        self.suggest_similar(target, scope).into_iter().next()
    }

    /// Check if an identifier exists in any scope
    pub fn contains(&self, identifier: &str) -> bool {
        self.identifiers
            .values()
            .any(|ids| ids.contains(&identifier.to_string()))
    }

    /// Check if an identifier exists in a specific scope
    pub fn contains_in_scope(&self, identifier: &str, scope: &str) -> bool {
        self.identifiers
            .get(scope)
            .map(|ids| ids.contains(&identifier.to_string()))
            .unwrap_or(false)
    }
}

/// Compute Levenshtein edit distance between two strings
#[allow(clippy::needless_range_loop)]
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    // Handle empty strings
    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    // Create DP table
    let mut dp = vec![vec![0; b_len + 1]; a_len + 1];

    // Initialize base cases
    for i in 0..=a_len {
        dp[i][0] = i;
    }
    for j in 0..=b_len {
        dp[0][j] = j;
    }

    // Fill DP table
    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };

            dp[i][j] = std::cmp::min(
                std::cmp::min(
                    dp[i - 1][j] + 1, // deletion
                    dp[i][j - 1] + 1, // insertion
                ),
                dp[i - 1][j - 1] + cost, // substitution
            );
        }
    }

    dp[a_len][b_len]
}

/// Determine if a suggestion should be offered based on edit distance
fn should_suggest(target: &str, candidate: &str, distance: usize) -> bool {
    // Don't suggest if the strings are identical
    if distance == 0 {
        return false;
    }

    let target_len = target.len();
    let _candidate_len = candidate.len();

    // For very short strings, be strict
    if target_len <= 2 {
        distance == 1
    }
    // For short strings, allow small distances
    else if target_len <= 5 {
        distance <= 2
    }
    // For longer strings, allow distance proportional to length
    else {
        distance <= 3 && distance <= target_len / 2
    }
}

/// Generate suggestions for common typos
pub fn generate_typo_suggestions(word: &str) -> Vec<String> {
    let mut suggestions = Vec::new();
    let chars: Vec<char> = word.chars().collect();

    // Transpositions (swapped adjacent characters)
    for i in 0..chars.len().saturating_sub(1) {
        let mut transposed = chars.clone();
        transposed.swap(i, i + 1);
        suggestions.push(transposed.into_iter().collect());
    }

    // Common replacements
    let replacements = [
        ('0', 'o'),
        ('o', '0'),
        ('1', 'l'),
        ('l', '1'),
        ('i', 'j'),
        ('j', 'i'),
    ];

    for (from, to) in &replacements {
        for (i, &ch) in chars.iter().enumerate() {
            if ch == *from {
                let mut replaced = chars.clone();
                replaced[i] = *to;
                suggestions.push(replaced.into_iter().collect());
            }
        }
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("a", ""), 1);
        assert_eq!(edit_distance("", "a"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("saturday", "sunday"), 3);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "def"), 3);
    }

    #[test]
    fn test_should_suggest() {
        // Very short strings
        assert!(should_suggest("x", "y", 1));
        assert!(!should_suggest("x", "xyz", 2));

        // Short strings
        assert!(should_suggest("foo", "fo", 1));
        assert!(should_suggest("foo", "bar", 2));
        assert!(!should_suggest("foo", "xyz", 3));

        // Longer strings
        assert!(should_suggest("variable", "varible", 1));
        assert!(should_suggest("function", "funtion", 1));
        assert!(!should_suggest("function", "xyz", 8));

        // Don't suggest identical strings
        assert!(!should_suggest("test", "test", 0));
    }

    #[test]
    fn test_suggestion_engine() {
        let mut engine = SuggestionEngine::new();

        engine.add_scope(
            "global",
            vec![
                "variable".to_string(),
                "function".to_string(),
                "constant".to_string(),
            ],
        );

        engine.add_scope(
            "local",
            vec!["local_var".to_string(), "local_fn".to_string()],
        );

        // Test suggestions
        let suggestions = engine.suggest_similar("varible", None);
        assert!(suggestions.contains(&"variable".to_string()));

        let suggestions = engine.suggest_similar("funtion", None);
        assert!(suggestions.contains(&"function".to_string()));

        let suggestions = engine.suggest_similar("local_va", Some("local"));
        assert!(suggestions.contains(&"local_var".to_string()));

        // Test no suggestions for very different strings
        let suggestions = engine.suggest_similar("xyz", None);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_typo_suggestions() {
        let suggestions = generate_typo_suggestions("test");
        assert!(suggestions.contains(&"etst".to_string())); // transposition
        assert!(suggestions.contains(&"tset".to_string())); // transposition

        let suggestions = generate_typo_suggestions("l0op");
        assert!(suggestions.contains(&"loop".to_string())); // 0 -> o
        assert!(suggestions.contains(&"l00p".to_string())); // o -> 0
    }
}
