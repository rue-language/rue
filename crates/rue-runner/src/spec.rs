use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;

/// Loader for normative specification files
pub struct SpecLoader {
    pub(crate) spec_file: Utf8PathBuf,
    pub(crate) specification: Specification,
}

/// Normative specification containing language rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Specification {
    /// Metadata about the specification
    pub metadata: SpecMetadata,

    /// Normative rules organized by category
    pub rules: IndexMap<String, IndexMap<String, Rule>>,
}

/// Metadata about the specification file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecMetadata {
    /// Version of the specification
    pub version: String,

    /// Description of the specification
    pub description: String,

    /// Language version this spec applies to
    pub language_version: String,
}

/// Individual normative rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Human-readable description of the rule
    pub description: String,

    /// Required behavior for compliant implementations
    pub requirement: String,

    /// Optional rationale for the rule
    pub rationale: Option<String>,

    /// Cross-references to related rules
    pub references: Vec<String>,

    /// Examples demonstrating the rule
    pub examples: Vec<RuleExample>,
}

/// Example demonstrating a rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleExample {
    /// Description of what the example demonstrates
    pub description: String,

    /// Rue code demonstrating the rule
    pub code: String,

    /// Expected behavior (compile success/failure, runtime behavior, etc.)
    pub expected: String,
}

impl SpecLoader {
    /// Create a new spec loader for the given specification file
    pub fn new(spec_file: &Utf8Path) -> Result<Self> {
        let content = fs::read_to_string(spec_file)
            .with_context(|| format!("Failed to read specification file: {}", spec_file))?;

        let specification: Specification = toml::from_str(&content)
            .with_context(|| format!("Failed to parse specification file: {}", spec_file))?;

        Ok(Self {
            spec_file: spec_file.to_owned(),
            specification,
        })
    }

    /// Get the loaded specification
    pub fn specification(&self) -> &Specification {
        &self.specification
    }

    /// Get a specific rule by category and name
    pub fn get_rule(&self, category: &str, name: &str) -> Option<&Rule> {
        self.specification
            .rules
            .get(category)
            .and_then(|cat| cat.get(name))
    }

    /// Validate that all referenced specs exist
    pub fn validate_spec_references(&self, spec_refs: &[&str]) -> Result<()> {
        for spec_ref in spec_refs {
            if let Some((category, name)) = spec_ref.split_once('.') {
                if self.get_rule(category, name).is_none() {
                    return Err(anyhow::anyhow!(
                        "Specification reference not found: {} (category: {}, rule: {})",
                        spec_ref,
                        category,
                        name
                    ));
                }
            } else {
                // Check if it's a category reference
                if !self.specification.rules.contains_key(*spec_ref) {
                    return Err(anyhow::anyhow!(
                        "Specification category not found: {}",
                        spec_ref
                    ));
                }
            }
        }
        Ok(())
    }

    /// Get all rules in a category
    pub fn get_category_rules(&self, category: &str) -> Option<&IndexMap<String, Rule>> {
        self.specification.rules.get(category)
    }

    /// List all available categories
    pub fn categories(&self) -> Vec<&str> {
        self.specification
            .rules
            .keys()
            .map(|s| s.as_str())
            .collect()
    }

    /// List all rules (as "category.rule" strings)
    pub fn all_rule_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for (category, rules) in &self.specification.rules {
            for rule_name in rules.keys() {
                names.push(format!("{}.{}", category, rule_name));
            }
        }
        names
    }
}

impl Specification {
    /// Create a default specification for testing
    pub fn default_test_spec() -> Self {
        let mut rules = IndexMap::new();

        // Add a sample type system rule
        let mut type_system = IndexMap::new();
        type_system.insert(
            "assignment-strictness".to_string(),
            Rule {
                description: "Variable assignment type checking".to_string(),
                requirement: "Assignments must maintain type compatibility".to_string(),
                rationale: Some("Type safety prevents runtime errors".to_string()),
                references: vec![],
                examples: vec![
                    RuleExample {
                        description: "Valid assignment".to_string(),
                        code: "let x: i64 = 42;".to_string(),
                        expected: "compile-pass".to_string(),
                    },
                    RuleExample {
                        description: "Invalid assignment".to_string(),
                        code: "let x: i64; x = \"hello\";".to_string(),
                        expected: "compile-fail".to_string(),
                    },
                ],
            },
        );

        rules.insert("type-system".to_string(), type_system);

        Self {
            metadata: SpecMetadata {
                version: "1.0.0".to_string(),
                description: "Rue Language Normative Specification".to_string(),
                language_version: "0.1.0".to_string(),
            },
            rules,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_valid_spec() {
        let spec = Specification::default_test_spec();
        let toml_content = toml::to_string_pretty(&spec).unwrap();

        let temp_file = NamedTempFile::new().unwrap();
        fs::write(temp_file.path(), toml_content).unwrap();

        let temp_path = Utf8PathBuf::try_from(temp_file.path().to_path_buf()).unwrap();
        let loader = SpecLoader::new(&temp_path).unwrap();

        assert_eq!(loader.specification().metadata.version, "1.0.0");
        assert!(
            loader
                .get_rule("type-system", "assignment-strictness")
                .is_some()
        );
    }

    #[test]
    fn test_validate_spec_references() {
        let spec = Specification::default_test_spec();
        let toml_content = toml::to_string_pretty(&spec).unwrap();

        let temp_file = NamedTempFile::new().unwrap();
        fs::write(temp_file.path(), toml_content).unwrap();

        let temp_path = Utf8PathBuf::try_from(temp_file.path().to_path_buf()).unwrap();
        let loader = SpecLoader::new(&temp_path).unwrap();

        // Valid reference
        assert!(
            loader
                .validate_spec_references(&["type-system.assignment-strictness"])
                .is_ok()
        );

        // Invalid reference
        assert!(loader.validate_spec_references(&["invalid.rule"]).is_err());

        // Category reference
        assert!(loader.validate_spec_references(&["type-system"]).is_ok());
    }

    #[test]
    fn test_list_categories_and_rules() {
        let spec = Specification::default_test_spec();
        let toml_content = toml::to_string_pretty(&spec).unwrap();

        let temp_file = NamedTempFile::new().unwrap();
        fs::write(temp_file.path(), toml_content).unwrap();

        let temp_path = Utf8PathBuf::try_from(temp_file.path().to_path_buf()).unwrap();
        let loader = SpecLoader::new(&temp_path).unwrap();

        let categories = loader.categories();
        assert_eq!(categories, vec!["type-system"]);

        let all_rules = loader.all_rule_names();
        assert_eq!(all_rules, vec!["type-system.assignment-strictness"]);
    }
}
